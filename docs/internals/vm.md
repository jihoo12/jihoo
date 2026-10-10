# VM and GC

`crates/jihoo-vm` runs hosted JIR modules, and compile-time code for every
profile. It is a register machine: each JIR function's registers become a frame
of values, and instructions are executed one by one, with bounds and division
checks that report errors instead of crashing.

## Running JIR

The VM runs JIR as it is, with a little prepared first: `Program`
(`crates/jihoo-vm/src/program.rs`) turns each instruction into an `Op` once per
function, so that what would otherwise be worked out every time is ready. A
call has the callee's index, not its name to look up; a constant is the value
it puts in its register; an integer operation knows its type. The other
instructions run from the JIR. Frames take their register vectors from a pool
of returned ones, so a call allocates nothing.

Compile-time evaluation keeps one module and its `Program` for a whole
compilation and adds to both as it needs functions
([Compiler](compiler.md#compile-time-evaluation)), so starting a VM for one
constant costs nothing per function.

Each step still finds the frame and its registers through the call stack, so
the VM spends about 20 cycles on a simple instruction. Fewer would take a
different register layout (one flat register stack, unchecked indexing), which
it does not have yet.

## Tasks

Tasks (`go f(x)`) run on one OS thread, interleaved by a deterministic
round-robin scheduler. A task runs until it waits on a channel, finishes, or has
run `TIME_SLICE` (1000) instructions and reaches a *safepoint*: a call, or a
jump back to an earlier block, which every loop has. Channels keep their
buffered values and the tasks waiting to send or receive. When no task can run
and `main` has not returned, the run stops with a deadlock error.

## GC

`crates/jihoo-vm/src/gc.rs` is a stop-the-world mark & sweep heap. A collection
may happen at any allocation once the heap grows past twice the size that
survived the last collection (1 MiB minimum).

- The roots are gathered in one place, `roots` in `crates/jihoo-vm/src/lib.rs`:
  the registers of every frame of every task, the channels tasks wait on, and
  values handed out to the embedder (`Vm::alloc_string`, used for comptime
  arguments), which stay alive as long as the VM. Values in a channel's buffer
  and the values of tasks waiting to send are reached through the channel.
- The invariant: whatever an instruction allocates from must already be
  reachable from the roots. Values read from registers are; values held only in
  Rust locals are not.
- A `GcRef` is a slot index plus the slot's generation, so using a freed object
  panics even after its slot has been reused, instead of reading the wrong
  object.
- `JIHOO_GC_STRESS=1` collects before every allocation, which turns a missing
  root into a panic on the first run instead of a rare heisenbug.

## Value semantics and in-place updates

jihoo values have value semantics, but the VM shares objects between copies and
updates them in place when it can.

- Each object has a *shared* bit, set when a second reference to it may appear:
  a `copy` to another register, an argument, a field or element of another
  object, a captured or sent value, a part read out of a parent, or a part of
  an object that is copied (the copy and the original then both hold it).
- `setfield`, `setelem` and `setpath` whose result replaces their source
  register update an unshared object in place, and copy a shared one; the copy
  is unshared again. So in `while i < n { a[i] = f(i) }` only the first write
  copies.
- Nested updates (`b.cells[r][c].v = x`) are one `setpath`, which updates in
  place down to the first shared object and copies from there. Nested reads are
  one `getpath`, so the objects on the way never get shared by being read.
- Cells are updated in place by `cellset`, since sharing them is their point;
  channels are mutable too, since tasks share them to communicate.

Filling and sorting a 20000-element array went from 9.9 s to 0.6 s with this.

The shared bit has to be right everywhere a reference is made, and a mistake
is silent: a copy changes along with its original. `JIHOO_VM_CHECK=1` checks
the bit against the heap: before every in-place update it counts the live
references to the object, from the roots and every object reachable from them,
and stops with a panic unless there is exactly one, the one being replaced. It
walks the heap at every update, so it is only for tests; the VM's own tests
always run with it, and running the fuzzer with it
([Testing](testing.md#fuzzing)) finds sharing bugs on the VM alone, without
comparing against LLVM.

## Values

| JIR type | on the VM |
|----------|-----------|
| integers, `bool` | an `i64` in canonical form (sign- or zero-extended by type) |
| floats | the bits of an `f64`; an `f32` is an `f64` that is exactly an `f32` value |
| `str` | a GC object holding the bytes |
| structs, enums, arrays | GC objects, shared and updated as above |
| `ref T` | a one-element GC object |
| `cell T`, `chan T` | GC objects with identity |
| function values | an index into the module's functions, or a GC object for a closure |
