# Testing

```sh
nix develop
cargo test                                   # unit tests, VM side of the differential tests
cmake --build backend-llvm/build
ctest --test-dir backend-llvm/build         # the backend on its own, with hand-written JIR
JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc cargo test --test differential
JIHOO_GC_STRESS=1 cargo test                 # with a collection at every allocation
nix flake check                              # everything, with the packaged toolchain
```

## Unit tests

Each crate has its own tests (`cargo test`). Most language tests are in
`crates/jihoo-sema/src/tests.rs`: small programs that must compile to the
expected JIR, run to the expected result on the VM, or fail with the expected
error message.

## Differential tests

`tests/diff/` holds core-only programs (no `print`, no `syscall`, no profile
attribute) that each define `fn entry() -> i64`.
`crates/jihoo-cli/tests/differential.rs` wraps every one of them three times —
as a hosted program run on the VM, and as a native and a freestanding program
compiled with LLVM — and checks that all three exit with the same status
(`entry() % 256`). This keeps the two backends honest about the semantics of
JIR.

The compiled halves need `jihoo-llc`, `ld.lld` and a C compiler; without
`JIHOO_LLC`, only the VM results are checked. To add a test, add a `.jh` file
with an `entry` function to `tests/diff/`.

## Backend tests

`backend-llvm/tests/` holds hand-written JIR modules that `ctest` compiles
with `jihoo-llc` alone, without the frontend. Each starts with an `; expect:`
line: `exit N` (link with `ld.lld` and run, which should exit with `N`), `trap`
(it should die of a failed bounds or division check) or `error TEXT`
(`jihoo-llc` should refuse it with a message containing `TEXT`). They pin down
what the backend does with what LLVM leaves undefined (wrapping, `MIN / -1`,
shift amounts, float-to-int casts) and that it refuses broken modules, including
ones of another [JIR version](../jir.md#versions). To add a test, add a `.jir`
file there and rerun `cmake` so it is picked up.

## JIR versions

`crates/jihoo-ir/tests/version.rs` checks that the Rust frontend, the backend
and `docs/jir.md` name the same JIR version, and keeps a fingerprint of
`docs/jir.md`: after editing it, the test fails until you decide whether the
change needs a new version ([Versions](../jir.md#versions)) and update the
fingerprint it prints.

## GC stress

`JIHOO_GC_STRESS=1` makes the VM collect before every allocation, which turns a
missing GC root into a panic on the first run instead of a rare heisenbug. Run
the tests this way after any change to the VM that allocates or adds roots.

## Continuous integration

`.github/workflows/test.yml` runs `nix flake check` on x86_64 and aarch64 Linux.
That builds the backend (running its `ctest`) and the frontend (running `cargo test`, with the differential tests
against the real backend and C compiler) and then builds and runs examples with
the packaged toolchain: all of them on both machines, except the inline
assembly example, which is x86_64 assembly. Clippy runs with
`-D warnings`.

`.github/workflows/pages.yml` builds this website, which checks that every page
in `docs/` is in the table of contents and that every internal link and anchor
resolves.
