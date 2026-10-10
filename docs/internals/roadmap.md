# Roadmap

## Done

1. Type checker (static types with inference) and typed JIR.
2. Differential tests across both backends.
3. Sized integers, pointers with loads and stores, structs, arrays, `size_of`,
   inline asm.
4. `comptime` on the VM, generic functions.
5. Expression macros, generic structs, bitwise operators, modules and an
   `alloc` library, `pub`, statement and item macros, `unique`/`ident`.
6. GC hardening (one root set, generation-checked references, stress mode);
   function values.
7. Sum types and `match`; `ref T` for recursive data; nested patterns, guards,
   `|` patterns and `match` expressions.
8. Closures, captured by value and lifted into functions.
9. Tasks and channels on the VM: one OS thread, a deterministic scheduler;
   `select`.
10. Comptime closures: closures passed to `comptime` parameters, one instance
    per closure, captured values as hidden arguments.
11. Coroutines for freestanding code, as a library on top of function values
    and `asm`.
12. A native profile: LLVM-compiled programs for an OS, linked with libc,
    calling C through `extern fn`.
13. Floats: `f32` and `f64`, identical on the VM and natively, and across into
    C.
14. Targets: x86_64 and aarch64 Linux with `--target`, recorded in JIR;
    per-target modules (`sys.x86_64.jh`); the standard library on both, with
    coroutines on libc's contexts.

## Next

- **Language:** automatic macro hygiene; field visibility.
- **Native code:** passing structs to C by value (per-target C ABI lowering);
  exporting jihoo functions under C names; a `print` for native code.
- **Floats:** exact math builtins for every profile (`sqrt`, `floor`, ...,
  which IEEE defines exactly); reading a float's bits.
- **Tooling:** a Rust-side JIR parser, so that `jihoo run file.jir` works.

## Library layers

The standard library is meant to grow into three layers:

```text
std    hosted only: GC types, I/O, collections
alloc  any compiled profile, given an allocator: Vec(T), strings
core   everywhere: integers, pointers, slices, Option
```

Today `alloc` is a bump arena with `Vec(T)` (`lib/alloc.jh`); the goal is for
freestanding code to bring its own allocator, written in jihoo on top of
`syscall`, and use the same collections with it.
