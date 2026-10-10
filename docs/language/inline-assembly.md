# Inline assembly

Native and freestanding programs can talk to the machine directly, with the
`syscall` builtin and with inline assembly.

```jihoo
#![freestanding]

fn write(fd: i64, buf: *u8, len: i64) -> i64 {
    return asm("syscall",
        out("rax") i64,
        in("rax") 1, in("rdi") fd, in("rsi") buf, in("rdx") len,
        clobber("rcx", "r11", "memory"))
}

fn bswap(x: u64) -> u64 {
    return asm("mov {out}, {0}", "bswap {out}", out(reg) u64, in(reg) x)
}

fn _start() -> i64 {
    write(1, "hi\n", 3)
    return 0
}
```

## `syscall`

`syscall(n, a, b, ...)` makes the raw Linux system call number `n` with up to
six arguments, integers or pointers, and returns the kernel's result as an
`i64` (a negative error number on failure). It works on x86_64 and aarch64,
but the numbers are the target's: `write` is 1 on x86_64 and 64 on aarch64.
The `sys` module has them for the target being built for
([per-target modules](modules.md#per-target-modules)):

```jihoo
import sys
syscall(sys.WRITE, 1, "hi\n", 3)     // write(1, "hi\n", 3)
```

The same goes for `asm`, whose template is the target's assembly: put it in a
module with one file per target.

## `asm`

`asm(template..., operands...)` is an expression of the output's type, or
`unit` without an output.

- **Template.** One or more string lines, joined with newlines. `{0}`, `{1}`,
  ... are the inputs in order and `{out}` the output. On x86_64 the syntax is
  Intel.
- **Output.** At most one: `out("rax") T` in a given register, or `out(reg) T`
  in one the compiler chooses.
- **Inputs.** `in("rdi") x` in a given register, `in(reg) x` in any, and
  `in(out) x` to start the output register with `x`, for instructions such as
  `xchg` or `inc` that update a register in place. Inputs are integers, bools,
  pointers or function values.
- **Clobbers.** `clobber("rcx", "memory")` lists registers the code changes and
  `"memory"` if it reads or writes memory. The flags are always clobbered.

A function value is a code pointer, so it can be an input and `call {0}` calls
it. `lib/coro.jh` builds stackful coroutines from a few lines of asm this way
([Standard library](../reference/standard-library.md#coro)).

## Limits

- Native and freestanding only.
- Like `syscall`, `asm` cannot run at compile time.
- The template is passed to LLVM as is; a mistake in it is reported by LLVM's
  assembler when the program is built ([JIR](../jir.md#builtins)).

See `examples/asm.jh`.
