# Modules

Every file is a module with its own namespace. `import` makes another module
available under a short name, through which its public items are used.

```jihoo
#![freestanding]
import alloc            // lib/alloc.jh
import io as out        // lib/io.jh, used as `out`

fn _start() -> i64 {
    let arena = alloc.arena_new(1 << 20)
    let v = alloc.vec_new(i64, &arena)
    alloc.push(i64, &v, 42)
    out.print_int(alloc.get(i64, &v, 0))
    return 0
}
```

## Using items of another module

Items of another module are always written `alias.item`: functions
(`alloc.push(...)`), macros (`alias.m!(...)`), types (`alias.T`,
`alias.Vec(i64)`), struct literals (`alloc.Arena { ... }`), enum variants
(`geo.Shape.Empty`) and constants (`alloc.PROT_WRITE`). There is no way to
import names unqualified.

## Visibility

Items are private to their module unless marked `pub`: `pub fn`, `pub struct`,
`pub enum`, `pub const`, `pub macro`, `pub extern fn`. A `pub` struct's fields
are all public, and so are a `pub` enum's variants. Using a private item from
another module is an error that names it:

```text
error: main.jh:4:5: `alloc.helper` is private to module `alloc`
```

## Finding modules

`import a.b` loads the file `a/b.jh` and names it `b`; `import a.b as c` names
it `c`. The file is searched for, in order:

1. next to the importing file,
2. in the directories given with `-I`,
3. in the directories listed in `JIHOO_PATH` (separated by `:`),
4. in the standard library, `lib/` ([Standard library](../reference/standard-library.md)).

Each file is loaded once, however many modules import it. Imports may be
cyclic, since items are analyzed lazily; a file that imports itself is an
error. Errors name the file they are in.

## Per-target modules

A program is built for one target, `x86_64` or `aarch64` Linux: the machine
`jihoo` runs on, or the one given with `--target`
([Command line](../reference/command-line.md#options)). In each directory,
`import a.b` first looks for `a/b.<target>.jh`, then for `a/b.jh`. So code that
differs between machines, such as syscall numbers or inline assembly, goes into
one file per target, and only the target's file is ever read:

```text
lib/sys.x86_64.jh     pub const WRITE = 1
lib/sys.aarch64.jh    pub const WRITE = 64
```

```jihoo
#![freestanding]
import sys              // lib/sys.x86_64.jh or lib/sys.aarch64.jh

fn _start() -> i64 {
    syscall(sys.WRITE, 1, "hi\n", 3)
    return 0
}
```

The module is `sys` either way, and its items are `sys.WRITE`. The files for
the targets should declare the same public items, since the code that imports
them is the same; a `sys.jh` beside them is the one for every other target, or
can say that a target is not supported.

## Modules and profiles

The root file — the one given to `jihoo` — chooses the [profile](profiles.md)
for the whole program. Other modules may require one: a module marked
`#![freestanding]` uses neither GC nor libc and can be imported by native and
freestanding programs, and one marked `#![native]`, such as `libc`, only by
native programs. A module without an attribute can be imported by any program.

## Generics across modules

Inside a [generic](generics.md) function or struct, names resolve in the module
that declares it, while type arguments can come from the caller:
`alloc.Vec(Point)` holds the caller's `Point`, and `alloc.push(Point, &v, p)`
works on it.

## In JIR

A module's items are prefixed with its name (`alloc.push`, `alloc.Vec(i64)`);
the root module's items keep their plain names. Every function of every loaded
module is in the JIR (generic ones once per instance); natively they are
internal symbols, so LLVM drops the ones the program never calls.
