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
- Integers are signed 64-bit decimals (`-5`, `42`). Float constants (only in
  `fconst`) are decimals with a `.` or an exponent (`1.5`, `-0.0`, `1e-7`), or
  `inf`, `-inf`, `nan`; they are read as the nearest `f64`.
- Strings are double-quoted. Escapes: `\n`, `\t`, `\\`, `\"`, `\xHH` (any byte).

## Module

```
jir 0                                ; format version, must come first
profile hosted|native|freestanding   ; language profile
struct ...                           ; zero or more structs
enum ...                             ; zero or more enums
extern ...                           ; zero or more C functions (native only)
fn ...                               ; zero or more functions
```

| profile        | runs on        | GC  | allowed builtins | pointers | C functions |
|----------------|----------------|-----|------------------|----------|-------------|
| `hosted`       | VM             | yes | `print`          | no       | no          |
| `native`       | LLVM + libc    | no  | `syscall`, `asm` | yes      | `extern`    |
| `freestanding` | LLVM, no libc  | no  | `syscall`, `asm` | yes      | no          |

The entry point is `@main` for hosted and native modules and `@_start` for
freestanding ones. It returns `unit` or `i64` and takes no parameters, except
that a native `@main` may take C's `(i32, **u8)` (argc and argv).

### Extern functions

```
extern @puts(*u8) -> i32
extern @printf(*u8, ...) -> i32
```

A C function the module calls, by its symbol name, with the C calling
convention. `call` and `funcref` name it like any other function, and a module
cannot have a function and an extern function of the same name. Its parameter
and result types are integers, floats, `bool`, pointers and function types made of
those, and a `unit` result (C's `void`); structs, enums and arrays go by
pointer. Integers narrower than 32 bits and bools are sign- or zero-extended as
the C ABI requires.

With `...`, the function takes more arguments after the listed ones, like
`printf`. A `call` may pass any integer, float, `bool`, pointer or function value
there, and they get C's default promotions (bools and integers narrower than 32
bits widen to 32, `f32` to `f64`). A variadic extern cannot be a `funcref`.

## Types

| type              | meaning                                  | LLVM          |
|-------------------|------------------------------------------|---------------|
| `unit`            | no value                                 | `{}`          |
| `bool`            | `true` / `false`                         | `i1`          |
| `i8` … `i64`      | signed integers                          | `i8` … `i64`  |
| `u8` … `u64`      | unsigned integers                        | `i8` … `i64`  |
| `f32`, `f64`      | IEEE 754 binary32 and binary64          | `float`, `double` |
| `str`             | GC-managed string, hosted only           | —             |
| `*T`              | raw pointer to `T`, native and freestanding only | `ptr` |
| `$Name`           | struct or enum, by value                 | named struct  |
| `expr`, `stmts`, `items` | code, inside macros; compile time only | —        |
| `[N x T]`         | array of `N` `T`s, by value              | `[N x T]`     |
| `fn(T, ...) -> R` | function value                           | `ptr`         |
| `ref T`           | immutable GC reference to a `T`, hosted only | —         |
| `chan T`          | channel of `T` values between tasks, hosted only | —     |
| `cell T`          | shared mutable `T`, hosted only          | —             |

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

## Enums

```
enum $Name { A, B(T, U), ... } size S align A
```

A sum type: a value is one of the variants, and each variant carries its own
payload values. Instructions refer to variants and payload values by index.
Structs and enums share one namespace. Like a struct, an enum may hold other
types by value but not itself, and `size S align A` is omitted when it contains
`str`.

The layout is the C layout of `struct { uint32_t tag; union { ... } payload; }`,
where each variant's payload is laid out like a struct of its values. Natively
the backend uses `{ i32, [N x iA] }`, with `A` the largest payload alignment, and
reads a variant's payload through a struct of its value types at the payload's
address. On the VM an enum is a GC object holding the tag and the payload.

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
| `%d = fconst X`                 |                       | `f32` or `f64` | float constant; for `f32`, `X` is exactly an `f32` value |
| `%d = unit`                     |                       | `unit` | the unit value |
| `%d = str "..."`                |                       | `str` (hosted) / `*u8` (native, freestanding) | string literal; compiled strings are NUL-terminated constant bytes |
| `%d = copy %a`                  | `T`                   | `T`    | copy |
| `%d = neg %a`                   | signed int or float   | same   | wrapping negation; for floats, flips the sign (also of 0 and NaN) |
| `%d = not %a`                   | `bool` or int         | same   | logical not, or bitwise not of an integer |
| `%d = add\|sub\|mul %a, %b`     | `T, T` (int)          | `T`    | wrapping arithmetic |
| `%d = div\|rem %a, %b`          | `T, T` (int)          | `T`    | signed or unsigned by type; truncating (VM traps on zero; native: undefined for now) |
| `%d = add\|sub\|mul\|div\|rem %a, %b` | `T, T` (float) | `T` | IEEE 754, rounded to nearest; `rem` is C's `fmod` (exact, sign of `%a`) |
| `%d = and\|or\|xor %a, %b`      | `T, T` (int or `bool`) | `T`   | bitwise (for `bool`: logical, both sides evaluated) |
| `%d = shl\|shr %a, %b`          | `T, T` (int)          | `T`    | shift by `%b` modulo the bit width; `shr` is arithmetic for signed types, logical for unsigned |
| `%d = add %a, %b`               | `str, str`            | `str`  | concatenation |
| `%d = add\|sub %a, %b`          | `*T, i64`             | `*T`   | pointer offset in elements of `T` |
| `%d = eq\|ne %a, %b`            | `T, T`: `bool`, int, float, `str`, `*U` | `bool` | equality (`str` compares contents; floats as IEEE: NaN is unequal to itself, `0.0 == -0.0`); the frontend compares structs, enums, arrays and refs with helper functions `@fn.eq.N` |
| `%d = lt\|le\|gt\|ge %a, %b`    | `T, T`: int, float or `*U` | `bool` | ordered comparison, signed or unsigned by type; pointers unsigned; false if either float is NaN |
| `%d = cast %a`                  | see below             | dst type | conversion |
| `%d = call @f(%a, ...)`         | parameter types of `@f` | return type of `@f` | call |
| `%d = funcref @f`               |                       | `fn(P...) -> R` of `@f` | function `@f` as a value |
| `%d = call %f(%a, ...)`         | `%f: fn(P...) -> R`, then `P...` | `R` | call a function value |
| `%d = closure @f(%c, ...)`      | the first parameter types of `@f` | `fn(P...) -> R`, the rest of `@f`'s signature | hosted only: a function value that calls `@f` with `%c, ...` before its own arguments |

`cast` allows: int → int (truncate, or sign-/zero-extend by the *source* type),
`bool` → int (0/1), int → float and float → float (to nearest), float → int
(dropping the fraction, saturating at the limits, NaN → 0), `*T` → `*U`, `*T` ↔
`i64`/`u64`, and any type to itself.

### Structs

| syntax                              | operands              | result | meaning |
|-------------------------------------|-----------------------|--------|---------|
| `%d = struct $S(%a, %b, ...)`       | every field, in order | `$S`   | build a struct |
| `%d = field %s, N`                  | `$S`                  | type of field N | read a field |
| `%d = setfield %s, N, %v`           | `$S`, type of field N | `$S`   | copy of `%s` with field N replaced |
| `%d = getpath %s, (step, ...)`      | a struct or array; each step `field N` or `elem %i` (`%i: i64`) | type at the end of the path | read a nested part; elements are bounds-checked |
| `%d = setpath %s, (step, ...), %v`  | as `getpath`, then the type at the end of the path | type of `%s` | copy of `%s` with the nested part replaced |

`setfield`, `setelem` and `setpath` describe values: natively and on the VM they
update in place when `%d` is `%s` (on the VM, only if no other reference to the
object exists).

### Enums

| syntax                              | operands              | result | meaning |
|-------------------------------------|-----------------------|--------|---------|
| `%d = variant K(%a, %b, ...)`       | the payload of variant K | the enum (type of `%d`) | build variant K |
| `%d = tag %e`                       | an enum               | `u32`  | the index of the variant `%e` holds |
| `%d = payload %e, K, N`             | an enum               | type of value N of variant K | read a payload value; `%e` must hold variant K (the VM checks; natively it is undefined) |

### References (hosted only)

| syntax                              | operands              | result | meaning |
|-------------------------------------|-----------------------|--------|---------|
| `%d = ref %v`                       | `T`                   | `ref T` | a new reference to a copy of `%v` |
| `%d = deref %r`                     | `ref T`               | `T`    | the value `%r` refers to |

A `ref` field breaks the rule that a struct or enum may not contain itself.
Types containing a `ref` have no fixed layout, like those containing `str`.

### Cells (hosted only)

| syntax                              | operands              | result | meaning |
|-------------------------------------|-----------------------|--------|---------|
| `%d = cell %v`                      | `T`                   | `cell T` | a new cell holding a copy of `%v` |
| `%d = cellget %c, (step, ...)`      | `cell T`, a path into `T` (may be empty) | type at the end of the path | read what the cell holds, or a part of it |
| `cellset %c, (step, ...), %v`       | `cell T`, a path, the type at its end | | replace what the cell holds, or a part of it |

### Tasks and channels (hosted only)

| syntax                              | operands              | result | meaning |
|-------------------------------------|-----------------------|--------|---------|
| `%d = chan %n`                      | `i64`                 | `chan T` | a new channel buffering up to `%n` values (0: sender and receiver meet) |
| `send %c, %v`                       | `chan T, T`           |        | send `%v`, waiting while the channel is full |
| `%d = recv %c`                      | `chan T`              | `T`    | receive a value, waiting until there is one |
| `spawn %f(%a, ...)`                 | `%f: fn(P...) -> R`, then `P...` | | call `%f` in a new task; the result is dropped |
| `%d = select [case; ...]`           | each case `recv %c -> %v` (`%v: T`) or `send %c, %v`, optionally a last `default` | `i64` | do the first case that can go ahead, waiting until one can; `%d` is its index. With `default` it does not wait: `%d` is the number of cases if none can go ahead |

The VM runs tasks on one thread with a deterministic round-robin scheduler; the
run ends when the entry function returns. A task is only switched out at a
call, at a jump to a block with a lower or equal number (every loop has one,
since blocks are numbered breadth-first), or when it waits on a channel.

### Arrays

`index` operands are `i64`. Element access is bounds-checked: the VM reports a
runtime error and native code traps.

| syntax                              | operands              | result | meaning |
|-------------------------------------|-----------------------|--------|---------|
| `%d = array(%a, %b, ...)`           | `N` values of type `T` | `[N x T]` | build an array |
| `%d = splat %v`                     | `T`                   | `[N x T]` | an array with every element `%v` |
| `%d = elem %a, %i`                  | `[N x T], i64`        | `T`    | read an element |
| `%d = setelem %a, %i, %v`           | `[N x T], i64, T`     | `[N x T]` | copy of `%a` with element `%i` replaced |

### Memory (native and freestanding only)

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
| `%d = syscall(%n, %a, ...)`     | int or `*T`, 1 to 7 operands | `i64` | native and freestanding only: raw Linux syscall `%n` |
| `print %a`                      | number, `bool` or `str` |      | hosted only: print the value and a newline (floats as the shortest text that reads back exactly: `0.1`, `1.0`, `1e100`, `inf`, `NaN`) |
| `%d = to_str %a`                | number or `bool`      | `str`  | hosted (and macros): the value as text, as `print` writes it |
| `%d = unique %p`                | `str`                 | `str`  | compile time only: `p` plus a number unique in this compilation |
| `%d = asm "tmpl", "cons"(%a, ...)` | int, `bool` or `*T` | int, `*T` or `unit` | native and freestanding only: inline assembly |
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

## Function values

A function value is an index into the module's functions on the VM and a code
pointer natively (8 bytes, aligned to 8). There is no null function value, and
function values have no equality. A call through a function value uses the same
calling convention as a direct call, including the aggregate rules below.

A closure (hosted only) is a function value too: on the VM, a GC object holding
the function and the captured values, which a call passes as the first
arguments. The frontend names the functions it lifts out of anonymous functions
`@fn.N`; `fn` is a keyword, so these never clash with user names.

## Native ABI

`jihoo-llc` passes aggregates (structs and arrays) by pointer: the caller passes
the address of its copy and the callee copies it into its own storage, and an
aggregate result is written through a hidden first parameter. Aggregates are
moved with `memcpy`; a freestanding module defines weak `memcpy`, `memmove`,
`memset`, and `fmod` and `fmodf` (which float `rem` becomes) because it has no
libc, and a native one uses libc's and libm's.

This is not the C ABI for aggregates, which is why extern functions only take
scalars. Scalars and pointers are passed the C way, so a jihoo function whose
parameters and result are floats, integers of at least 32 bits, pointers or `unit` can
be handed to C as a callback (as `qsort`'s comparator, say).

In a native module the program's functions are internal symbols named
`jihoo.<name>`, so they never clash with C symbols, and the module defines C's
`int main(int argc, char **argv)`, which calls `jihoo.main` and returns its
result truncated to `int` (0 for `unit`). Native objects are position
independent, since the C compiler that links them usually makes a PIE.

## Planned

- `gcref` types for GC-managed objects beyond `str` and structs.
- A Rust-side JIR parser so `jihoo run file.jir` works.
