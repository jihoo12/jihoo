# Closures

`fn(params) -> R { body }` in an expression is an anonymous function. It may use
the local variables around it, which makes it a *closure*.

```jihoo
fn adder(n: i64) -> fn(i64) -> i64 {
    return fn(x) { return x + n }        // captures `n`
}

fn count(xs: [i64; 4], keep: fn(i64) -> bool) -> i64 {
    let n = 0
    let i = 0
    while i < 4 {
        if keep(xs[i]) { n = n + 1 }
        i = i + 1
    }
    return n
}

fn main() {
    let add5 = adder(5)
    print(add5(10))                      // 15

    let limit = 5
    print(count([3, 8, 5, 1], fn(x) { return x <= limit }))   // 3
    let sq = fn(x: i64) -> i64 { return x * x }
    print(sq(7))                         // 49
}
```

## Anonymous functions

Parameter and result types may be left out when the expected type is a function
type — an argument, a typed `let`, a return value, as in `adder` above.
Otherwise they are written out, and a missing `-> R` means `unit`. The body must
start on the same line as the parameters (`fn(x) {`); a `fn(...)` without a body
is a function type.

A closure has the same type as any other function value, `fn(A) -> R`, so
functions that take functions take closures too. Like other function values,
closures cannot be compared.

A closure cannot call itself by name, since it has none; use a named function
for recursion.

## Capturing by value

A closure captures the variables it uses by value, when it is made:

```jihoo
let n = 1
let get = fn() -> i64 { return n }
n = 2
print(get())                 // 1: the closure has its own copy
```

- Changing the variable later does not change the closure.
- Assigning to a captured variable inside the closure is an error, since the
  change would be lost between calls.

This is the same value semantics as everywhere else, and it means closures
share no mutable state. For state that a closure keeps between calls, or shares
with others, capture a [cell](cells.md):

```jihoo
let count = cell(0)
let next = fn() -> i64 {
    *count = *count + 1
    return *count
}
```

## How closures are compiled

The body becomes a function of its own, `fn.N` in JIR, whose first parameters
are the captured values (`crates/jihoo-sema/src/closures.rs`).

- One that captures nothing is a plain `funcref`, which works everywhere,
  including freestanding code and compile-time constants.
- One that captures is `closure @fn.N(%captured...)`: a GC object on the VM,
  holding the function and the captured values. So it is only a run-time value
  in hosted programs.

## Comptime closures

A closure passed to a `comptime` parameter is not a value at run time, so
compiled code can use capturing closures too:

```jihoo
#![freestanding]
import io

fn each(comptime f: fn(i64), xs: [i64; 5]) {
    let i = 0
    while i < 5 {
        f(xs[i])
        i = i + 1
    }
}

fn _start() -> i64 {
    let xs = [3, 14, 1, 59, 26]
    let limit = 10
    each(fn(x) { if x > limit { io.print_int(x) } }, xs)   // no GC needed
    return 0
}
```

- The function gets an instance for that closure, like for any other comptime
  argument, and the instance takes the captured values as hidden arguments after
  its own; `f(x)` in it is a direct call, `call @fn.N(captured..., x)`. No heap
  and no indirect call: this is how Rust compiles `impl Fn` arguments.
- Inside the instance, the closure can be called, passed on to another
  `comptime` parameter, or captured by another closure that is passed on.
  Using it as a value (storing or returning it) makes a closure value, which
  needs the GC: fine in hosted code, an error in compiled code.
- Each closure written in the source is its own instance, so a function called
  with many different closures is compiled many times.

See `examples/closures.jh` and `examples/comptime_closures.jh`.
