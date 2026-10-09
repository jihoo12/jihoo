# jihoo

An easy programming language with two faces:

- **hosted** (default): runs on a VM with a garbage collector.
- **freestanding**: no GC, no std, raw syscalls. Compiled through LLVM into tiny
  static binaries.

Both share one intermediate representation, JIR. See [docs/design.md](docs/design.md)
for the architecture and [docs/jir.md](docs/jir.md) for the IR format.

```jihoo
fn greet(name: str) -> str {
    return "Hello, " + name + "!"
}

fn main() {
    print(greet("jihoo"))
}
```

```jihoo
#![freestanding]

fn _start() -> i64 {
    syscall(1, 1, "hi\n", 3) // write(1, "hi\n", 3)
    return 0                 // exit(0)
}
```

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

# native build of a freestanding program
mkdir -p out
JIHOO_LLC=backend-llvm/build/jihoo-llc cargo run -- build examples/freestanding.jh -o out/hello
./out/hello; echo $?
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
| `tests/diff`           | jihoo    | programs that must behave the same on the VM and natively |

## Status

v0: static types with local inference; `bool`, `i8`…`u64` with arithmetic and
bitwise operators, structs and
bounds-checked arrays (both profiles), `size_of`/`align_of`, `str` (hosted),
pointers with load/store, `&` and inline asm (freestanding);
functions, `if`/`while`, `&&`/`||`, `as` casts, `print` (hosted) and `syscall`
(freestanding, x86_64 and aarch64 Linux); compile-time evaluation with `const`
and `comptime`, run on the VM; generic functions and structs through
compile-time parameters;
macros that run at compile time and return code.
See [examples/pointers.jh](examples/pointers.jh), [examples/asm.jh](examples/asm.jh),
[examples/alloc.jh](examples/alloc.jh) (an allocator and `Vec(T)`),
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
