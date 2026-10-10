# Testing

```sh
nix develop
cargo test                                   # unit tests, VM side of the differential tests
cmake --build backend-llvm/build
ctest --test-dir backend-llvm/build         # the backend on its own, with hand-written JIR
JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc cargo test --test differential
JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc JIHOO_FUZZ_SEEDS=2000 cargo test --release --test fuzz
JIHOO_GC_STRESS=1 cargo test                 # with a collection at every allocation
JIHOO_VM_CHECK=1 cargo test                  # checking every in-place update against the heap
nix flake check                              # everything, with the packaged toolchain
```

## Unit tests

Each crate has its own tests (`cargo test`). Most language tests are in
`crates/jihoo-sema/src/tests.rs`: small programs that must compile to the
expected JIR, run to the expected result on the VM, or fail with the expected
error message.

## Differential tests

`tests/diff/` holds core-only programs (no `print`, no `syscall`, no profile
attribute) that each define `fn entry() -> i64` and may print values with
`out(x: i64)`. `crates/jihoo-cli/tests/differential.rs` wraps every one of them
three times — as a hosted program run on the VM, and as a native and a
freestanding program compiled with LLVM — and checks that all three print the
same lines and exit with the same status (`entry() % 256`). The wrapper
(`crates/jihoo-cli/tests/common/mod.rs`) defines `out` with each profile's way
of printing. This keeps the two backends honest about the semantics of JIR.

The compiled halves need `jihoo-llc`, `ld.lld` and a C compiler; without
`JIHOO_LLC`, only the VM results are checked. To add a test, add a `.jh` file
with an `entry` function to `tests/diff/`.

## Fuzzing

`crates/jihoo-cli/tests/fuzz.rs` makes random programs of the same kind and
checks them the same way: the VM against the freestanding build, and every
fourth against the native one too. A seed makes one program: integers of every
width and signedness, bools and `f64`, casts and every operator, arrays and
structs nested in each other, copied, updated deep inside, compared and passed
to and returned from functions, `if`, `match` and loops with `break` and
`continue`. Programs print values as they go and every element of every
variable at the end. They cannot trap (divisors are made odd, indexes are taken
modulo the length), so every difference is a bug in the VM or the backend.

```sh
cargo test --test fuzz                                   # 64 seeds
JIHOO_FUZZ_SEEDS=5000 cargo test --release --test fuzz   # more
JIHOO_FUZZ_SEED=1234 cargo test --test fuzz              # one seed
JIHOO_FUZZ_SHOW=1 JIHOO_FUZZ_SEED=1234 cargo test --test fuzz -- --nocapture
```

(with `JIHOO_LLC` set; without it, the programs only run on the VM). A failure
names the seed, the first output line that differs, and a file with the
program. The default 64 seeds are a quick check; some bugs need more: the old
VM bug where a copy saw later updates of the original was found by about 1 in
6 seeds, while comparing `u64`s as signed takes about 1 in 80, since only
values above `i64::MAX` show it. Run a few thousand seeds after changing the
VM or the backend.

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

## VM checks

`JIHOO_VM_CHECK=1` makes the VM count, before every in-place update, the live
references to the object it updates, and panic unless there is only the one
being replaced ([VM and GC](vm.md#value-semantics-and-in-place-updates)). The
VM's unit tests always run this way. Run the fuzzer with it after touching how
the VM shares objects: it finds a sharing bug from the VM's run alone, before
any output differs. With the bug of 18e4a33 put back, 11 of the 64 default
seeds failed this way, with no backend at all.

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
