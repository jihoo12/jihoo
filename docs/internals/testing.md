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
They compile on the stack the command line gives the compiler
(`jihoo_syntax::on_compiler_stack`, 256 MB reserved), not on a test thread's
2 MB, since the parser, the checker and macro expansion recurse over the tree;
`deep_code_fits_the_compiler_stack` checks that it is enough for code close to
the deepest the parser accepts, in a debug build too.

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
width and signedness, bools and `f64`, casts and every operator, arrays,
structs and tuples nested in each other, copied, updated deep inside, compared
and passed to and returned from functions (sometimes as a part of a value of
the function's own), `if`, `match` on integers and on tuples with nested
patterns and guards, `let` patterns, and loops with `break` and `continue`.
Most also declare a trait with a default, impls of it for random structs and
scalars and a generic impl for pairs, derived `Ord` and `Hash`
(`meta.derive!`), and a function bounded by the trait, and call them as
methods, through the trait and the type, with inferred type arguments, and
through `<` and `hash_of`.
Programs print values as they go and every element of every
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

Programs with lists also run `lists.fuse!` pipelines of random `map` and
`filter` closures, and the same stages as a loop written out. Both profiles
expand the same macro, so comparing them could not catch a wrong expansion;
the loop can, and a program whose two results differ fails its seed.

With `JIHOO_VALGRIND` set to valgrind's path, the native programs with lists
(in `tests/diff-lists` and the fuzzer's list mode) run under it, and a leak or
an invalid access fails them. They are built with `-O0` for it: LLVM may
remove an allocation that is never read, or one freed twice, which is
undefined, and with it the very mistake of the ownership pass that valgrind
is there to show. At `-O2`, a missing `drop` and a value handed over to two
parameters at once both went unnoticed.

A program that does not finish within `JIHOO_TEST_TIMEOUT` seconds (120 by
default), built or run, fails too, so a miscompiled loop that never ends
shows as a failure instead of a test run that never ends.

## C and JIR files

`crates/jihoo-cli/tests/differential.rs` also checks what crosses into C:
`native_print_matches_the_vm` prints integers, text and some ten thousand
floats natively and on the VM and compares them byte for byte, and
`native_structs_cross_into_c` builds `tests/ffi/structs.jh` with
`tests/ffi/structs.c`, which pass structs of every kind the C ABIs treat
differently to each other by value, both ways. It runs for this machine; to
run it for the other target too, give a C compiler and a way to run its
programs:

```sh
JIHOO_CROSS_CC=aarch64-unknown-linux-gnu-gcc JIHOO_CROSS_RUN=qemu-aarch64 \
  JIHOO_LLC=$PWD/backend-llvm/build/jihoo-llc cargo test --test differential structs
```

`crates/jihoo-cli/tests/jir.rs` writes every example and differential program
as JIR, reads it back (`jihoo emit-ir file.jir` must print the same text), and
runs the hosted ones from their JIR, which must do what their source does.

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
