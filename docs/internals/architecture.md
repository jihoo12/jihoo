# Architecture

jihoo aims to be an easy language that can still go all the way down to bare
syscalls. One surface language is lowered to one IR, JIR; that IR is either run
by a VM or compiled by LLVM.

```text
               ┌─────────────── Rust ───────────────┐      ┌──── C++ ─────┐
 source.jh ──► │ jihoo-syntax ─► jihoo-sema ──► JIR │ ──►  │  jihoo-llc   │ ──► .o ─┬─► cc + libc ──► program  (native)
               │   (parse)      (typecheck    │    │ .jir │ (LLVM 21)    │         └─► ld.lld ─────► static ELF (freestanding)
               │                 + lower)     ▼    │      └──────────────┘
               │                           jihoo-vm│
               └────────────────────────────────────┘
```

## Components

| path | language | what |
|------|----------|------|
| `crates/jihoo-syntax` | Rust | lexer, parser, AST, and the module loader |
| `crates/jihoo-sema` | Rust | type checking and lowering from the AST to JIR, compile-time evaluation, macros |
| `crates/jihoo-ir` | Rust | JIR data types, the text printer, layouts, the verifier |
| `crates/jihoo-vm` | Rust | the register VM, its task scheduler and the GC heap |
| `crates/jihoo-cli` | Rust | the `jihoo` command |
| `backend-llvm` | C++ | `jihoo-llc`: JIR text → LLVM → object file |
| `lib` | jihoo | the standard library |
| `tests/diff` | jihoo | programs that must behave the same on the VM and natively |
| `site` | Astro | this website |

## A program's way through

1. **Load** (`crates/jihoo-syntax/src/loader.rs`). The root file and every file
   it imports, transitively, are parsed into one AST per module. Each file is
   loaded once; positions carry a file number, so errors name the right file.
2. **Analyze** (`crates/jihoo-sema`). Item macros are expanded, then every
   function is type checked and lowered to typed JIR in one pass, generic ones
   once per instance. Constants, `comptime` expressions and macros run on the VM
   along the way. See [Compiler](compiler.md).
3. **Verify** (`crates/jihoo-ir/src/verify.rs`). The JIR module is checked on its
   own: register types, operand types, terminators, and that the profile allows
   every instruction. This catches frontend bugs before a backend sees them.
4. **Run or compile.** A hosted module runs on the VM ([VM and GC](vm.md)). A
   native or freestanding one is printed as text, compiled by `jihoo-llc`
   ([LLVM backend](llvm-backend.md)) and linked by `jihoo build`.

## Why the IR is a text file

The frontend (Rust) and the LLVM backend (C++) only talk through `.jir` files
([JIR](../jir.md)). This keeps the IR an explicit, versioned contract, lets the
backend be developed and tested on its own with hand-written `.jir`, and keeps
the Rust build free of LLVM.

## Shared rules

The typing rules of operators live once, in `crates/jihoo-ir/src/types.rs`, and
are used both by the type checker and by the IR verifier, so the two cannot
disagree. The C layout rules (`crates/jihoo-ir/src/layout.rs`) are recorded in
JIR and checked again by the backend against the real target.
