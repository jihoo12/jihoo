# Command line

`jihoo` compiles native and freestanding programs and runs hosted ones on the
VM. Programs are native unless their root file says otherwise
([Profiles](../language/profiles.md)). Run `jihoo help` for a summary.

```sh
jihoo run hello.jh                   # compile and run
jihoo run args.jh -- one two         # with arguments for `main(argc, argv)`
jihoo build args.jh -o args          # compile to an executable
jihoo emit-ir fib.jh                 # print the program's JIR
```

## Commands

### `jihoo run <file.jh> [link inputs] [-- args]`

Compiles a program and runs it on this machine, with `args` as its arguments.
Its exit status is the program's: what `main` returns (modulo 256), or 0 if
`main` returns nothing.

- A **native** or **freestanding** program is built as `jihoo build` would,
  into a temporary directory that is removed afterwards, and run. A program
  that traps (an out-of-bounds index, division by zero, `trap()`) is killed by
  a signal; `jihoo run` says so and exits with 128 plus the signal's number, as
  a shell reports it. What the program printed before is not lost.
- A **hosted** program (`#![hosted]`) runs on the VM. It takes no arguments
  and no link inputs. A runtime error (also a deadlock) is printed as
  `error: runtime error in @function: message` and exits with status 1.

### `jihoo build <file.jh> [-o out] [link inputs]`

Compiles a native or freestanding program to an executable, `out`, which
defaults to the input's name without `.jh`. An output that would replace the
input itself (a source without an extension, or `-o` naming it) is an error.
The steps are:

1. check the program and write its JIR (`out.jihoo.jir`);
2. compile the JIR to an object file with `jihoo-llc` (`out.jihoo.o`);
3. link: a **native** program with the C compiler, together with libc, libm and
   the link inputs; a **freestanding** program with `ld.lld -static
   --gc-sections -e _start`, with nothing else.

With `--target`, a program can be built for the other machine: a freestanding
one needs nothing more (`ld.lld` links for both), a native one a C compiler for
that machine in `JIHOO_CC`. The intermediate files are removed afterwards. Hosted programs are refused,
since they run on the VM with `jihoo run`.

Link inputs (native programs only, for `build` and `run`) are passed on to the C compiler:

| input | meaning |
|-------|---------|
| `-l <lib>`, `-l<lib>` | link with `lib<lib>` |
| `-L <dir>`, `-L<dir>` | look for libraries in `<dir>` |
| `file.c`, `file.o`, `file.a`, `file.so` | compile or link in that file |

### `jihoo emit-ir <file.jh> [-o out]`

Checks a program of any profile and prints its [JIR](../jir.md), or writes it
to `out`. Useful to see what the compiler made of a program: monomorphized
generics, expanded macros, folded constants.

### JIR files

Every command also takes a JIR file, `file.jir`, in place of a source file:
`jihoo run file.jir` runs it (a hosted module on the VM), `jihoo build file.jir`
compiles a native or freestanding one, and `jihoo emit-ir file.jir` prints it
again. The module is read as it is ([JIR](../jir.md)), checked by the same
verifier the compiler's output passes, and must be of the JIR version this
`jihoo` writes. It was made for the target its `target` line names, so
`--target` cannot change it.

```sh
jihoo emit-ir fib.jh -o fib.jir      # write the JIR, look at it or edit it
jihoo run fib.jir                    # and run it
```

## Options

| option | meaning |
|--------|---------|
| `-o <path>` | the output file (`build`, `emit-ir`) |
| `-I <dir>` | also look for imported modules in `<dir>`; may be repeated |
| `-O0` … `-O3` | how much the LLVM backend optimizes native and freestanding programs, `-O2` by default (`build`, `run`). `-O0` keeps every allocation and free the compiler inserts, which is what checking memory with valgrind needs: optimized code may drop or merge them |
| `--target <t>` | the machine to compile for, `x86_64` or `aarch64` (Linux); the default is this machine. It picks the [per-target modules](../language/modules.md#per-target-modules) and is recorded in the JIR (`build`, `emit-ir`; `run` only runs on this machine) |
| `--cost` | also print where a native program copies lists, strings and closures, and why (`build`); see [below](#copies) |

## Copies

Natively, the compiler copies a list (or a string, or a closure's captured
values) only where the program's meaning needs it: where a value is still
used after it is assigned, stored or passed on to something that keeps or
changes it ([Lists in native programs](../language/lists.md#lists-in-native-programs)).
`jihoo build --cost` lists those places:

```text
$ jihoo build --cost shelf.jh
shelf.jh:17:1: copies [i64] in @grow: parameter 1 is copied on entry, since the function changes or keeps it; a caller that hands it over at its last use calls a version that does not copy it
shelf.jh:53:17: copies [i64] in @report: a part read out of a value is copied, since the part or the value changes while the other is still used
2 copies
```

Each line is one copy in the program: where, of what type, in which function
(as JIR names it, so `@f.own` is the version of `f` that takes its arguments
over), and why. A copy of a `ref` or a cell is not listed: it counts one more
holder. Functions the program never calls are left out, as is the copying
version of a function whose callers all hand their arguments over.

To keep a function free of copies, declare it [`nocopy`](../language/nocopy.md):
a copy it or anything it calls makes is then a compile error, with the same
reasons.

## Environment

| variable | meaning | default |
|----------|---------|---------|
| `JIHOO_PATH` | directories to look for imported modules in, separated by `:` | none |
| `JIHOO_LLC` | the LLVM backend | `jihoo-llc` in `PATH` |
| `JIHOO_LD` | the linker for freestanding programs | `ld.lld` in `PATH` |
| `JIHOO_CC` | the C compiler that links native programs | `cc` in `PATH` |
| `JIHOO_GC_STRESS` | `1`: collect garbage before every allocation, to find GC bugs | off |
| `JIHOO_VM_CHECK` | `1`: before every in-place update, check that nothing else refers to the object, to find sharing bugs | off |
| `JIHOO_LOCS` | set: write source positions into the JIR of every program, as `--cost` does, to test that they change nothing | off |

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
| `--version` | print the [JIR version](../jir.md#versions) it reads; it refuses modules of any other |
