# Differential tests

Each `*.jh` file here is a core-only program (no `print`, no `syscall`, no
`#![...]` attribute) that defines `fn entry() -> i64`.

`crates/jihoo-cli/tests/differential.rs` wraps it three times — as a hosted program run on
the VM, and as a native and a freestanding program compiled with LLVM — and checks that all exit
with the same status (`entry() % 256`).

The compiled halves need `jihoo-llc`, `ld.lld` and a C compiler (`cc`); inside
`nix develop`, run:

```sh
cmake --build backend-llvm/build
JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc cargo test --test differential
```

Without `JIHOO_LLC`, the test only checks the VM results.
