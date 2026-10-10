# Pointers

Native and freestanding programs manage memory themselves, through raw
pointers. `*T` is the address of a `T`.

```jihoo
#![freestanding]

struct Node {
    value: i64
    next: *Node          // recursion only through pointers
}

fn sum(list: *Node) -> i64 {
    let total = 0
    while list != 0 as *Node {
        total = total + list.value   // `p.field` reads through a pointer
        list = list.next
    }
    return total
}

fn _start() -> i64 {
    let c = Node { value: 3, next: 0 as *Node }
    let b = Node { value: 2, next: &c }
    let a = Node { value: 1, next: &b }
    return sum(&a)                   // exit status 6
}
```

## Making pointers

- `&x` takes the address of a local variable, or of a field or element of one
  (`&p.x`, `&buf[0]`). The pointer is valid until the function returns.
- `&*p` and `&p[i]` are just pointers computed from `p`.
- `0 as *T` is the null pointer; pointers convert to and from `i64` and `u64`
  with `as`, and between pointer types (`p as *u8`).
- Memory beyond the stack comes from an allocator: `libc.malloc` in native
  programs, `alloc.alloc` or `alloc.alloc_array` (an arena over `mmap`) in
  either ([Standard library](../reference/standard-library.md)).

## Using pointers

| expression | meaning |
|------------|---------|
| `*p` | the `T` that `p: *T` points to; assignable |
| `p.x` | a field of the struct `p` points to; assignable |
| `p[i]` | `*(p + i)`; not bounds-checked |
| `p + n`, `p - n` | `p` moved by `n` whole elements (`n: i64`); `p + 1` on `*i64` moves 8 bytes |
| `p == q`, `p < q` | address comparison |

With `p: *[T; N]`, a pointer to an array, `p[i]` indexes the array — with a
bounds check — and `len(p)` is `N`.

## Pointers and safety

Pointers are raw, as in C: nothing checks that they point to live memory, and
using a dangling or null pointer is undefined behavior. What jihoo does check:

- array indexing, also through `*[T; N]`;
- struct layouts, which JIR records and the LLVM backend verifies against the
  target ([Structs](structs.md#layout));
- that GC values and pointers never mix: `str`, `ref`, `cell` and `chan` do not
  exist where pointers do.

## Not in hosted code

Hosted programs have no pointers; they share data through
[refs](references.md) and [cells](cells.md). Pointer types, `&`, and `*` on a
pointer are compile-time errors there.
