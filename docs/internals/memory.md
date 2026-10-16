# Design: native memory

Status: steps 1 to 4 are done: [counted buffers](#1-counted-buffers-done),
[uniqueness checks that fold away](#2-uniqueness-checks-that-fold-away-done),
[the ownership checker](#3-ownership-the-compiler-checks-done) and
[reuse for `map` and `filter`](#4-reuse-done-for-map-and-filter). Programs
that needed copies are 25 to 100 times faster, and nothing else got slower
([measurements](#measurements)). Step 5 is left. Nothing here changes
the language: values keep their meaning, and hosted programs do not change.

## Why

Native programs have no GC. The ownership pass
(`crates/jihoo-ir/src/ownership.rs`) decides from the program alone where
each list, string and closure moves, where it is only looked at (a view), and
where it must be copied, and the copy it inserts is a deep one: all of the
value, every list inside it included. That design has served well: most
programs copy nothing, nothing is counted at run time, and `nocopy` and
`jihoo build --cost` can say exactly what a function copies. Writing real
programs has shown its two weaknesses.

### A copy costs as much as the value

When the pass cannot prove that a value is not used again, it copies it, and
the copy takes time in proportion to the whole value. A small change to a
program can therefore change its complexity:

| program | what happened | native before | after a fix in the pass |
|---------|---------------|---------------|-------------------------|
| `examples/json.jh`, 0.9 MB | each `match` arm copied the rest of the input | 19 s | 0.03 s |
| counting 300,000 words with `maps` | `m = maps.set(m, w, n)` copied the map, because `w` was still in a list | 129 s | 0.10 s |

Both were fixed by teaching the pass one more case (each arm of a `match`
takes apart what it matches; a call hands over what it can). Each fix is
sound and worth keeping, but the next program will find the next case: the
pass is a sequence of special cases, and the fallback when none applies is
the most expensive operation there is. The VM has no such cliffs: copies share
until one is written (copy on write), so the JSON example took 0.8 s there
before the fix, slow as an interpreter is but in proportion to the input.

Some programs need the copies, and no analysis removes them. Keeping old
versions of a value is one; sharing a list among many records is another:

| program | native (deep copies) | VM (shared until written) |
|---------|----------------------|---------------------------|
| `snapshot`: an undo history of a 200 x 200 grid, 2,000 edits | 0.53 s, 643 MB | 0.02 s, 17 MB |
| `shared_tags`: 20,000 records holding the same 100 strings | 0.13 s, 112 MB | 0.01 s, 6 MB |
| `tree_edit`: 200 versions of a tree of 37,000 nodes, one leaf changed in each | 0.24 s, 252 MB | 0.03 s, 9 MB |

Here native code, which should be the fast profile, is 10 to 30 times slower
than the interpreter and uses 20 to 75 times the memory.

### Safety rests on one analysis

Native memory is safe if the ownership pass is right: every value is freed
exactly once, after its last use, and no view outlives its root. When the
pass is wrong, the result is not a compile error but a leak, a double free or
a use after free in the program. The tests catch such mistakes only by
running programs under valgrind: the leak in `v = items[k]` (fixed with the
JSON example) had been there since lists became native, and showed up only
when a program happened to do that. The more cases the pass learns to avoid
copies, the more there is to get wrong, and the first problem pushes it to
learn more.

## What other languages do

- **Rust**: ownership and borrows are checked, and copies (`clone`) are
  written by hand. Nothing is counted, and nothing is copied unless asked,
  but programs carry lifetimes, and sharing needs `Rc`/`Arc` by hand.
- **Swift**: arrays and strings are counted buffers, copied on write when
  shared. Copies cost O(1); every write checks that the buffer is unique,
  and the optimizer hoists those checks out of loops (its "COW array"
  optimizations) and removes counting it can prove unneeded.
- **Lean 4 and Koka (Perceus)**: everything is counted, and the compiler
  removes most of the counting: it infers which parameters are only borrowed,
  frees each value at its last use, and when a unique value dies where a
  value of the same size is built, it reuses the memory ("functional but in
  place"). Values crossing threads are marked, and only their counts are
  atomic.
- **Hylo (Val)** and **Mojo**: value semantics without references; parameters
  are passed as borrows (`let`), as in-place mutations (`inout`) or as moves
  (`sink`), and a checker enforces exclusivity. Copies are explicit or
  inferred at last use.

jihoo's semantics are Hylo's (values, no references into them, paths instead),
and its inference of moves and consuming versions is Perceus-like. What it
lacks is the cheap fallback that Swift and Perceus have: when the analysis
gives up, they count, and jihoo copies everything.

## The proposal: static ownership, counted sharing

Keep the ownership pass, and make the copy it falls back to cheap.

### 1. Counted buffers (done)

A list's or string's heap buffer starts with a count of the values that hold
it; the header (`{ data, len, cap }`) points just past the count, so nothing
else changes shape.

- `clone` of a list or string counts one more holder and copies the header:
  O(1), however large the value. A clone of a struct, an enum or an array
  clones its parts, so it costs the number of owning fields, not the size.
- `drop` counts one fewer; the last holder frees the elements and the buffer.
- Every write into a list's buffer (`push`, `pop`, `setelem`, and `setpath`,
  `take` and `cellset` through elements) first makes it unique: a buffer
  held more than once is copied, one level only (its elements are counted
  once more, not copied).
- Strings never change in place, so their copies share the bytes for good.

Values mean exactly what they meant: a write still never shows through
another copy, as the VM's copy on write already ensures. JIR, sema and the
ownership pass do not change. The pass now decides where counting is
avoided; when it gives up, the program pays O(1) and possibly a one-level
copy at the next write, not a copy of everything.

Counts are atomic, since values move between threads (`go`, channels, `ref`s
shared by threads). Reading the count to check uniqueness is a plain load on
x86_64 and an `ldar` on aarch64.

### 2. Uniqueness checks that fold away (done)

A write checks the count of each list on its path. In a tight loop that was
the one cost left: the `inplace` benchmark, a grid written element by
element, took twice as long with counted buffers. Two things fixed it.

**Known-unique flags.** For each list on the path of a write, a function
keeps a flag: raised when the check passed, lowered by anything that could
give the buffer another holder. In JIR that can only be:

- a `clone`, `repeat`, `splat`, `slice` or list `+` (which count what they
  copy), a call (the callee may keep a copy of what it is lent), or any
  other instruction not known to be harmless;
- writing the register anew, or a register the path indexes with;
- replacing a list on the path whole (`g[i] = row` puts in a buffer that may
  be shared), or taking or popping it out.

Writes below a list do not lower its flag: a unique buffer stays unique when
it is written. A write checks the count only when the flag is down.

The flags are runtime values, not the result of an analysis: they are
allocas, which LLVM turns into SSA values, so where paths join they need no
reasoning, and where they are known (after the first round of a loop) LLVM
folds the test away. They are correct by construction as long as every
event above lowers them; `tests/diff-lists/shared_writes.jh` shares buffers
between writes in each of those ways, and fails if calls and clones stop
lowering them.

**Headers that stay in registers.** The slow path took the header's
address, which made every header escape, so LLVM reloaded it from memory
around every write and could not vectorize. It now takes and returns the
data pointer.

With both, `inplace` takes as long as without counting. The alternative,
an explicit `unique %r, (path)` in JIR that the ownership pass hoists out of
loops (as Swift's optimizer hoists its array checks), would also work, but
it needs an analysis in the pass and a JIR change, and the flags reach the
same speed without either.

### 3. Ownership the compiler checks (done)

The ownership pass now writes down what it decided (JIR 21): for each
function, the parameters it takes over (`own`), the registers that are views
(`views`), and the part reads that move their part out (`moves`). Everything
else follows from what each instruction consumes, borrows, defines or
changes in place. `crates/jihoo-ir/src/owncheck.rs` then follows every
owning register along every path, independently of how the pass reasoned,
and requires that:

- each value is moved or dropped exactly once, and not used after;
- no value is left at a `ret` or a `tailcall` (no leak), and a value taken
  apart had all its owning parts moved out;
- only the function's own values are dropped, moved or changed in place,
  never a borrowed parameter or a view;
- a view is used only while the value it shares memory with is alive and
  unchanged, except in place at a part disjoint from the view's;
- a value is in the same state on every way into a block.

It runs on every native program, after the pass and before `nocopy` and the
backend; a failure is an internal compiler error that names the function,
the block and the instruction. It costs about 0.01 s on `examples/json.jh`.

It catches the pass's mistakes where they are made. With the leak that
`v = items[k]` had (fixed with the JSON example) put back into the pass, the
checker rejects both programs that do that, at the instruction that loses
the value; valgrind had found it only by running one. Leaving out the clones
of lists that are used again makes 9 of the 28 list tests fail to compile
("%24 is a view, but this moves it"), and leaving out the drops on edges
between blocks makes 22 of them fail ("still the function's own here: a
leak").

So the pass can keep learning to avoid copies, and a mistake in it shows
up as a compile error, not as memory corruption in a program.

### 4. Reuse (done for `map` and `filter`)

A buffer that is unique when its value dies can hold the value built from
it. Perceus reuses the memory of a dying constructor for the next one of
the same size; for lists the gain is in writing the result into the input,
because `map` allocates its result before the input dies. Measured on 20
rounds of `xs = lists.map(xs, f)` over 2 million integers: 0.10 s and
33 MB, where a loop writing `xs[i]` in place takes 0.04 s and 17 MB.

jihoo has a way to say this without teaching the compiler about `map`:
[rewrite rules](../language/rules.md), which the compiler checks against
what they replace. `lib/lists.jh` now has

```jihoo
pub fn map_in_place(comptime T: type, xs: [T], f: fn(T) -> T) -> [T] replaces map(xs, f) {
    for i in 0..len(xs) {
        xs[i] = f(xs[i])
    }
    return xs
}
```

and `filter_in_place`, which moves the kept elements to the front and pops
the rest. A rule applies only where the types fit, so `map_in_place` only
replaces a `map` to the same type. It writes `xs`, so it has a consuming
version, which `xs = lists.map(xs, f)` calls: nothing is allocated. Where
`xs` is still used, the call copies it once, at the first write, as
`map` did. The `map` above now takes 0.05 s and 17 MB.

Rules apply only where the functions they are given are inert (no effects,
certain to return), since rules may reorder calls. To count `x % 3 == 0`
as inert, the effect analysis now knows that dividing by a constant other
than 0 and -1 cannot trap. A function with a loop (`strings.to_upper`) is
is not inert. But the in-place rules call `f` just as `map` and `filter`
do, once for each element, in order, so they can apply to any `f`. The rule
checker now shows this: the first time a rule would apply to a function
that is not inert, it runs the rule and its pattern again with functions
that record each call (which parameter, and with what) and trap at one of
the first few calls, and compares the records, the output and where each
side stops ([Rules that keep the order of calls](../language/rules.md#rules-that-keep-the-order-of-calls)).
The in-place rules pass, so `words = lists.map(words, ...)` with
`strings.to_upper`, or a `map` whose `f` prints, allocates nothing either. `map_map` does not,
and still fuses only inert functions.

### 5. Later, in the language

`inout` parameters (Hylo's) would let a function change the caller's value in
place without returning it (`skip_space(&inp)` instead of `inp =
skip_space(inp)`), which makes the common case explicit rather than inferred.
That is a language change and is not needed for the steps above.

## Measurements

Steps 1 and 2 are in the LLVM backend only (`backend-llvm/src/codegen.cpp`):
counted buffers, O(1) clones, uniqueness checked inline before every write
with the known-unique flags, and the slow path that copies one level. They
were built on the branch `memory-rc-prototype`, one change at a time.
Each step passed every test, the backend tests, the differential tests
under valgrind and 3,000 to 4,000 random programs of the fuzzer, 500 more of
them under valgrind. The benchmarks and the script that runs them
(`run.sh`) are in `bench/memory/`.

Best of three runs, x86_64, `-O2`.

| program | before | counted | memory, before → counted |
|---------|--------|---------|--------------------------|
| `snapshot` | 0.51 s | 0.02 s | 643 MB → 14 MB |
| `shared_tags` | 0.14 s | < 0.01 s | 112 MB → 2.7 MB |
| `tree_edit` | 0.24 s | < 0.01 s | 252 MB → 3.3 MB |
| `words` (300,000 words, `maps`) | 0.11 s | 0.11 s | 25 MB |
| `pushpop` (5 x 5M pushes and pops) | 0.18 s | 0.18 s | 41 MB |
| `trees` (200 trees of 22,000 nodes, built and summed) | 0.17 s | 0.17 s | 2.5 MB |
| `inplace` (2000 x 2000 grid, 10 passes of writes) | 0.06 s | 0.05 s | 33 MB |

`before` is the backend before step 1; `counted` is the backend now, with
atomic counts, the flags of step 2, and one more change: the last holder of a
buffer frees it without an atomic write, since a count of 1 means no other
thread holds it to count itself in (as Lean 4 does). The programs that
needed copies become 25 to more than 100 times faster and use 20 to 75
times less memory, now faster than the VM, as a native program should be.
The others take the same time.

How it got there is worth keeping. With the count checked in a
call before each write, `inplace` took 0.21 s; checked inline, 0.13 s (how
the count is read, acquire, relaxed or plain, made no difference);
non-atomic counts, 0.09 s; with the flags and headers kept in registers,
0.05 s. `trees`, which frees many small lists, took 0.18 s until the last
holder stopped doing an atomic write.

Eight threads copying, changing and dropping copies of one grid held in a
`ref` (`tests/threads/shared_buffers.jh`) gave the right
result in 200 runs at `-O0` and `-O2`, and ran clean under valgrind.

The cost in memory is 8 bytes for each list or string buffer.

## What changes for users

- `jihoo build --cost` lists the same places as before, the copies the
  program's meaning needs; each now costs O(1), plus, at the first write
  to a list whose memory is still shared, a copy of that list's elements.
- `nocopy` keeps its meaning, "no copy of a list, a string or a closure":
  a function that makes no copies shares nothing, so no write in it copies
  memory it shared. (A value its caller shared may still be copied at the
  function's first write to it: the caller's copy, paid late.)
- Nothing changes for hosted programs, freestanding programs (which have no
  lists) or C: lists do not cross into C, and string literals stay
  constants.

## Plan

1. ~~Counted buffers in the backend.~~ Done.
2. ~~Uniqueness checks that fold away.~~ Done.
3. ~~Explicit ownership in JIR and the checker.~~ Done.
4. ~~Reuse of unique buffers~~, for `map` and `filter`, as rules, for any
   `f`. Done.
5. `inout` parameters, if programs still want them.
