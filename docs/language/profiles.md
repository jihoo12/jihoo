# Profiles

A program chooses where it runs with a *profile*, set by an attribute at the top
of its root file. There are three, one per place a program can run: on an
operating system, on the jihoo VM, or on bare metal. Without an attribute, a
program is native.

```jihoo
// native: no attribute (or `#![native]`)
import libc

fn main() {
    print("an ordinary program, compiled with LLVM")
    libc.printf("and linked with libc\n")
}
```

```jihoo
#![hosted]

fn main() {
    print("on the VM, with a GC")
}
```

```jihoo
#![freestanding]

fn _start() -> i64 {
    syscall(60, 7)   // exit(7) on x86_64 Linux (93 on aarch64); no libc at all
    return 0
}
```

## What each profile has

|                 | hosted (`#![hosted]`)   | native (default)               | freestanding (`#![freestanding]`) |
|-----------------|-------------------------|--------------------------------|-----------------------------------|
| runs on         | the jihoo VM            | an OS, as a normal program     | anything: no OS libraries at all  |
| command         | `jihoo run`             | `jihoo run`, or `jihoo build` for an executable | `jihoo run`, `jihoo build` |
| backend         | VM                      | LLVM, linked by `cc` with libc | LLVM, static binary via `ld.lld`  |
| memory          | GC, managed by the VM   | lists and `str` freed by the compiler; pointers manual (`malloc`, or `alloc`) | manual (`alloc` over `mmap`) |
| string literals | `str`                   | `str`, or `*u8` where one is expected | `*u8`, NUL-terminated     |
| output          | `print`                 | `print`, or C (`libc.printf`)  | `syscall` (`io.puts`)             |
| C functions     | no                      | `extern fn`, `export fn`, C files, `-l` | `export fn`              |
| pointers        | no                      | `*T`, `&x`, `*p`, `p[i]`       | `*T`, `&x`, `*p`, `p[i]`          |
| syscalls / asm  | no                      | yes                            | yes                               |
| concurrency     | `go` tasks, channels, `select`, one thread | `go` threads, channels, `select`, atomics, locks (`sync`) | atomics, locks (`sync`) |
| lists, `str`, `ref`, `cell`, capturing closures | yes | yes, freed by the compiler | no                  |
| `chan`, `select`, tasks | yes             | yes, as threads; tasks share no cells | no                    |
| entry point     | `fn main()`             | `fn main()` or `fn main(argc: i32, argv: **u8)` | `fn _start()`    |

Everything else — integers and floats, structs, arrays, enums, `match`, function
values, generics, `comptime` and macros — is the same in every profile.

The one thing a hosted program can do that a native one cannot is share a
`cell` between tasks: hosted tasks take turns on one thread, while native tasks
are threads, and cells are not locked. Natively, a task that would get a cell is
a compile error, or a trap where the compiler cannot see it (a closure that
captured a cell, started with `go`). Tasks pass values over channels instead;
see [Cells](cells.md#cells-and-tasks).

`comptime` and macros run on the VM in every profile, so the VM stays part of
the compiler even for programs that never use it at run time.

## Native and freestanding

Native and freestanding code are the same language: no GC, raw
[pointers](pointers.md), `syscall` and [inline asm](inline-assembly.md). They
differ in what is around the program:

- A **native** program is an ordinary process. The C runtime starts it and
  calls `main`; it can [call any C function](calling-c.md), and its exit status
  is what `main` returns.
- A **freestanding** program brings everything itself, starting at `_start`.
  Returning from `_start` exits the process with the returned value (0 for
  `unit`). The [standard library](../reference/standard-library.md) has an
  allocator, output and coroutines written for it.

This mirrors Rust's `std` versus `no_std` / `no_main`; jihoo's hosted profile
sits above both, like a managed language.

Native and freestanding builds target x86_64 and aarch64 Linux: the machine
`jihoo` runs on, or the one given with `--target`. `syscall` takes the raw
syscall number of the target and `asm` its assembly, so code that uses them
directly is not portable; put it in a module with one file per target
([per-target modules](modules.md#per-target-modules)), as the standard library's
`sys` does, or use libc in a native program.

## Entry points

The entry function returns nothing or an `i64`, which becomes the exit status:

| profile      | entry | parameters |
|--------------|-------|------------|
| hosted       | `main` | none |
| native       | `main` | none, or `(argc: i32, argv: **u8)` as in C |
| freestanding | `_start` | none |

## The profile belongs to the program

The profile is a property of the program, not a command line switch: only the
root file (the one given to `jihoo`) may choose it, and it cannot choose two.
Code that uses a feature its profile lacks is rejected at compile time, with an
error that names the feature, so a program never silently changes meaning
between profiles:

```text
error: hello.jh:3:5: `print` needs the VM or the C library, so it is not available in freestanding mode (write to a file descriptor instead, as `io.puts` does)
```

A library module may state what it needs with the same attributes. A library
marked `#![freestanding]` needs neither GC nor libc, so native and freestanding
programs can import it; one marked `#![native]` (such as `libc`) only native
programs, and one marked `#![hosted]` only hosted programs. In a library an
attribute is a requirement, so a library without one is not native-only. A library without an attribute can be imported by any program, and
each use is checked against the importing program's profile.
