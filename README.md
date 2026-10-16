# jihoo

An easy programming language with three profiles:

- **native** (default): no GC; compiled through LLVM into a normal program for
  the operating system, linked with libc. Lists, strings and closures are freed
  by the compiler; calls C directly (`extern fn`).
- **hosted** (`#![hosted]`): runs on a VM with a garbage collector.
- **freestanding** (`#![freestanding]`): no GC, no libc, raw syscalls. Compiled
  through LLVM into tiny static binaries.

All three share one intermediate representation, JIR. Documentation lives at
**<https://jihoo12.github.io/jihoo/>**, built from [docs/](docs) (contents in
[docs/README.md](docs/README.md)) and [examples/](examples): the language one
topic at a time, a reference (builtins, standard library, command line, and
[JIR](docs/jir.md), the IR format), and how the implementation works.

```jihoo
fn greet(name: str) -> str {
    return "Hello, " + name + "!"
}

fn main() {
    print(greet("jihoo"))
}
```

```jihoo
import libc                  // native, the default: C is right there

fn main(argc: i32, argv: **u8) -> i64 {
    libc.printf("hello, %s\n", argv[0])
    return 0
}
```

```jihoo
#![freestanding]
import sys                   // lib/sys.x86_64.jh or lib/sys.aarch64.jh

fn _start() -> i64 {
    syscall(sys.WRITE, 1, "hi\n", 3)
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

# LLVM backend, which native programs (the default) need
cmake -S backend-llvm -B backend-llvm/build -G Ninja
cmake --build backend-llvm/build
export JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc

cargo run -- run examples/hello.jh
cargo run -- run examples/native.jh -- a b; echo $?
cargo run -- emit-ir examples/fib.jh

# executables: a native program (linked with libc by `cc`) and a freestanding one
mkdir -p out
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
| `lib`                  | jihoo    | the standard library (`prelude`, `strings`, `lists`, `maps`, `iters`, `meta`, `libc`, `alloc`, `io`, `coro`, `sync`, `thread`, `channel`) |
| `docs`                 | Markdown | the documentation, one file per page of the website |
| `site`                 | Astro    | the website ([site/README.md](site/README.md)) |
| `tests/diff`           | jihoo    | programs that must behave the same on the VM and natively |

## Status

v0. In every profile: static types with local inference; `bool`, `i8`…`u64`
and `f32`/`f64`, with exact float builtins (`sqrt`, `floor`, `round`, ...);
structs, tuples, bounds-checked arrays and enums with `match`, `Option` and `Result`;
`while` and `for` loops (over ranges, arrays, lists, strings and
iterators); blocks as expressions; function values; tail calls, which never
grow the stack; generic functions and structs, with inferred type arguments;
traits with impls and bounds (`comptime T: Ord`),
associated types and supertraits, and the standard traits `Ord`, `Hash` and
`Show`, which `meta.derive!` writes, and `Iterator`, which `for` walks;
modules with private items and fields; contracts (`requires`, `ensures`),
checked at run time and tried while compiling with made-up inputs, the
smallest that breaks one becoming a compile error; `nocopy` functions, which
natively must copy no list, string or closure, with whatever they call;
rewrite rules (`replaces`), which the compiler checks against what they
replace before it uses them, and applies only where the code they move has
no effects, or where the checks show that they call the functions they are
given as the pattern does; compile-time evaluation (`const`,
`comptime`) and macros, both run on the VM, with functions on code, `meta` to
look inside code and `type_of` to ask its type.

- **native and hosted:** `str` (bytes, slices, ordering), lists `[T]` with
  `push` and `pop`, `print` and `read_line`, `ref` for recursive data, `cell`
  for shared state, closures, tasks with channels and `select`; the program's
  arguments (`args()`), whole files (`read_file`, `write_file`) and floats in
  text (`parse_f64`); the `strings`, `lists`, `maps`, `iters` and `files`
  libraries. Natively these are freed by the compiler (copies share memory, counted, until one is written) and
  tasks are threads, which share no cells; on the VM a GC frees them and tasks
  take turns on one thread.
- **native and freestanding:** pointers, `syscall` and inline asm, for x86_64
  and aarch64 Linux (`--target` builds for either, and modules can have one
  file per target); closures passed to `comptime` parameters; functions C can
  call (`export fn`); atomics; the `alloc`, `io`, `sys` and `sync` (futex
  locks) libraries.
- **native:** `print`; C functions through `extern fn` and the `libc` module,
  structs passed to C by value, C files and libraries linked in; threads on
  every core (`go`, and the `thread` library) and channels and `Select`
  between them (the `channel` library); the `coro` library.

See the [examples](examples) and the [roadmap](docs/internals/roadmap.md).

```sh
# differential tests: VM vs native
JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc cargo test --test differential
```

## License

Apache-2.0
