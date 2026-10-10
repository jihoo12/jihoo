# Operators

Operators work on values of one type at a time: both operands of a binary
operator have the same type, with literals taking the type of the other side.
They follow Rust and C, with a few exceptions noted below.

## Precedence

From the tightest to the loosest. All binary operators are left-associative:
`a - b - c` is `(a - b) - c`.

| precedence | operators | kind |
|------------|-----------|------|
| 1 | `f(x)`  `a[i]`  `s.x` | call, index, field |
| 2 | `-x`  `!x`  `*p`  `&x`  `ref x`  `comptime x` | prefix |
| 3 | `x as T` | conversion |
| 4 | `*`  `/`  `%` | multiplicative |
| 5 | `+`  `-` | additive |
| 6 | `<<`  `>>` | shift |
| 7 | `&` | bitwise and |
| 8 | `^` | bitwise xor |
| 9 | `\|` | bitwise or |
| 10 | `<`  `<=`  `>`  `>=` | ordering |
| 11 | `==`  `!=` | equality |
| 12 | `&&` | logical and |
| 13 | `\|\|` | logical or |

As in Rust and unlike C, comparisons bind looser than the bitwise operators, so
`x & 1 == 0` is `(x & 1) == 0`. Unlike Rust, ordering binds tighter than
equality, so `a < b == c < d` compares two bools.

A prefix operator applies to everything after it at its level: `comptime a * b`
is `(comptime a) * b`, so write `comptime (a * b)` for a whole product.

## Arithmetic

| operator | operands | result |
|----------|----------|--------|
| `+` `-` `*` | integers, floats | the same type; integers wrap around |
| `/` `%` | integers, floats | integers truncate; floats per IEEE (`%` is `fmod`) |
| `-x` | signed integers, floats | negation |
| `+` | `str`, `str` | concatenation (hosted) |
| `+` `-` | `*T` and `i64` | a pointer moved by whole elements ([Pointers](pointers.md)) |

See [Types](types.md#integers) for overflow and division by zero.

## Comparison

| operator | operands | result |
|----------|----------|--------|
| `==` `!=` | any type made of comparable parts | `bool` ([Equality](equality.md)) |
| `<` `<=` `>` `>=` | integers, floats, pointers of the same type | `bool` |

Integers compare signed or unsigned according to their type, and pointers by
address. Strings, structs, enums and arrays have equality but no ordering.

## Bitwise

| operator | operands | result |
|----------|----------|--------|
| `&` `\|` `^` | integers | bitwise and, or, xor |
| `&` `\|` `^` | bools | and, or, xor — both sides are always evaluated |
| `!x` | integers | every bit flipped (Rust's `!`, C's `~`) |
| `<<` `>>` | integers of one type | shift |

`>>` is arithmetic (sign-filling) for signed types and logical for unsigned
ones. The shift amount is taken modulo the bit width, so `x << 64` on an `i64`
is `x`, on the VM and natively alike.

## Logical

| operator | operands | result |
|----------|----------|--------|
| `&&` `\|\|` | bools | and, or — the right side is only evaluated if needed |
| `!x` | bool | not |

## Pointers and references

| operator | meaning | page |
|----------|---------|------|
| `&place` | the address of a variable, field or element | [Pointers](pointers.md) |
| `*p` | what a pointer, `ref` or `cell` holds | [Pointers](pointers.md), [References](references.md), [Cells](cells.md) |
| `ref e` | a new reference to a copy of `e` | [References](references.md) |

## Not operators

There are no compound assignments (`+=`), no increment (`++`), no ternary
`?:` (use a [`match` expression](patterns.md#match-expressions)), and no
operator overloading.
