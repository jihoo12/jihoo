# jihoo design

jihoo aims to be an easy language that can still go all the way down to bare
syscalls. One surface language is lowered to one IR (JIR); that IR is either run by
a VM or compiled by LLVM.

```
               ┌─────────────── Rust ───────────────┐      ┌──── C++ ─────┐
 source.jh ──► │ jihoo-syntax ─► jihoo-sema ──► JIR │ ──►  │  jihoo-llc   │ ──► .o ──► ld.lld ──► ELF
               │   (parse)      (typecheck    │    │ .jir │ (LLVM 21)    │
               │                 + lower)     ▼    │      └──────────────┘
               │                           jihoo-vm│
               └────────────────────────────────────┘
```

## Profiles

A program chooses its profile with a file attribute.

|                 | hosted (default)        | freestanding (`#![freestanding]`) |
|-----------------|-------------------------|-----------------------------------|
| backend         | VM                      | LLVM, native static binary        |
| memory          | GC, managed by the VM   | manual                            |
| library         | std (strings, `print`…) | core only                         |
| syscalls / asm  | no                      | yes                               |
| entry point     | `fn main()`             | `fn _start()`                     |

The profile is a property of the program, not a command line switch. Code that
uses GC features is rejected in freestanding mode at compile time (see
`crates/jihoo-ir/src/verify.rs`), so a program never silently changes meaning
between modes. This mirrors Rust's `no_std` / `no_main`.

### Planned library layers

```
std    hosted only: GC types, I/O, collections
alloc  any profile, given an allocator: Vec[T, A], String[A]
core   everywhere: integers, pointers, slices, Option
```

Freestanding code should not have to start from raw `mmap` every time; `alloc`
will let it bring its own allocator (written in jihoo on top of `syscall`).

## Types

Static types, with inference for local variables:

```jihoo
fn area(w: i64, h: i64) -> i64 {   // signatures are written out
    let a = w * h                  // `a: i64` is inferred
    return a
}
```

- Types: `unit`, `i64`, `bool`, plus `str` (hosted only, garbage collected) and
  `ptr` (freestanding only, raw byte pointer). A string literal is a `str` when
  hosted and a `ptr` to constant bytes when freestanding.
- A function without `-> T` returns `unit`.
- `if`/`while` conditions and the operands of `&&`, `||`, `!` must be `bool`;
  there is no implicit integer-to-bool conversion.
- A function that returns a value must `return` on every path that reaches the
  end of its body.

Type checking and lowering happen in one pass in `crates/jihoo-sema` (the same
shape as Zig's Sema): each expression is checked and turned into typed JIR at the
same time. The operator rules live in `crates/jihoo-ir/src/types.rs` and are shared
with the IR verifier, so the checker and the verifier cannot disagree. Errors are
reported per function, so one run shows the first error in every function.

## Testing

- Unit tests in each crate (`cargo test`).
- Differential tests (`tests/diff/`): each program runs on the VM and as a native
  binary, and both must exit with the same status. This keeps the two backends
  honest about the semantics of JIR.

## Why the IR is a text file

The frontend (Rust) and the LLVM backend (C++) only talk through `.jir` files
(`docs/jir.md`). This keeps the IR an explicit, versioned contract, lets the backend
be developed and tested on its own with hand-written `.jir`, and keeps the Rust build
free of LLVM.

## Metaprogramming (planned)

Compile-time execution will run on the VM, even when the final target is
freestanding: the compiler lowers `comptime` code to JIR, runs it on the embedded
VM, and splices the result back. The only rule is that comptime code cannot use
`syscall`/asm. AST macros will build on the same mechanism.

## GC

`crates/jihoo-vm/src/gc.rs` is a stop-the-world mark & sweep heap. The roots are the
registers of every VM frame; a collection may happen at any allocation once the heap
grows past twice the size that survived the last collection (1 MiB minimum).

## Roadmap

1. ~~Type checker (static types with inference) and typed JIR.~~
2. ~~Differential tests across both backends.~~ Rust-side JIR parser.
3. Sized integers, pointers with loads/stores, structs, inline `asm` blocks.
4. `comptime` on the VM.
5. `alloc` layer; AST macros.
