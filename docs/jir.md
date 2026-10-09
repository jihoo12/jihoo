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

struct $Node { value: i64, next: *$Node } size 16 align 8

fn @sum(*$Node) -> i64 {
  regs *$Node i64 i64 i64 *$Node bool *i64 i64 i64 **$Node *$Node
bb0:
  %1 = const 0
  %2 = copy %1
  jmp bb1
bb1:
  %3 = const 0
  %4 = cast %3
  %5 = ne %0, %4
  br %5, bb2, bb3
bb2:
  %6 = fieldptr %0, 0
  %7 = load %6
  %8 = add %2, %7
  %2 = copy %8
  %9 = fieldptr %0, 1
  %10 = load %9
  %0 = copy %10
  jmp bb1
bb3:
  ret %2
}
```

## Lexical rules

- The format is line-oriented: one header, label, instruction or terminator per line.
- `;` starts a comment that runs to the end of the line (outside string literals).
- `%N` is a register, `@name` a function, `$Name` a struct, `bbN` a block.
  Struct names that are not identifiers are quoted: instances of generic structs
  are `$"Pair(i64)"`.
  Function names may contain `.`: instances of generic functions are `@max.0`, ...
- Integers are signed 64-bit decimals (`-5`, `42`).
- Strings are double-quoted. Escapes: `\n`, `\t`, `\\`, `\"`, `\xHH` (any byte).

## Module

```
jir 0                          ; format version, must come first
profile hosted|freestanding    ; language profile
struct ...                     ; zero or more structs
fn ...                         ; zero or more functions
```

| profile        | runs on | GC  | allowed builtins | pointers |
|----------------|---------|-----|------------------|----------|
| `hosted`       | VM      | yes | `print`          | no       |
| `freestanding` | LLVM    | no  | `syscall`        | yes      |

The entry point is `@main` for hosted modules and `@_start` for freestanding ones.
It takes no parameters and returns `unit` or `i64`.

## Types

| type              | meaning                                  | LLVM          |
|-------------------|------------------------------------------|---------------|
| `unit`            | no value                                 | `{}`          |
| `bool`            | `true` / `false`                         | `i1`          |
| `i8` … `i64`      | signed integers                          | `i8` … `i64`  |
| `u8` … `u64`      | unsigned integers                        | `i8` … `i64`  |
| `str`             | GC-managed string, hosted only           | —             |
| `*T`              | raw pointer to `T`, freestanding only    | `ptr`         |
| `$Name`           | struct, by value                         | named struct  |
| `expr`, `stmts`, `items` | code, inside macros; compile time only | —        |
| `[N x T]`         | array of `N` `T`s, by value              | `[N x T]`     |

`str` and `*T` are deliberately separate: GC references and raw pointers must never
mix. That separation is what will later allow GC-enabled native builds.

In the Rust IR and in the VM, an integer is stored as an `i64` in canonical form:
sign-extended for signed types, zero-extended for unsigned ones (`u64` keeps its bit
pattern). `const` values must already be canonical.

## Structs

```
struct $Name { field: T, field: T, ... } size S align A
```

Field names are only for readability; instructions refer to fields by index. A
struct may contain other structs and arrays by value, but not itself (directly or
through other structs or arrays); use a pointer for recursive data.

`size S align A` is the struct's layout as computed by the frontend
(`crates/jihoo-ir/src/layout.rs`: C rules for 64-bit targets). It is what
`size_of`/`align_of` were folded to, so the LLVM backend rejects the module if the
target's data layout disagrees. It is omitted for structs without a fixed layout
(those containing `str`, which only exist on the VM).

Structs and arrays are values: `copy` copies all fields or elements, and
`setfield`/`setelem` produce a new value.

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

In the table, `int` means any integer type and `%a: T` means `%a` has type `T`.
The result type is what the destination register must be declared as.

### Values and arithmetic

| syntax                          | operands              | result | meaning |
|---------------------------------|-----------------------|--------|---------|
| `%d = const N`                  |                       | int or `bool` | constant; `bool` is 0 or 1 |
| `%d = unit`                     |                       | `unit` | the unit value |
| `%d = str "..."`                |                       | `str` (hosted) / `*u8` (freestanding) | string literal; freestanding strings are NUL-terminated constant bytes |
| `%d = copy %a`                  | `T`                   | `T`    | copy |
| `%d = neg %a`                   | signed int            | same   | wrapping negation |
| `%d = not %a`                   | `bool` or int         | same   | logical not, or bitwise not of an integer |
| `%d = add\|sub\|mul %a, %b`     | `T, T` (int)          | `T`    | wrapping arithmetic |
| `%d = div\|rem %a, %b`          | `T, T` (int)          | `T`    | signed or unsigned by type; truncating (VM traps on zero; native: undefined for now) |
| `%d = and\|or\|xor %a, %b`      | `T, T` (int or `bool`) | `T`   | bitwise (for `bool`: logical, both sides evaluated) |
| `%d = shl\|shr %a, %b`          | `T, T` (int)          | `T`    | shift by `%b` modulo the bit width; `shr` is arithmetic for signed types, logical for unsigned |
| `%d = add %a, %b`               | `str, str`            | `str`  | concatenation |
| `%d = add\|sub %a, %b`          | `*T, i64`             | `*T`   | pointer offset in elements of `T` |
| `%d = eq\|ne %a, %b`            | `T, T`: `bool`, int, `str`, `*U` | `bool` | equality (`str` compares contents) |
| `%d = lt\|le\|gt\|ge %a, %b`    | `T, T`: int or `*U`   | `bool` | ordered comparison, signed or unsigned by type; pointers unsigned |
| `%d = cast %a`                  | see below             | dst type | conversion |
| `%d = call @f(%a, ...)`         | parameter types of `@f` | return type of `@f` | call |

`cast` allows: int → int (truncate, or sign-/zero-extend by the *source* type),
`bool` → int (0/1), `*T` → `*U`, `*T` ↔ `i64`/`u64`, and any type to itself.

### Structs

| syntax                              | operands              | result | meaning |
|-------------------------------------|-----------------------|--------|---------|
| `%d = struct $S(%a, %b, ...)`       | every field, in order | `$S`   | build a struct |
| `%d = field %s, N`                  | `$S`                  | type of field N | read a field |
| `%d = setfield %s, N, %v`           | `$S`, type of field N | `$S`   | copy of `%s` with field N replaced |

### Arrays

`index` operands are `i64`. Element access is bounds-checked: the VM reports a
runtime error and native code traps.

| syntax                              | operands              | result | meaning |
|-------------------------------------|-----------------------|--------|---------|
| `%d = array(%a, %b, ...)`           | `N` values of type `T` | `[N x T]` | build an array |
| `%d = splat %v`                     | `T`                   | `[N x T]` | an array with every element `%v` |
| `%d = elem %a, %i`                  | `[N x T], i64`        | `T`    | read an element |
| `%d = setelem %a, %i, %v`           | `[N x T], i64, T`     | `[N x T]` | copy of `%a` with element `%i` replaced |

### Memory (freestanding only)

| syntax                          | operands              | result | meaning |
|---------------------------------|-----------------------|--------|---------|
| `%d = load %p`                  | `*T`                  | `T`    | read memory |
| `store %p, %v`                  | `*T, T`               |        | write memory |
| `%d = addr %r`                  | `T`                   | `*T`   | address of register `%r` (valid until the function returns) |
| `%d = fieldptr %p, N`           | `*$S`                 | `*F` (F = type of field N) | address of a field |
| `%d = elemptr %p, %i`           | `*[N x T], i64`       | `*T`   | address of an element; bounds-checked |

### Builtins

| syntax                          | operands              | result | meaning |
|---------------------------------|-----------------------|--------|---------|
| `%d = syscall(%n, %a, ...)`     | int or `*T`, 1 to 7 operands | `i64` | freestanding only: raw Linux syscall `%n` |
| `print %a`                      | int, `bool` or `str`  |        | hosted only: print the value and a newline |
| `%d = to_str %a`                | int or `bool`         | `str`  | hosted (and macros): the value as text |
| `%d = unique %p`                | `str`                 | `str`  | compile time only: `p` plus a number unique in this compilation |
| `%d = asm "tmpl", "cons"(%a, ...)` | int, `bool` or `*T` | int, `*T` or `unit` | freestanding only: inline assembly |
| `%d = quote ["p0", "p1", ...](kind %h, ...)` | see below | `expr`, `stmts` or `items` | compile time only: code from template pieces and holes |

Each `quote` hole has a kind: `expr` (code in parentheses, or an int, bool or
`str` literal), `ident` (a `str` or `expr` that is an identifier, inserted as
is), `stmts` (`stmts` or `expr` code on lines of its own) or `items` (`items`
code on lines of its own).

`asm` passes `tmpl` and `cons` to LLVM unchanged: the template uses LLVM operand
references (`${0}` is the output if there is one, then the inputs, in order;
`$$` is a literal `$`) and `cons` is an LLVM constraint string (`=r`, `{rdi}`,
`0`, `~{memory}`, ...) with one input constraint per operand. On x86_64 the
backend uses the Intel dialect and adds `~{dirflag},~{fpsr},~{flags}`, as clang
does. Bool operands are passed as `i8`.

## Terminators

| syntax                 | meaning |
|------------------------|---------|
| `jmp bbN`              | jump |
| `br %c, bbT, bbF`      | `%c: bool`; go to `bbT` if true, else `bbF` |
| `ret %a`               | return `%a`, which must have the function's return type |
| `unreachable`          | control never gets here |

Returning from `@_start` exits the process: with the returned value if `@_start`
returns `i64`, with 0 if it returns `unit`.

## Native ABI

`jihoo-llc` passes aggregates (structs and arrays) by pointer: the caller passes
the address of its copy and the callee copies it into its own storage, and an
aggregate result is written through a hidden first parameter. Aggregates are
moved with `memcpy`; the module defines weak `memcpy`, `memmove` and `memset`
because freestanding programs have no libc.

## Planned

- `gcref` types for GC-managed objects beyond `str` and structs.
- A Rust-side JIR parser so `jihoo run file.jir` works.
