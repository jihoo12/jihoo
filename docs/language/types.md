# Types

jihoo is statically typed. Function signatures are written out, and the types of
local variables are inferred:

```jihoo
fn area(w: i64, h: i64) -> i64 {   // signatures are written out
    let a = w * h                  // `a: i64` is inferred
    return a
}
```

## All types

| type | values | profiles | page |
|------|--------|----------|------|
| `unit` | the one value of a function without a result | all | |
| `bool` | `true`, `false` | all | |
| `i8` `i16` `i32` `i64` | signed integers | all | [below](#integers) |
| `u8` `u16` `u32` `u64` | unsigned integers | all | [below](#integers) |
| `f32` `f64` | IEEE 754 floats | all | [below](#floats) |
| `str` | immutable text | hosted, native | [below](#strings) |
| `[T; N]` | `N` values of type `T` | all | [Arrays](arrays.md) |
| `[T]` | any number of values of type `T` | hosted | [Lists](lists.md) |
| `Name`, `Name(args)` | a struct or enum | all | [Structs](structs.md), [Enums](enums.md) |
| `fn(A, B) -> R` | a function value | all | [Function values](function-values.md) |
| `*T` | a raw pointer | native, freestanding | [Pointers](pointers.md) |
| `ref T` | an immutable reference to a GC value | hosted | [References](references.md) |
| `cell T` | shared, mutable state | hosted | [Cells](cells.md) |
| `chan T` | a channel between tasks | hosted | [Tasks and channels](tasks-and-channels.md) |
| `type`, `expr`, `stmts`, `items` | types and code, at compile time | all | [Generics](generics.md), [Macros](macros.md) |

A function without `-> T` returns `unit`. Types of other modules are written
`alias.Name` ([Modules](modules.md)).

## Integers

`i8` … `i64` are signed (two's complement) and `u8` … `u64` unsigned. The
number is the width in bits.

- An integer literal (`1_000`, `0xff`, `0b1010`) takes its type from context:
  a typed `let`, the other operand (`p[i] == 0`), an argument, a field, a return
  value. Without context it is an `i64`. It must fit its type:
  `let c: u8 = 300` is an error.
- Different integer types never mix implicitly: `a + b` needs both to have the
  same type, and an `i32` cannot be passed where an `i64` is expected. Convert
  with [`as`](#conversions).
- `+`, `-` and `*` wrap around on overflow, the same on the VM and natively
  (`255 as u8 + 1` is `0`). `/` and `%` truncate towards zero; `%` has the sign
  of the left operand. The one signed division that overflows, `MIN / -1`,
  wraps to `MIN`, and `MIN % -1` is `0`.
- Dividing by zero stops the program: the VM reports `division by zero`, and
  compiled code traps, as for an out-of-bounds index.
- Negation (`-x`) is only defined on signed integers.

## Floats

`f32` and `f64` are IEEE 754 binary32 and binary64, and behave bit for bit the
same on the VM and natively: there is no fast-math and no fused multiply-add.

- A float literal (`1.5`, `2e10`, `1.5e-3`) is an `f64` unless an `f32` is
  expected. An integer literal may stand where a float is expected
  (`x * 2`, `let y: f64 = 3`) if its value is exact.
- `+ - * /` round to the nearest value. `/` by zero gives an infinity or NaN,
  and `%` is C's `fmod` (the result has the sign of the left operand).
- Comparisons follow IEEE: NaN is unequal to everything, itself too, and
  `0.0 == -0.0`.
- `print` writes the shortest text that reads back as the same value: `0.1`,
  `1.0`, `1e100`, `inf`, `NaN`.
- `sqrt`, `floor`, `ceil`, `trunc`, `round`, `abs` and `copysign` are
  builtins, exact and the same everywhere, and `to_bits`/`from_bits` read and
  make a float's bits ([Builtins](../reference/builtins.md#floats)). A NaN
  has no visible sign or payload: machines disagree about which NaN they make.
- Floats cannot be [`match`ed](patterns.md) or used with bitwise operators.

## Booleans

`bool` is `true` or `false`. Conditions of `if` and `while`, and the operands of
`&&` and `||`, must be `bool`: there is no implicit conversion from integers or
pointers. `b as i64` gives 0 or 1.

## Strings

`str` is the type of text in hosted and native programs. A string is an
immutable sequence of bytes, usually UTF-8. `+` concatenates strings,
`==` and `<` compare them, `print` writes one, and `to_str` makes one from a
number or bool:

```jihoo
let name = "jihoo"
let line = "hello, " + name + " " + to_str(2026)
print(line == "hello, jihoo 2026")   // true
print(len(line))                     // 17: the length in bytes
print(line[0])                       // 104: a byte, `u8`
print(line[7:12])                    // jihoo: bytes 7 up to 12
print(line[:5] + line[12:])          // hello 2026
```

- `len(s)` is the number of bytes. `"세계"` is 6 bytes long.
- `s[i]` is byte `i`, a `u8`, bounds-checked. It cannot be assigned:
  strings are immutable.
- `s[lo:hi]` is a new string of bytes `lo` up to `hi`; `s[lo:]` goes to the
  end and `s[:hi]` starts at 0. The program stops unless `0 <= lo <= hi <=
  len(s)`. A slice may cut a multi-byte character in two; its bytes are kept
  as they are.
- `<`, `<=`, `>` and `>=` compare byte by byte, a prefix first:
  `"app" < "apple" < "banana"`.
- `from_bytes(b)` makes a string from a list of bytes `b: [u8]`
  ([Lists](lists.md)), and `read_line()` reads one from standard input
  ([Builtins](../reference/builtins.md#hosted)).

The `strings` library has searching, splitting, trimming and parsing on top
of these ([Standard library](../reference/standard-library.md#strings)).

In native programs a string literal is a `str`, except where a `*u8` is
expected, as for an argument of a C function: there it is a pointer to
constant NUL-terminated bytes, ready to pass to C. Natively, strings are freed
like [lists](lists.md#lists-in-native-programs), and a literal costs no
memory: copying one copies nothing. Freestanding programs have no `str`; a
literal is always a `*u8`. [Macros](macros.md) can use `str` in every profile,
since they only run at compile time.

## Conversions

`as` converts explicitly. It binds tighter than every binary operator, so
`a + b as i64` converts only `b`.

| from → to | result |
|-----------|--------|
| integer → integer | truncated, or sign- or zero-extended by the source type (`-1 as u8` is `255`) |
| `bool` → integer | 0 or 1 |
| integer or float → float | the nearest value |
| float → integer | the fraction dropped, saturating at the type's limits; NaN becomes 0 (as in Rust) |
| `*T` → `*U` | the same address |
| `*T` ↔ `i64`, `u64` | the address as a number, and back |
| `T` → `T` | unchanged |

There are no other conversions; in particular nothing converts to `bool`
(write `n != 0`).

## Inference

Inference is local: the type of a `let` without an annotation is the type of
its value. Where a value has no type of its own — integer and float literals,
array literals, `Option.None`, an anonymous function without annotations — it
takes the *expected type* from where it is used: a typed `let`, a parameter, a
field, a return value, the other side of an operator. If there is none and no
default applies, the error says so.
