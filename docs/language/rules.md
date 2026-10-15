# Rewrite rules

A function can say that it computes the same as some other code, and replace
that code wherever the program has it. The compiler **checks the claim before
it uses the rule**. It runs both on the VM with inputs it makes up, including
functions for parameters that take functions, and refuses a rule they
disagree on.

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

`jihoo emit-ir` shows the calls of rules.

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
result it cannot compare (function values), or whose code cannot run while
compiling (C functions, `syscall`).

These are tests, not proofs, and they share the limits of contracts:

- **Sample functions have no effects.** A rule that changes how many times,
  or in which order, the functions it is given are called (as `map_map`
  does: `g, g, g, f, f, f` becomes `g, f, g, f, g, f`) gives the same values
  for the functions it is checked with. A function that prints or changes a
  [cell](cells.md) can tell the difference.
- **Large inputs are not tried.** Inputs that need more than 100 000 VM steps
  are skipped, as for contracts ([Limits](../reference/limits.md)).

A classic that the checks catch: `a >> 1` is not `a / 2`. Division rounds
towards zero and a shift rounds down, and `a = -1` tells them apart. The rule
must say where it holds instead, `replaces div(abs(a), 2)`.

See `examples/rules.jh`.
