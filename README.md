# jihoo

An easy programming language with three profiles:

- **hosted** (default): runs on a VM with a garbage collector.
- **native** (`#![native]`): no GC; compiled through LLVM into a normal program
  for the operating system, linked with libc. Calls C directly (`extern fn`).
- **freestanding** (`#![freestanding]`): no GC, no libc, raw syscalls. Compiled
  through LLVM into tiny static binaries.

All three share one intermediate representation, JIR. Documentation lives at
**<https://jihoo12.github.io/jihoo/>** (built from [docs/](docs) and
[examples/](examples)); see [docs/design.md](docs/design.md) for the language and
architecture and [docs/jir.md](docs/jir.md) for the IR format.

```jihoo
fn greet(name: str) -> str {
    return "Hello, " + name + "!"
}

fn main() {
    print(greet("jihoo"))
}
```

```jihoo
#![native]
import libc

fn main(argc: i32, argv: **u8) -> i64 {
    libc.printf("hello, %s\n", argv[0])
    return 0
}
```

```jihoo
#![freestanding]

fn _start() -> i64 {
    syscall(1, 1, "hi\n", 3) // write(1, "hi\n", 3)
    return 0                 // exit(0)
}
```

## Install

The whole toolchain (VM, LLVM backend, linker, standard library) is a Nix flake:

```sh
nix run github:jihoo12/jihoo -- run hello.jh       # try it
nix profile install github:jihoo12/jihoo           # install `jihoo`
```

Flake outputs: `packages.<system>.default` (the toolchain, also `jihoo`),
`jihoo-frontend` (VM only), `jihoo-llc`, `overlays.default`, `apps.default`, and
`checks` (`nix flake check` builds and runs examples with the packaged toolchain).
Building the frontend runs the test suite, including the VM-vs-native
differential tests.

## Getting started

Everything comes from `flake.nix` (Rust toolchain, LLVM 21, CMake, lld).

```sh
nix develop                 # or `direnv allow`

# frontend + VM
cargo build
cargo test
cargo run -- run examples/hello.jh
cargo run -- emit-ir examples/fib.jh

# LLVM backend
cmake -S backend-llvm -B backend-llvm/build -G Ninja
cmake --build backend-llvm/build

# native builds: a native program (linked with libc by `cc`) and a freestanding one
mkdir -p out
export JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc
cargo run -- build examples/native.jh -o out/native && ./out/native a b; echo $?
cargo run -- build examples/freestanding.jh -o out/hello && ./out/hello; echo $?
```

Or build the whole toolchain (`jihoo`, `jihoo-llc`, `ld.lld`) with Nix:

```sh
nix build
./result/bin/jihoo run examples/hello.jh
```

## Layout

| path                   | language | what |
|------------------------|----------|------|
| `crates/jihoo-syntax`  | Rust     | lexer, parser, AST |
| `crates/jihoo-sema`    | Rust     | type checking + lowering AST → JIR |
| `crates/jihoo-ir`      | Rust     | JIR data types, text printer, verifier |
| `crates/jihoo-vm`      | Rust     | register VM and GC heap |
| `crates/jihoo-cli`     | Rust     | the `jihoo` command |
| `backend-llvm`         | C++      | `jihoo-llc`: JIR text → LLVM → object file |
| `lib`                  | jihoo    | the standard library (`import libc`, `import alloc`, `import io`) |
| `site`                 | Astro    | the website ([site/README.md](site/README.md)) |
| `tests/diff`           | jihoo    | programs that must behave the same on the VM and natively |

## Status

v0: static types with local inference; `bool`, `i8`…`u64` and `f32`/`f64` with arithmetic and
bitwise operators, structs and
bounds-checked arrays (every profile), `size_of`/`align_of`, `str` (hosted),
pointers with load/store, `&` and inline asm (native and freestanding);
C functions through `extern fn`, C files and libraries linked in (native);
functions, `if`/`while`, `&&`/`||`, `as` casts, `print` (hosted) and `syscall`
(native and freestanding, x86_64 and aarch64 Linux); compile-time evaluation with `const`
and `comptime`, run on the VM; generic functions and structs through
compile-time parameters;
macros that run at compile time and return code.
modules with `import`, and a small standard library in [lib/](lib)
(`libc`; `alloc`: an arena allocator and `Vec(T)`; `io`). See
[examples/native.jh](examples/native.jh), [examples/floats.jh](examples/floats.jh),
[examples/pointers.jh](examples/pointers.jh), [examples/asm.jh](examples/asm.jh),
[examples/arena.jh](examples/arena.jh),
[examples/comptime.jh](examples/comptime.jh),
[examples/generics.jh](examples/generics.jh),
[examples/macros.jh](examples/macros.jh) and the roadmap in
[docs/design.md](docs/design.md).

```sh
# differential tests: VM vs native
JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc cargo test --test differential
```

## License

Apache-2.0
