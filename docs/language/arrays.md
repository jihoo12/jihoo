# Arrays

`[T; N]` is a fixed number `N` of values of type `T`.

```jihoo
let xs = [5, 3, 9]            // [i64; 3]
let buf: [u8; 64] = [0; 64]   // literals take their element type from context
buf[0] = 72
let n = len(buf)              // 64, a constant
```

## Array types and literals

- `[T; N]` is a value type, like a struct, in every profile: assignment and
  argument passing copy the whole array.
- `N` is a compile-time integer: a literal, a constant, a `comptime` parameter
  or any expression the compiler can evaluate (`[u8; size_of(Node) * 2]`).
- `[a, b, c]` lists the elements; `[v; N]` repeats one value `N` times. Element
  types come from the expected type if there is one, so `[0; 64]` can be an
  array of `u8`.
- `len(a)` is the length, an `i64` constant.
- Arrays nest: `[[i64; 3]; 3]` is a 3×3 grid, indexed `g[r][c]`.

## Indexing

`a[i]` reads an element and `a[i] = v` writes one; the index is an `i64`.
Indexing an array is bounds-checked in every profile: the VM stops with
`index 5 out of bounds for length 3`, and compiled code traps.

Indexing a raw pointer (`p[i]` with `p: *T`) is not checked. With
`p: *[T; N]`, `p[i]` and `len(p)` work on the array `p` points to, and `&a[i]`
is a pointer to one element, so `&buf[0]` is how a buffer becomes a `*u8`
([Pointers](pointers.md)).

## Performance

Natively, element writes happen in place. On the VM an array is a GC object
shared between copies; an element write is O(1) when only one variable holds
the array, as in a loop filling it, and the first write after the array was
copied somewhere (`let b = a`, an argument, a field) copies it once
([VM and GC](../internals/vm.md#value-semantics-and-in-place-updates)).

To share one array between functions without copying, keep it in a
[cell](cells.md) (hosted) or pass a pointer (compiled).

## Equality

`==` compares arrays of the same type element by element
([Equality](equality.md)). Arrays of different lengths are different types and
cannot be compared.
