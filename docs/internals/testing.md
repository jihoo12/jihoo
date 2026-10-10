# Testing

```sh
nix develop
cargo test                                   # unit tests, VM side of the differential tests
cmake --build backend-llvm/build
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

## GC stress

`JIHOO_GC_STRESS=1` makes the VM collect before every allocation, which turns a
missing GC root into a panic on the first run instead of a rare heisenbug. Run
the tests this way after any change to the VM that allocates or adds roots.

## Continuous integration

`.github/workflows/test.yml` runs `nix flake check` on x86_64 and aarch64 Linux.
That builds the frontend (running `cargo test`, with the differential tests
against the real backend and C compiler) and then builds and runs examples with
the packaged toolchain: the hosted and native ones everywhere, and the
freestanding ones, which use x86_64 syscalls, on x86_64. Clippy runs with
`-D warnings`.

`.github/workflows/pages.yml` builds this website, which checks that every page
in `docs/` is in the table of contents and that every internal link and anchor
resolves.
