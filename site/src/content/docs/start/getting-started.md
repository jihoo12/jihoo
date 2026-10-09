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

Programs are *hosted* unless they say otherwise: they start at `main`, run on
the VM, and have a garbage collector and `str`.

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

## A freestanding program

Add `#![freestanding]` and the same language compiles to a native binary with no
GC and no libc. The program starts at `_start`; returning from it exits the
process. The standard library has an allocator and output for such programs.

```jihoo title="native.jh"
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
jihoo build native.jh -o native
./native
```

## Commands

| Command | What it does |
|---|---|
| `jihoo run file.jh` | Run a hosted program on the VM. |
| `jihoo build file.jh -o out` | Compile a freestanding program to a static binary. |
| `jihoo emit-ir file.jh` | Print the program's [JIR](../../reference/jir/), the IR both backends share. |

`-I dir` adds a directory to search for imported modules, as does `JIHOO_PATH`.

## Next steps

- The language, one topic at a time, starting with [profiles](../../language/profiles/).
- Complete programs in [examples](../../examples/hello/).
- How the compiler fits together: [architecture](../../internals/architecture/).
