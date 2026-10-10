# LLVM backend

`backend-llvm` is `jihoo-llc`, a small C++ program on LLVM 21 that compiles a
native or freestanding JIR module to an object file. `jihoo build` runs it and
then links the result.

## Steps

1. **Parse** the `.jir` text (`backend-llvm/src/jir_parser.cpp`) into the C++
   mirror of the IR (`backend-llvm/src/jir.h`). The `jir N` header comes first,
   and a module of any version but the backend's own (`jihoo-llc --version`) is
   refused there ([Versions](../jir.md#versions)).
2. **Generate LLVM IR** (`backend-llvm/src/codegen.cpp`). Every JIR register
   gets an `alloca`; `mem2reg` turns them into SSA values, so the JIR need not
   be SSA. Structs and enums become named LLVM structs, and the backend checks
   each layout JIR records (`size S align A`) against the target's data layout,
   so a mismatch is a compile error instead of silent memory corruption.
3. **Optimize** with LLVM's standard pipeline for the level (`-O2` by default).
4. **Emit** an object file for the target: position independent for native
   programs, which the C compiler usually links into a PIE, and static for
   freestanding ones.

## What each profile gets

- **Native.** The program's functions are internal symbols named
  `jihoo.<name>`, so they never clash with C symbols. The module defines C's
  `int main(int argc, char **argv)`, which calls `jihoo.main` and returns its
  result truncated to `int` (0 for `unit`). `extern fn`s are external
  declarations, called with the C calling convention.
- **Freestanding.** `_start` is the only external symbol. Returning from it
  makes the `exit` syscall (60 on x86_64, 93 on aarch64) with the result. The
  module defines weak `memcpy`, `memmove`, `memset`, `fmod` and `fmodf`, which
  LLVM may call, since there is no libc to provide them.

## Calling convention

Aggregates (structs, enums and arrays) are passed by pointer: the caller passes
the address of its copy and the callee copies it into its own storage, and an
aggregate result is written through a hidden first parameter. Scalars and
pointers are passed the C way. This is why C functions only take scalars and
pointers, and why jihoo functions with scalar signatures work as C callbacks.
The details are in [JIR](../jir.md#native-abi).

## Checks and traps

- Array indexing is bounds-checked and traps (`llvm.trap`) when out of range.
- Integer `/` and `%` trap when dividing by zero, where the VM reports an
  error, and signed `MIN / -1` wraps to `MIN` (remainder 0) as on the VM. LLVM
  leaves both undefined, so the backend checks the divisor before dividing.
- Floats use no fast-math flags and no fused multiply-add, so results match the
  VM bit for bit.

## Targets

`syscall` and `asm` are lowered for x86_64 (Intel syntax) and aarch64; other
targets are rejected. `--target` picks a triple other than the host's, and
`--emit-llvm` writes the LLVM IR as text, which is the quickest way to see what
the backend made of a program ([Command line](../reference/command-line.md#jihoo-llc)).
