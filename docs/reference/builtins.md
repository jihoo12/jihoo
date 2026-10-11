# Builtins

Builtins look like function calls but are part of the language: some accept
several types, some take a type as an argument, and none can be used as a
function value. A local variable of a non-function type does not hide them, so
`let len = len(a)` works.

## Everywhere

| builtin | result | description |
|---------|--------|-------------|
| `len(a)` | `i64` | the length of an array `a: [T; N]`, or of the array `a: *[T; N]` points to, a constant; in hosted programs also the number of bytes of a `str` |
| `size_of(T)` | `i64` | the size of type `T` in bytes, by C layout rules for 64-bit targets; a constant |
| `align_of(T)` | `i64` | the alignment of type `T` in bytes; a constant |

`size_of` and `align_of` take a type, and work for every type with a fixed
layout: not `str`, `ref`, `cell`, `chan`, or types containing them
([Structs](../language/structs.md#layout)).

## Floats

These take an `f32` or an `f64` and work in every profile and at compile time,
with the same results everywhere: IEEE 754 defines each of them exactly.

| builtin | result | description |
|---------|--------|-------------|
| `sqrt(x)` | the type of `x` | the square root, correctly rounded; NaN below zero |
| `floor(x)` | the type of `x` | the largest integral value not above `x` |
| `ceil(x)` | the type of `x` | the smallest integral value not below `x` |
| `trunc(x)` | the type of `x` | `x` with the fraction dropped |
| `round(x)` | the type of `x` | the nearest integral value, halfway cases away from zero (`round(2.5)` is `3.0`) |
| `abs(x)` | the type of `x` | the magnitude of `x` |
| `copysign(x, y)` | the type of `x` | the magnitude of `x` with the sign of `y`, of the same type |
| `to_bits(x)` | `u32` for `f32`, `u64` for `f64` | the bits of `x` |
| `from_bits(n)` | `f32` for `u32`, `f64` for `u64` | the float with bits `n` |

A literal argument is an `f64` unless an `f32` is expected (`let s: f32 =
sqrt(2)`). Integral results keep the sign of `x`, so `floor(-0.0)` and
`round(-0.4)` are `-0.0`.

A NaN has no sign or payload in jihoo, since machines disagree about which
NaN an operation makes: `to_bits` gives every NaN as the one quiet NaN with a
clear sign (`0x7ff8000000000000`, or `0x7fc00000` for an `f32`), and
`copysign(x, y)` with a NaN `y` is `abs(x)`.

Unlike the other builtins, these names can be taken: a module that defines a
function called `sqrt` (or declares `extern fn sqrt`) calls its own `sqrt`.

## Hosted and native

| builtin | result | description |
|---------|--------|-------------|
| `print(x)` | `unit` | writes a number, `bool` or text and a newline to stdout: a `str` on the VM, a NUL-terminated `*u8` (such as a string literal) natively |

`print` writes integers in decimal, bools as `true`/`false`, and floats as the
shortest text that reads back as the same value (`0.1`, `1.0`, `1e100`, `inf`,
`NaN`), the same on the VM and natively. To print other values, print their
parts. During compile-time evaluation `print` writes to stderr. Natively it
writes through the C library's `stdout`, which is buffered: output written
other ways (`libc.write`, `syscall`) can come before it, and a program that
traps loses what is still buffered. Freestanding programs have no `print`; the
`io` library writes to file descriptors.

## Hosted

| builtin | result | description |
|---------|--------|-------------|
| `to_str(x)` | `str` | a number or `bool` as text, as `print` would write it (also in macros) |
| `read_line()` | `str` | the next line of standard input with its `\n` (or without, for a last line that has none), or `""` at the end of the input; not at compile time |
| `ref e` | `ref T` | a new [reference](../language/references.md) to a copy of `e` |
| `cell(v)` | `cell T` | a new [cell](../language/cells.md) holding a copy of `v` |
| `chan(T)`, `chan(T, n)` | `chan T` | a new [channel](../language/tasks-and-channels.md), unbuffered or buffering `n` values |
| `send(c, v)` | `unit` | sends `v`, waiting while the channel is full |
| `recv(c)` | `T` | receives a value, waiting until there is one |

As with the float builtins, a module may take the name `read_line`: one that
defines a function of that name calls its own.

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
