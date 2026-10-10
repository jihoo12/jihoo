# Profiles

A program chooses where it runs with a *profile*, set by an attribute at the top
of its root file. There are three, one per place a program can run: on the jihoo
VM, on an operating system, or on bare metal.

```jihoo
// hosted: no attribute
fn main() {
    print("on the VM")
}
```

```jihoo
#![native]
import libc

fn main() {
    libc.printf("an ordinary program, linked with libc\n")
}
```

```jihoo
#![freestanding]

fn _start() -> i64 {
    syscall(60, 7)   // exit(7) on x86_64 Linux; no libc at all
    return 0
}
```

## What each profile has

|                 | hosted (default)        | native (`#![native]`)          | freestanding (`#![freestanding]`) |
|-----------------|-------------------------|--------------------------------|-----------------------------------|
| runs on         | the jihoo VM            | an OS, as a normal program     | anything: no OS libraries at all  |
| command         | `jihoo run`             | `jihoo build`                  | `jihoo build`                     |
| backend         | VM                      | LLVM, linked by `cc` with libc | LLVM, static binary via `ld.lld`  |
| memory          | GC, managed by the VM   | manual (`malloc`, or `alloc`)  | manual (`alloc` over `mmap`)      |
| string literals | `str`                   | `*u8`, NUL-terminated          | `*u8`, NUL-terminated             |
| output          | `print`                 | C (`libc.printf`)              | `syscall` (`io.puts`)             |
| C functions     | no                      | `extern fn`, C files, `-l`     | no                                |
| pointers        | no                      | `*T`, `&x`, `*p`, `p[i]`       | `*T`, `&x`, `*p`, `p[i]`          |
| syscalls / asm  | no                      | yes                            | yes                               |
| `ref`, `cell`, tasks, capturing closures | yes | no                  | no                                |
| entry point     | `fn main()`             | `fn main()` or `fn main(argc: i32, argv: **u8)` | `fn _start()`    |

Everything else — integers and floats, structs, arrays, enums, `match`, function
values, generics, `comptime` and macros — is the same in every profile.

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

Native and freestanding builds target x86_64 and aarch64 Linux. `syscall` takes
the raw syscall number of the target, so a program that calls it directly (and
the `alloc`, `io` and `coro` libraries, written for x86_64) is not portable
between the two; native programs can use libc instead.

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
error: hello.jh:3:5: `print` needs std and is not available in freestanding mode
```

A library module may state what it needs with the same attributes. A library
marked `#![freestanding]` needs neither GC nor libc, so native and freestanding
programs can import it; one marked `#![native]` (such as `libc`) only native
programs. A library without an attribute can be imported by any program, and
each use is checked against the importing program's profile.
