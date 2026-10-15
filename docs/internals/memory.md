# Design: native memory

Status: a proposal, with a prototype of its first step on the branch
`memory-rc-prototype` and the measurements below. Nothing here changes the
language: values keep their meaning, and hosted programs do not change.

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

### 1. Counted buffers (prototyped)

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

### 2. Uniqueness checks the compiler can hoist

A write checks the count of each list on its path. In a tight loop that is
the one cost left (the `inplace` row below: 0.06 s before, 0.13 s with the
checks). After the first write the lists on the path are unique, and they stay
so until something can give them another holder. In JIR that can only be:

- a `clone`, `repeat` or `splat` of the value, or of a view into it;
- a call it, or a view into it, is lent to (the callee may keep a copy);
- writing a list on the path whole (`g[i] = row` puts in a buffer that may
  be shared), or writing a register the path indexes with.

Writes at or below a path do not end its uniqueness: a unique buffer stays
unique when it is written. So "the lists on path P of `%r` are unique" is an
available-expressions fact that the ownership pass can compute forward over
the CFG, and a write needs no check for the levels the fact covers. Two
changes to JIR carry it:

- `unique %r, (path)`: makes the lists along the path unique where the path
  exists, and does nothing (no bounds trap) where it does not, so that it can
  be placed before a loop that may not run.
- a flag on each write saying which levels on its path are known unique.

For `for i { for j { g[i][j] = ... } }` the pass puts `unique %g, (elem %i)`
before the inner loop; the fact then holds on the inner loop's entry and on
its back edge, and the writes inside check nothing. This is what Swift's
optimizer does for arrays, but on JIR the facts are simpler to get: there are
no references into values, so nothing else can make `g[i]` shared while the
loop runs.

### 3. Ownership the backend can check

To make the pass's mistakes compile errors instead of memory errors, JIR
would say what the pass decided instead of leaving it implicit in liveness:
which part reads are views and which move a part out, and which parameters a
function takes over. Then a small checker, run on every native module before
the backend (and always in the tests), verifies that each owned value is
moved or dropped exactly once on every path, that nothing is used after it
is moved, and that no view is used after its root changes. This is
translation validation: the pass may grow clever, and the checker stays
simple. A debug mode can also count views at run time and check, at every
write, that no view of the buffer is alive.

### 4. Reuse

With counts, a buffer that is unique when its value dies can be reused for
the value built from it, as Perceus does. `lists.map` over a list handed
over, with elements of the same size, would write the results into the same
buffer: no allocation and no free. Today the pass gets this only for `xs =
f(xs)` written in place.

### 5. Later, in the language

`inout` parameters (Hylo's) would let a function change the caller's value in
place without returning it (`skip_space(&inp)` instead of `inp =
skip_space(inp)`), which makes the common case explicit rather than inferred.
That is a language change and is not needed for the steps above.

## The prototype

The branch `memory-rc-prototype` implements step 1 in the LLVM backend only
(`backend-llvm/src/codegen.cpp`): counted buffers, O(1) clones, uniqueness
checked inline before every write, and the slow path that copies one level.
It passes every test, the backend tests, the differential tests under
valgrind, and 7,000 random programs of the fuzzer (4,000 before checks were
inline, 3,000 after), 1,100 more of them under valgrind. The benchmarks and
the script that runs them are in `bench/memory/` on the branch.

Best of three runs, x86_64, `-O2`; `main` is the current backend.

| program | `main` | counted, atomic | counted, not atomic | memory, `main` → counted |
|---------|--------|-----------------|---------------------|--------------------------|
| `snapshot` | 0.50 s | 0.02 s | < 0.01 s | 643 MB → 14 MB |
| `shared_tags` | 0.13 s | < 0.01 s | < 0.01 s | 112 MB → 2.7 MB |
| `tree_edit` | 0.25 s | < 0.01 s | < 0.01 s | 252 MB → 3.3 MB |
| `json` (0.9 MB) | 0.02 s | 0.02 s | 0.02 s | 15 MB → 10 MB |
| `words` (300,000 words, `maps`) | 0.11 s | 0.09 s | 0.11 s | 25 MB |
| `pushpop` (5 x 5M pushes and pops) | 0.19 s | 0.18 s | 0.18 s | 41 MB |
| `trees` (200 trees of 22,000 nodes, built and summed) | 0.16 s | 0.18 s | 0.18 s | 2.5 MB |
| `inplace` (2000 x 2000 grid, 10 passes of writes) | 0.07 s | 0.13 s | 0.09 s | 33 MB |

Programs that copied nothing before cost about what they did (`pushpop`,
`words`, `json`; `trees` 12% more, for the count in each new buffer): their
values were moved, not counted, and stay so. The programs that needed copies
become 25 to more than 100 times faster and use 20 to 75 times less memory,
now faster than the VM, as a native program should be. The loss is
`inplace`, a loop that writes a grid element by element: twice as slow with
atomic counts, 30% slower without. That is the check before each write,
which step 2 removes. Checking the count inline rather than in a call
already took `inplace` from 0.21 s to 0.13 s; how the count is read (acquire,
relaxed or plain) made no measurable difference.

The cost in memory is 8 bytes for each list or string buffer.

## What changes for users

- `jihoo build --cost` would list the places that count instead of copy,
  which are cheap, and should rather list the writes that may copy a shared
  buffer, which are the ones that cost.
- `nocopy` keeps its meaning, "no copy of a list, a string or a closure",
  checked the same way; a copy is now O(1), so it guards against counting and
  against one-level copies at writes.
- Nothing changes for hosted programs, freestanding programs (which have no
  lists) or C: lists do not cross into C, and strings literals stay
  constants.

## Plan

1. Counted buffers in the backend, as prototyped, with `--cost` reporting
   writes that may copy. JIR does not change.
2. `unique` in JIR and hoisting in the ownership pass, until `inplace` is as
   fast as before.
3. Explicit ownership in JIR and the checker, run in the tests and, if cheap
   enough, always.
4. Reuse of unique buffers.
5. `inout` parameters, if programs still want them.

Steps 1 and 2 are the speed, 3 the safety; each is useful alone.
