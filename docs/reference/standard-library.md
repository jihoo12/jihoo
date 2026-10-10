# Standard library

The standard library is a handful of jihoo modules in `lib/`, found by `import`
after the program's own directories ([Modules](../language/modules.md#finding-modules)).
Hosted programs need none of it: `print`, `str` and the GC are built in.

| module | profiles | what |
|--------|----------|------|
| [`libc`](#libc) | native | declarations of C library functions |
| [`alloc`](#alloc) | native, freestanding (x86_64 Linux) | an arena allocator over `mmap`, and `Vec(T)` |
| [`io`](#io) | native, freestanding (x86_64 Linux) | output straight to file descriptors |
| [`coro`](#coro) | native, freestanding (x86_64 Linux) | stackful coroutines |

`alloc`, `io` and `coro` are marked `#![freestanding]`: they use neither GC nor
libc, only `syscall` and `asm`, so native programs can import them too. They
use x86_64 syscall numbers and registers.

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

Stackful coroutines, written in jihoo with a few lines of inline asm
(`examples/coroutines.jh`). A coroutine runs a function on a stack of its own;
`yield` hands a value back to whoever resumed it and waits, and `resume`
continues it until its next `yield`.

```jihoo
#![freestanding]
import coro
import io

fn squares(c: *coro.Coro) {
    let i = 1
    while i <= 3 {
        coro.yield(c, i * i)
        i = i + 1
    }
}

fn _start() -> i64 {
    let c = coro.new(squares, 64 * 1024)   // a 64 KiB stack from mmap
    while coro.resume(&c) {
        io.print_int(c.value)              // 1, 4, 9
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
| `fn free(c: *Coro)` | frees the stack; the coroutine must not be resumed afterwards |

- A coroutine can resume others, so they compose into pipelines.
- Coroutines are cooperative and run on one thread. A started `Coro` must not
  move, since its body holds a pointer to it.
- The context switch saves, on the running stack and below the red zone, the
  address to continue at, the registers that calls preserve, and its input
  registers, then loads the other stack pointer, restores that side's registers
  and `ret`s to its address. A new coroutine starts by calling its function on
  the fresh stack. The labels are numeric local labels, so this stays correct
  when LLVM inlines it in several places.
