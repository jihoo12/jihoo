# Pattern matching

`match` compares a value against patterns, in order, and runs the first arm
whose pattern matches. It works on enums, structs, integers and bools, and the
compiler checks that every possible value is handled.

```jihoo
enum Shape { Circle(i64), Rect(i64, i64), Empty }
struct Point { x: i64, y: i64 }

fn describe(s: Shape, p: Point) -> i64 {
    match s {
        Circle(0) | Empty => return 0
        Rect(w, h) if w == h => return w * w     // a guard
        Rect(w, h) => return w * h
        Circle(r) => return 3 * r * r
    }
}

fn axis(p: Point) -> i64 {
    match p {
        Point { x: 0, y: 0 } => return 0
        Point { x: 0, .. } => return 1
        Point { y, .. } if y > 0 => return 2
        _ => return 3
    }
}
```

## Arms

Each arm is `pattern => body` or `pattern if guard => body`. The body is a block
(`=> { ... }`) or a single statement (`=> return 0`, `=> total = total + n`).
Arms are separated by newlines or commas.

Arms are tried in order; the first one whose pattern matches, and whose guard
(a `bool` expression) holds, runs. The guard can use the names the pattern
binds.

## Patterns

| pattern | matches |
|---------|---------|
| `_` | anything |
| `x` | anything, bound to a new local `x` |
| `Empty`, `Rect(p, q)` | that variant of the matched enum, and its payload |
| `Point { x, y: 0, .. }` | a struct; `x` alone is `x: x`, `..` ignores the other fields |
| `0`, `-1`, `true` | that value |
| `Circle(_) \| Empty`, `Some(1 \| 2)` | either pattern |

- Patterns nest: `Some(Rect(w, 0))`, `Pair { a: Some(x), b: None }`.
- Variants are written without the enum, whose type is known. A name is a
  variant if the matched enum has one by that name, and a new variable
  otherwise. A name that starts with an uppercase letter must be a variant, so
  a misspelled variant is an error, not a binding.
- Every alternative of a `|` pattern binds the same names, with the same types.
- Patterns read through [refs](references.md): `Cons(x, Cons(y, _))` matches a
  list whose tail is a `ref List`. A name or `_` takes the ref itself.
- Floats, strings, arrays and pointers cannot be matched against patterns;
  compare them with `==` in a guard.

## Exhaustiveness

A `match` must cover every value, and every arm must match some value the
arms above it miss. Both are checked with Maranget's usefulness algorithm, so
nested patterns are handled exactly. An error names a value that is missed, as
a pattern:

```text
error: shapes.jh:8:5: `match` does not cover `Some(Rect(_, _))`
```

Arms with a guard do not count towards covering, since the guard may be false.
An arm that can never match, or an alternative of a `|` pattern that can never
match, is an error too. Since an exhaustive `match` has no fall-through, a
function whose arms all `return` needs no `return` after it.

## `match` expressions

`match` is also an expression, with a value after each `=>`:

```jihoo
let word = match n % 15 {
    0 => "fizzbuzz"
    r if r % 3 == 0 => "fizz"
    r if r % 5 == 0 => "buzz"
    _ => to_str(n)
}
```

Every arm has the type of the first one (or the expected type). At the start of
a statement, `match` is the statement form, whose arms are statements; to use a
`match` value as a statement on its own, assign it.

This is also how to write a conditional expression:
`let sign = match x < 0 { true => -1, false => 1 }`.

The implementation is in `crates/jihoo-sema/src/patterns.rs`.
