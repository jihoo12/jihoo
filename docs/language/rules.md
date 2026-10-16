# Rewrite rules

A function can say that it computes the same as some other code, and replace
that code wherever the program has it. The compiler **checks the claim before
it uses the rule**. It runs both on the VM with inputs it makes up, including
functions for parameters that take functions, and refuses a rule they
disagree on. And it only applies a rule where that cannot change what the
program prints or where it traps ([Rules and effects](#rules-and-effects)):
where the functions it is given have no effects, or where it calls them
just as the code it replaces does.

```jihoo
import lists

// `map` of a `map`, in one pass and without the list in between.
fn map_map(comptime T: type, comptime U: type, comptime V: type,
           xs: [T], g: fn(T) -> U, f: fn(U) -> V) -> [V]
    replaces lists.map(lists.map(xs, g), f)
{
    let out: [V] = []
    for x in xs {
        push(out, f(g(x)))
    }
    return out
}

fn main() {
    let xs: [i64] = [1, 2, 3]
    // Compiled as `map_map(xs, ..., ...)`.
    print(lists.map(lists.map(xs, fn(x: i64) -> i64 { return x + 1 }),
                    fn(x: i64) -> i64 { return x * 10 }))      // [20, 30, 40]
}
```

Write `g(f(x))` in the body by mistake and the build stops:

```text
error: fuse.jh:6:5: `map_map` does not do what it replaces: with xs = [0],
g = fn(x) x + 1, f = fn(x) 0, `lists.map(lists.map(xs, g), f)` gives [0],
but `map_map` gives [1]
```

Other languages have rules like these: GHC's `RULES` pragmas, the
algebraic rewrites inside compilers. They are trusted as written, and a wrong
one silently changes what programs compute. Here a rule is an ordinary
function, so it type-checks like one, and the compiler tests it the way it
tries to break [contracts](contracts.md).

## Writing a rule

`replaces pattern` goes between a function's signature and its body, with
[`requires`, `ensures`](contracts.md) and [`nocopy`](nocopy.md) in any order.
The function is the rule:

- **The pattern is a call.** It is made of calls, operators, literals, names
  of constants and functions, and the function's parameters.
- **Each parameter appears exactly once** in the pattern. It stands for
  whatever is written in its place in a matching call.
- **Comptime parameters must be types.** They are inferred at each call, as
  for any generic function.
- **The function returns what the pattern does.**

```jihoo
fn power(x: i64, n: i64) -> i64 { ... }

fn square(x: i64) -> i64 replaces power(x, 2) {
    return x * x
}
```

`power(a + 1, 2)` now compiles as `square(a + 1)`, and `power(a, 3)` stays
as it is.

## Where a rule applies

Every call in the program that matches a pattern calls the rule instead, in
every module, including the standard library's. A call matches when it calls
the same functions, by the item the names refer to, not by spelling. The
literals and names must be the same, in the same places, wherever the pattern
has them.

The parts that stand for parameters are evaluated **once each, in the order
they are written**, as the original call would have evaluated them, and then
passed to the rule. The other parts of the original call are never run. If
the parts' types do not fit the rule's parameters, the rule does not apply
and the call is compiled as written: a rule for `i64` leaves a call on `i32`
alone. When several rules match, the first that fits applies, in order of
their names. Rules do not apply inside rules, so a rule can call what it
replaces.

`jihoo emit-ir` shows the calls of rules. The standard library has some:
`lists.map` to the same type and `lists.filter` become `map_in_place` and
`filter_in_place`, which write into the list they are given
([Standard library](../reference/standard-library.md#lists)). They take
part in the order of names like any other rule.

## Rules and effects

A rule changes when code runs. `map_map` calls `g` and `f` in turn for each
element, where the two `map`s call `g` for every element first, then `f`.
And the parts of the pattern that are not parameters, such as the inner
`lists.map`, now run after all the parts instead of between them. Values
cannot tell the difference, but a function that prints, changes a
[cell](cells.md) or traps can:

```jihoo
let ys = lists.map(lists.map(xs, fn(x: i64) -> i64 { print(x)
                                                     return x + 1 }),
                   fn(x: i64) -> i64 { print(x * 10)
                                       return x })
// As written: 1 2 3 20 30 40. Fused, it would print 1 20 2 30 3 40.
```

So a rule only applies where the compiler can see that moving the code
changes nothing. It looks at the code of each matching call, and at what
every function it calls does, through all their calls. Code is **inert**
when it has no effect and is certain to return. It must not print or read
input, use cells, pointers, atomics, channels or tasks, call a C function, a
function value or `syscall` or `asm`, and must not trap or loop: no integer
division, no indexing, no `pop`, no loop, no recursion. A rule applies to a
call only when:

- **Every function the call passes to it is inert, and visible**, or the
  rule [keeps the order of calls](#rules-that-keep-the-order-of-calls). A
  visible function is written in the call (`fn(x: i64) -> i64 { return x + k }`,
  which may capture values) or named (`inc`). A function value from a
  variable, a parameter or another call could be anything.
- **No part moves past code that can be seen.** A part evaluated after one
  of the pattern's own calls or operators must be inert, unless those are.
  In `sub(neg(a), b)`, any `b` may move past `neg`, which only negates. In
  `sub(tenth(a), b)`, where `tenth` divides, `b` must be inert: `tenth(0)`
  traps before `b` prints anything, but the rule would evaluate `b` first.
- **Every part right of `&&` or `||` is inert**, since the call as written
  may not evaluate it, and the rule always does.

Otherwise the call is compiled as written, and does what it says. Both
effects and traps count: a function that prints but cannot trap, or one
that can trap but does nothing else, can each tell the order apart.

The parts themselves may do anything when nothing moves past them: the
first part, and the parts before the pattern's first call that is not
inert. `lists.map(lists.map(load(), g), f)` fuses, and `load()` prints once,
first, as written.

### Rules that keep the order of calls

Some rules call the functions they are given just as their pattern does.
The standard library's `map_in_place` calls `f` once for each element, in
order, as `lists.map` does; it only puts the results somewhere else. Then
nothing a function does can tell the rule from the pattern, and the rule
applies to any function, not only inert ones:

```jihoo
let words: [str] = ["map", "in", "place"]
// `to_upper` loops, so it is not inert; compiled as `map_in_place` anyway.
words = lists.map(words, fn(w: str) -> str { return strings.to_upper(w) })
// Prints 1 2 3 as written, and so does `map_in_place`.
xs = lists.map(xs, fn(x: i64) -> i64 { print(x)
                                       return x * 2 })
```

The compiler finds this out by trying, the first time a rule would apply
to a function that is not inert
([What is checked](#what-is-checked)). A rule keeps the order of calls
when, for every input tried:

- it calls the same functions, with the same arguments, in the same order,
  and as many times as the pattern;
- it prints the same as the pattern in between, and returns the same;
- when one of the functions traps at some call, it has printed the same up
  to there and traps there too, as the pattern does. A rule that checks
  something itself and traps before calling `f`, where the pattern calls
  `f` first, does not keep the order.

`map_map` does not: it calls `g` and `f` in turn, where the pattern calls
every `g` first. So it still fuses only `map`s of inert functions. A rule
that calls `f` from the last element to the first, or twice for each one,
gives the same values as `map` for inert functions, and applies only to
those.

Such a rule may be given any function value, even one from a variable or a
parameter. The other conditions above stay as they are: a part that moves
past the pattern's own calls must be inert, and so must a part right of
`&&` or `||`. A function held inside another value, such as a struct, is
never traced, so a part that holds one still keeps the call as written.

## What is checked

Each rule the program compiles is run next to its pattern, on about a
thousand inputs. Both must give the same value, and print the same, or both
must trap. Inputs are made the way they are for contracts: edge values first,
then seeded random ones, the smallest difference reported. Parameters of
function type get small functions the compiler writes:

| result type | functions tried |
|-------------|-----------------|
| integer | `fn(x) 0`, `x`, `x + 1`, `x * 2`, `-x`, `x * x`, `x + y`, `fn(x) 7` |
| float | `fn(x) 0.0`, `x`, `x + 1.0`, `x * 2.0`, `fn(x) 1.5` |
| `bool` | `false`, `true`, `x % 2 == 0`, `x > 0`, and `x` and `!x` of a bool |
| anything else | a parameter of that type, and a few constants |

A generic rule is checked at every instance the program uses, and also with
`i64` for every type parameter. So a rule is checked even if no call in the
program uses it.

The compiler refuses a rule it cannot check: one whose parameters it cannot
make up (pointers, cells, structs whose fields another module keeps), whose
result it cannot compare (function values), whose code cannot run while
compiling (C functions, `syscall`), or that reads standard input (both
sides run with none).

To find out whether a rule keeps the order of calls, the compiler runs it
next to its pattern once more, at the instance a call needs, on a quarter
as many inputs (their values are checked already), with each
function it is given wrapped in one that records the call: which parameter
the function was given for, and its arguments where they are numbers,
bools or strings. Each input is tried twice: once as it is, and once with
the wrapped functions trapping at one of the first four calls, counted
across all of them. The records, what each side prints, its result and
where and why it traps must all be the same. A rule that does not pass
is not an error: it applies only to inert functions, as before.

These are tests, not proofs, and they share the limits of contracts:

- **Only some inputs are tried.** A rule that holds for every input tried
  may still be wrong for another, and the sample functions are a few simple
  inert ones. That is why a rule applies only to inert functions
  ([Rules and effects](#rules-and-effects)) unless it is shown to keep the
  order of calls: for inert functions, the order and number of calls cannot
  be seen, so the values are all there is to compare. Arguments that are
  lists, structs or enums are not recorded, only that a call was made.
- **Large inputs are not tried.** Inputs that need more than 100 000 VM steps
  are skipped, as for contracts ([Limits](../reference/limits.md)).

What counts as inert is a conservative reading of the code, which keeps more
calls as written than it has to:

- A function with any loop is taken to possibly loop forever, so a callback
  that sums a list is not inert.
- Integer division and indexing may trap, even by a constant or in bounds.
- A pattern's call of a generic function (`lists.map` in `map_map`) is never
  inert, so the parts after it must be.
- Running out of memory or stack is not counted as a trap.

A classic that the checks catch: `a >> 1` is not `a / 2`. Division rounds
towards zero and a shift rounds down, and `a = -1` tells them apart. The rule
must say where it holds instead, `replaces div(abs(a), 2)`.

See `examples/rules.jh`.
