# Design: code as data

Status: the five steps are done; what is left is in [Done when](#done-when).

## Why

A pipeline of list functions written with `lists` builds a new list at every
step. `examples/pipe.jh` fuses `map`, `filter` and a
sum into one loop with a macro, and the numbers say that this is where the time
goes. Natively, over a million elements, fifty times:

| version | time |
|---------|------|
| `lists.map`, `lists.filter`, `lists.fold` | 134 ms |
| the same with `comptime` closures | 130 ms |
| written out as one loop by hand | 45 ms |
| fused by the `pipe!` macro | 44 ms |

Specializing the function calls gains nearly nothing; not building the lists in
between gains a factor of three. Fusion is what makes functional code as fast as
a loop.

Other languages fuse in the compiler (Haskell's rewrite rules) or through lazy
iterator types that the optimizer inlines (Rust). jihoo can do it in a library:
macros already run ordinary jihoo on the VM while compiling. What stops it is
how little a macro can do with the code it is given. The prototype had to be
called as

```jihoo
pipe!("s", xs, "map filter sum",
    fn(x: i64) -> i64 { return x * 3 + k },
    fn(x: i64) -> bool { return x % 2 == 0 },
    unused)
```

where the goal is

```jihoo
let s = fuse!(xs, map(fn(x: i64) -> i64 { return x * 3 + k }),
                  filter(fn(x: i64) -> bool { return x % 2 == 0 }),
                  sum())
```

Each difference is something macros lack:

1. Code (`expr`) can only be handled inside a macro body, so the code that
   builds a stage cannot be a function of its own.
2. A macro takes a fixed number of arguments, so unused slots must be filled.
3. A macro cannot look inside code, so the stages are named in a string instead
   of being read from `map(f)`.
4. A macro that makes statements cannot be used as an expression, so the
   result goes into a variable the caller names.
5. A macro does not know the types of the code it is given, so the sum is an
   `i64`.

The same gaps stop other features from being libraries: deriving equality,
printing or serialization for a struct needs to read the struct's fields (3);
a test framework needs a variable number of cases (2). Every feature that is a
library is one that the VM, the native and the freestanding profiles do not
each have to implement.

## What compile time is for

Running code while compiling is not the point by itself. A program that only
computes values at compile time, such as a table of primes, could as well be a
separate program that writes a file, which is how Go does it (`go generate`).
What a generator outside the compiler cannot do is see the program: where a
pipeline is written, which local variables its functions use, what type its
elements have. Fusing pipelines that way means parsing and type checking jihoo
again, outside the compiler, and keeping the output in step with the source.

So these steps strengthen what compile-time code can see and give back, not
what it can compute: code it is handed, the shape of that code, the types of
the program. The test for a feature is whether it needs to know the code or the
types where it is used. If it does not, a script that writes a file is simpler,
and fine.

## Steps

Each step is useful on its own and shortens `examples/pipe.jh`, which serves as
the measure. They are ordered by how much they unlock against what they cost.

### 1. Compile-time functions (done)

A function whose signature mentions `expr`, `stmts` or `items` becomes a
*compile-time function*: it may only be used by macros and other
compile-time functions, and it is never part of the compiled program. Such a
signature used to be an error (`crates/jihoo-sema/src/env.rs`, "only available
in macros").

The check moved from the declaration to the use: sema knows when it analyzes
code for the VM at compile time (`in_macro`), which such a function's body now
is, so a call or a function value in runtime code is the error instead
(`check_code_fn` in `crates/jihoo-sema/src/lib.rs`). Code values stay text on
the VM.

Fixes gap 1. Done: `examples/pipe.jh` builds its stages with two such functions.
Their templates are hygienic as part of the calling macro's expansion, so the
other names they write resolve where the macro is defined, not where the
function is.

### 2. Variable numbers of code arguments (done)

The last parameter of a macro may be `rest: [expr]`: the remaining arguments,
as a list of code. Lists already exist on the VM, so the macro walks them with
`for`.

Fixes gap 2. Done: `examples/pipe.jh` takes one function per stage, and binds
each to a variable named with `unique` before the loop.

### 3. Looking inside code (done)

A `meta` library module gives the shape of a piece of code, one level at a time:

```jihoo
pub enum Node {
    Call(str, [expr]),           // f(a, b): the callee's name and the arguments
    Name(str),
    Int(i64),
    Str(str),
    Binary(str, expr, expr),     // the operator and both sides
    Func([str], expr),           // fn(x, y) { ... }: parameter names and the whole literal
    Other,                       // anything else; use the code as a whole
}

pub fn node(e: expr) -> Node     // a compile-time function (step 1)
```

The children are `expr` again, not nodes: a macro looks only as deep as it
needs to and puts the rest back with holes, as it does today. Building code
stays with `quote`.

Done, with the parser written in jihoo (`lib/meta.jh`) rather than a builtin
around `jihoo_syntax`: that needed no VM instruction that parses, only
`codetext` (JIR 14), the text of code with its hygiene marks and back, which
the builtins `code_text` and `to_code` expose. `node` finds the operator that
applies last outside brackets and strings, and otherwise tells calls, names,
integer and string literals and anonymous functions apart. Hygiene holds
because the children are cut out of the text unchanged: code from the caller
stays the caller's, and marked names stay marked.

A struct or enum may now hold code, as `Node` does, and a function whose
resolved signature holds code is a compile-time function, even when it does
not write `expr` itself (`Env::holds_code`). Such types are left out of the
compiled module, and a function of the program that uses one is an error.

`tests/diff-lists/fusion.jh` checks `node` on the shapes it knows, and fused
pipelines against `lists`, on the VM and natively. What remains open: `meta`
uses `str`, so freestanding programs cannot import it, and the jihoo parser
is not yet checked against the Rust one on every file in the repository.

Fixes gap 3. `examples/pipe.jh` is now `fuse!("s", xs, map(f), filter(g),
sum())`, as fast as the loop written by hand.

### 4. Statement macros as expressions (done)

`let s = fuse!(...)` needs statements where an expression is expected. Two
ways:

- **Block expressions** in the language: `{ stmts; value }` as an expression,
  as in Rust.
- **Hoisting**: an `expr` macro may return `quote { stmts } then (value)`, and
  sema runs the statements just before the statement that holds the call. Only
  sema changes, but it is wrong inside `a && m!()` or a `while` condition,
  where the call does not always run; those places would be errors.

Done with block expressions, which turned out to touch no more than hoisting:
sema lowers a block to the same control flow as its statements, so JIR, the
VM, the backend and the ownership pass see nothing new. An `expr` macro
returns `quote({ ... value })`.

Fixes gap 4. `examples/pipe.jh` is now `let s = fuse!(xs, map(f), filter(g),
sum())`, and a fused pipeline can stand anywhere an expression can.

### 5. Types at compile time (done)

`type_of(e)` in a macro: the type `e` would have where the macro was called.
With it, `fuse!` works for any element type, as `lists` does with
`comptime T: type`.

Done ([Types of code](../language/macros.md#types-of-code)), in JIR 15:

- The VM cannot ask the compiler anything while a macro runs, so `typeof`
  looks the code up in a table of types the VM is given, and stops the run
  when it is not there, naming the code. The compiler types that code in the
  caller's scope, adds it to the table and runs the macro again from the
  start (`Env::run_typed`); `unique` restarts at the same number. A macro
  that asks about three pieces of code runs four times.
- Typing lowers the code where the macro was called and takes the
  instructions out again (`FnCx::save` and `restore`), so the code is checked
  exactly as it would be compiled. An anonymous function in it is lifted
  once more, and the copy is never called.
- The type comes back as text, as builtin types are written; a struct or
  enum as `#type:N`, an index into a table of the compiler's, so that
  generic instances and other modules' types need no spelling that the
  caller's module may not have. A hole in a type position (`let x: $t`, the
  new hole kind `type`) puts it back.
- The code is typed before the macro's output exists, so it cannot use the
  names the macro's own templates declare.

Fixes gap 5. `fuse!` builds an expression of the type each stage gives
(`$xs[0]`, then `(f)(...)` for each `map`) and asks `type_of` about it, to
start the sum at the right zero.

## Done when

`examples/pipe.jh` defines `fuse!` as above, generic over the element type,
and:

- it runs as fast as the loop written by hand, natively (done);
- a differential test checks that fused pipelines print what the same
  pipelines written with `lists` print, on the VM and natively
  (`tests/diff-lists/fusion.jh`, done);
- the fuzzer writes random pipelines (not yet);
- `fuse!` moves from the example into the library, with more ways to end a
  pipeline than a sum (done: `lists.fuse!`, with `sum`, `count` and `collect`;
  `meta` is `#![comptime]`, so it costs programs nothing and works in every
  profile).

## Costs and risks

- **Compile time.** Macros run on the VM, so heavier macros make compiling
  slower. This gives the VM's speed a purpose beyond hosted programs.
- **Error messages.** An error in produced code points at the macro call
  ([Macros](../language/macros.md)), so an error in fused code points at the
  `fuse!` call; messages that also name the stage would be better.
- **Hygiene.** Code cut out by `node` and put back in another expansion must
  keep its marks; `macros_are_hygienic` and `stringify_and_nested_macros` in
  `crates/jihoo-sema/src/tests.rs` are the model for its tests.

Not goals: compile-time I/O; untyped code values (a node is an ordinary enum);
changing what any program means at run time.
