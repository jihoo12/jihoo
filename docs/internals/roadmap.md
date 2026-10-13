# Roadmap

## Done

1. Type checker (static types with inference) and typed JIR.
2. Differential tests across both backends.
3. Sized integers, pointers with loads and stores, structs, arrays, `size_of`,
   inline asm.
4. `comptime` on the VM, generic functions.
5. Expression macros, generic structs, bitwise operators, modules and an
   `alloc` library, `pub`, statement and item macros, `unique`/`ident`.
6. GC hardening (one root set, generation-checked references, stress mode);
   function values.
7. Sum types and `match`; `ref T` for recursive data; nested patterns, guards,
   `|` patterns and `match` expressions.
8. Closures, captured by value and lifted into functions.
9. Tasks and channels on the VM: one OS thread, a deterministic scheduler;
   `select`.
10. Comptime closures: closures passed to `comptime` parameters, one instance
    per closure, captured values as hidden arguments.
11. Coroutines for freestanding code, as a library on top of function values
    and `asm`.
12. A native profile: LLVM-compiled programs for an OS, linked with libc,
    calling C through `extern fn`.
13. Floats: `f32` and `f64`, identical on the VM and natively, and across into
    C.
14. Targets: x86_64 and aarch64 Linux with `--target`, recorded in JIR;
    per-target modules (`sys.x86_64.jh`); the standard library on both, with
    coroutines on libc's contexts.
15. Macro hygiene: a template's locals are its own, and its other names
    resolve where the macro is defined.
16. Field visibility: `pub` fields, also through macros.
17. Float builtins, the same in every profile: `sqrt`, `floor`, `ceil`,
    `trunc`, `round`, `abs`, `copysign`, and a float's bits (`to_bits`,
    `from_bits`).
18. `print` in native programs, with the VM's output to the byte.
19. Structs by value to and from C, lowered per target (x86_64 System V,
    AAPCS64), and `export fn` for functions C calls.
20. A Rust-side JIR reader: `jihoo run file.jir`, `jihoo build file.jir`.
21. A hosted standard library: strings as bytes with `len`, `s[i]`, slices
    and ordering; lists `[T]` with `push` and `pop`, in place when unshared;
    `read_line`; the prelude (`Option`, `Result`); `lib/strings.jh` and
    `lib/lists.jh`. JIR version 5.
22. `trap()` in every profile (JIR version 6), and a checked `alloc`: bounds
    checks on `Vec`, traps on misuse, overflow-safe sizes, private fields,
    `arena_free`.
23. Threads and atomics (JIR version 7): `atomic_load`, `atomic_store`,
    `atomic_swap`, `atomic_add` and the other read-modify-writes,
    `atomic_cas` and `fence`, sequentially consistent; `lib/sync.jh`, futex
    locks (`Mutex`, `Cond`, `Group`) for native and freestanding code;
    `lib/thread.jh`, POSIX threads for native code; `lib/channel.jh`,
    channels between threads with `close`, and `Select` over them (`send`
    and `recv` are now names a module may take); `go f(x)` in native code,
    starting a detached thread (JIR version 8); `channel.wait_ms`, a Select
    with a timeout, and `thread.now_ns`.
24. Memory safety without a GC or a borrow checker: paths
    (`path(Root).a[_]`, `root[p]`, `path_index`), references that hold
    indices instead of addresses; lists in native programs, freed by an
    ownership pass over JIR that moves values at their last use, clones
    them where the original is still used, reads parts as views, and makes
    consuming versions of functions for `xs = f(xs)` (JIR version 9:
    `clone` and `drop`). Checked by hosted-versus-native tests and a list
    mode of the fuzzer, under valgrind.
25. `str`, capturing closures, `ref`, `cell`, tasks, channels and `select` in
    native programs, then native as the default profile: a program without an
    attribute is native, `#![hosted]` asks for the VM, and `jihoo run`
    compiles and runs native and freestanding programs. Native tasks are
    threads and share no cells; `comptime` and macros still run on the VM.
26. `for` loops in every profile: ranges `lo..hi`, and lists, arrays and
    strings, with the index (`for i, x in xs`). The loop walks the value it
    started with; natively that is a view of the list, which the ownership
    pass now also makes of any copy whose original is still used, unless one
    of them changes. The libraries use `for` where it fits.
27. Tuples in every profile: `(A, B)` types, `(a, b)`, `t.0`, tuple patterns
    and `let (a, b) = ...`. A tuple type is a struct named by its spelling,
    so JIR and both backends are unchanged.
28. Inferred type arguments: a call to a generic function may leave out its
    `comptime T: type` arguments, which are matched from the other arguments
    (`lists.sort_by(xs, fn(a, b) { return a < b })`); literals and anonymous
    functions take what those decide. The first step of
    [traits](traits.md).
29. Traits: `trait`, `impl` and bounds (`comptime T: Ord + Hash`), with
    default bodies, method calls (`x.f()`), `Trait.f(x)` and `Type.f()`. An
    impl's functions are compiled per type like generic functions and called
    directly, so JIR is unchanged; impls live in the trait's or the type's
    module, and every one is checked. The prelude has `Ord`, for integers
    and `str`, and `lists.sort(xs)` sorts by it.
    Generic impls (`impl(T: Ord) Ord for Option(T)`), matched against a
    type's shape and applying when their parameters' bounds hold; the prelude
    orders options, lists and tuples.
    The standard traits: `Ord` (floats in total order), `Hash` with
    `hash_of`, and `Show`, which `print` uses; `lib/maps.jh`, hash maps
    `Map(K, V)` for keys with `Hash` and `==`.
    `<`, `<=`, `>` and `>=` on any type with an `Ord`, through `compare`.

## Next

- **Code as data** ([design](code-as-data.md)). Done: functions on code,
  which macros build their code with, macros taking any number of code
  arguments, and `meta.node` to look inside code (JIR 14: `codetext`), with
  which `examples/pipe.jh` fuses `map`, `filter` and sums into one loop, three
  times as fast as `lists`; blocks as expressions, so that a fused pipeline is
  a value; `type_of` (JIR 15), so that it works for any element type;
  `lists.fuse!` in the library, with `sum`, `count` and `collect`; and
  `#![comptime]` modules, so `meta` costs programs nothing and works in
  freestanding ones; random pipelines in the fuzzer, each checked against
  the same stages written out as a loop.
- **Traits** ([design](traits.md)), before v1: deriving `Show`, `Hash` and
  `Ord` with macros.
- **Memory:** a report of where native code copies lists (`jihoo build
  --cost`), which needs source positions in JIR; moving a field out of a
  value at its last use instead of copying it.

- **Floats:** `fma`, which IEEE also defines exactly, but freestanding code on
  x86_64 would need an exact software version.
- **Native code:** structs by value through function values handed to C
  (callbacks take scalars only); `print` in freestanding programs.
- **Concurrency:** weaker memory orders (acquire/release) for code that needs
  the speed; threads in freestanding programs, over `clone`; tasks on more
  than one core in the VM, which needs a GC that threads can share.
- **Hosted library:** files; parsing floats.

## Library layers

The standard library is meant to grow into three layers:

```text
std    hosted only: GC types, I/O, collections
alloc  any compiled profile, given an allocator: Vec(T), strings
core   everywhere: integers, pointers, slices, Option
```

Today `std` is the prelude, `strings` and `lists`, on the VM's `str` and `[T]`,
and `alloc` is a bump arena with `Vec(T)` (`lib/alloc.jh`); the goal is for
freestanding code to bring its own allocator, written in jihoo on top of
`syscall`, and use the same collections with it.
