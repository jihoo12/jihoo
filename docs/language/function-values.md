# Function values

Functions are values: they can be stored in variables, fields and arrays,
passed to other functions and returned from them.

```jihoo
struct Command {
    name: str
    run: fn(i64) -> i64          // `fn(A, B) -> R`; without `-> R` it returns unit
}

fn square(x: i64) -> i64 { return x * x }

fn map(xs: [i64; 4], f: fn(i64) -> i64) -> [i64; 4] {
    let i = 0
    while i < 4 {
        xs[i] = f(xs[i])
        i = i + 1
    }
    return xs
}

fn twice(comptime f: fn(i64) -> i64, x: i64) -> i64 { return f(f(x)) }

map(xs, square)                  // a function name is a value
commands[i].run(7)               // so is anything of a function type
pick(1)(2, 3)                    // a function returning a function
```

## Function types

`fn(A, B) -> R` is the type of a function taking an `A` and a `B` and returning
an `R`; `fn(A)` returns `unit`. The `-> R` must be on the same line as the
`fn(...)`, so a field of function type ends at the newline.

## Making and calling function values

- A function name used as a value has type `fn(params) -> ret`. Module
  functions work the same way (`alloc.push` as a value). Generic functions and
  macros are not values, and [builtins](../reference/builtins.md) such as
  `print` are not functions.
- [Anonymous functions and closures](closures.md) are function values too.
- Any expression of a function type can be called: `f(x)`, `s.f(x)`,
  `fs[0](x)`, `make()(x)`.
- A local variable of a function type hides a function of the same name when
  called; a local of another type does not, so `let len = len(a)` still works.

## In each profile

Function values are plain values in every profile: on the VM an index into the
module's functions, natively a code pointer (8 bytes, so `size_of` and struct
layouts work, and C can call them: see [Calling C](calling-c.md#callbacks)). In
JIR they are `funcref @f` and `call %r(...)`.

Closures that capture variables need the GC, so only hosted programs can store
them; see [Closures](closures.md#comptime-closures) for how compiled code uses
them anyway.

## Direct calls

A function known at compile time is called directly, without indirection:

- a `comptime f: fn(...)` parameter, as in `twice` above — the function gets one
  instance per function passed;
- a constant, `const F = square`. Compile-time code can compute function values,
  `const G = choose(1)`, and store them in constant structs and arrays.

## Comparing

Function values cannot be compared with `==`. Closures are function values too,
and there is no good answer to whether two closures are equal.
