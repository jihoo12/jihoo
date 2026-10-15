# Lists

`[T]` is a list: any number of values of type `T`, growing and shrinking as
the program runs. Lists work in hosted and native programs, with the same
meaning; freestanding programs, which have no allocator, use arrays or
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

## Lists in native programs

Natively there is no GC. Since no two variables ever share a list, the
compiler can tell from the program alone where each list is last used, and
frees it there; nothing is counted or collected while the program runs.

- Assigning or passing a list that is not used again afterwards moves it: no
  copy. A copy is made only where the original is still used, as the
  meaning of the program requires.
- Reading a part, as `len(g.nodes[i].edges)`, copies nothing, also while
  another part of the same value changes (`let t = s.text` as `s.at += 1`).
- A value matched and not used again gives its parts away: in `match
  parse(s) { Done(v, rest) => ... }` each arm takes `v` and `rest` out of
  the result instead of copying them.
- A function that only reads a list parameter never copies it. One that
  changes it copies it once, as on the VM, unless the caller's list was at
  its last use: then the function takes it over, so `xs = f(xs)` changes
  `xs` in place. The same holds for a part: `rows[i] = f(rows[i])` and
  `b.items = f(b.items)` move the part out and back.
- A thread started with `go f(xs)` gets its own list, and frees it.

`jihoo build --cost` lists every place a program copies a list, and why
([Command line](../reference/command-line.md#copies)), and a function declared
[`nocopy`](nocopy.md) is checked to copy nothing at all.

A pointer cannot reach a list, or a value holding one: `&xs` is an error, as
is reading such a value through a pointer, since only its variable may free
its memory. To refer to a part of a value, use a [path](paths.md).

## Layout

Lists have no fixed layout, so `size_of([T])` is an error, and they cannot be
passed to C. On the VM they live on the GC heap; natively a list is a
pointer to its elements, from `malloc`, with its length and capacity. In JIR
a list is `[T]`, built with `array` or `repeat` and changed with `push` and
`pop` ([JIR](../jir.md#lists-and-strings-hosted-and-lists-natively)).
