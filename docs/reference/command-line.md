# Command line

`jihoo` runs hosted programs on the VM and compiles native and freestanding
ones. Run `jihoo help` for a summary.

```sh
jihoo run hello.jh                   # hosted: run on the VM
jihoo build args.jh -o args          # native or freestanding: compile
jihoo emit-ir fib.jh                 # print the program's JIR
```

## Commands

### `jihoo run <file.jh>`

Compiles a hosted program and runs it on the VM. Its exit status is what
`main` returns (modulo 256), or 0 if `main` returns nothing; a runtime error
(such as an out-of-bounds index, division by zero or a deadlock) is printed as
`error: runtime error in @function: message` and exits with status 1.

Native and freestanding programs are refused: they need `jihoo build`.

### `jihoo build <file.jh> [-o out] [link inputs]`

Compiles a native or freestanding program to an executable, `out`, which
defaults to the input's name without `.jh`. The steps are:

1. check the program and write its JIR (`out.jihoo.jir`);
2. compile the JIR to an object file with `jihoo-llc` (`out.jihoo.o`);
3. link: a **native** program with the C compiler, together with libc, libm and
   the link inputs; a **freestanding** program with `ld.lld -static
   --gc-sections -e _start`, with nothing else.

The intermediate files are removed afterwards. Hosted programs are refused,
since they run with `jihoo run`.

Link inputs (native programs only) are passed on to the C compiler:

| input | meaning |
|-------|---------|
| `-l <lib>`, `-l<lib>` | link with `lib<lib>` |
| `-L <dir>`, `-L<dir>` | look for libraries in `<dir>` |
| `file.c`, `file.o`, `file.a`, `file.so` | compile or link in that file |

### `jihoo emit-ir <file.jh> [-o out]`

Checks a program of any profile and prints its [JIR](../jir.md), or writes it
to `out`. Useful to see what the compiler made of a program: monomorphized
generics, expanded macros, folded constants.

## Options

| option | meaning |
|--------|---------|
| `-o <path>` | the output file (`build`, `emit-ir`) |
| `-I <dir>` | also look for imported modules in `<dir>`; may be repeated |

## Environment

| variable | meaning | default |
|----------|---------|---------|
| `JIHOO_PATH` | directories to look for imported modules in, separated by `:` | none |
| `JIHOO_LLC` | the LLVM backend | `jihoo-llc` in `PATH` |
| `JIHOO_LD` | the linker for freestanding programs | `ld.lld` in `PATH` |
| `JIHOO_CC` | the C compiler that links native programs | `cc` in `PATH` |
| `JIHOO_GC_STRESS` | `1`: collect garbage before every allocation, to find GC bugs | off |

The Nix package sets these so that everything is found; from a source checkout,
set `JIHOO_LLC` to the backend you built
([Getting started](../../site/src/content/docs/start/getting-started.md)).

## Errors

Compile errors are printed as `error: file:line:col: message`, one per
function that has an error, so one run shows the first error in every function.
The exit status is then 1.

## `jihoo-llc`

The backend can also be used on its own, for instance on hand-written JIR:

```sh
jihoo-llc prog.jir -o prog.o            # an object file
jihoo-llc prog.jir -o prog.ll --emit-llvm
```

| option | meaning |
|--------|---------|
| `-o <path>` | the output file (required) |
| `--emit-llvm` | write textual LLVM IR instead of an object file |
| `-O0` … `-O3` | optimization level (default `-O2`) |
| `--target <triple>` | target triple (default: the host) |
