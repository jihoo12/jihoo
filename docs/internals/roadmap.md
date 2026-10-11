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
15. Macro hygiene: a template's locals are its own, and its other names
    resolve where the macro is defined.
16. Field visibility: `pub` fields, also through macros.
17. Float builtins, the same in every profile: `sqrt`, `floor`, `ceil`,
    `trunc`, `round`, `abs`, `copysign`, and a float's bits (`to_bits`,
    `from_bits`).
18. `print` in native programs, with the VM's output to the byte.
19. Structs by value to and from C, lowered per target (x86_64 System V,
    AAPCS64), and `export fn` for functions C calls.
20. A Rust-side JIR reader: `jihoo run file.jir`, `jihoo build file.jir`.
21. A hosted standard library: strings as bytes with `len`, `s[i]`, slices
    and ordering; lists `[T]` with `push` and `pop`, in place when unshared;
    `read_line`; the prelude (`Option`, `Result`); `lib/strings.jh` and
    `lib/lists.jh`. JIR version 5.

## Next

- **Floats:** `fma`, which IEEE also defines exactly, but freestanding code on
  x86_64 would need an exact software version.
- **Native code:** structs by value through function values handed to C
  (callbacks take scalars only); `print` in freestanding programs.
- **Hosted library:** a map keyed by `str`; files; parsing floats; `for`
  loops over lists and ranges, so that library code needs fewer `while` loops.

## Library layers

The standard library is meant to grow into three layers:

```text
std    hosted only: GC types, I/O, collections
alloc  any compiled profile, given an allocator: Vec(T), strings
core   everywhere: integers, pointers, slices, Option
```

Today `std` is the prelude, `strings` and `lists`, on the VM's `str` and `[T]`,
and `alloc` is a bump arena with `Vec(T)` (`lib/alloc.jh`); the goal is for
freestanding code to bring its own allocator, written in jihoo on top of
`syscall`, and use the same collections with it.
