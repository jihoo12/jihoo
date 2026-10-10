# Variables and control flow

Inside a function body, statements run in order. jihoo has one way to declare a
variable, two branching forms (`if` and [`match`](patterns.md)), one loop
(`while`), and `return`.

```jihoo
fn collatz(n: i64) -> i64 {
    let steps = 0
    while n != 1 {
        if n % 2 == 0 {
            n = n / 2
        } else {
            n = 3 * n + 1
        }
        steps = steps + 1
    }
    return steps
}
```

## `let`

`let name = value` declares a variable and gives it its first value; there are
no uninitialized variables. The type is inferred from the value, or written
out, in which case the value must have that type (and literals take it):

```jihoo
let n = 10            // i64
let b: u8 = 200       // the literal is a u8
let p = Point { x: 1, y: 2 }
```

Variables can be assigned again (`n = n + 1`); there is no `mut`. Parameters
are variables too, except `comptime` parameters, which cannot be assigned.

A variable is visible from its `let` to the end of the block it is declared in.
Declaring a name that is already visible makes a new variable that hides the old
one from then on, even with a different type:

```jihoo
let x = "42"
let x = 42            // a new variable; the str is no longer reachable by name
```

## Assignment

`place = value` stores into a *place*: a variable, a field (`p.x`), an array
element (`a[i]`), and in compiled code what a pointer points to (`*p`, `p[i]`,
`p.x` through a pointer), or the contents of a [cell](cells.md) (`*c`, `c.x`).
Places nest: `line.a.x = 1`, `grid[r][c] = 0`.

Since structs and arrays are [values](structs.md#value-semantics), assigning to
a part of one changes only that variable. There are no compound assignment
operators: write `i = i + 1`.

## `if`

```jihoo
if n < 0 {
    print("negative")
} else if n == 0 {
    print("zero")
} else {
    print("positive")
}
```

The condition must be a `bool` — there is no implicit conversion from integers
or pointers — and needs no parentheses; the braces are required. `if` is a
statement, not an expression; to choose a value, use a
[`match` expression](patterns.md#match-expressions):

```jihoo
let sign = match n < 0 { true => -1, false => 1 }
```

A struct literal cannot appear directly in a condition, since
`if p == Point { ... }` would be ambiguous; wrap it in parentheses:
`if p == (Point { x: 0, y: 0 }) { ... }`.

## `while`

`while cond { body }` runs the body as long as the condition, a `bool`, holds.
It is the only loop; count with a variable:

```jihoo
let i = 0
while i < len(xs) {
    total = total + xs[i]
    i = i + 1
}
```

There is no `break` or `continue` yet. To leave a loop early, make the
condition false, or put the loop in a function of its own and `return` from it.

## `return`

`return value` leaves the function with a value; a function without a result
type ends with `return` or by reaching the end of its body. A function that has
a result must `return` on every path that reaches the end of its body. The
compiler does not reason about conditions, so a loop such as `while true { ... }`
still needs a `return` after it:

```text
error: f.jh:3:1: missing `return`: `find` must return i64
```

An exhaustive [`match`](patterns.md) whose arms all return is enough, since it
has no way to fall through.

## Other statements

| statement | page |
|-----------|------|
| an expression on its own, usually a call: `f(x)` | [Functions](functions.md) |
| `match value { pattern => ... }` | [Pattern matching](patterns.md) |
| `go f(x)` and `select { ... }` | [Tasks and channels](tasks-and-channels.md) |
| `name!(...)`, a statement macro | [Macros](macros.md#statement-and-item-macros) |

There are no nested function declarations; use a named function at the top
level, or a [closure](closures.md) (`let f = fn(x: i64) -> i64 { ... }`).
