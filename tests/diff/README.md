# Differential tests

Each `*.jh` file here is a core-only program (no `print`, no `syscall`, no
`#![...]` attribute) that defines `fn entry() -> i64` and may call
`out(x: i64)` to print a value.

`crates/jihoo-cli/tests/differential.rs` wraps it three times — as a hosted
program run on the VM, and as a native and a freestanding program compiled with
LLVM — and checks that all three print the same lines with `out` and exit with
the same status (`entry() % 256`). The wrapper defines `out` for each profile
(`print`, `printf`, `io.print_int`), so a program must not define `out`, `main`,
`_start`, `io` or `libc` itself.

The compiled halves need `jihoo-llc`, `ld.lld` and a C compiler (`cc`); inside
`nix develop`, run:

```sh
cmake --build backend-llvm/build
JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc cargo test --test differential
```

Without `JIHOO_LLC`, the test only checks the VM results.

`crates/jihoo-cli/tests/fuzz.rs` does the same with random programs; see
[Testing](../../docs/internals/testing.md#fuzzing).
