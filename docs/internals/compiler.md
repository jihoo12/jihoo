# Compiler

The frontend is `crates/jihoo-sema`: it type checks a program and lowers it to
JIR in the same pass, runs compile-time code on the VM, and expands macros.

## One pass: check and lower

Type checking and lowering happen together, in the same shape as Zig's Sema:
each expression is checked and turned into typed JIR at the same time, with the
expected type passed down so that literals and inferred forms know what to
become. The operator rules live in `crates/jihoo-ir/src/types.rs` and are shared
with the IR verifier.

Errors are reported per function: analysis of a function stops at its first
error, and the others go on, so one run shows the first error in every function.

## Lazy analysis

`crates/jihoo-sema/src/env.rs` analyzes module-level items lazily: struct
fields, signatures, constants and function bodies are computed on first use and
memoized. So items can refer to each other in any order, and modules can
import each other in a cycle. Afterwards every non-generic function of every
module is lowered; generic ones only per instance. A query that needs its own result (`const A = A`, or `comptime f()`
inside `f`) is reported as a cycle.

## Compile-time evaluation

`crates/jihoo-sema/src/comptime.rs` evaluates `const`, `comptime`, array
lengths, comptime arguments and macros. The expression is lowered into a helper
function, which runs on the VM with a step limit, and the result is converted
back into JIR constants. The functions runs need are kept in one module per
compilation that only grows: a run adds its helper and whatever it calls that
no earlier run needed, so a thousand constants that call the same functions
copy them once, not a thousand times. A run adds nothing unless everything it
calls compiles, so every function in that module has all its callees there. Since this runs on the VM, it works for freestanding programs
too, but cannot use pointers, `syscall`, `asm` or C functions.

## Contracts

`crates/jihoo-sema/src/contracts.rs` lowers `requires` and `ensures`
([Contracts](../language/contracts.md)): `requires` checks at the start of the
body, after the entry block's hoisted constants, so a call of itself that
became a jump checks them again; and one exit block that every `return` of a
function with `ensures` jumps to, which checks those clauses against `result`
and copies of the parameters made at the start. A false clause calls a function
of its own, `f.requires.0` or `f.ensures.1`, whose body is `trap`, so JIR and
the backends see nothing new, and the VM's error names the clause.

`crates/jihoo-sema/src/refute.rs` then tries to break every contract, once the
whole program is lowered without errors (earlier, a recursive function would
still be in progress). For each function with a contract whose parameter types
it can make up, it lowers a helper `f.pre` that evaluates the `requires`
clauses, gets `f` and its callees ready like a compile-time run, and calls them
on the VM with made-up inputs: special values first, then seeded random ones.
When a call fails, the VM's call stack (`Vm::trace`) says how: an innermost
frame `f.ensures.N` holds the value returned, `g.requires.N` sits on the frame
of the call of `g` that broke it, and anything else is a trap. Errors that say
the code cannot run on the VM (pointers, `syscall`) make it give up on the
function. The failing input is shrunk while the failure stays the same.

## Rewrite rules

`crates/jihoo-sema/src/rules.rs` applies and checks rules
([Rewrite rules](../language/rules.md)). Rules are indexed by the key of the
function their pattern calls. When `FnCx::call` resolves a call to a key that
has rules, it matches the call's AST against each pattern: items by key,
literals by value, the rule's parameters standing for any code. It then
lowers the matched parts in source order with a snapshot taken: their types
are unified strictly against the rule's parameters, as inference does. If
they fit, the call goes to the rule (an instance, for a generic one).
Otherwise the snapshot is restored and the call compiled as written. Rules
are off in a rule's body and in the pattern helper.

A rule also moves code, which the checks below cannot see: the functions a
call passes are called as the rule calls them, and the pattern's own calls
and operators run after all the parts instead of between them. So
`apply_rule` walks the pattern in evaluation order (`pattern_steps`) and,
for each part, looks at the JIR it just lowered, from a `Mark` of each
block's length. Using `crates/jihoo-sema/src/effects.rs`, it
gives up (and rolls back) unless:

- a part evaluated after one of the pattern's calls or operators is
  *inert*, or every such call or operator before it is. An operator is inert
  on numbers and bools, except integer `/` and `%`; a call is inert when its
  function (not generic) is;
- a part right of `&&` or `||` is inert;
- a function value a part gives was made by that part's code, a `funcref`
  (possibly a hoisted one) or `closure` of a function that is inert;
- no other part's type holds a function value.

`Env::effects_of` summarizes a JIR function, through everything it calls, as
a set: prints, reads input, uses shared state (cells, pointers, atomics,
channels, tasks), runs unknown code (function values, C functions,
`syscall`, `asm`, compile-time-only instructions, functions still being
compiled), and may fail (integer division, bounds checks, `trap`, `pop`,
any jump back to an earlier block, recursion). Inert is the empty set.
Summaries are memoized, except those that met a function still being
compiled or summarized, which assume the worst of it. Closures capture by
value, so a closure's captured values are read-only in its body.

Once the program is lowered, each rule (plus each generic rule at `i64` for
its type parameters) gets a helper `f.pattern` that evaluates the pattern.
Both run on the VM with inputs from `refute.rs`'s generator, plus functions
written for parameters of function type (`f.arg.N`), which are all inert.
Results are compared as constants (any NaN equals any other), along with
what each side prints; traps only match traps. A rule or pattern that
reads standard input is refused, since both run with none.

## Copy-free functions

`crates/jihoo-sema/src/nocopy.rs` checks `nocopy` functions
([Copy-free functions](../language/nocopy.md)) in native programs, right after
the ownership pass. It starts from the function's consuming version when there
is one (the caller is taken to hand its arguments over), and walks the calls
the pass chose, breadth first. The first `clone` that copies memory
(`jihoo_ir::cost::in_function`, the list `--cost` prints) is an error at the
copy or at the call that leads to it. A call of a function value is followed
when the register can only hold one known function. When no parameter of the
root can hold a function value, every function named by a `funcref` or
`closure` in the walk is treated as called. Otherwise such a call is an error.
Sema writes source positions (`loc`) for programs with a `nocopy` function so
that errors can point at the copy, and removes them again unless `--cost` asked
for them.

## Generics

`crates/jihoo-sema/src/generic.rs` makes one instance of a generic function per
distinct set of compile-time arguments, named `name.N` in JIR (`max.0`,
`max.1`). The body is checked per instance. Callers only need an instance's
signature, so instances can be recursive. Generic structs become one struct
type per argument list, `$"Pair(i64)"`.

## Macros

`crates/jihoo-sema/src/macros.rs` runs macros on the VM. A macro's `quote` is
JIR's `quote` instruction, which builds source text from template pieces and
holes; the result is parsed again (`jihoo_syntax::parse_expr`, `parse_stmts`,
`parse_items`) and analyzed in the caller's scope. Item macros are expanded
before analysis, in rounds (`expand_item_macros` in `crates/jihoo-sema/src/lib.rs`).

Macros are hygienic ([Macros](../language/macros.md#scope-and-hygiene)), with
code that stays text until it is parsed:

1. When a macro is parsed, the names its templates write get a *mark*: the
   template text keeps `tmp#` for `tmp` (`Parser::mark`). Names in places that
   declare or refer to something are marked, and field names; not variants,
   item names or built-in types, and never what a hole inserts.
2. Each expansion numbers its marks, `tmp#` to `tmp#7`
   (`jihoo_syntax::number_marks`), with a number unique in the compilation. The
   lexer accepts such names only in macro output, so a program cannot write one.
3. Once the output is parsed, `crates/jihoo-sema/src/hygiene.rs` decides what
   each marked name of the expansion is. A local it declares keeps its marked
   name, which no other code can spell, so the scopes keep it apart from the
   caller's. Any other name becomes `#M:name`, read as if written in module M,
   the macro's own (`Env::key`, and `Env::viewer` for privacy), or the
   caller's module if the macro's does not have it. A field name becomes
   `#M:x` too: `Env::field` lets it reach the private fields of module M's
   structs. Code passed on to another macro as an argument is text, so its
   names are renamed the same way first.

## Closures

`crates/jihoo-sema/src/closures.rs` lifts the body of every anonymous function
into a function of its own, `fn.N`, whose first parameters are the captured
values. A closure without captures is a `funcref`; one with captures a
`closure` instruction. A closure passed to a `comptime` parameter becomes part
of the instance's identity, and its captures are passed as hidden arguments.

## Patterns and equality

`crates/jihoo-sema/src/patterns.rs` checks `match` for exhaustiveness and
useless arms with Maranget's usefulness algorithm, and lowers it to `tag`,
`payload`, comparisons and branches. `crates/jihoo-sema/src/equality.rs` makes a
helper function `fn.eq.N` for each type compared with `==`.

## Places

`crates/jihoo-sema/src/place.rs` handles assignment targets and reads of nested
parts: variables, fields, elements, `*p`, cells. A nested update such as
`b.cells[r][c].v = x` becomes one `setpath` (or `cellset`), and a nested read one
`getpath`, so the VM can update in place ([VM and GC](vm.md)).

## Ownership

Native lists have no GC. `crates/jihoo-ir/src/ownership.rs` runs on the
finished JIR of a native module and decides, from liveness, where an owning
value (a list, or a value holding one) moves, where it is cloned, and where
it is dropped; it also makes the consuming versions of functions (`f.own`),
moves the parts out of a value that is only taken apart (`Some((rest,
x))`, in each arm of a `match`), freeing just the parts left in it, and moves a part (a field, an element, or a path of them) out of a value
with `take` where the part is written again before anything reads it.
It needs no borrow checker because values never share memory: pointers may
not reach owning values (checked in `place.rs`), and references into values
are [paths](../language/paths.md) (`paths.rs`), which hold indices.

## Other modules

| file | what |
|------|------|
| `crates/jihoo-sema/src/enums.rs` | enum construction, and inferring generic enum arguments |
| `crates/jihoo-sema/src/tasks.rs` | `go`, channels and `select` |
| `crates/jihoo-sema/src/asm.rs` | `asm` operands and templates |
| `crates/jihoo-sema/src/tests.rs` | the frontend's unit tests |
