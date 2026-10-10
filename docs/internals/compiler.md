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
   template text keeps `tmp#` for `tmp` (`Parser::mark`). Only names in places
   that declare or refer to something are marked, not fields, variants, item
   names or built-in types, and never what a hole inserts.
2. Each expansion numbers its marks, `tmp#` to `tmp#7`
   (`jihoo_syntax::number_marks`), with a number unique in the compilation. The
   lexer accepts such names only in macro output, so a program cannot write one.
3. Once the output is parsed, `crates/jihoo-sema/src/hygiene.rs` decides what
   each marked name of the expansion is. A local it declares keeps its marked
   name, which no other code can spell, so the scopes keep it apart from the
   caller's. Any other name becomes `#M:name`, read as if written in module M,
   the macro's own (`Env::key`, and `Env::viewer` for privacy), or the
   caller's module if the macro's does not have it. Code passed on to another
   macro as an argument is text, so its names are renamed the same way first.

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

## Other modules

| file | what |
|------|------|
| `crates/jihoo-sema/src/enums.rs` | enum construction, and inferring generic enum arguments |
| `crates/jihoo-sema/src/tasks.rs` | `go`, channels and `select` |
| `crates/jihoo-sema/src/asm.rs` | `asm` operands and templates |
| `crates/jihoo-sema/src/tests.rs` | the frontend's unit tests |
