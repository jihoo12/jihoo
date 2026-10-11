# Lists

`[T]` is a list: any number of values of type `T`, growing and shrinking as
the program runs. Lists are hosted only; compiled programs use arrays, or
`alloc.Vec(T)` ([Standard library](../reference/standard-library.md#alloc)).

```jihoo
let xs: [i64] = [3, 1, 2]     // a list, because a list is expected
push(xs, 4)                   // [3, 1, 2, 4]
print(len(xs))                // 4
print(pop(xs))                // 4; xs is [3, 1, 2] again
xs[0] = 10
let tail = xs[1:]             // [1, 2], a new list
let ys = xs + [5, 6]          // [10, 1, 2, 5, 6]
let zeros: [u8] = [0; 1024]   // 1024 zeros; the length may be any i64
```

## Making lists

- A list literal is written like an array literal, `[a, b, c]` or `[v; n]`,
  where a list is expected: a typed `let`, a parameter, a field, a return
  value, an element of another list. Elsewhere `[1, 2]` is an array
  `[i64; 2]`.
- `[]` is the empty list; it always needs a list type from where it is used.
- In `[v; n]`, `n` is any `i64` expression, worked out when the program runs.
- `a + b` is a new list with the elements of `a` and then those of `b`.

## Using lists

| operation | result |
|-----------|--------|
| `len(xs)` | the number of elements, an `i64` |
| `xs[i]`, `xs[i] = v` | element `i`, bounds-checked as for arrays |
| `xs[lo:hi]`, `xs[lo:]`, `xs[:hi]` | a new list of elements `lo` up to `hi`, as for [strings](types.md#strings) |
| `push(xs, v)` | appends `v` |
| `pop(xs)` | removes the last element and gives it; the program stops if the list is empty |
| `a == b`, `a != b` | the same length and equal elements ([Equality](equality.md)) |

`push` and `pop` change the list they are given, which must be something that
can be assigned: a variable, a part of one (`s.items`, `grid[i]`), or what a
cell holds (`*c`, `c.items`). An index past the end is an error, as for
arrays: a list grows only by `push`.

## Lists are values

Like arrays and structs, lists are values: assigning a list, passing it to a
function or storing it in a field makes a copy, and changing one copy never
changes another.

```jihoo
fn add_one(xs: [i64]) -> [i64] {
    push(xs, 1)               // changes the function's own copy
    return xs
}

let a: [i64] = [0]
let b = add_one(a)
print(len(a))                 // 1
print(len(b))                 // 2
```

To have several parts of a program change the same list, keep it in a
[cell](cells.md): `push(*c, v)` changes what every holder of the cell sees.

## Performance

A copy costs nothing until one of the copies is changed: the VM shares the
elements and copies them once, at the first change after the list was copied
somewhere ([VM and GC](../internals/vm.md#value-semantics-and-in-place-updates)).
So:

- `push`, `pop` and element writes on a list that nothing else holds take
  constant time (`push` amortized): a loop building a list of `n` elements is
  O(n). This holds for a list in a variable, in a field of a struct in a
  variable, in another list, or in a cell.
- Passing a list to a function and changing it there copies it once, for that
  call. A function that only reads its list never copies it.
- `xs[lo:hi]` and `a + b` make new lists, so they take time in proportion to
  their length.

## Strings and bytes

A `str` is not a list, but converts to and from one: `from_bytes(b)` makes a
string of the bytes in `b: [u8]`, and `strings.bytes(s)` gives the bytes of a
string ([Standard library](../reference/standard-library.md#strings)).
Pushing bytes to a `[u8]` and converting it once at the end is how to build a
long string in time proportional to its length, where `s = s + piece` in a loop
copies the text so far each time.

## Library

`lib/lists.jh` has generic functions on lists: `sort`, `map`, `filter`,
`fold`, `reverse`, `index_of`, `contains` and `range`
([Standard library](../reference/standard-library.md#lists)).

## Layout

Lists live on the GC heap and have no fixed layout, so `size_of([T])` is an
error, and they cannot be passed to C. In JIR a list is `[T]`, built with
`array` or `repeat` and changed with `push` and `pop`
([JIR](../jir.md#lists-and-strings-hosted-only)).
