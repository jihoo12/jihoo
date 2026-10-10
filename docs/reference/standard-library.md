# Standard library

The standard library is a handful of jihoo modules in `lib/`, found by `import`
after the program's own directories ([Modules](../language/modules.md#finding-modules)).
Hosted programs need none of it: `print`, `str` and the GC are built in.

| module | profiles | what |
|--------|----------|------|
| [`libc`](#libc) | native | declarations of C library functions |
| [`sys`](#sys) | native, freestanding | Linux syscall numbers of the target |
| [`alloc`](#alloc) | native, freestanding | an arena allocator over `mmap`, and `Vec(T)` |
| [`io`](#io) | native, freestanding | output straight to file descriptors |
| [`coro`](#coro) | native | stackful coroutines |

`sys`, `alloc` and `io` are marked `#![freestanding]`: they use neither GC nor
libc, only `syscall`, so native programs can import them too. `coro` is marked
`#![native]`: it uses the C library to switch stacks. All of them work on both
targets, x86_64 and aarch64 Linux.

## libc

```jihoo
#![native]
import libc

fn main() {
    let p = libc.malloc(16)
    libc.snprintf(p, 16, "%ld", 42 as i64)
    libc.puts(p)
    libc.free(p)
}
```

`lib/libc.jh` declares C functions with `pub extern fn`, so they are used as
`libc.name`. Only the ones a program calls are linked in. C types map as
described in [Calling C](../language/calling-c.md#types-across-the-boundary).

| group | functions |
|-------|-----------|
| stdio | `printf`, `snprintf`, `puts`, `putchar`, `getchar`, `fflush` |
| stdlib | `malloc`, `calloc`, `realloc`, `free`, `exit`, `abort`, `atoi`, `strtol`, `strtod`, `atof`, `getenv`, `qsort` |
| string | `strlen`, `strcmp`, `strncmp`, `strchr`, `memcpy`, `memmove`, `memset`, `memcmp` |
| unistd | `read`, `write`, `close` |
| math (`f64`) | `sqrt`, `cbrt`, `pow`, `exp`, `log`, `log2`, `log10`, `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `hypot`, `fabs`, `floor`, `ceil`, `round`, `trunc`, `fmod` |
| math (`f32`) | `sqrtf`, `sinf`, `cosf` |

Anything missing can be declared with `extern fn` in the program itself;
declarations of the same function in several modules must agree.

## sys

The Linux system call numbers of the target, in one file per target:
`lib/sys.x86_64.jh` and `lib/sys.aarch64.jh`
([per-target modules](../language/modules.md#per-target-modules)).

```jihoo
#![freestanding]
import sys

fn _start() -> i64 {
    syscall(sys.WRITE, 1, "hi\n", 3)
    return 0
}
```

| item | x86_64 | aarch64 |
|------|--------|---------|
| `WRITE` | 1 | 64 |
| `MMAP` | 9 | 222 |
| `MUNMAP` | 11 | 215 |
| `EXIT` | 60 | 93 |

## alloc

```jihoo
#![freestanding]
import alloc

fn _start() -> i64 {
    let arena = alloc.arena_new(1 << 20)        // 1 MiB from mmap
    let v = alloc.vec_new(i64, &arena)
    alloc.push(i64, &v, 42)
    alloc.push(i64, &v, 7)
    return alloc.get(i64, &v, 0) + alloc.pop(i64, &v)   // 49
}
```

### Arena

An arena hands out memory from one block, and everything is freed at once by
dropping it.

| item | description |
|------|-------------|
| `struct Arena { base: *u8, used: i64, cap: i64 }` | a block of `cap` bytes, of which `used` are handed out |
| `fn arena_new(cap: i64) -> Arena` | a new arena of `cap` bytes from `mmap`, or one with `cap == 0` if the kernel refused |
| `fn alloc(a: *Arena, size: i64, align: i64) -> *u8` | `size` bytes aligned to `align` (a power of two), or null when the arena is full |
| `fn alloc_array(comptime T: type, a: *Arena, n: i64) -> *T` | room for `n` values of type `T` |
| `PROT_READ`, `PROT_WRITE`, `MAP_PRIVATE`, `MAP_ANONYMOUS` | the `mmap` constants it uses |

### Vec(T)

A growable array in an arena. Its storage doubles when full (4, 8, 16, ...);
the old storage stays in the arena until the arena is dropped.

| item | description |
|------|-------------|
| `struct Vec(T: type) { data: *T, len: i64, cap: i64, arena: *Arena }` | `len` elements at `data`, room for `cap` |
| `fn vec_new(comptime T: type, a: *Arena) -> Vec(T)` | an empty vector allocating from `a` |
| `fn push(comptime T: type, v: *Vec(T), x: T) -> bool` | appends `x`; false if the arena has no room left |
| `fn get(comptime T: type, v: *Vec(T), i: i64) -> T` | element `i` (not bounds-checked) |
| `fn set(comptime T: type, v: *Vec(T), i: i64, x: T)` | replaces element `i` (not bounds-checked) |
| `fn pop(comptime T: type, v: *Vec(T)) -> T` | removes and returns the last element; the vector must not be empty |

## io

```jihoo
#![freestanding]
import io

fn _start() -> i64 {
    io.puts("hello\n")
    io.print_int(-42)
    return 0
}
```

| item | description |
|------|-------------|
| `const STDOUT = 1`, `const STDERR = 2` | file descriptors |
| `fn write(fd: i64, buf: *u8, len: i64) -> i64` | the `write` syscall: bytes written, or a negative error |
| `fn strlen(s: *u8) -> i64` | the length of a NUL-terminated string |
| `fn puts(s: *u8)` | writes a NUL-terminated string, such as a literal, to stdout (no newline added) |
| `fn print_int(n: i64)` | writes `n` in decimal and a newline to stdout |
| `fn exit(code: i64)` | ends the program with exit status `code` |

## coro

Stackful coroutines for native programs (`examples/coroutines.jh`), with the C
library's `makecontext` and `swapcontext` doing the switching. A coroutine runs a function on a stack of its own;
`yield` hands a value back to whoever resumed it and waits, and `resume`
continues it until its next `yield`.

```jihoo
#![native]
import coro
import libc

fn squares(c: *coro.Coro) {
    let i = 1
    while i <= 3 {
        coro.yield(c, i * i)
        i = i + 1
    }
}

fn main() -> i64 {
    let c = coro.new(squares, 64 * 1024)   // a 64 KiB stack from malloc
    while coro.resume(&c) {
        libc.printf("%ld\n", c.value)     // 1, 4, 9
    }
    coro.free(&c)
    return 0
}
```

| item | description |
|------|-------------|
| `struct Coro` | a coroutine; the fields meant for programs are `value: i64`, the last yielded value (also a way to pass one in), and `data: *u8`, for the body's own state; the others belong to the library |
| `fn new(body: fn(*Coro), size: i64) -> Coro` | a coroutine that will run `body` on a stack of `size` bytes; it starts on the first `resume`, and is done from the start if no stack could be had |
| `fn resume(c: *Coro) -> bool` | runs `c` until it yields (true: `c.value` is the value) or its body returns (false); resuming a finished coroutine does nothing |
| `fn yield(c: *Coro, v: i64)` | called by the body: hands `v` to the resumer, and returns when resumed |
| `fn free(c: *Coro)` | frees the stack and the saved contexts; the coroutine must not be resumed afterwards |

- A coroutine can resume others, so they compose into pipelines.
- Coroutines are cooperative and run on one thread. A started `Coro` must not
  move, since its body holds a pointer to it.
- Each coroutine has a C `ucontext_t` for itself and one for its resumer.
  `resume` and `yield` swap between them with `swapcontext`; the first
  `resume` sets up the coroutine's with `makecontext`, whose `uc_link` makes
  the resumer go on when the body returns. jihoo treats a `ucontext_t` as
  bytes (8 KiB, more than glibc's 968 on x86_64 and 4560 on aarch64), apart
  from the fields at its start, which are at the same offsets on both.
- `makecontext` and `swapcontext` are in glibc but not in every C library
  (musl has neither).
