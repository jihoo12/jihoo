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

In both compiled profiles the program's functions are internal symbols named
`jihoo.<name>`, so they never clash with C symbols, and each exported function
is also an external C function of its own name (see below).

- **Native.** The module defines C's `int main(int argc, char **argv)`, which
  calls `jihoo.main` and returns its result truncated to `int` (0 for `unit`).
  `extern fn`s are external declarations, called with the C calling
  convention. `print` calls the C library: `printf` for integers, `puts` for
  bools and strings, and for floats `jihoo.print_float`, which the module
  defines. It finds the shortest digits that read back (`strtod`, `strtof`)
  from the exact digits `snprintf` gives, rounding ties up as the VM does, so
  native and VM output agree to the byte.
- **Freestanding.** `_start` is the only external symbol besides exported
  functions. Returning from it makes the `exit` syscall (60 on x86_64, 93 on
  aarch64) with the result. The module defines weak `memcpy`, `memmove`,
  `memset`, `fmod`, `fmodf`, `floor`, `ceil`, `trunc`, `round` and their `f`
  forms, which LLVM may call (on x86_64 without SSE4.1, the rounding
  intrinsics become calls), since there is no libc to provide them; an
  exported function of one of those names takes its place.

## Calling convention

Between the program's own functions, aggregates (structs, enums and arrays)
are passed by pointer: the caller passes the address of its copy and the callee
copies it into its own storage, and an aggregate result is written through a
hidden first parameter. Scalars and pointers are passed the C way, which is
why jihoo functions with scalar signatures work as C callbacks. The details
are in [JIR](../jir.md#native-abi).

## The C ABI

C passes structs by value its own way, which LLVM leaves to the compiler of
the calling language, as clang does for C. `backend-llvm/src/cabi.cpp` decides
it for each parameter and result of an `extern fn` or exported function, by
the target's rules: on x86_64 (System V) by the class of each eightbyte of a
struct of up to 16 bytes, with the free registers counted so a struct goes
wholly in registers or wholly on the stack (`byval`); on aarch64 (AAPCS64) as
up to four floats in SIMD registers (`[N x float]`), else up to 16 bytes in
general registers (`i64`, `[2 x i64]`), else by pointer to a copy. Larger
results go through `sret`.

A call to C copies each struct into a temporary laid out as its registers and
loads them from there, and stores a returned one back the same way. An
exported function `f` gets a C function that does the reverse and calls
`jihoo.f`. `tests/ffi/structs.jh` and `structs.c` pass every kind of struct
both ways; the C signatures match what clang makes for the same C on both
targets.

## Checks and traps

- Array indexing is bounds-checked and traps (`llvm.trap`) when out of range.
- Integer `/` and `%` trap when dividing by zero, where the VM reports an
  error, and signed `MIN / -1` wraps to `MIN` (remainder 0) as on the VM. LLVM
  leaves both undefined, so the backend checks the divisor before dividing.
- Floats use no fast-math flags and no fused multiply-add, so results match the
  VM bit for bit.
- Which NaN an operation makes differs between machines (x86_64 sets the sign
  of `0.0 / 0.0`, aarch64 and LLVM's constant folding do not), so no NaN's
  sign or payload is visible: `bitcast` reads every NaN as the one quiet NaN,
  and `copysign` with a NaN sign gives the magnitude.

## Targets

A module names its target (`target x86_64` or `target aarch64`), and the
backend compiles for that machine's Linux triple, whatever machine it runs on;
a module without the line is compiled for the host. `syscall` and `asm` are
lowered for x86_64 (Intel syntax) and aarch64. `--emit-llvm` writes the LLVM IR
as text, which is the quickest way to see what the backend made of a program ([Command line](../reference/command-line.md#jihoo-llc)).
