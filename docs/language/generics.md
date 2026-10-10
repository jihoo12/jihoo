# Generics

A function or struct with compile-time parameters is generic: it is compiled
once for each distinct set of arguments (monomorphization), like C++ templates
and Zig's `comptime`.

```jihoo
fn max(comptime T: type, a: T, b: T) -> T {
    if a > b { return a }
    return b
}

fn ramp(comptime N: i64) -> [i64; N] {
    let out = [0; N]
    let i = 0
    while i < N {
        out[i] = i
        i = i + 1
    }
    return out
}

fn main() {
    print(max(u8, 7, 200))      // instance `max.0` with T = u8
    print(max(i64, -3, -7))     // instance `max.1` with T = i64
    print(len(ramp(5)))         // returns [i64; 5]
}
```

## Generic functions

- A `comptime` parameter makes a function generic. `comptime T: type` takes a
  type; any other `comptime x: U` takes a value of type `U` computed at compile
  time on the VM.
- Type arguments are written in expression position and read back as types:
  `u8`, `*u8`, `[u8; 4]` and `fn(i64) -> i64` all parse as expressions.
- Inside an instance, `T` stands for its type and `N` for a constant, also in
  array lengths and `size_of`. Comptime parameters cannot be assigned.
- There are no constraints on `T`: the body is checked per instance, and errors
  name the instance:

  ```text
  error: max.jh:2:10: cannot apply `>` to str and str (in `max` with T = str)
  ```

- Callers only need an instance's signature, so instances can call themselves.
  Runaway instantiation (`f(n + 1)` inside `f`) stops after 1000 instances.
- Instances are named `name.N` in JIR (`crates/jihoo-sema/src/generic.rs`).
  Instances needed only at compile time (`comptime ramp(3)`) still appear in the
  module; natively they are internal and LLVM drops them.

A `comptime` parameter of a function type passes a function at compile time:
see [Function values](function-values.md#direct-calls) and
[comptime closures](closures.md#comptime-closures).

## Generic structs

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

- Struct parameters are always compile-time; writing `comptime` is optional.
- Each distinct set of arguments is its own struct type, named like `Pair(i64)`
  in messages and `$"Pair(i64)"` in JIR. Arguments are compared by value, so
  `Buf(CAP * 2)` and `Buf(16)` are the same type when `CAP` is 8.
- Literals name the instance: `Pair(i64) { a: 1, b: 2 }`.
- A generic struct can point to itself (`next: *Node(T)`), but not contain
  itself.
- Like generic functions, a generic struct's fields are checked per instance.

[Enums](enums.md#generic-enums) take parameters the same way, and their
arguments can often be inferred.

`lib/alloc.jh` puts this together for freestanding code: a bump allocator over
`mmap` and a growable `Vec(T)` built on it, all in jihoo
([Standard library](../reference/standard-library.md#alloc),
`examples/arena.jh`).
