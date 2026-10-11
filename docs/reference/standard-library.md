# Standard library

The standard library is a handful of jihoo modules in `lib/`, found by `import`
after the program's own directories ([Modules](../language/modules.md#finding-modules)).
One of them, the prelude, needs no import.

| module | profiles | what |
|--------|----------|------|
| [prelude](#prelude) | all | `Option(T)` and `Result(T, E)`, without an import |
| [`strings`](#strings) | hosted | searching, splitting, joining, trimming and parsing text |
| [`lists`](#lists) | hosted | sorting, mapping and searching lists |
| [`libc`](#libc) | native | declarations of C library functions |
| [`sys`](#sys) | native, freestanding | Linux syscall numbers of the target |
| [`alloc`](#alloc) | native, freestanding | an arena allocator over `mmap`, and a checked `Vec(T)` |
| [`io`](#io) | native, freestanding | output straight to file descriptors |
| [`coro`](#coro) | native | stackful coroutines |

`strings` and `lists` work on `str` and lists, which only hosted programs have.
`sys`, `alloc` and `io` are marked `#![freestanding]`: they use neither GC nor
libc, only `syscall`, so native programs can import them too. `coro` is marked
`#![native]`: it uses the C library to switch stacks. All of them work on both
targets, x86_64 and aarch64 Linux.

## prelude

`lib/prelude.jh` declares the types every program can name without an import,
in every profile:

```jihoo
pub enum Option(T: type) {      // a value that may be missing
    Some(T)
    None
}

pub enum Result(T: type, E: type) {   // a value, or why there is none
    Ok(T)
    Err(E)
}
```

```jihoo
fn find(xs: [i64], x: i64) -> Option(i64) {
    let i = 0
    while i < len(xs) {
        if xs[i] == x {
            return Option.Some(i)
        }
        i += 1
    }
    return Option.None
}

fn main() {
    let xs: [i64] = [4, 5, 6]
    match find(xs, 5) {
        Some(i) => print(i)        // 1
        None => print("missing")
    }
}
```

A module's own item of the same name comes first, so a program that declares
its own `Option` uses that one. The compiler carries the prelude in itself, so
it is the one it was built with whatever the search path holds; in JIR its
types are named `prelude.Option(i64)` and so on.

## strings

Text functions for hosted programs, written in jihoo on top of `len`, `s[i]`,
`s[lo:hi]` and `from_bytes` ([Types](../language/types.md#strings)). A `str` is
bytes; these functions treat only ASCII specially (case, spaces, digits), so
they never split a multi-byte character they did not find themselves.

```jihoo
import strings

fn main() {
    let parts = strings.split("a,b,c", ",")      // ["a", "b", "c"]
    print(strings.join(parts, " + "))            // a + b + c
    print(strings.to_upper(strings.trim("  hi ")))   // HI
    match strings.parse_int(" -42 ") {
        Some(n) => print(n + 1)                  // -41
        None => print("not a number")
    }
}
```

| function | description |
|----------|-------------|
| `index_of(s, sub) -> i64` | where `sub` first occurs in `s`, or -1 |
| `index_from(s, sub, from) -> i64` | the same, from byte `from` on |
| `last_index_of(s, sub) -> i64` | where `sub` last occurs, or -1 |
| `contains(s, sub)`, `starts_with(s, prefix)`, `ends_with(s, suffix)` | `bool` |
| `count(s, sub) -> i64` | how many times `sub` occurs, not overlapping |
| `split(s, sep) -> [str]` | the parts between occurrences of `sep`: `split("a,,b", ",")` is `["a", "", "b"]` |
| `fields(s) -> [str]` | the words: the runs of bytes that are not ASCII spaces |
| `lines(s) -> [str]` | the lines, without `\n` or `\r\n` |
| `join(parts, sep) -> str` | `parts` with `sep` between each two |
| `repeat(s, n) -> str` | `s` written `n` times |
| `replace(s, old, new) -> str` | every `old` replaced by `new` |
| `trim(s)`, `trim_start(s)`, `trim_end(s)` | `s` without ASCII spaces at both ends, the start, or the end |
| `to_upper(s)`, `to_lower(s)` | ASCII letters in the other case |
| `bytes(s) -> [u8]` | the bytes of `s` |
| `from_byte(b) -> str` | the one-byte string of `b` |
| `is_space(b)`, `is_digit(b)`, `is_letter(b)` | ASCII classes of a byte, `bool` |
| `parse_int(s) -> Option(i64)` | the decimal integer `s` writes, with an optional sign and spaces around it, or `None` if it is not one or does not fit |

`sep`, `sub` and `old` must not be empty. `split`, `join`, `repeat` and
`replace` build their results byte by byte in a `[u8]`, so they take time in
proportion to their input and output.

## lists

Generic functions on [lists](../language/lists.md) for hosted programs. Each
takes the element type first, as generic functions do, and returns a new list:
the one passed in is never changed.

```jihoo
import lists

fn main() {
    let xs: [i64] = [3, 1, 2]
    let sorted = lists.sort(i64, xs, fn(a: i64, b: i64) -> bool { return a < b })
    let doubled = lists.map(i64, i64, sorted, fn(x: i64) -> i64 { return x * 2 })
    print(lists.index_of(i64, doubled, 4))      // 1
}
```

| function | description |
|----------|-------------|
| `sort(T, xs, less: fn(T, T) -> bool) -> [T]` | sorted by `less`; stable, O(n log n) (merge sort) |
| `map(T, U, xs, f: fn(T) -> U) -> [U]` | `f` of each element |
| `filter(T, xs, keep: fn(T) -> bool) -> [T]` | the elements `keep` is true for |
| `fold(T, A, xs, init: A, f: fn(A, T) -> A) -> A` | `f(... f(f(init, xs[0]), xs[1]) ...)` |
| `reverse(T, xs) -> [T]` | back to front |
| `index_of(T, xs, x) -> i64` | where `x` first is, by `==`, or -1 |
| `contains(T, xs, x) -> bool` | whether `x` is in `xs` |
| `range(lo, hi) -> [i64]` | `lo` up to but not including `hi` |

`examples/words.jh` puts `read_line`, `strings` and `lists` together: it counts
the words of its input and prints the most common ones.

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
    let n = alloc.get(i64, &v, 0) + alloc.pop(i64, &v)   // 49
    alloc.arena_free(&arena)
    return n
}
```

Misuse stops the program with [`trap()`](builtins.md#everywhere), as an array
index out of bounds does: an index outside the Vec, `pop` on an empty Vec, an
alignment that is not a power of two, a negative size or count. Running out
of memory is not misuse: `alloc` gives null and `push` gives false, and
nothing changes. The structs' fields are private, so only the module changes
them, and every size computation is checked for overflow.

### Arena

An arena hands out memory from one block, and everything is freed at once by
`arena_free`. Nothing it handed out may be used afterwards.

| item | description |
|------|-------------|
| `struct Arena` | a block of memory, of which a part is handed out; its fields are private |
| `fn arena_new(cap: i64) -> Arena` | a new arena of `cap` bytes from `mmap`, or one with no room (`arena_cap` is 0) if `cap` is not positive or the kernel refused |
| `fn arena_free(a: *Arena)` | gives the memory back with `munmap`; afterwards the arena has no room. Freeing twice does nothing |
| `fn arena_used(a: *Arena) -> i64` | the bytes handed out so far, padding included |
| `fn arena_cap(a: *Arena) -> i64` | the bytes the arena has in all |
| `fn alloc(a: *Arena, size: i64, align: i64) -> *u8` | `size` bytes aligned to `align`, or null when the arena has no room; traps if `size` is negative or `align` is not a power of two |
| `fn alloc_array(comptime T: type, a: *Arena, n: i64) -> *T` | room for `n` values of type `T`, or null; traps if `n` is negative |
| `PROT_READ`, `PROT_WRITE`, `MAP_PRIVATE`, `MAP_ANONYMOUS` | the `mmap` constants it uses |

### Vec(T)

A growable array in an arena. Its storage doubles when full (4, 8, 16, ...);
the old storage stays in the arena until the arena is freed.

| item | description |
|------|-------------|
| `struct Vec(T: type)` | elements in an arena; its fields are private |
| `fn vec_new(comptime T: type, a: *Arena) -> Vec(T)` | an empty vector allocating from `a` |
| `fn vec_len(comptime T: type, v: *Vec(T)) -> i64` | the number of elements |
| `fn vec_cap(comptime T: type, v: *Vec(T)) -> i64` | room before it grows |
| `fn vec_data(comptime T: type, v: *Vec(T)) -> *T` | the elements as a raw pointer, for a syscall or C; not checked, and valid only until the next `push` |
| `fn push(comptime T: type, v: *Vec(T), x: T) -> bool` | appends `x`; false, changing nothing, if the arena has no room left |
| `fn get(comptime T: type, v: *Vec(T), i: i64) -> T` | element `i`; traps unless `0 <= i < vec_len` |
| `fn set(comptime T: type, v: *Vec(T), i: i64, x: T)` | replaces element `i`; traps unless `0 <= i < vec_len` |
| `fn pop(comptime T: type, v: *Vec(T)) -> T` | removes and returns the last element; traps if the vector is empty |
| `fn clear(comptime T: type, v: *Vec(T))` | removes every element, keeping the storage |

What the library cannot check:

- A `Vec` holds a pointer to its elements, so a copy (`let w = v`) shares them
  with the original, and pushing to both makes them overwrite each other.
  Keep one `Vec` and pass a pointer to it, as the functions take it.
- Memory from an arena must not be used after `arena_free`, and a `Vec` must
  not outlive its arena.

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
| `struct Coro` | a coroutine; its public fields are `value: i64`, the last yielded value (also a way to pass one in), and `data: *u8`, for the body's own state; the others are private to the library |
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
