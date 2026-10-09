# JIR — jihoo IR text format (version 0)

JIR is the contract between the Rust frontend/VM and the C++ LLVM backend.
`jihoo emit-ir` writes it, `jihoo-llc` reads it. Both sides must agree on this
document; the Rust definitions live in `crates/jihoo-ir/src/lib.rs` and the C++ ones
in `backend-llvm/src/jir.h`.

## Example

```
; jihoo IR
jir 0
profile freestanding

fn @fib params 1 regs 10 {
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

| profile        | runs on | GC  | allowed builtins |
|----------------|---------|-----|------------------|
| `hosted`       | VM      | yes | `print`          |
| `freestanding` | LLVM    | no  | `syscall`        |

The entry point is `@main` for hosted modules and `@_start` for freestanding ones.
Both take no parameters.

## Functions

```
fn @name params P regs R {
bb0:
  ...
}
```

- Arguments arrive in registers `%0 .. %(P-1)`. Every register index must be `< R`.
- Blocks are numbered `bb0, bb1, ...` in order. `bb0` is the entry block.
- Every block ends with exactly one terminator.
- Every function returns one value. Functions with nothing to return return `0`.

## Values

In v0 every register holds one 64-bit value:

- integers, and booleans as `0`/`1`;
- strings: a GC-managed string on the VM, or the address of NUL-terminated constant
  data when freestanding.

Registers are **not** SSA: a register may be written more than once (the frontend
maps each `let` variable to one register). The LLVM backend gives each register an
`alloca` and relies on `mem2reg`.

## Instructions

| syntax                        | meaning |
|-------------------------------|---------|
| `%d = const N`                | `%d = N` |
| `%d = str "..."`              | string literal |
| `%d = copy %a`                | `%d = %a` |
| `%d = neg %a`                 | `-%a` (wrapping) |
| `%d = not %a`                 | `1` if `%a == 0`, else `0` |
| `%d = add\|sub\|mul %a, %b`   | wrapping arithmetic |
| `%d = div\|rem %a, %b`        | signed division (VM traps on zero; native: undefined for now) |
| `%d = eq\|ne\|lt\|le\|gt\|ge %a, %b` | signed comparison, result `0`/`1` |
| `%d = call @f(%a, ...)`       | call; argument count must match `params` |
| `%d = syscall(%n, %a, ...)`   | freestanding only: raw Linux syscall `%n` with up to 6 arguments |
| `print %a`                    | hosted only: print an int or string and a newline |

On the VM, `add`, `eq` and `ne` also accept two strings (`add` concatenates).

## Terminators

| syntax                 | meaning |
|------------------------|---------|
| `jmp bbN`              | jump |
| `br %c, bbT, bbF`      | go to `bbT` if `%c != 0`, else `bbF` |
| `ret %a`               | return `%a` |

Returning from `@_start` exits the process with that value as the status
(the backend lowers it to the `exit` syscall).

## Planned

- Real types in the IR (`i8..i64`, `ptr`, `gcref`, structs). Keeping `gcref` and raw
  `ptr` apart is what will later allow GC-enabled native builds.
- Loads/stores and inline `asm` blocks for freestanding code.
- A Rust-side JIR parser so `jihoo run file.jir` works and so the two backends can be
  tested against each other on the same file.
