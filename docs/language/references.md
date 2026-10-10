# References

`ref T` is an immutable reference to a `T` on the GC heap. It is how hosted
programs build recursive data such as lists and trees.

```jihoo
enum List(T: type) {
    Cons(T, ref List(T))
    Nil
}

fn sum(l: List(i64)) -> i64 {
    match l {
        Cons(x, rest) => return x + sum(*rest)
        Nil => return 0
    }
}

struct Point { x: i64, y: i64 }

fn main() {
    let l = List.Cons(1, ref List.Cons(2, ref List.Nil))
    print(sum(l))                  // 3
    let p = ref Point { x: 1, y: 2 }
    print(p.x)                     // fields and elements read through a ref
}
```

## Making and reading refs

- `ref e` makes a new reference holding a copy of `e`.
- `*r` is the value it refers to, and `r.x` and `r[i]` read a field or element
  through it.
- [Patterns](patterns.md) read through refs: `Cons(x, Cons(y, _))` matches a
  list whose tail is a `ref List`.

## Immutable

Refs are immutable: `*r = v` and `r.x = v` are errors. So a value shared through
refs behaves exactly as if it had been copied, and jihoo keeps its value
semantics. A changed version of a structure is built from the parts that change
and refs to the parts that do not, as in `examples/lists.jh`:

```jihoo
// A new list with `x` in front: the old list is shared, not copied.
fn push(l: ref List(i64), x: i64) -> List(i64) {
    return List.Cons(x, l)
}
```

`==` on refs compares the values they refer to ([Equality](equality.md)). For
mutable shared state, use a [cell](cells.md).

## Recursive types

A type cannot contain itself by value, since its size would be infinite. A
`ref` breaks that rule, which is what makes lists and trees possible: a
`ref List` field holds a reference to a list, not the list itself.

## Hosted only

Like `str`, refs need the GC, which owns the value. Native and freestanding code
uses [pointers](pointers.md) instead. On the VM a ref is a one-element heap
object; in JIR it is the type `ref T`, made with `ref` and read with `deref`.
