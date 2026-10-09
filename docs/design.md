# jihoo design

jihoo aims to be an easy language that can still go all the way down to bare
syscalls. One surface language is lowered to one IR (JIR); that IR is either run by
a VM or compiled by LLVM.

```
               ┌─────────────── Rust ───────────────┐      ┌──── C++ ─────┐
 source.jh ──► │ jihoo-syntax ─► jihoo-sema ──► JIR │ ──►  │  jihoo-llc   │ ──► .o ──► ld.lld ──► ELF
               │   (parse)      (typecheck    │    │ .jir │ (LLVM 21)    │
               │                 + lower)     ▼    │      └──────────────┘
               │                           jihoo-vm│
               └────────────────────────────────────┘
```

## Profiles

A program chooses its profile with a file attribute.

|                 | hosted (default)        | freestanding (`#![freestanding]`) |
|-----------------|-------------------------|-----------------------------------|
| backend         | VM                      | LLVM, native static binary        |
| memory          | GC, managed by the VM   | manual                            |
| library         | std (strings, `print`…) | core only                         |
| pointers        | no                      | `*T`, `&x`, `*p`, `p[i]`          |
| syscalls / asm  | no                      | yes                               |
| entry point     | `fn main()`             | `fn _start()`                     |

The profile is a property of the program, not a command line switch. Code that
uses GC features is rejected in freestanding mode at compile time (see
`crates/jihoo-ir/src/verify.rs`), so a program never silently changes meaning
between modes. This mirrors Rust's `no_std` / `no_main`.

### Planned library layers

```
std    hosted only: GC types, I/O, collections
alloc  any profile, given an allocator: Vec[T, A], String[A]
core   everywhere: integers, pointers, slices, Option
```

Freestanding code should not have to start from raw `mmap` every time; `alloc`
will let it bring its own allocator (written in jihoo on top of `syscall`).

## Types

Static types, with inference for local variables:

```jihoo
fn area(w: i64, h: i64) -> i64 {   // signatures are written out
    let a = w * h                  // `a: i64` is inferred
    return a
}
```

- Types: `unit`, `bool`, `i8`…`i64`, `u8`…`u64`, structs, arrays, function types
  `fn(A, B) -> R`, plus `str` (hosted only, garbage collected) and `*T`
  (freestanding only, raw pointer). A string literal is a `str` when hosted and a
  `*u8` to constant bytes when freestanding.
- Bitwise operators `&`, `|`, `^`, `<<`, `>>` work on integers, and `!` flips
  every bit of an integer. `>>` is arithmetic for signed types and logical for
  unsigned ones; shift amounts are taken modulo the bit width, so `x << 64` on an
  `i64` is `x`, the same on the VM and natively. `&`, `|`, `^` on bools evaluate
  both sides. Precedence follows Rust: comparisons bind looser than `&`.
- Integer literals (`1_000`, `0xff`, `0b1010`) take their type from context (`let c: u8 = 65`, `p[i] == 0`,
  arguments, fields), defaulting to `i64`, and must fit that type. Different
  integer types never mix implicitly; convert with `as`.
- A function without `-> T` returns `unit`.
- `if`/`while` conditions and the operands of `&&`, `||`, `!` must be `bool`;
  there is no implicit integer-to-bool conversion.
- A function that returns a value must `return` on every path that reaches the
  end of its body.

Type checking and lowering happen in one pass in `crates/jihoo-sema` (the same
shape as Zig's Sema): each expression is checked and turned into typed JIR at the
same time. The operator rules live in `crates/jihoo-ir/src/types.rs` and are shared
with the IR verifier, so the checker and the verifier cannot disagree. Errors are
reported per function, so one run shows the first error in every function.

## Structs and pointers

```jihoo
struct Node {
    value: i64
    next: *Node          // recursion only through pointers
}

fn sum(list: *Node) -> i64 {
    let total = 0
    while list != 0 as *Node {
        total = total + list.value   // `p.field` reads through a pointer
        list = list.next
    }
    return total
}
```

- Structs are values, in both profiles: assignment and argument passing copy them,
  and `p.x = 1` changes only `p`. Nested updates (`line.a.x = 1`) rebuild the outer
  struct with `setfield`.
- On the VM, a struct is an immutable GC object; a field update allocates a new
  one. Natively, structs are plain LLVM aggregates.
- Pointer arithmetic counts in elements (`p + 1` on `*i64` moves 8 bytes);
  `p[i]` is `*(p + i)`.
- `&x` takes the address of a local variable or a field of one; the pointer is
  valid until the function returns. `&*p` and `&p[i]` are just pointers.
- Assignment targets are *places*: variables, fields, `*p` and `p[i]`
  (`crates/jihoo-sema/src/place.rs`).

## Arrays

```jihoo
let xs = [5, 3, 9]            // [i64; 3]
let buf: [u8; 64] = [0; 64]   // literals take their element type from context
buf[0] = 72
let n = len(buf)              // 64, a constant
```

- `[T; N]` is a value type like a struct, in both profiles. `N` is an integer
  literal.
- Indexing an array is bounds-checked: the VM reports an error, native code traps.
  Indexing a raw pointer (`p[i]` with `p: *T`) is not checked.
- `p[i]` and `len(p)` with `p: *[T; N]` work on the array `p` points to, and
  `&a[i]` is a pointer to an element, so `&buf[0]` is how a buffer becomes a `*u8`.
- On the VM an array is an immutable GC object, like a struct, so an element write
  copies the array: O(N). Natively, writes happen in place.

`size_of(T)` and `align_of(T)` are `i64` constants, computed with C layout rules for
64-bit targets (`crates/jihoo-ir/src/layout.rs`). JIR records each struct's layout
and the LLVM backend checks it against the target, so a mismatch is a compile
error instead of silent memory corruption. Types containing `str` have no layout.

As in Rust, a struct literal cannot appear directly in an `if`/`while` condition
(`if p == Point { ... }` would be ambiguous); wrap it in parentheses.

## Modules

```jihoo
#![freestanding]
import alloc            // lib/alloc.jh
import io as out        // lib/io.jh, used as `out`

fn _start() -> i64 {
    let arena = alloc.arena_new(1 << 20)
    let v = alloc.vec_new(i64, &arena)
    alloc.push(i64, &v, 42)
    out.print_int(alloc.get(i64, &v, 0))
    return 0
}
```

- Every file is a module with its own namespace. Items of another module are
  always written `alias.item`: functions, macros (`alias.m!(...)`), types
  (`alias.T`, `alias.Vec(i64)`), struct literals and constants.
- Items are private to their module unless marked `pub` (`pub fn`, `pub struct`,
  `pub const`, `pub macro`). A `pub` struct's fields are all public. Using a
  private item from another module is an error that names it:
  `` `alloc.helper` is private to module `alloc` ``.
- `import a.b` loads `a/b.jh` and names it `b` (`import a.b as c` to rename). The
  loader (`crates/jihoo-syntax/src/loader.rs`) looks next to the importing file,
  then in the `-I` directories, `JIHOO_PATH`, and the standard library in `lib/`.
  Each file is loaded once; imports may be cyclic, which lazy analysis handles.
  A file that would import itself is an error.
- In JIR, a module's items are prefixed with its name (`alloc.push`,
  `alloc.Vec(i64)`); the root module's items keep their plain names. Inside a
  generic, names resolve in the module that declares it, while type arguments can
  come from the caller: `alloc.Vec(Point)` holds the caller's `Point`.
- The root file chooses the profile. A library marked `#![freestanding]` can only
  be imported by freestanding programs.
- Source positions carry a file number, so errors name the file they are in.

The standard library so far: `alloc` (an arena over `mmap` and `Vec(T)`) and `io`
(`puts`, `print_int`, `write`, `exit`), both freestanding and x86_64 Linux only.

## Compile-time evaluation

```jihoo
const COUNT = 10
const PRIMES = primes()            // runs `primes` while compiling

fn primes() -> [i64; COUNT] { ... }

fn main() {
    print(PRIMES[COUNT - 1])
    print(comptime fib(25))        // one expression, evaluated while compiling
    let buf: [u8; size_of(Node) * 2] = [0; 32]
}
```

- `const NAME = expr` declares a compile-time constant; `comptime e` evaluates one
  expression (it binds like a prefix operator, so write `comptime (a * b)` for a
  whole product); array lengths may be any integer expression.
- Evaluation runs on the VM, for freestanding programs too: the expression is
  lowered into a helper function, and that function plus everything it can call
  is executed. The result (integers, bools, strings, structs, arrays) is spliced
  back into the IR as constants. `print` during evaluation goes to stderr.
- Compile-time code cannot use pointers or `syscall` (the VM has neither), cannot
  read local variables, and is stopped after 100 million steps.
- Constants are built once per function, at its start, and reused, so a lookup
  table indexed in a loop is not rebuilt on every iteration.

To make this possible, `crates/jihoo-sema/src/env.rs` analyzes module-level items
lazily: struct fields, signatures, constants and function bodies are computed on
first use and memoized. Items can refer to each other in any order; a query that
needs its own result (`const A = A`, or `comptime f()` inside `f`) is reported as
a cycle. This is the same approach as Zig's lazy analysis.

## Generics

```jihoo
fn max(comptime T: type, a: T, b: T) -> T {
    if a > b { return a }
    return b
}
fn ramp(comptime N: i64) -> [i64; N] { ... }

max(u8, x, 200)      // instance `max.0` with T = u8
max(i64, -3, -7)     // instance `max.1` with T = i64
ramp(5)              // returns [i64; 5]
```

- A `comptime` parameter makes a function generic. `comptime T: type` takes a
  type; any other `comptime x: U` takes a value computed on the VM. The function
  is compiled once per distinct set of comptime arguments (monomorphization), as
  `name.N` in JIR (`crates/jihoo-sema/src/generic.rs`).
- Type arguments are written in expression position and read back as types:
  `u8`, `*u8` and `[u8; 4]` already parse as expressions.
- Inside an instance, `T` resolves to its type and `N` to a constant, also in
  array lengths and `size_of`. Comptime parameters cannot be assigned.
- The body is checked per instance, like C++ templates and Zig, and errors name
  the instance: `cannot apply `>` to str and str (in `max` with T = str)`.
- Callers only need an instance's signature, so instances can recurse. Runaway
  instantiation (`f(n + 1)` inside `f`) stops after 1000 instances.
- Instances needed only at compile time (`comptime ramp(3)`) still appear in the
  module; natively they are internal and dropped by LLVM.

### Generic structs

```jihoo
struct Vec(T: type) {
    data: *T
    len: i64
    cap: i64
    arena: *Arena
}

fn push(comptime T: type, v: *Vec(T), x: T) { ... }

let v = vec_new(i64, &arena)     // a Vec(i64)
push(i64, &v, 42)
```

- Struct parameters are always compile-time (`comptime` is optional). Each
  distinct set of arguments is its own struct type, named like `Pair(i64)` in
  messages and as `$"Pair(i64)"` in JIR. `Buf(CAP * 2)` and `Buf(16)` are the
  same type.
- Literals name the instance: `Pair(i64) { a: 1, b: 2 }`. A generic struct can
  point to itself (`next: *Node(T)`), but not contain itself.
- Like generic functions, a generic struct's fields are checked per instance.

`lib/alloc.jh` puts this together for freestanding code: a bump allocator over
`mmap`, and a growable `Vec(T)` built on it, all in jihoo (see
`examples/arena.jh`).

## Function values

```jihoo
struct Command {
    name: str
    run: fn(i64) -> i64          // `fn(A, B) -> R`; without `-> R` it returns unit
}

fn map(xs: [i64; 4], f: fn(i64) -> i64) -> [i64; 4] { ... xs[i] = f(xs[i]) ... }
fn twice(comptime f: fn(i64) -> i64, x: i64) -> i64 { return f(f(x)) }

map(xs, square)                  // a function name is a value
commands[i].run(7)               // so is anything of a function type
pick(1)(2, 3)                    // a function returning a function
```

- A function name used as a value has type `fn(params) -> ret`. Module
  functions work the same way (`alloc.push` as a value). Generic functions and
  macros are not values; builtins such as `print` are not functions.
- Any expression of a function type can be called: `f(x)`, `s.f(x)`,
  `fs[0](x)`, `make()(x)`. A local of a function type hides a function of the
  same name when called; a local of another type does not, so `let len = len(a)`
  still works.
- Function values are plain values in both profiles: on the VM an index into
  the module's functions, natively a code pointer (8 bytes, so `size_of` and
  struct layouts work). In JIR they are `funcref @f` and `call %r(...)`.
- A function known at compile time is called directly, without indirection:
  a `comptime f: fn(...)` parameter (one instance per function passed), or a
  constant (`const F = inc`). Compile-time code can compute function values,
  `const G = choose(1)`, and store them in constant structs and arrays.
- Function values cannot be compared with `==`. They are code pointers today,
  but closures will be function values too, and there is no good answer to
  whether two closures are equal.
- No closures yet: a function value cannot capture local variables. That is
  the next step; see the roadmap.

## Macros

```jihoo
macro power(x: expr, n: i64) -> expr {
    let e = quote(1)
    let i = 0
    while i < n {
        e = quote($e * $x)
        i = i + 1
    }
    return e
}
macro expect(cond: expr) -> expr {
    return quote(report($cond, $(stringify(cond))))
}

power!(y + 1, 2)    // expands to ((1) * (y + 1)) * (y + 1)
```

- A macro is a function that runs at compile time, on the VM, and returns code
  (`expr`). It is called as `name!(...)` and the code it returns replaces the call
  (`crates/jihoo-sema/src/macros.rs`).
- `expr` parameters receive the arguments as code, unevaluated; integer, bool and
  `str` parameters receive values computed at compile time.
- `quote(template)` builds code. The template must be an expression, checked when
  the macro is parsed; `$x` and `$(e)` are holes. An `expr` is inserted in
  parentheses, so precedence cannot change; integers, bools and strings are
  inserted as literals. `stringify(e)` gives the source text of code.
- The result is compiled in the caller's scope, so it can use the caller's
  variables: macros are not hygienic, like C macros. Errors in produced code point
  at the call and say which macro produced it. Expansion stops 64 levels deep.
- Macro bodies may use `str` even in freestanding programs, since they only run on
  the VM; a `str` inserted into code becomes a string literal. Macros are
  type checked even when unused, and are never part of the compiled program.

### Statement and item macros

```jihoo
macro swap(a: expr, b: expr) -> stmts {
    let t = unique("tmp")          // `tmp__0`, `tmp__1`, ...: never the caller's name
    let tv = ident(t)              // the name as code, for expression positions
    return quote {
        let $t = $a
        $a = $b
        $b = $tv
    }
}

macro adders(n: i64) -> items {
    let out = quote items {}
    let i = 1
    while i <= n {
        out = quote items {
            $out
            fn $("add" + to_str(i))(x: i64) -> i64 { return x + $i }
        }
        i = i + 1
    }
    return out
}

adders!(3)            // at the top level: defines add1, add2, add3
fn main() {
    swap!(x, y)       // on a line of its own: three statements in this block
}
```

- A macro returns `expr`, `stmts` or `items`, built with `quote(...)`,
  `quote { ... }` and `quote items { ... }`. An `expr` macro is used as an
  expression, a `stmts` macro as a statement of its own (its `let`s stay visible
  after it), and an `items` macro at the top level of a module.
- A hole's position decides what it takes: in an expression, code (parenthesized)
  or a literal value; in a name position (`fn $name`, `let $v`, a struct name),
  a `str` that must be an identifier; as a statement of its own, `stmts` or
  `expr`; as an item of its own, `items`.
- Macros are not hygienic by themselves; `unique(prefix)` gives a name that is
  new in the whole compilation, and `ident(name)` turns a name into code. Use
  them for temporaries a macro introduces.
- `to_str(x)` turns an integer or bool into a `str` (in hosted programs and
  macros), handy for building names.
- Item macros are expanded before the program is analyzed, in rounds: each round
  analyzes lazily what the macros need, runs them, and adds the produced items to
  the module that called the macro. Produced code may call item macros, so this
  repeats, up to 16 rounds.

## Inline assembly

```jihoo
#![freestanding]
fn write(fd: i64, buf: *u8, len: i64) -> i64 {
    return asm("syscall",
        out("rax") i64,
        in("rax") 1, in("rdi") fd, in("rsi") buf, in("rdx") len,
        clobber("rcx", "r11", "memory"))
}
fn bswap(x: u64) -> u64 {
    return asm("mov {out}, {0}", "bswap {out}", out(reg) u64, in(reg) x)
}
```

- `asm(...)` is an expression of the `out` type, or `unit` without one. Template
  lines are joined with newlines; `{0}`, `{1}`, ... are the inputs and `{out}` the
  output. On x86_64 the syntax is Intel.
- Operands: `out("rax") T` / `in("rdi") x` use that register, `reg` lets the
  compiler choose, and `in(out) x` starts the output register with `x` (for
  instructions such as `xchg` or `inc` that update a register in place).
  `clobber(...)` lists registers and `"memory"`; the flags are always clobbered.
- Freestanding only. Like `syscall`, asm cannot run at compile time.

The `syscall` builtin stays as a portable shortcut (x86_64 and aarch64) for the
most common use of asm.

## GC

`crates/jihoo-vm/src/gc.rs` is a stop-the-world mark & sweep heap. A collection may
happen at any allocation once the heap grows past twice the size that survived the
last collection (1 MiB minimum).

- The roots are gathered in one place, `Vm::roots`: the registers of every VM frame,
  plus values handed out to the embedder (`Vm::alloc_string`, used for comptime
  arguments), which stay alive as long as the VM. Future roots, such as the stacks
  of other tasks or values waiting in a channel, go there too.
- The invariant: whatever an instruction allocates from must already be reachable
  from the roots. Values read from registers are; values held only in Rust locals
  are not.
- A `GcRef` is a slot index plus the slot's generation, so using a freed object
  panics even after its slot has been reused, instead of reading the wrong object.
- `JIHOO_GC_STRESS=1` collects before every allocation, which turns a missing
  root into a panic on the first run instead of a rare heisenbug.

## Why the IR is a text file

The frontend (Rust) and the LLVM backend (C++) only talk through `.jir` files
(`docs/jir.md`). This keeps the IR an explicit, versioned contract, lets the backend
be developed and tested on its own with hand-written `.jir`, and keeps the Rust build
free of LLVM.

## Testing

- Unit tests in each crate (`cargo test`).
- Differential tests (`tests/diff/`): each program runs on the VM and as a native
  binary, and both must exit with the same status. This keeps the two backends
  honest about the semantics of JIR.
- `JIHOO_GC_STRESS=1 cargo test` runs everything with a collection at every
  allocation. Run it after any change to the VM that allocates or adds roots.

## Roadmap

1. ~~Type checker (static types with inference) and typed JIR.~~
2. ~~Differential tests across both backends.~~ Rust-side JIR parser.
3. ~~Sized integers, pointers with loads/stores, structs, arrays, `size_of`,
   inline asm.~~
4. ~~`comptime` on the VM, generic functions.~~
5. ~~Expression macros, generic structs, bitwise operators, modules and an
   `alloc` library, `pub`, statement and item macros, `unique`/`ident`.~~
   Automatic hygiene; field visibility.
6. ~~GC hardening (one root set, generation-checked references, stress mode);
   function values.~~
7. Sum types and `match`.
8. Closures, lowered in the frontend to a function plus a struct of captured
   values (captured by value, like every other value in jihoo).
9. Goroutine-like tasks and channels on the VM: one OS thread, a deterministic
   scheduler, the stacks of all tasks as GC roots. Freestanding code gets
   coroutines as a library on top of function values and `asm`.
