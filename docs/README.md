# jihoo documentation

This directory is the source of the documentation website,
<https://jihoo12.github.io/jihoo/>. Each file is one page; this list is the
site's table of contents and sidebar, in order (`site/scripts/toc.mjs` reads it).
Links between pages are ordinary relative Markdown links, so the files read the
same on GitHub.

## Language

### Basics

- [Profiles](language/profiles.md)
- [Syntax](language/syntax.md)
- [Variables and control flow](language/control-flow.md)
- [Types](language/types.md)
- [Operators](language/operators.md)
- [Functions](language/functions.md)
- [Modules](language/modules.md)

### Data

- [Structs](language/structs.md)
- [Arrays](language/arrays.md)
- [Enums](language/enums.md)
- [Pattern matching](language/patterns.md)
- [Equality](language/equality.md)

### Functions as values

- [Function values](language/function-values.md)
- [Closures](language/closures.md)

### Hosted programs

- [References](language/references.md)
- [Cells](language/cells.md)
- [Tasks and channels](language/tasks-and-channels.md)

### Compile time

- [Compile-time evaluation](language/compile-time-evaluation.md)
- [Generics](language/generics.md)
- [Macros](language/macros.md)

### Native and freestanding

- [Pointers](language/pointers.md)
- [Calling C](language/calling-c.md)
- [Inline assembly](language/inline-assembly.md)

## Reference

- [Syntax summary](reference/syntax-summary.md)
- [Builtins](reference/builtins.md)
- [Standard library](reference/standard-library.md)
- [Command line](reference/command-line.md)
- [Limits](reference/limits.md)
- [JIR](jir.md)

## Internals

- [Architecture](internals/architecture.md)
- [Compiler](internals/compiler.md)
- [VM and GC](internals/vm.md)
- [LLVM backend](internals/llvm-backend.md)
- [Testing](internals/testing.md)
- [Roadmap](internals/roadmap.md)
