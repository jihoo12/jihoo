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

- Types: `unit`, `bool`, `i8`…`i64`, `u8`…`u64`, structs, enums, arrays, function types
  `fn(A, B) -> R`, plus `str`, `ref T` and `chan T` (hosted only, garbage
  collected) and `*T` (freestanding only, raw pointer). A string literal is a
  `str` when hosted and a `*u8` to constant bytes when freestanding.
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
- Anonymous functions and closures are function values too; see below.

## Closures

```jihoo
fn adder(n: i64) -> fn(i64) -> i64 {
    return fn(x) { return x + n }        // captures `n`
}

let limit = 5
let small = filter(xs, fn(x) { return x <= limit })
let sq = fn(x: i64) -> i64 { return x * x }
```

- `fn(params) -> R { body }` is an anonymous function. Parameter and return
  types may be left out when the expected type is a function type (an argument,
  a typed `let`, a return value); otherwise they are written out, and a missing
  `-> R` means unit.
- It may use the local variables around it. They are captured by value, when
  the closure is made: changing the variable later does not change the closure,
  and assigning to a captured variable inside the closure is an error (the
  change would be lost between calls). This is the same value semantics as
  everywhere else, and it means closures share no mutable state.
- A closure has the same type as any other function value, `fn(A) -> R`, so
  functions that take functions take closures too. Like other function values,
  closures cannot be compared.
- The body becomes a function of its own, `fn.N` in JIR, whose first parameters
  are the captured values (`crates/jihoo-sema/src/closures.rs`). One that
  captures nothing is a plain `funcref`, which works everywhere, including
  freestanding code and compile-time constants. One that captures is
  `closure @fn.N(%captured...)`: a GC object on the VM, so hosted only.
- A closure cannot call itself by name (it has none); use a named function.

### Comptime closures

```jihoo
#![freestanding]
fn each(comptime f: fn(i64), xs: [i64; 5]) { ... f(xs[i]) ... }

let limit = 10
each(fn(x) { if x > limit { io.print_int(x) } }, xs)   // no GC needed
```

- A closure passed to a `comptime` function parameter is not a value at run
  time. The function gets an instance for that closure (like any other comptime
  argument), and the instance takes the captured values as hidden arguments,
  after its own; `f(x)` in it is a direct call `call @fn.N(captured..., x)`.
  No heap, no indirect call: this is how Rust compiles `impl Fn` arguments.
- So capturing closures work in freestanding code this way. Inside the
  instance, the closure can be called, passed on to another `comptime`
  parameter, or captured by another closure passed on. Using it as a value
  (storing or returning it) makes a closure value, which needs the GC: fine in
  hosted code, an error in freestanding code.
- Each closure written in the source is its own instance, so a function called
  with many different closures is compiled many times.

## Enums and `match`

```jihoo
enum Shape {
    Circle(i64)
    Rect(i64, i64)
    Empty
}
enum Option(T: type) { Some(T), None }

fn area(s: Shape) -> i64 {
    match s {
        Circle(r) => return 3 * r * r
        Rect(w, h) => return w * h
        Empty => return 0
    }
}

let s = Shape.Rect(3, 4)
let o: Option(u8) = Option.None      // `Option(u8)` from the expected type
return Option.Some(i)                // ... or from the payload, or the return type
```

- An enum (sum type) holds one of its variants, each with its own payload of
  values. Variants are written after the enum: `Shape.Empty`, `Shape.Rect(3, 4)`,
  `geo.Shape.Empty`, `Option(i64).Some(1)`. Enums are values like structs, in
  both profiles, and share the struct rules: `pub`, generic parameters, no
  containing themselves by value.
- For a generic enum the arguments can be left out: they are taken from the
  expected type (a typed `let`, an argument, a return value), or else from
  payload values declared with exactly a type parameter (`Some(T)`).
  `let x = Option.None` cannot be inferred and is an error that says so.
- `match` works on enums, structs, integers and bools. Arms are tested in
  order; the first one whose pattern matches, and whose guard (`if cond`, if
  any) holds, runs.
- Patterns nest (`crates/jihoo-sema/src/patterns.rs`):

  | pattern | matches |
  |---------|---------|
  | `_` | anything |
  | `x` | anything, bound to a new local `x` |
  | `Empty`, `Rect(p, q)` | a variant of the matched enum, and its payload |
  | `Point { x, y: 0, .. }` | a struct; `x` alone is `x: x`, `..` ignores the other fields |
  | `0`, `-1`, `true` | that value |
  | `Circle(_) \| Empty`, `Some(1 \| 2)` | either pattern; every alternative binds the same names, with the same types |

  Variants are written without the enum, whose type is known. A name is a
  variant if the matched enum has one by that name, and else a new variable;
  a name that starts with an uppercase letter must be a variant, so a
  misspelled variant is an error, not a binding.
- Patterns read through refs: `Cons(x, Cons(y, _))` matches a list whose tail
  is a `ref List`. A name or `_` takes the ref itself.
- A `match` must cover every value, and every arm must match some value the
  arms above it miss; both are checked with Maranget's usefulness algorithm, so
  nested patterns are handled exactly. An error names a value that is missed,
  as a pattern (`` `match` does not cover `Some(Rect(_, _))` ``); arms with a
  guard do not count towards covering, and an alternative of a `|` pattern that
  can never match is an error too. Since an exhaustive `match` has no
  fall-through, a function whose arms all `return` needs no `return` after it.
- `match` is also an expression, with a value after each `=>`:

  ```jihoo
  let word = match n % 15 {
      0 => "fizzbuzz"
      r if r % 3 == 0 => "fizz"
      _ => to_str(n)
  }
  ```

  Every arm has the type of the first one (or the expected type). At the start
  of a statement, `match` is the statement form, whose arms are statements.
- In JIR an enum is `enum $Shape { Circle(i64), Rect(i64, i64), Empty }` with
  `variant`, `tag` and `payload` instructions. Natively it has the C layout of
  a `u32` tag followed by a union of the payloads (`size_of(Shape)` is 24); on
  the VM it is a GC object.
- Not yet: equality on enums. A type cannot contain itself by value; recursive
  data goes through a `ref` (hosted) or a pointer (freestanding).

## References

```jihoo
enum List(T: type) {
    Cons(T, ref List(T))
    Nil
}

fn sum(l: List(i64)) -> i64 {
    match l {
        Cons(x, rest) => return x + sum(*rest)
        Nil => return 0
    }
}

let l = List.Cons(1, ref List.Cons(2, ref List.Nil))
let p = ref Point { x: 1, y: 2 }
print(p.x)                     // fields and elements read through a ref
```

- `ref T` is an immutable reference to a `T` on the GC heap. `ref e` makes one
  holding a copy of `e`; `*r` is the value, and `r.x` and `r[i]` read through it.
- Refs are immutable: `*r = v` and `r.x = v` are errors. So a value shared
  through refs behaves exactly as if it had been copied, and jihoo keeps its
  value semantics; a new version is built from the parts that change and refs
  to the parts that do not (see `examples/lists.jh`). There is no `==` on refs.
- A `ref` breaks the rule that a type cannot contain itself, which is what makes
  lists and trees possible.
- Hosted only, like `str`: the GC owns the value. Freestanding code uses
  pointers. On the VM a ref is a one-element heap object; in JIR it is `ref T`,
  with `ref` and `deref` instructions.

## Tasks and channels

```jihoo
enum Msg { Item(i64), Done }

fn numbers(out: chan Msg, n: i64) {
    let i = 0
    while i < n {
        send(out, Msg.Item(i))
        i = i + 1
    }
    send(out, Msg.Done)
}

let c = chan(Msg)            // unbuffered; `chan(Msg, 8)` buffers 8 values
go numbers(c, 5)
go fn() { send(results, work(id)) }()
match recv(c) { ... }
```

- `go f(x)` runs a call in a new task, like a goroutine. The function and its
  arguments are evaluated first, in the task that says `go`; any function value
  works, closures included. The result of the call is dropped.
- `chan(T)` makes a channel of `T` values, and `chan(T, n)` one that buffers up
  to `n`. `send(c, v)` waits while the channel is full (an unbuffered one is
  full until a receiver comes), and `recv(c)` waits until there is a value.
  The type is `chan T`.
- Tasks share nothing but channels: arguments are copies, closures capture by
  value, and refs are immutable. So there are no data races to worry about.
- There is no `close`: to say "no more values", send a value that says so, as
  `Msg.Done` above; `match` then handles both cases.
- `select` waits on several channels at once (`examples/select.jh`):

  ```jihoo
  select {
      let n = recv(numbers) => total = total + n
      recv(quit) => running = false
      send(log, line) => {}
      _ => print("nothing ready")     // optional: do not wait
  }
  ```

  The channels and the values to send are evaluated first, in order. Then the
  first case that can go ahead without waiting does, and its arm runs; if
  several can, the first in source order wins (Go picks at random; jihoo stays
  reproducible). With `_` and no case ready, the `_` arm runs. Otherwise the
  task waits on all the channels, and the first case another task makes
  possible goes ahead; its waits on the other channels are cancelled.
- Tasks run on one OS thread. The VM's scheduler is round-robin and
  deterministic: a task runs until it waits on a channel, finishes, or has run
  1000 instructions, then the next ready task gets its turn. The same program
  prints the same output on every run, which keeps tests reliable.
- The program ends when `main` returns, even if other tasks are still running
  or waiting (as in Go). If every task waits on a channel, the run stops with
  `deadlock: every task is waiting on a channel`. An error in any task stops
  the whole run.
- Hosted only: tasks need the VM's scheduler. In JIR, `chan`, `send`, `recv`,
  `select` and `spawn`.

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

- The roots are gathered in one place, `roots` in `crates/jihoo-vm/src/lib.rs`:
  the registers of every frame of every task, the channels tasks wait on, and
  values handed out to the embedder (`Vm::alloc_string`, used for comptime
  arguments), which stay alive as long as the VM. Values in a channel's buffer
  and the values of tasks waiting to send are reached through the channel.
- Every object is immutable except channels.
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
7. ~~Sum types and `match`; `ref T` for recursive data; nested patterns,
   guards, `|` patterns and `match` expressions.~~
8. ~~Closures, captured by value and lifted into functions.~~
9. ~~Tasks and channels on the VM: one OS thread, a deterministic scheduler;
   `select`.~~
10. ~~Comptime closures: closures passed to `comptime` parameters, one
    instance per closure, captured values as hidden arguments.~~
11. Coroutines for freestanding code, as a library on top of function values
    and `asm`.
