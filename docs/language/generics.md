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
- Type arguments are written in expression position and read back as types
  (or [left out](#inferred-type-arguments)):
  `u8`, `*u8`, `[u8; 4]` and `fn(i64) -> i64` all parse as expressions.
- Inside an instance, `T` stands for its type and `N` for a constant, also in
  array lengths and `size_of`. Comptime parameters cannot be assigned.
- `comptime T: Ord` takes only types that implement a [trait](traits.md#bounds),
  checked at the call. Either way the body is checked per instance, and errors
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

## Inferred type arguments

A call may leave out the type arguments, giving one argument per runtime
parameter; the types are then inferred from the other arguments:

```jihoo
let x: u8 = 7
max(x, 200)                                  // T = u8: the literal takes it from `x`
max("app", "apple")                          // T = str
lists.map(xs, fn(v) -> str { return to_str(v) })   // T from `xs`, U from the function
lists.sort_by(words, fn(a, b) { return a < b })    // the function's types from T
```

- Each argument with a type of its own is matched against its parameter's type:
  `[T]` against `[u8]` gives `T = u8`, `Pair(T)` against `Pair(i64)` gives
  `T = i64`, and likewise through tuples, pointers, function types and the
  other type constructors.
- Arguments that take their type from where they are used, literals and
  anonymous functions with types left out, wait for the arguments that decide
  their parameter's type, whatever their order, and are then checked against
  it. A literal nothing decides takes its default type, as in `let`:
  `max(3, -5)` is an `i64` instance.
- An anonymous function's types left out must be known from other arguments:
  in `lists.map(xs, fn(v) { ... })` nothing says what `U` is, so the error asks
  for `-> U` on the function, or for the type arguments.
- Only `comptime T: type` parameters are inferred. A function with a value
  parameter (`comptime N: i64`) is always called with all its arguments.
- An argument whose type contradicts an earlier one is an error that names
  both: `` `T` of `max` is str here, but u8 from an earlier argument ``.

Inferred or written out, the same types give the same instance.

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
- An instance is in JIR only when the program's code or its other types use
  it. One used only while compiling, in a macro or for `size_of`, is not, so a
  macro of a freestanding program may use `Option(str)` or `(str, i64)`; the
  same holds for [tuple](tuples.md) types.
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
