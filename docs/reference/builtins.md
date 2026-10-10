# Builtins

Builtins look like function calls but are part of the language: some accept
several types, some take a type as an argument, and none can be used as a
function value. A local variable of a non-function type does not hide them, so
`let len = len(a)` works.

## Everywhere

| builtin | result | description |
|---------|--------|-------------|
| `len(a)` | `i64` | the length of an array `a: [T; N]`, or of the array `a: *[T; N]` points to; a constant |
| `size_of(T)` | `i64` | the size of type `T` in bytes, by C layout rules for 64-bit targets; a constant |
| `align_of(T)` | `i64` | the alignment of type `T` in bytes; a constant |

`size_of` and `align_of` take a type, and work for every type with a fixed
layout: not `str`, `ref`, `cell`, `chan`, or types containing them
([Structs](../language/structs.md#layout)).

## Hosted

| builtin | result | description |
|---------|--------|-------------|
| `print(x)` | `unit` | writes a number, `bool` or `str` and a newline to stdout |
| `to_str(x)` | `str` | a number or `bool` as text, as `print` would write it (also in macros) |
| `ref e` | `ref T` | a new [reference](../language/references.md) to a copy of `e` |
| `cell(v)` | `cell T` | a new [cell](../language/cells.md) holding a copy of `v` |
| `chan(T)`, `chan(T, n)` | `chan T` | a new [channel](../language/tasks-and-channels.md), unbuffered or buffering `n` values |
| `send(c, v)` | `unit` | sends `v`, waiting while the channel is full |
| `recv(c)` | `T` | receives a value, waiting until there is one |

`print` writes integers in decimal, bools as `true`/`false`, and floats as the
shortest text that reads back as the same value (`0.1`, `1.0`, `1e100`, `inf`,
`NaN`). To print other values, print their parts. During compile-time
evaluation `print` writes to stderr.

## Native and freestanding

| builtin | result | description |
|---------|--------|-------------|
| `syscall(n, args...)` | `i64` | the raw Linux system call `n` with up to six integer or pointer arguments ([Inline assembly](../language/inline-assembly.md#syscall)) |
| `asm(template..., operands...)` | the output type, or `unit` | inline assembly ([Inline assembly](../language/inline-assembly.md#asm)) |

Neither can run at compile time.

## Macros only

| builtin | result | description |
|---------|--------|-------------|
| `quote(e)`, `quote { ... }`, `quote items { ... }` | `expr`, `stmts`, `items` | code built from a template with `$x` / `$(e)` holes |
| `stringify(e)` | `str` | the source text of the code `e` |
| `unique(prefix)` | `str` | `prefix` plus a number, a name unused in the whole compilation |
| `ident(name)` | `expr` | the name in the `str` `name` as code |

See [Macros](../language/macros.md).

## Not builtins

The following are syntax rather than builtins, and are described with the
language: `as` ([Types](../language/types.md#conversions)), `comptime e`
([Compile-time evaluation](../language/compile-time-evaluation.md)), `go f(x)`
and `select` ([Tasks and channels](../language/tasks-and-channels.md)), `&x` and
`*p` ([Pointers](../language/pointers.md)).
