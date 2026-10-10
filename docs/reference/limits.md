# Limits

Fixed limits of the compiler and the VM. Each one stops a program that would
otherwise run or expand forever, with an error that says which limit was hit.

| limit | value | what happens |
|-------|-------|--------------|
| compile-time evaluation | 100 million VM steps per evaluation | `evaluation did not finish (step limit reached)` |
| macro expansion depth | 64 nested expansions | `macro expansion is too deep`; does the macro expand to itself? |
| item macro rounds | 16 | `item macros still produce item macros after 16 rounds` |
| generic instances | 1000 instances of generic functions | `too many instances of generic functions`; does one instantiate itself forever? |
| VM call depth | 10 000 frames | `stack overflow` at run time |
| VM time slice | 1000 instructions | not an error: the next task runs at the next safepoint ([Tasks](../language/tasks-and-channels.md#scheduling)) |
| `syscall` arguments | the number and up to 6 arguments | compile error |
| `asm` outputs | at most 1 | compile error |
| integer literals | at most `18446744073709551615` (`u64::MAX`), and within their type | compile error |
| nesting | 1000 levels of expressions, blocks, types and patterns; each operand of a chain like `1 + 1 + 1` counts as one more | `code is nested too deeply` |

The GC starts collecting once the heap is larger than twice what survived the
last collection, and not below 1 MiB ([VM and GC](../internals/vm.md#gc)). There
is no fixed heap limit.

The limits are constants in the source: `FUEL` in
`crates/jihoo-sema/src/comptime.rs`, `MAX_DEPTH` in
`crates/jihoo-sema/src/macros.rs`, `MAX_ITEM_ROUNDS` in
`crates/jihoo-sema/src/lib.rs`, `MAX_INSTANCES` in
`crates/jihoo-sema/src/generic.rs`, `MAX_NESTING` in
`crates/jihoo-syntax/src/parser.rs`, and `MAX_CALL_DEPTH` and `TIME_SLICE` in
`crates/jihoo-vm/src/lib.rs`.
