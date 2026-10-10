# Enums

An enum (a sum type) holds one of several *variants*, each with its own payload
of values. [`match`](patterns.md) takes it apart.

```jihoo
enum Shape {
    Circle(i64)
    Rect(i64, i64)
    Empty
}

fn area(s: Shape) -> i64 {
    match s {
        Circle(r) => return 3 * r * r
        Rect(w, h) => return w * h
        Empty => return 0
    }
}

fn main() {
    let s = Shape.Rect(3, 4)
    print(area(s))                 // 12
    print(area(Shape.Empty))       // 0
}
```

## Declaring an enum

`enum Name { Variant, Variant(T, U) ... }` lists the variants, separated by
newlines or commas. A variant has a payload of zero or more values, given by
their types. Enums share the rules of structs: they can be `pub`, take
parameters, and cannot contain themselves except through a
[`ref`](references.md) or a pointer.

## Making values

Variants are written after the enum: `Shape.Empty`, `Shape.Rect(3, 4)`, and
for another module's enum `geo.Shape.Empty`. A variant with a payload is called
like a function, with its payload values as arguments.

## Generic enums

```jihoo
enum Option(T: type) {
    Some(T)
    None
}

let a = Option(i64).Some(1)          // the arguments written out
let o: Option(u8) = Option.None      // taken from the expected type
fn find(x: i64) -> Option(i64) {
    return Option.Some(x)            // ... or from the return type
}
```

For a generic enum the arguments can be left out. They are then taken from the
expected type (a typed `let`, an argument, a return value), or else from payload
values declared with exactly a type parameter (`Some(T)`). `let x = Option.None`
gives no clue and is an error that says so.

## Using enums

- [`match`](patterns.md) is how a program asks which variant it has and gets
  at the payload.
- Enums are values, like structs: assignment copies them.
- `==` compares variant and then payload ([Equality](equality.md)).

## Layout

Natively an enum has the C layout of a `u32` tag followed by a union of the
payloads, each laid out like a struct of its values: `size_of(Shape)` above is
24 (a 4-byte tag, 4 bytes of padding, two `i64`s). On the VM it is a GC object
holding the tag and the payload. In JIR an enum is
`enum $Shape { Circle(i64), Rect(i64, i64), Empty }` with `variant`, `tag` and
`payload` instructions ([JIR](../jir.md#enums)).
