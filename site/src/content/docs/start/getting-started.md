---
title: Getting started
description: Install the jihoo toolchain with Nix, then run and build your first programs.
---

## Install

The toolchain is packaged with [Nix](https://nixos.org). It includes the `jihoo`
command, the LLVM backend, the linker and the standard library, so there is
nothing else to set up.

Try it without installing:

```sh
nix run github:jihoo12/jihoo -- run hello.jh
```

Install it into your profile:

```sh
nix profile install github:jihoo12/jihoo
```

Or use it from your own flake:

```nix
{
  inputs.jihoo.url = "github:jihoo12/jihoo";

  outputs = { self, nixpkgs, jihoo }: {
    # jihoo.packages.<system>.default is the toolchain;
    # jihoo.overlays.default adds `pkgs.jihoo`.
  };
}
```

Native builds currently target x86_64 and aarch64 Linux. Hosted programs run
anywhere the VM builds.

### From source

```sh
git clone https://github.com/jihoo12/jihoo
cd jihoo
nix develop                          # Rust, LLVM 21, CMake, lld
cargo build
cmake -S backend-llvm -B backend-llvm/build -G Ninja
cmake --build backend-llvm/build
export JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc
cargo run -- run examples/hello.jh
```

## Your first program

Programs are *native* unless they say otherwise: they start at `main`, and
`jihoo run` compiles them through LLVM and runs them. Strings, lists and
closures are freed by the compiler; there is no garbage collector.

```jihoo title="hello.jh"
fn fib(n: i64) -> i64 {
    if n < 2 {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}

fn main() {
    let name = "jihoo"
    print("Hello, " + name + "!")
    print(fib(20))
}
```

```sh
jihoo run hello.jh
```

## Calling C

A native program is an ordinary program for your operating system, linked with
libc, and able to call any C function. `main` may take C's `argc` and `argv`,
and what it returns is the exit status.

```jihoo title="args.jh"
import libc                            // printf, malloc, qsort, ...

extern fn labs(n: i64) -> i64          // any other C function

fn main(argc: i32, argv: **u8) -> i64 {
    let i: i32 = 0
    while i < argc {
        libc.printf("argv[%d] = %s\n", i, argv[i as i64])
        i = i + 1
    }
    return labs(-7)
}
```

```sh
jihoo run args.jh -- hello; echo $?    # 7
jihoo build args.jh -o args            # or keep the executable
./args hello; echo $?                  # 7
```

C files and libraries can be linked in too: `jihoo build main.jh util.c -lz`.

## A hosted program

Add `#![hosted]` and the program runs on the jihoo VM instead, with a garbage
collector, and needs no LLVM or C compiler. Its tasks take turns on one thread,
so they may share a [`cell`](../../language/cells/); native tasks are threads
and pass values over channels instead.

```jihoo title="vm.jh"
#![hosted]

fn main() {
    print("on the VM")
}
```

## A freestanding program

Add `#![freestanding]` instead and it compiles to a static binary with no GC and
no libc at all. The program starts at `_start`; returning from it exits the
process. The standard library has an allocator and output for such programs.

```jihoo title="bare.jh"
#![freestanding]
import alloc
import io

fn _start() -> i64 {
    let arena = alloc.arena_new(1 << 20)
    let v = alloc.vec_new(i64, &arena)
    let i = 0
    while i < 10 {
        alloc.push(i64, &v, i * i)
        i = i + 1
    }
    io.print_int(alloc.get(i64, &v, 9))   // 81
    return 0
}
```

```sh
jihoo build bare.jh -o bare
./bare
```

The `alloc` and `io` libraries make x86_64 Linux system calls; on aarch64, use a
native program instead.

## Commands

| Command | What it does |
|---|---|
| `jihoo run file.jh [-- args]` | Compile and run a program (a hosted one on the VM); its exit status is what `main` returns. |
| `jihoo build file.jh -o out` | Compile a native program (linked with libc), or a freestanding one to a static binary. |
| `jihoo emit-ir file.jh` | Print the program's [JIR](../../reference/jir/), the IR both backends share. |

`-I dir` adds a directory to search for imported modules, as does `JIHOO_PATH`.
All options and environment variables are in the
[command line reference](../../reference/command-line/).

## Next steps

- The language, one topic at a time, starting with [profiles](../../language/profiles/)
  and [syntax](../../language/syntax/).
- What is built in: [builtins](../../reference/builtins/) and the
  [standard library](../../reference/standard-library/).
- Complete programs in [examples](../../examples/hello/).
- How the compiler fits together: [architecture](../../internals/architecture/).
