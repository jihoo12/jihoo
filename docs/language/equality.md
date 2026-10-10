# Equality

`==` and `!=` work on every type made of comparable parts, and compare
structurally.

```jihoo
p == Point { x: 1, y: 2 }
Option.Some(3) != Option(i64).None
[1, 2, 3] == [1, 2, 3]
list(50) == list(50)          // enum List { Cons(i64, ref List), Nil }
```

## What compares how

| type | compares |
|------|----------|
| integers, `bool` | by value |
| floats | as IEEE 754: NaN is unequal to everything, `0.0 == -0.0` |
| `str` | by contents |
| structs | field by field |
| enums | by variant, then payload |
| arrays | element by element (same type, so same length) |
| `ref T` | by the value it refers to |
| pointers | by address |
| function values, closures | cannot be compared |
| channels, cells | cannot be compared |

A `ref` compares by the value it refers to: refs are immutable, so which object
a ref points to cannot be observed, and comparing what it refers to is the only
meaning that fits. So lists and trees built from refs can be compared directly.

A [cell](cells.md) has identity — two cells holding equal values are still two
cells — so cells cannot be compared; compare what they hold, `*a == *b`.

A type that holds a function value, a channel or a cell cannot be compared
either, and the error says which part is to blame.

Both sides must have the same type, and there is no ordering (`<`) on structs,
enums, arrays or strings.

## How it works

Each compared type gets a helper function `fn.eq.N` in JIR, made on first use
(`crates/jihoo-sema/src/equality.rs`). Helpers call each other, and themselves
for recursive types, so nothing new was needed in JIR or the backends.
