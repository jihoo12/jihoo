# Structs

A struct groups named fields into one value.

```jihoo
struct Point {
    x: i64
    y: i64
}

struct Line { a: Point, b: Point }

fn main() {
    let p = Point { x: 1, y: 2 }
    let line = Line { a: p, b: Point { y: 5, x: 4 } }
    line.a.x = 10                  // changes `line` only
    print(p.x)                     // 1
    print(line.a.x + line.b.y)     // 15
}
```

## Declaring a struct

`struct Name { field: Type ... }` lists the fields, separated by newlines or
commas. A struct may contain other structs, enums and arrays by value, but not
itself — not even through other types — since its size would be infinite.
Recursive data goes through a [`ref`](references.md) in hosted code or a
[pointer](pointers.md) in compiled code:

```jihoo
struct Node {
    value: i64
    next: *Node          // native and freestanding
}
```

A struct with parameters, `struct Vec(T: type) { ... }`, is generic: see
[Generics](generics.md#generic-structs).

## Struct literals

`Name { field: value, ... }` builds a struct. Every field must be given, once,
in any order; field values take their expected type from the field, so
`Point { x: 1, y: 2 }` works for any integer field type. A struct of another
module is written `alias.Name { ... }`, and an instance of a generic struct
`Pair(i64) { a: 1, b: 2 }`.

A struct literal cannot appear directly in an `if`, `while` or `match` head;
put it in parentheses there.

## Fields

`s.x` reads a field, and `s.x = v` assigns one. Fields nest: `line.a.x`.
Through a pointer (`p: *Point`), `p.x` reads and writes the field it points to,
as `->` does in C. Through a [`ref`](references.md) fields can be read, and
through a [`cell`](cells.md) read and assigned.

## Value semantics

Structs are values in every profile: assignment and argument passing copy
them, and `p.x = 1` changes only `p`.

```jihoo
let a = Point { x: 1, y: 2 }
let b = a
b.x = 100
print(a.x)          // still 1
```

This is the rule for every type in jihoo except pointers, cells and channels,
whose purpose is sharing. It is cheap: on the VM a struct is shared between
copies and only copied when one of them is changed while the other is still
in use ([VM and GC](../internals/vm.md#value-semantics-and-in-place-updates)),
and natively structs are plain LLVM aggregates.

## Layout

In compiled code a struct has the C layout for 64-bit targets: fields in order,
each aligned to its natural alignment, the size rounded up to the largest
alignment. `size_of(T)` and `align_of(T)` give these as `i64` constants:

```jihoo
struct Header { tag: u8, len: u32, offset: i64 }

const H = size_of(Header)    // 16: 1 byte, 3 padding, 4, then 8
```

A struct whose layout matches a C struct can be shared with C through a
pointer ([Calling C](calling-c.md)). Types containing `str`, `ref`, `cell` or
`chan` exist only on the VM and have no layout.

## Equality

`==` and `!=` compare structs field by field; see [Equality](equality.md).
Structs have no ordering and cannot be printed directly.
