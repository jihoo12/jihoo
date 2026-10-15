# Copy-free functions

Native programs have no GC. Lists, strings and closures move at their last
use and are copied only where the original is still needed, so a copy is
always a *meaning* of the program, never a runtime policy
([Lists in native programs](lists.md#lists-in-native-programs)). That also makes copies easy to
add by accident: one extra read of a list after it was handed on is enough.

`nocopy` makes the absence of copies part of a function's signature, checked
by the compiler:

```jihoo
fn grow(xs: [i64], n: i64) -> [i64] nocopy {
    for i in 0..n {
        push(xs, i * i)
    }
    return xs
}
```

When the program is compiled natively, `grow` and **every function it can
call** must not copy a list, a string or a closure's captured values. If one
does, the build fails, saying where the copy is, why it is there, and through
which calls the function reaches it:

```jihoo
fn twice(xs: [i64]) -> [i64] nocopy {
    let ys = xs
    push(ys, 1)
    push(ys, len(xs))      // `xs` is still used, so `ys = xs` copied it
    return ys
}
```

```text
error: twice.jh:2:12: `twice` is `nocopy`, but it copies [i64] here: parameter 1
is assigned and copied, since it is still used afterwards
```

A copy deeper down is reported at the call that leads to it:

```text
error: rev.jh:4:26: `rev` is `nocopy`, but this call of `reverse` copies str
(in `reverse`, line 37): a part read out of a value is copied, since the part
or the value changes while the other is still used
```

`nocopy` goes between the signature and the body, with
[`requires` and `ensures`](contracts.md) in any order. It is a word only
there.

## What counts

`nocopy` describes what calling the function costs when the caller **hands its
arguments over**, passing values it does not use again. A caller that keeps
using a list it passed makes the function copy it on entry. That copy is the
caller's, and the caller's own `--cost` lists it:

```jihoo
let a: [i64] = [9, 8]
print(sorted(a))           // copies `a` for `sorted`: `a` is printed next
print(a)
print(sorted([3, 1, 2]))   // copies nothing
```

The check looks at the program after the compiler has decided where values move
(the copies `jihoo build --cost` lists), in the function and everything it
calls, through the calls the compiler chose. Each callee is checked as it is
called from there, so it costs nothing extra.

These are not copies:

- moving a value at its last use, and reading a part of a value (`len(xs)`,
  `b.items[i]`), which stays a view;
- `ref`, `cell` and `chan` values, whose copies share one box and only count
  one more holder;
- numbers, bools, structs and arrays of them.

## Function values

Calls of function values are followed too. When no parameter of the function
can hold a function value, every closure it meets was made by it or by
something it calls. The compiler then checks those closures as if they were
called, wherever they end up: inside an iterator adapter, a struct, or a `go`
statement. A closure passed as a `comptime` parameter is a direct call
anyway:

```jihoo
import iters

fn odd_squares(xs: [i64]) -> i64 nocopy {
    let sq = iters.map(iters.of(xs), fn(x: i64) -> i64 { return x * x })
    return iters.sum(iters.filter(sq, fn(x: i64) -> bool { return x % 2 == 1 }))
}
```

When a parameter can hold a function value, its calls are an error, since the
caller could pass a closure that copies anything:

```text
error: `apply` is `nocopy`, but it calls a function value, whose copies the
compiler cannot see; call a function by its name, or take it as a `comptime`
parameter
```

## Profiles

Only native programs copy this way, so only they are checked. On the
[VM](../internals/vm.md#value-semantics-and-in-place-updates), lists are shared
and copied on the first write, so a hosted program accepts `nocopy` and runs
the same. Freestanding programs have no lists, strings or closures to copy.

## Finding copies

`jihoo build --cost` lists every copy of a whole program
([Command line](../reference/command-line.md#copies)). `nocopy` pins one
function down so that a later change cannot bring a copy back unnoticed. Both
read the same analysis, and give the same reasons.

Making the standard library copy-free under `nocopy` found three places where
the compiler copied without need, and one in the library. All four are fixed:

- A function that only passes a list on to one that keeps it now gets a
  consuming version too, so `f(xs)` → `g(xs)` → `keep(xs)` hands the list all
  the way down.
- A part handed on while the rest of the value is still used moves out on its
  own. An iterator adapter can give its inner iterator away and keep its
  closure.
- Two parts read from the same value no longer stop each other from moving.
- `lists.sort_by` read its list's length after moving the list, which made
  the move a copy. It now reads the length first.

So the iterator adapters of `lib/iters.jh` copy nothing over a list that was
handed to them, and neither does `lists.sort`.
