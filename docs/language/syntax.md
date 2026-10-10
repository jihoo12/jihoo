# Syntax

jihoo looks like a small Rust or Go without semicolons. A source file is a
list of items — functions, types, constants, imports and macros — in any order;
statements live inside function bodies.

```jihoo
// A comment runs to the end of the line.
const LIMIT = 100                  // a compile-time constant

struct Point {                     // fields on lines of their own...
    x: i64
    y: i64
}

fn main() {
    let p = Point { x: 1, y: 2 }   // ...or separated by commas
    if p.x < LIMIT { print(p.x + p.y) }
}
```

## Files and attributes

A file is UTF-8 text with the extension `.jh`. It may start with *attributes*
of the form `#![name]`, before any item; the only ones are `#![native]` and
`#![freestanding]`, which choose the [profile](profiles.md).

## Comments

`//` starts a comment that runs to the end of the line. There are no block
comments. The comment block at the top of a file in `examples/` doubles as the
text of its page on this site.

## Statements and newlines

A statement ends at a newline, a `;`, or the `}` that closes its block, so
semicolons are only needed to put several statements on one line:

```jihoo
let a = 1; let b = 2
if a < b { print(a) } else { print(b) }
```

Because a newline ends a statement, an expression continues onto the next line
only if the line break comes where the expression cannot end yet: after a binary
operator, an opening bracket or a comma.

```jihoo
let total = first +
    second                  // continues: `+` needs a right-hand side
let wrong = first
    + second                // error: a statement cannot start with `+`
```

Likewise, `(`, `[` and `.` must be on the same line as what they call, index or
read a field of, and a function type's `-> R` must be on the same line as its
`fn(...)`.

## Identifiers and keywords

Identifiers are ASCII letters, digits and `_`, not starting with a digit. By
convention types and enum variants are `CamelCase` and everything else is
`snake_case`; in [patterns](patterns.md) a name that starts with an uppercase
letter must be a variant.

These words are reserved:

```text
as      asm     cell    chan    comptime  const   else    enum
extern  false   fn      go      if        import  let     macro
match   pub     quote   ref     return    select  struct  true
while
```

A few more words have a meaning only in one place and are ordinary names
elsewhere: `type` (in `comptime T: type`), `expr`, `stmts` and `items` (macro
result types), `in`, `out`, `reg` and `clobber` (inside `asm`), and `_` (in
patterns and `select`).

## Literals

| literal | examples | notes |
|---------|----------|-------|
| integer | `42`, `1_000_000`, `0xff`, `0b1010` | decimal, hex or binary; `_` separates digits. Its type comes from context, defaulting to `i64` ([Types](types.md#integers)). |
| float   | `1.5`, `2e10`, `1.5e-3` | needs a digit after the `.`: `1.` is the integer `1` followed by `.`. An `f64` unless an `f32` is expected. |
| bool    | `true`, `false` | |
| string  | `"hello\n"` | escapes `\n`, `\t`, `\r`, `\0`, `\\`, `\"`. A `str` when hosted, a NUL-terminated `*u8` when compiled. |
| array   | `[1, 2, 3]`, `[0; 64]` | a list, or a value repeated a number of times ([Arrays](arrays.md)). |
| struct  | `Point { x: 1, y: 2 }` | every field, in any order ([Structs](structs.md)). |

A minus sign is the negation operator, not part of the literal, but a negated
literal is checked as a whole: `-128` fits in an `i8` and
`-9223372036854775808` in an `i64`. A literal can be as large as `u64` allows
(`0xffff_ffff_ffff_ffff`), as long as it fits the type it gets; without context
that is `i64`, so `9223372036854775808` on its own is an error.

## Items

| item | form | page |
|------|------|------|
| function | `fn name(a: T, ...) -> R { ... }` | [Functions](functions.md) |
| C function | `extern fn name(a: T, ...) -> R` | [Calling C](calling-c.md) |
| struct | `struct Name { field: T ... }`, `struct Name(T: type) { ... }` | [Structs](structs.md) |
| enum | `enum Name { A, B(T, U) ... }` | [Enums](enums.md) |
| constant | `const NAME = expr`, `const NAME: T = expr` | [Compile-time evaluation](compile-time-evaluation.md) |
| import | `import a.b`, `import a.b as c` | [Modules](modules.md) |
| macro | `macro name(a: expr, ...) -> expr { ... }` | [Macros](macros.md) |
| item macro call | `name!(...)` at the top level | [Macros](macros.md#statement-and-item-macros) |

Any item but an import may be marked `pub` to make it visible to other modules.
Items can refer to each other in any order.

The [syntax summary](../reference/syntax-summary.md) lists every statement and
expression form on one page.
