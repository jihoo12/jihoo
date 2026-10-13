# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

jihoo is a programming language with three profiles that share one IR (JIR):
**native** (the default: LLVM + libc, no GC, the compiler frees lists/strings/closures),
**hosted** (`#![hosted]`: register VM with a GC), and **freestanding**
(`#![freestanding]`: LLVM, no libc, raw syscalls, static ELF). Targets are
x86_64 and aarch64 Linux.

## Environment and build

Everything comes from `flake.nix` (Rust, LLVM 21, CMake, Ninja, lld). Work inside
`nix develop` (or `direnv allow`); for tools not in the dev shell use `nix-shell -p <pkg>`.

```sh
cargo build                                                   # frontend + VM
cmake -S backend-llvm -B backend-llvm/build -G Ninja          # once (rerun to pick up new backend tests)
cmake --build backend-llvm/build                              # jihoo-llc
export JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc            # needed for native/freestanding programs

cargo run -- run examples/hello.jh                            # compiles + runs (native by default)
cargo run -- run examples/native.jh -- a b                    # args after --
cargo run -- emit-ir examples/fib.jh                          # print JIR
cargo run -- build examples/freestanding.jh -o out/hello
nix build                                                     # whole packaged toolchain → ./result/bin/jihoo
```

`import` searches the importing file's directory, `-I` dirs, `JIHOO_PATH`, then the checkout's `lib/`.

## Tests

```sh
cargo test                                         # all Rust tests (VM side only without JIHOO_LLC)
cargo test -p jihoo-sema <name>                    # one sema test (most language tests live in crates/jihoo-sema/src/tests.rs)
ctest --test-dir backend-llvm/build                # backend alone, on hand-written JIR
ctest --test-dir backend-llvm/build -R <name>      # one backend test
JIHOO_LLC=... cargo test --test differential       # VM vs native vs freestanding
JIHOO_LLC=... cargo test --test differential <name>
JIHOO_LLC=... JIHOO_FUZZ_SEEDS=2000 cargo test --release --test fuzz
JIHOO_FUZZ_SEED=1234 JIHOO_FUZZ_SHOW=1 cargo test --test fuzz -- --nocapture   # reproduce one seed
JIHOO_GC_STRESS=1 cargo test                       # collect at every allocation (after VM alloc/root changes)
JIHOO_VM_CHECK=1 cargo test                        # check in-place updates aren't shared
JIHOO_VALGRIND=$(which valgrind) ...               # leaks/invalid accesses fail native list tests/fuzz seeds (built -O0)
cargo clippy --all-targets --locked -- -D warnings # CI runs this
nix flake check                                    # everything CI runs
```

Run a few thousand fuzz seeds (with `JIHOO_LLC`) after changing the VM, the
ownership pass or the backend. Details: `docs/internals/testing.md`.

Test kinds to add to:
- `tests/diff/*.jh`: core-only programs (no `print`, `syscall` or `#![...]`) with
  `fn entry() -> i64`, printing via `out(x: i64)`; must not define `out`, `main`,
  `_start`, `io` or `libc`. Run on all three profiles, outputs and exit status must match.
- `tests/diff-lists/*.jh`: same, but use lists, so hosted vs native only.
- `tests/threads/`, `tests/ffi/`: driven by specific tests in `crates/jihoo-cli/tests/differential.rs`.
- `backend-llvm/tests/*.jir`: first line `; expect: exit N | trap | object T | error TEXT`.
- Language tests that are about semantics compile their programs as hosted explicitly.

## Architecture

```
source.jh → jihoo-syntax (load+parse) → jihoo-sema (typecheck+lower) → JIR → jihoo-ir verify
          → hosted: jihoo-vm
          → native/freestanding: JIR text → jihoo-llc (C++/LLVM) → .o → cc+libc | ld.lld
```

- **Rust and C++ only talk through `.jir` text files.** The Rust build never links
  LLVM. JIR is a versioned contract documented in `docs/jir.md`; Rust types in
  `crates/jihoo-ir/src/lib.rs`, C++ types in `backend-llvm/src/jir.h`, readers in
  `crates/jihoo-ir/src/parse.rs` and `backend-llvm/src/jir_parser.cpp`.
- **Changing JIR**: bump `JIR_VERSION` (Rust), `jir::kVersion` (C++) and the title of
  `docs/jir.md` together, and add a row to its version table, for any change that lets
  the sides disagree. `crates/jihoo-ir/tests/version.rs` fingerprints `docs/jir.md`: any
  edit fails it until you update the printed fingerprint.
- **Loader** (`crates/jihoo-syntax/src/loader.rs`) is the only place the target matters:
  `import a` prefers `a.<target>.jh` over `a.jh` (e.g. `lib/sys.x86_64.jh`). It also adds
  `lib/channel.jh` to native programs that use `chan`/`select`.
- **Sema** checks and lowers in one pass (Zig-Sema style), with module items analyzed
  lazily and memoized in `env.rs` (so order/cyclic imports work). Generics are
  monomorphized (`name.N`); closures are lifted to `fn.N`; `const`/`comptime`/macros run
  **on the VM** even for native/freestanding programs (so they can't use pointers,
  syscalls, asm or C). Macros are hygienic via marks (`hygiene.rs`). See `docs/internals/compiler.md`.
- **Shared rules**: operator typing lives once in `crates/jihoo-ir/src/types.rs`, used by
  both sema and the verifier; C layouts (`layout.rs`) are recorded in JIR and rechecked by
  the backend (`cabi.cpp`).
- **Native memory**: `crates/jihoo-ir/src/ownership.rs` inserts `clone`/`drop` from liveness
  (moves, borrows, views). On the VM the GC and copy-on-write give the same value semantics;
  the differential tests and fuzzer enforce that the two agree.
- Native runtime pieces (channels, select, threads) are written in jihoo in `lib/` and called
  by the backend via exported `jihoo_*` functions.

## Docs and website

`docs/` is the website's source (one Markdown file per page) along with `examples/*.jh`
(header comment becomes the page text). Every page must be listed in `docs/README.md`;
the site build (`site/`, Astro: `nix develop .#site; cd site; npm ci; npm run build`)
fails on missing TOC entries or broken links/anchors. When a feature changes, update the
relevant `docs/` page, README's Status section and `docs/internals/roadmap.md` as appropriate.
Docs and comments are written in English.

## Commits

Commit messages: short imperative subject (e.g. "Closures in native programs"), then
prose paragraphs explaining what changed and why.
