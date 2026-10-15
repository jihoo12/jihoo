# Contracts

A function can say what it needs and what it promises. The compiler then
**tries to break the promise before the program ever runs**. It calls the
function on the VM with inputs it makes up, and if one breaks the contract,
that input is a compile error.

```jihoo
fn mid(lo: i32, hi: i32) -> i32
    requires lo <= hi
    ensures lo <= result && result <= hi
{
    return (lo + hi) / 2
}
```

```text
error: mid.jh:3:5: contract broken: mid(-2147483648, -1) returns 1073741823,
so `ensures lo <= result && result <= hi` is false
```

`lo + hi` overflows when both are large or both are negative: the bug that sat
in binary searches for decades. The fix, for indices that are never negative:

```jihoo
fn mid(lo: i32, hi: i32) -> i32
    requires 0 <= lo && lo <= hi
    ensures lo <= result && result <= hi
{
    return lo + (hi - lo) / 2
}
```

This compiles. Drop `0 <= lo` and it fails again, because `hi - lo` overflows
for `mid(-2147483648, 0)`.

## Clauses

`requires cond` and `ensures cond` go between a function's signature and its
body, as many as needed, each a `bool` expression:

- **`requires`** is the caller's duty. It may read the parameters.
- **`ensures`** is the function's duty. It may read the parameters, as they
  were when the function was called (even if the body assigned to them), and
  `result`, the value returned. A function that returns nothing has no
  `ensures`.

Clauses can call functions, match, and do anything an expression can:

```jihoo
fn insert(xs: [i64], x: i64) -> [i64]
    requires is_sorted(xs)
    ensures is_sorted(result) && len(result) == len(xs) + 1
{ ... }

fn area(s: Shape) -> i64
    requires match s { Square(n) => n >= 0 && n < 1000, Rect(w, h) => w >= 0 && h >= 0 && w < 1000 && h < 1000 }
    ensures result >= 0
{ ... }
```

`requires`, `ensures` and `result` are only special there. Everywhere else
they are ordinary names.

## What the compiler tries

After the program type-checks, every function with a contract is run on the VM,
as [compile-time code](compile-time-evaluation.md) is. It gets about a
thousand inputs:

1. **Values where code goes wrong**: `0`, `1`, `-1`, the smallest and largest
   integer of the type and their neighbours; `0.0`, `-0.0`, infinities and NaN;
   `""`, `"a"` and a few short strings; the empty list and short ones. Every
   combination is tried while there are few, then as many as fit in a third of
   the inputs.
2. **Random values**, growing in size, from a seed fixed by the function's name,
   so the same program always gives the same answer.

Structs, tuples, arrays, enums and `ref`s are built from their parts. Inputs
that a `requires` clause rules out are skipped. The contract is broken when, for
an input that meets every `requires`:

- an **`ensures` clause is false** for the value returned;
- the function **calls another function with arguments that break its
  `requires`**. The caller is to blame:

  ```text
  contract broken: hyp(0, 1) calls isqrt(-1), which breaks `isqrt`'s `requires n >= 0`
  ```

- the function **traps**: an index out of bounds, a division by zero, `pop` of
  an empty list, `trap()`. A function with a contract has promised to work for
  every input its `requires` allows:

  ```text
  contract broken: get([], 0) traps (index 0 out of bounds for length 0); it meets
  every `requires`, but a function with a contract must not trap
  ```

The first input that fails is then **made smaller** while it fails the same
way: integers move towards 0, strings and lists get shorter, an enum becomes
a variant without a payload. So the error shows the simplest case, such as
`reverse("a") returns ""` rather than a random twelve-letter string.

When the clause that failed belongs to a recursive call deeper down, the
message says that a call of the function inside it returned the value, rather
than the call it shows. Making the input smaller usually reaches that call
directly.

## Checked when the program runs, too

Trying inputs is testing, not proof: a contract that survives may still break
on an input nobody tried. So every clause is also checked as the program runs,
in every profile. `requires` is checked when the function starts, `ensures`
when it returns. A false clause traps. On the VM, the error names the clause:

```text
error: runtime error in @half.requires.0: trap: the program called `trap()`
```

Natively and in freestanding programs this is the same signal as any other
trap. In a debugger, the backtrace shows the function `half.requires.0`, and
an `ensures` failure's function (`f.ensures.1`) receives the value that was
returned.

The checks cost what the clauses cost to evaluate. A natively compiled
`ensures` that reads a list parameter keeps it alive until the function
returns, and that may mean a copy if the body changes it; `jihoo build --cost`
shows it.

## What the compiler cannot try

Some functions are only checked when they run:

- functions with a parameter the compiler cannot make up: pointers, function
  values, cells, channels, and structs whose fields are private to another
  module. A `pub` function taking its own module's struct with private fields
  is in this group too: callers only get the values that module's functions
  make, which may keep rules the compiler does not know. A tuple, a struct with
  all fields `pub`, or a struct passed to a function that only its own module
  can call is built freely;
- functions whose code cannot run while compiling: anything that reaches a C
  function, a pointer, `syscall` or `asm`;
- generic functions and the functions of impls, except the instances the
  program uses (those are the ones compiled), and functions that take closures
  as `comptime` arguments;
- inputs that need more than 100 000 VM steps, which are skipped. All the
  inputs of one function share a budget of 20 million steps. See
  [Limits](../reference/limits.md).

## Tail calls

A function with `ensures` makes no [tail calls](functions.md#tail-calls): its result has to
be checked after the call returns, so `return f(x)` is an ordinary call, and so
is a call of itself. A function with only `requires` keeps them, and a call of
itself still runs in constant stack, checking `requires` on every round.

See `examples/contracts.jh`.
