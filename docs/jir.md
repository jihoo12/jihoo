# JIR — jihoo IR text format (version 0)

JIR is the contract between the Rust frontend/VM and the C++ LLVM backend.
`jihoo emit-ir` writes it, `jihoo-llc` reads it. Both sides must agree on this
document; the Rust definitions live in `crates/jihoo-ir/src/lib.rs` and the C++ ones
in `backend-llvm/src/jir.h`. The typing rules are implemented once, in
`crates/jihoo-ir/src/types.rs`, and checked by `crates/jihoo-ir/src/verify.rs`.

## Example

```
; jihoo IR
jir 0
profile freestanding

fn @fib(i64) -> i64 {
  regs i64 i64 bool i64 i64 i64 i64 i64 i64 i64
bb0:
  %1 = const 2
  %2 = lt %0, %1
  br %2, bb1, bb2
bb1:
  ret %0
bb2:
  %3 = const 1
  %4 = sub %0, %3
  %5 = call @fib(%4)
  %6 = const 2
  %7 = sub %0, %6
  %8 = call @fib(%7)
  %9 = add %5, %8
  ret %9
}
```

## Lexical rules

- The format is line-oriented: one header, label, instruction or terminator per line.
- `;` starts a comment that runs to the end of the line (outside string literals).
- `%N` is a register, `@name` a function, `bbN` a block.
- Integers are signed 64-bit decimals (`-5`, `42`).
- Strings are double-quoted. Escapes: `\n`, `\t`, `\\`, `\"`, `\xHH` (any byte).

## Module

```
jir 0                          ; format version, must come first
profile hosted|freestanding    ; language profile
fn ...                         ; zero or more functions
```

| profile        | runs on | GC  | allowed builtins | types                 |
|----------------|---------|-----|------------------|-----------------------|
| `hosted`       | VM      | yes | `print`          | `unit i64 bool str`   |
| `freestanding` | LLVM    | no  | `syscall`        | `unit i64 bool ptr`   |

The entry point is `@main` for hosted modules and `@_start` for freestanding ones.
It takes no parameters and returns `unit` or `i64`.

## Types

| type   | meaning                                   | LLVM    |
|--------|-------------------------------------------|---------|
| `unit` | no value                                  | `{}`    |
| `i64`  | 64-bit signed integer                     | `i64`   |
| `bool` | `true` / `false`                          | `i1`    |
| `str`  | GC-managed string, hosted only            | —       |
| `ptr`  | raw byte pointer, freestanding only       | `ptr`   |

`str` and `ptr` are deliberately separate types: GC references and raw pointers
must never mix. That separation is what will later allow GC-enabled native builds.

## Functions

```
fn @name(T1, T2, ...) -> R {
  regs T1 T2 ... Tn
bb0:
  ...
}
```

- The `regs` line, right after the header, gives the type of every register
  `%0 .. %(n-1)`. It must start with the parameter types: arguments arrive in
  `%0 .. %(params-1)`.
- Blocks are numbered `bb0, bb1, ...` in order. `bb0` is the entry block.
- Every block ends with exactly one terminator.

Registers are **not** SSA: a register may be written more than once (the frontend
maps each `let` variable to one register), but always with values of its declared
type. The LLVM backend gives each register an `alloca` and relies on `mem2reg`.

## Instructions

In the table, `%a: T` means `%a` must have type `T`, and the result type is what the
destination register must be declared as.

| syntax                          | operands             | result | meaning |
|---------------------------------|----------------------|--------|---------|
| `%d = const N`                  |                      | `i64` or `bool` | constant; for `bool`, nonzero is true |
| `%d = unit`                     |                      | `unit` | the unit value |
| `%d = str "..."`                |                      | `str` (hosted) / `ptr` (freestanding) | string literal; freestanding strings are NUL-terminated constant bytes |
| `%d = copy %a`                  | `%a: T`              | `T`    | copy |
| `%d = neg %a`                   | `i64`                | `i64`  | wrapping negation |
| `%d = not %a`                   | `bool`               | `bool` | logical not |
| `%d = add\|sub\|mul %a, %b`     | `i64, i64`           | `i64`  | wrapping arithmetic |
| `%d = div\|rem %a, %b`          | `i64, i64`           | `i64`  | signed division (VM traps on zero; native: undefined for now) |
| `%d = add %a, %b`               | `str, str`           | `str`  | concatenation |
| `%d = add\|sub %a, %b`          | `ptr, i64`           | `ptr`  | pointer offset in bytes |
| `%d = eq\|ne %a, %b`            | `T, T` (`T` ≠ `unit`) | `bool` | equality (`str` compares contents) |
| `%d = lt\|le\|gt\|ge %a, %b`    | `i64, i64` / `ptr, ptr` | `bool` | signed (`i64`) or unsigned (`ptr`) comparison |
| `%d = call @f(%a, ...)`         | the parameter types of `@f` | return type of `@f` | call |
| `%d = syscall(%n, %a, ...)`     | `i64` or `ptr`, 1 to 7 operands | `i64` | freestanding only: raw Linux syscall `%n` |
| `print %a`                      | `i64`, `bool` or `str` |      | hosted only: print the value and a newline |

## Terminators

| syntax                 | meaning |
|------------------------|---------|
| `jmp bbN`              | jump |
| `br %c, bbT, bbF`      | `%c: bool`; go to `bbT` if true, else `bbF` |
| `ret %a`               | return `%a`, which must have the function's return type |
| `unreachable`          | control never gets here |

Returning from `@_start` exits the process: with the returned value if `@_start`
returns `i64`, with 0 if it returns `unit`.

## Planned

- Sized integers (`i8`..`i32`, `u*`), loads/stores, structs, and inline `asm`
  blocks for freestanding code.
- `gcref` types for GC-managed objects beyond `str`.
- A Rust-side JIR parser so `jihoo run file.jir` works.
