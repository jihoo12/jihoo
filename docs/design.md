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

- Types: `unit`, `bool`, `i8`…`i64`, `u8`…`u64`, structs, plus `str` (hosted
  only, garbage collected) and `*T` (freestanding only, raw pointer). A string
  literal is a `str` when hosted and a `*u8` to constant bytes when freestanding.
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

## Testing

- Unit tests in each crate (`cargo test`).
- Differential tests (`tests/diff/`): each program runs on the VM and as a native
  binary, and both must exit with the same status. This keeps the two backends
  honest about the semantics of JIR.

## Why the IR is a text file

The frontend (Rust) and the LLVM backend (C++) only talk through `.jir` files
(`docs/jir.md`). This keeps the IR an explicit, versioned contract, lets the backend
be developed and tested on its own with hand-written `.jir`, and keeps the Rust build
free of LLVM.

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

Planned next: AST macros on the same machinery, and generic structs (functions
that return types).

## GC

`crates/jihoo-vm/src/gc.rs` is a stop-the-world mark & sweep heap. The roots are the
registers of every VM frame; a collection may happen at any allocation once the heap
grows past twice the size that survived the last collection (1 MiB minimum).

## Roadmap

1. ~~Type checker (static types with inference) and typed JIR.~~
2. ~~Differential tests across both backends.~~ Rust-side JIR parser.
3. ~~Sized integers, pointers with loads/stores, structs, arrays, `size_of`,
   inline asm.~~
4. ~~`comptime` on the VM, generic functions.~~
5. `alloc` layer; AST macros.
