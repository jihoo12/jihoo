# Tuples

A tuple groups a few values without naming a struct for them: `(a, b)` is a
value of type `(A, B)`. Tuples are most useful for returning more than one
value.

```jihoo
fn divmod(a: i64, b: i64) -> (i64, i64) {
    return (a / b, a % b)
}

fn main() {
    let (q, r) = divmod(17, 5)
    print(q)                // 3
    print(r)                // 2
    let t = divmod(9, 4)
    print(t.0 + t.1)        // 3
}
```

## Tuple types

`(A, B, ...)` is the type of tuples of an `A`, a `B`, and so on: at least two
parts, none of them `unit`. Two tuple types with the same parts are the same
type wherever they are written, and their parts are public: a tuple is not
owned by a module the way a struct is.

Tuples work in every profile and can hold any value their profile has: lists
and strings in hosted and native code, pointers in native and freestanding
code. They can appear wherever a type can, such as in struct fields, array
and list elements, enum payloads and generic arguments (`Pair((i64, bool))`,
`max((u8, i64), a, b)`).

## Building and reading tuples

`(a, b)` builds a tuple; each part takes its expected type from the context,
so `let t: (u8, i64) = (1, 2)` works. `(a)` is just `a` in parentheses, and
`(a,)` is an error rather than a tuple of one value.

`t.0`, `t.1`, ... read the parts, in order. Like fields, they nest
(`n.0.1`) and can be assigned (`t.0 = 5`, `t.1 += 1`).

## Patterns and `let`

`(p, q)` is a pattern that matches a tuple whose parts match `p` and `q`, in
`match` like any other [pattern](patterns.md):

```jihoo
match (x % 3, x % 5) {
    (0, 0) => print("fizzbuzz")
    (0, _) => print("fizz")
    (_, 0) => print("buzz")
    _ => print(x)
}
```

`let` takes a pattern too when it starts with `(`: `let (a, b) = value` binds
`a` and `b` for the rest of the block. The pattern must match every value of
its type, so `let (0, b) = t` is an error; use `match` for that. Parts that
are structs can be taken apart in the same `let`:
`let (Point { x, .. }, n) = pair`.

## Value semantics

Tuples are values, like structs: copies never share parts, and comparing two
tuples with `==` compares their parts ([Equality](equality.md)). A tuple is a
struct whose name is its type, so it costs exactly what the struct would.
