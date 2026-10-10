# Functions

```jihoo
fn fib(n: i64) -> i64 {
    if n < 2 {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}

fn greet(name: str) {             // no `-> R`: returns unit
    print("hello, " + name)
}
```

## Declarations

`fn name(param: Type, ...) -> Result { body }` declares a function. Every
parameter has a written type, and so does the result; without `-> Result` the
function returns `unit`. A function that returns a value must
[`return`](control-flow.md#return) on every path.

Functions are module-level items, declared in any order: a function can call
one declared further down, and functions can be mutually recursive. They are
private to their [module](modules.md) unless declared `pub fn`.

There is no overloading, no default arguments, and no methods: functions that
work on a type take it as a parameter, usually first (`push(v, x)`; for a
struct to be changed, `push(&v, x)` in compiled code).

## Calls

`f(a, b)` evaluates the arguments from left to right, then calls `f`.
Arguments are passed by value: a function gets its own copy of every struct and
array, so it cannot change the caller's, and changing a parameter (`n = n - 1`)
changes only the function's copy. To let a function change something, pass a
[pointer](pointers.md) in compiled code or a [cell](cells.md) in hosted code.

The VM allows 10 000 nested calls; deeper recursion stops the program with
`stack overflow`. Compiled code is limited by the size of the OS stack.

## Entry points

A program starts at `main` (hosted, native) or `_start` (freestanding), which
returns nothing or an `i64` exit status. See [Profiles](profiles.md#entry-points).

## Compile-time parameters

A parameter marked `comptime` is given at compile time, and makes the function
generic: it is compiled once for each distinct set of compile-time arguments.

```jihoo
fn max(comptime T: type, a: T, b: T) -> T {
    if a > b { return a }
    return b
}

let m = max(u8, 3, 200)
```

See [Generics](generics.md). A `comptime` parameter of a function type makes
calls to it direct, and lets a [closure](closures.md#comptime-closures) be
passed without the GC.

## Functions as values

A function's name, used without calling it, is a value of type
`fn(params) -> result` that can be stored, passed and returned:

```jihoo
fn apply(f: fn(i64) -> i64, x: i64) -> i64 {
    return f(x)
}

apply(fib, 10)
```

See [Function values](function-values.md) and [Closures](closures.md), which
also covers anonymous functions (`fn(x: i64) -> i64 { return x * x }`).

## Builtins

Some names, such as `print`, `len` and `syscall`, look like functions but are
built into the compiler: they may accept several types and cannot be used as
values. They are listed in [Builtins](../reference/builtins.md).
