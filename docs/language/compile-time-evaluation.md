# Compile-time evaluation

Ordinary jihoo functions can run while the program is compiled, and their
results become constants of the program — in every profile, freestanding
included.

```jihoo
const COUNT = 10
const PRIMES = primes()            // runs `primes` while compiling

fn is_prime(n: i64) -> bool {
    let d = 2
    while d * d <= n {
        if n % d == 0 { return false }
        d = d + 1
    }
    return true
}

fn primes() -> [i64; COUNT] {
    let out = [0; COUNT]
    let n = 0
    let candidate = 2
    while n < COUNT {
        if is_prime(candidate) {
            out[n] = candidate
            n = n + 1
        }
        candidate = candidate + 1
    }
    return out
}

fn fib(n: i64) -> i64 {
    if n < 2 { return n }
    return fib(n - 1) + fib(n - 2)
}

struct Node { value: i64, next: i64 }

fn main() {
    print(PRIMES[COUNT - 1])       // 29
    print(comptime fib(25))        // 75025, evaluated while compiling
    let buf: [u8; size_of(Node) * 2] = [0; 32]
    print(len(buf))                // 32
}
```

## `const`

`const NAME = expr` declares a compile-time constant, and `const NAME: T = expr`
one of a given type. Constants are module-level items: they can be `pub`, used
from other modules as `alias.NAME`, and refer to each other and to functions in
any order.

The value can be any expression whose result is made of integers, floats,
bools, strings, structs, enums, arrays and function values.

## `comptime`

`comptime e` evaluates one expression while compiling and puts the result in
its place. It binds like a prefix operator, so write `comptime (a * b)` for a
whole product.

Array lengths, `comptime` arguments and the parameters of generic structs are
evaluated the same way, so they may be any integer expression the compiler can
compute: `[u8; size_of(Node) * 2]`, `Buf(CAP * 2)`.

## How it runs

Evaluation runs on the VM, for compiled programs too: the expression is lowered
into a helper function, and that function plus everything it can call is
executed. The result is spliced back into the IR as constants. `print` during
evaluation goes to stderr, which is handy for debugging.

Constants are built once per function, at its start, and reused, so a lookup
table indexed in a loop is not rebuilt on every iteration.

## What compile-time code cannot do

- use pointers, `syscall`, `asm` or C functions, since the VM has none of them;
- read local variables of the function it appears in (it runs before the
  program does);
- run forever: it is stopped after 100 million VM steps.

## Order and cycles

Module-level items are analyzed lazily: struct fields, signatures, constants
and function bodies are computed on first use and memoized
(`crates/jihoo-sema/src/env.rs`, the same approach as Zig's lazy analysis). So
items can refer to each other in any order. A query that needs its own result —
`const A = A`, or `comptime f()` inside `f` — is reported as a cycle.

See `examples/comptime.jh`, and [Generics](generics.md) and
[Macros](macros.md), which build on this.
