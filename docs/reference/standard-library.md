# Standard library

The standard library is a handful of jihoo modules in `lib/`, found by `import`
after the program's own directories ([Modules](../language/modules.md#finding-modules)).
One of them, the prelude, needs no import.

| module | profiles | what |
|--------|----------|------|
| [prelude](#prelude) | all | `Option(T)`, `Result(T, E)` and the traits `Ord`, `Hash`, `Show` and `Iterator`, without an import |
| [`strings`](#strings) | hosted, native | searching, splitting, joining, trimming and parsing text |
| [`lists`](#lists) | hosted, native | sorting, mapping and searching lists, and fused pipelines (`fuse!`) |
| [`maps`](#maps) | hosted, native | hash maps, `Map(K, V)`, for keys with `Hash` and `==` |
| [`files`](#files) | hosted, native | reading and writing whole files, with messages for what failed |
| [`iters`](#iters) | all (`range`, `take`, `count`, `sum`), hosted, native | iterators: ranges, lists, and `map`, `filter`, `zip` over others |
| [`meta`](#meta) | all, while compiling | the shape of code, for macros that read the code they are given |
| [`libc`](#libc) | native | declarations of C library functions |
| [`sys`](#sys) | native, freestanding | Linux syscall numbers of the target |
| [`alloc`](#alloc) | native, freestanding | an arena allocator over `mmap`, and a checked `Vec(T)` |
| [`io`](#io) | native, freestanding | output straight to file descriptors |
| [`coro`](#coro) | native | stackful coroutines |
| [`sync`](#sync) | native, freestanding | locks for threads: `Mutex`, `Cond`, `Group` |
| [`thread`](#thread) | native | threads on all cores, with POSIX threads |
| [`channel`](#channel) | native | channels between threads, with `close` |

`strings`, `lists`, `maps` and `files` work on `str` and lists, which hosted and native programs have.
`sys`, `alloc`, `io` and `sync` are marked `#![freestanding]`: they use neither
GC nor libc, only `syscall`, so native programs can import them too. `coro`,
`thread` and `channel` are marked `#![native]`: they use the C library to
switch stacks, start threads and allocate. All of them work on both targets,
x86_64 and aarch64 Linux.

## prelude

`lib/prelude.jh` declares the types and traits every program can name without
an import, in every profile:

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

The prelude also declares four [traits](../language/traits.md#the-standard-traits):

| trait | functions | implemented for |
|-------|-----------|-----------------|
| `Ord` | `compare(a, b) -> i64`, `less(a, b) -> bool` | numbers (floats in IEEE total order), `str`, and `Option`, lists and tuples of two or three parts whose parts have one |
| `Hash` | `hash(x, h: Hasher) -> Hasher` | integers, `bool`, `str`, and lists, `Option` and tuples of hashable parts |
| `Show` | `show(x) -> str` | numbers, `bool`, `str`, and lists, `Option`, `Result` and tuples of parts with one |
| `Iterator` | `type Item`, `next(it) -> Option((Self, Self.Item))` | nothing: types of programs and libraries, which `for` walks |

`hash_of(x) -> u64` is the hash of a value with `Hash`, the same in every
profile. `print(x)` prints `x.show()` for a value that is not a number, a
`bool` or a `str`. `Show` and the impls for `str` and lists need what hosted and
native programs have; `Ord` and `Hash` for the other types work in every
profile.

A module's own item of the same name comes first, so a program that declares
its own `Option` uses that one. The prelude's functions, types and impls are
only part of a program that uses them, so it costs nothing otherwise. The
compiler carries the prelude in itself, so it is the one it was built with
whatever the search path holds; in JIR its types are named
`prelude.Option(i64)` and so on.

## strings

Text functions for hosted and native programs, written in jihoo on top of `len`, `s[i]`,
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
| `parse_float(s) -> Option(f64)` | the decimal number `s` writes (`[+-]digits[.digits][(e\|E)[+-]digits]`, spaces around it allowed) as the nearest `f64`, or `None` if it is not one; too large is infinite (`parse_float("1e999")`) |

`sep`, `sub` and `old` must not be empty. `split`, `join`, `repeat` and
`replace` build their results byte by byte in a `[u8]`, so they take time in
proportion to their input and output.

## lists

Generic functions on [lists](../language/lists.md) for hosted and native programs. Each
returns a new list: the one passed in is never changed. The element types are
inferred from the arguments ([Generics](../language/generics.md#inferred-type-arguments)),
or can be written first, as in the table.

```jihoo
import lists

fn main() {
    let xs: [i64] = [3, 1, 2]
    let sorted = lists.sort(xs)
    let doubled = lists.map(sorted, fn(x) -> i64 { return x * 2 })
    print(lists.index_of(doubled, 4))      // 1
}
```

| function | description |
|----------|-------------|
| `sort(T: Ord, xs) -> [T]` | sorted by the elements' [`Ord`](../language/traits.md#ord), least first; stable, O(n log n) (merge sort) |
| `sort_by(T, xs, less: fn(T, T) -> bool) -> [T]` | sorted by `less`; stable, O(n log n) |
| `min(T: Ord, xs) -> Option(T)`, `max(T: Ord, xs) -> Option(T)` | the least or greatest element, the first of equal ones; `None` for an empty list |
| `map(T, U, xs, f: fn(T) -> U) -> [U]` | `f` of each element |
| `filter(T, xs, keep: fn(T) -> bool) -> [T]` | the elements `keep` is true for |
| `fold(T, A, xs, init: A, f: fn(A, T) -> A) -> A` | `f(... f(f(init, xs[0]), xs[1]) ...)` |
| `reverse(T, xs) -> [T]` | back to front |
| `index_of(T, xs, x) -> i64` | where `x` first is, by `==`, or -1 |
| `contains(T, xs, x) -> bool` | whether `x` is in `xs` |
| `range(lo, hi) -> [i64]` | `lo` up to but not including `hi` |

`examples/words.jh` puts `read_line`, `strings` and `lists` together: it counts
the words of its input and prints the most common ones.

### fuse!

`lists.fuse!(xs, stage...)` runs a pipeline over `xs` as one loop, building no
list between the stages, so it is as fast as the loop written out by hand;
`map`, `filter` and `fold` above build a new list each.

```jihoo
let total = lists.fuse!(xs,
    filter(fn(x: i64) -> bool { return x > 0 }),
    map(fn(x: i64) -> f64 { return x as f64 / 2.0 }),
    sum())                                       // an f64
let names = lists.fuse!(people, filter(adult), map(name_of), collect())
```

| stage | does |
|-------|------|
| `map(f)` | replaces each value `v` with `f(v)` |
| `filter(keep)` | drops `v` unless `keep(v)` |
| `sum()` | ends the pipeline: the sum of the values, of the type the stages end with; also when no end is given |
| `count()` | ends the pipeline: how many values are left, an `i64` |
| `collect()` | ends the pipeline: a list of the values |

The functions are evaluated once, before the loop. `fuse!` is a
[macro](../language/macros.md) written in jihoo: it reads the stages with
[`meta`](#meta) and asks `type_of` for the type the last stage gives. A stage
it does not know stops the compilation with a message that names it.
`examples/pipe.jh` compares it with `map`, `filter` and `fold`.

## files

Whole files for hosted and native programs, on the builtins `read_file`,
`write_file` and `append_file` ([Builtins](builtins.md#hosted-and-native)),
the same on the VM and natively. What failed is a message that names the file
and says why, in the words of C's `strerror`.

```jihoo
import files
import strings

fn main() -> i64 {
    let a = args()
    if len(a) != 1 {
        files.eprint("usage: count FILE")
        return 2
    }
    match files.read(a[0]) {
        Ok(text) => print(len(strings.lines(text)))
        Err(msg) => {
            files.eprint(msg)          // notes.txt: No such file or directory
            return 1
        }
    }
    return 0
}
```

| function | description |
|----------|-------------|
| `read(path) -> Result(str, str)` | the whole file, or why it cannot be read |
| `write(path, text) -> Option(str)` | makes the file hold `text`, creating it if missing; why it could not, or `None` |
| `append(path, text) -> Option(str)` | adds `text` at the end of the file, creating it if missing; why it could not, or `None` |
| `eprint(text)` | writes `text` and a newline to standard error |
| `message(path, err) -> str` | `path: ` and what error number `err` means |
| `error_text(err) -> str` | what error number `err` means (`"Permission denied"`), or `"error N"` for one the module does not know |

The module also names the error numbers the builtins give most often:
`ENOENT`, `EACCES`, `EISDIR`, `ENOTDIR`, `EEXIST`, `EINVAL`, `ENOSPC` and a
few more, with Linux's values, which are the same on both targets.

## maps

Hash maps for hosted and native programs. A `Map(K, V)` holds a value for each
of some keys, found by the keys' [`Hash`](../language/traits.md#the-standard-traits)
and `==`. A map is a value, like a list: `set` and `remove` return a new map
and leave the one passed in alone, and `m = maps.set(m, k, v)` changes `m` in
place, natively without copying it.

```jihoo
import maps

fn main() {
    let counts = maps.new(str, i64)
    for w in ["to", "be", "or", "not", "to"] {
        match maps.get(counts, w) {
            Some(n) => counts = maps.set(counts, w, n + 1)
            None => counts = maps.set(counts, w, 1)
        }
    }
    print(maps.get(counts, "to"))           // Some(2)
    print(maps.size(counts))                // 4
}
```

| function | description |
|----------|-------------|
| `new(K: Hash, V) -> Map(K, V)` | an empty map |
| `size(m) -> i64` | how many keys `m` has |
| `get(m, k) -> Option(V)` | the value for `k`, if any |
| `has(m, k) -> bool` | whether there is a value for `k` |
| `set(m, k, v) -> Map(K, V)` | `m` with `v` for `k` |
| `remove(m, k) -> Map(K, V)` | `m` without `k` |
| `keys(m) -> [K]`, `values(m) -> [V]`, `entries(m) -> [(K, V)]` | its contents, in no useful order but the same in every profile |

The keys are kept in buckets by their hash, two to a bucket on average at
most; a lookup hashes the key once and compares it with the few keys in its
bucket.

## iters

Iterators, values of types with the prelude's
[`Iterator`](../language/traits.md#iterator), which [`for`](../language/control-flow.md#for)
walks, and adapters that make new ones out of them. Nothing runs until
something walks the result, and no list is built between the stages:

```jihoo
import iters

fn main() {
    let xs: [i64] = [1, 2, 3, 4, 5]
    let odd = iters.filter(iters.of(xs), fn(x: i64) -> bool { return x % 2 == 1 })
    for x in iters.map(odd, fn(x: i64) -> i64 { return x * x }) {
        print(x)                                    // 1, 9, 25
    }
    print(iters.sum(iters.take(iters.range(0, 1000000), 4)))     // 6
}
```

| function | description |
|----------|-------------|
| `range(lo, hi) -> Range` | the integers from `lo` up to `hi - 1` |
| `of(xs: [T]) -> Of(T)` | the elements of a list |
| `map(it, f: fn(I.Item) -> U) -> Map(I, U)` | `f` of each item |
| `filter(it, keep: fn(I.Item) -> bool) -> Filter(I)` | the items `keep` is true for |
| `take(it, n) -> Take(I)` | the first `n` items |
| `zip(a, b) -> Zip(A, B)` | pairs of items, until either ends |
| `collect(it) -> [I.Item]` | the items, in a list |
| `fold(it, init, f: fn(A, I.Item) -> A) -> A` | the items folded into `init` |
| `count(it) -> i64`, `sum(it) -> I.Item` | how many items, and their sum |

An iterator is a value: walking one leaves it as it was. Natively the inner
iterator of an adapter, and a list it walks, moves from one round to the
next and is never copied. `range`, `take`, `count` and `sum` work in freestanding programs
too; the others need closures or lists.

## meta

The shape of code, one level deep, for [macros](../language/macros.md) and
functions on code. `meta.node(e)` reads the text of `e` and gives a `Node`;
its parts are code again, to look into further or to put back with holes.

```jihoo
import meta

// `doubled!(f(a, b))` adds up the arguments twice: `a + a + b + b`.
macro doubled(call: expr) -> expr {
    match meta.node(call) {
        Call(f, args) => {
            let out = quote(0)
            for a in args {
                out = quote($out + $a + $a)
            }
            return out
        }
        _ => return call
    }
}
```

| `Node` | for |
|--------|-----|
| `Call(callee, args)` | `f(a, b)` or `m.f(a, b)`: the callee as written, a `str`, and the arguments, `[expr]` |
| `Name(n)` | a name, such as `x` or `m.f` |
| `Int(n)` | an integer literal, an `i64` |
| `Str(s)` | a string literal: its value |
| `Binary(op, lhs, rhs)` | the operator that applies last, such as `"+"` in `a + b * c`, and both sides |
| `Func(params, f)` | an anonymous function: the names of its parameters, and the whole function |
| `Other` | anything else: unary operators, fields, indexing, casts, literals of structs, arrays and floats, `match`, macro calls |

`meta` is marked `#![comptime]`: it only exists while compiling, and none of
it, nor the `strings` it uses, is part of the program
([Modules](../language/modules.md#modules-and-profiles)), so any program can
import it, freestanding ones included. It is written in jihoo on top of
`code_text` and `to_code` ([Macro builtins](../language/macros.md#macro-builtins)).
[`lists.fuse!`](#fuse) uses it to read the stages of a pipeline.

`meta.shape(t)` gives the parts of a struct or enum type, where the macro is
called: `Struct(fields)`, the field names, `Enum(variants)`, each a
`Variant { name, values }` with how many values it holds, or `Other`. `t`
may be a generic type's name alone (`Pair`), and `meta.params(t)` then gives
its parameters (`["A", "B"]`), and none for any other type.

`meta.derive!(Show, Hash, Ord, T)` is an item macro that writes impls of
those [traits](../language/traits.md#deriving) for the struct or enum `T`,
or, for a generic type's name, generic impls, part by part:

```jihoo
import meta

struct Version { major: i64, minor: i64 }
enum Token { Num(i64), Op(str, i64), End }

meta.derive!(Show, Hash, Ord, Version)
meta.derive!(Show, Ord, Token)

fn main() {
    print(Version { major: 1, minor: 2 })        // Version { major: 1, minor: 2 }
    print(Token.Op("+", 2) < Token.End)        // true: variants in declared order
}
```

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
| `FUTEX` | 202 | 98 |

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

## sync

Locks for threads that share memory (`examples/threads.jh`), built on the
[atomic builtins](../language/threads.md#atomics) and the Linux kernel's
futexes: a thread that has to wait sleeps in the kernel instead of spinning,
and a lock nobody waits for is taken and released without a system call. The
module uses no libc, so freestanding programs can import it as well as native
ones; native programs start threads with [`thread`](#thread).

```jihoo
#![native]
import sync

struct Account {
    lock: sync.Mutex
    balance: i64
}

fn deposit(a: *Account, n: i64) {
    sync.lock(&a.lock)
    a.balance = a.balance + n
    sync.unlock(&a.lock)
}
```

| item | description |
|------|-------------|
| `struct Mutex` | a lock that one thread holds at a time; its fields are private |
| `fn mutex_new() -> Mutex` | a new, unlocked Mutex |
| `fn lock(m: *Mutex)` | takes the lock, waiting while another thread holds it; a thread that takes a lock it holds already waits forever |
| `fn try_lock(m: *Mutex) -> bool` | takes the lock if it is free, and says whether it did |
| `fn unlock(m: *Mutex)` | releases the lock, waking a thread that waits for it; the program stops if the Mutex is not locked |
| `struct Cond` | a condition variable; its fields are private |
| `fn cond_new() -> Cond` | a new Cond |
| `fn wait(c: *Cond, m: *Mutex)` | releases `m` (which the thread holds), sleeps until a `signal` or `broadcast`, and takes `m` again; it can also return without one, so wait in a loop that checks the condition |
| `fn signal(c: *Cond)` | wakes one thread waiting on `c` |
| `fn broadcast(c: *Cond)` | wakes every thread waiting on `c` |
| `struct Group` | a count of unfinished work, like Go's `sync.WaitGroup`; its fields are private |
| `fn group_new() -> Group` | a new Group with a count of zero |
| `fn group_add(g: *Group, n: i64)` | adds `n` (which may be negative) to the count; at zero, `group_wait` returns. The program stops if the count leaves 0 to 2^31 - 1 |
| `fn group_done(g: *Group)` | `group_add(g, -1)`: one piece of work is finished |
| `fn group_wait(g: *Group)` | waits until the count is zero |

- A Mutex, Cond or Group is shared by pointer and must stay where it is while
  threads use it: a copy is an unrelated second one.
- The Mutex is Ulrich Drepper's from "Futexes Are Tricky": its state is 0
  (free), 1 (held) or 2 (held, and a thread may be sleeping on it), and
  `unlock` makes a `futex` call only in state 2. A Cond is a sequence number
  that every signal changes, so a waiter that has released the Mutex does not
  miss a signal. All futex calls are private to the process.
- A Mutex is not fair: a thread that unlocks and locks again at once may get
  the lock again before one that was waiting.

## thread

Threads for native programs, on the C library's POSIX threads: functions that
run at the same time, on every core of the machine. Coordinate them with the
atomics or [`sync`](#sync) ([Threads and atomics](../language/threads.md)).
`go f(x)` starts a thread with less ceremony, but one that cannot be joined;
this library's threads give back their result, and report failing to start.

```jihoo
#![native]
import thread

fn double(arg: *u8) -> i64 {
    return *(arg as *i64) * 2
}

fn main() -> i64 {
    let x = 21
    match thread.spawn(double, &x as *u8) {
        Some(t) => return thread.join(&t)     // exit status 42
        None => return 1
    }
}
```

| item | description |
|------|-------------|
| `struct Thread` | a started thread; its fields are private |
| `fn spawn(body: fn(*u8) -> i64, arg: *u8) -> Option(Thread)` | starts a thread that calls `body(arg)`; `None` if the system has no thread or memory to spare |
| `fn join(t: *Thread) -> i64` | waits for `t` to finish and gives what `body` returned; joining the same `Thread` twice stops the program (a copy of one does not know the other was joined, so join only one) |
| `fn cpus() -> i64` | the number of cores online, at least 1 |
| `fn yield()` | lets other threads run before this one goes on |
| `fn sleep_ms(ms: i64)` | sleeps for `ms` milliseconds |
| `fn now_ns() -> i64`, `fn now_ms() -> i64` | the time on a clock that only goes forward (`CLOCK_MONOTONIC`), in nanoseconds or milliseconds: subtract two readings to time something |

- Every thread must be joined once: `spawn` takes a little memory from
  `malloc` for the thread's start, which `join` frees.
- What `arg` points to must outlive the thread, for instance by joining the
  thread before the function holding it returns.
- A thread that traps stops the whole program, as it would in C.
- `main` returning ends the program, even while threads are still running.

## channel

Channels between threads (`examples/pipeline.jh`): what hosted programs'
[`chan`](../language/tasks-and-channels.md) is for tasks, for native threads
started with [`thread`](#thread). Values arrive in the order they were sent,
and `close` tells receivers that no more will come, as in Go.

```jihoo
#![native]
import channel
import thread

fn squares(arg: *u8) -> i64 {
    let c = *(arg as *channel.Chan(i64))
    let i = 1
    while i <= 3 {
        channel.send(i64, c, i * i)
        i = i + 1
    }
    channel.close(i64, c)
    return 0
}

fn main() -> i64 {
    match channel.new(i64, 0) {
        Some(c) => {
            let t = thread.spawn(squares, &c as *u8)
            let sum = 0
            let more = true
            while more {
                match channel.recv(i64, c) {
                    Some(x) => sum = sum + x      // 1, 4, 9
                    None => more = false          // closed, and nothing left
                }
            }
            match t {
                Some(h) => thread.join(&h)
                None => {}
            }
            channel.free(i64, c)
            return sum                            // exit status 14
        }
        None => return 1
    }
}
```

| item | description |
|------|-------------|
| `struct Chan(T: type)` | a channel of `T` values; a handle, so copies of it are the same channel; its fields are private |
| `fn new(comptime T: type, cap: i64) -> Option(Chan(T))` | a channel that buffers up to `cap` values; with `cap` 0 none, so that each sender waits for a receiver; None if there is no memory. A negative `cap` stops the program |
| `fn send(comptime T: type, c: Chan(T), v: T)` | sends `v`, waiting while the channel is full (unbuffered: until a receiver took it); sending on a closed channel stops the program |
| `fn recv(comptime T: type, c: Chan(T)) -> Option(T)` | the next value, waiting until there is one; None once the channel is closed and empty |
| `fn close(comptime T: type, c: Chan(T))` | no more values will be sent; closing twice stops the program |
| `fn count(comptime T: type, c: Chan(T)) -> i64` | the values sent and not yet received |
| `fn free(comptime T: type, c: Chan(T))` | gives the channel's memory back; nothing may use it afterwards |

- A channel is a ring buffer of bytes in memory from `malloc`, guarded by a
  [`sync.Mutex`](#sync), with one `sync.Cond` for receivers and one for
  senders. It holds bytes rather than `T`s so that one `Select` can wait on
  channels of different types; the functions that take `T` are typed doors
  to it.
- Any type can be sent; values are copied in and out, as by assignment.

### Select

A `Select` waits on several channels at once, like hosted programs'
[`select`](../language/tasks-and-channels.md#select). `select` is a keyword,
so the functions are `wait` and `poll`. Cases are added once and stay, so a
loop can wait on the same cases again and again:

```jihoo
let n: i64 = 0
let stop = false
let sel = channel.select_new()
let got_n = channel.on_recv(i64, &sel, numbers, &n)      // case 0
let got_stop = channel.on_recv(bool, &sel, quit, &stop)  // case 1
let running = true
while running {
    let i = channel.wait(&sel)       // the case that went ahead
    if !channel.received(&sel) {
        channel.off(&sel, i)         // closed: leave it out from now on
        running = i != got_n
    } else if i == got_n {
        total = total + n
    } else {
        running = false
    }
}
```

| item | description |
|------|-------------|
| `struct Select` | the cases of a select; its fields are private |
| `fn select_new() -> Select` | a Select without cases |
| `fn on_recv(comptime T: type, sel: *Select, c: Chan(T), dst: *T) -> i64` | adds a case that receives from `c` into `*dst`, and gives its number (cases count from 0) |
| `fn on_send(comptime T: type, sel: *Select, c: Chan(T), src: *T) -> i64` | adds a case that sends `*src` on `c`, read when the case goes ahead, and gives its number |
| `fn wait(sel: *Select) -> i64` | does the first case that can go ahead, waiting until one can, and gives its number |
| `fn poll(sel: *Select) -> i64` | the same without waiting: -1 if no case can go ahead, like a `_` arm |
| `fn wait_ms(sel: *Select, ms: i64) -> i64` | like `wait`, but -1 if no case could go ahead within `ms` milliseconds; each call starts its own clock, as `case <-time.After(d)` does in a Go loop. Zero or less is `poll`; with no case on, it only sleeps |
| `fn received(sel: *Select) -> bool` | whether the receive that `wait` or `poll` just did got a value (false: its channel was closed and empty, and `*dst` is unchanged) |
| `fn off(sel: *Select, i: i64)` | leaves case `i` out from now on, as Go programs do by setting a channel to nil |
| `const MAX_CASES = 16` | the most cases one Select has |

- A receive case can go ahead when its channel has a value, or is closed and
  empty. A send case can go ahead when its channel has room; on an
  unbuffered channel, when a receiver is waiting (in `recv`, or in a `wait`
  with a receive case on it), and then it does not wait for the receiver to
  take the value.
- If several cases can go ahead, the first added wins, as with hosted
  `select`.
- What `dst` and `src` point to must stay valid while the Select is used.
- Misuse stops the program: `wait` with no case on (it would wait forever),
  more than `MAX_CASES` cases, `off` with a number that is not a case, and a
  send case on a closed channel.
- While it waits, a Select links a node per case into its channels and
  sleeps on a futex word of its own, which a channel bumps whenever a value
  comes or goes or it closes; it then looks at its cases again. `wait_ms`
  gives the futex only the time left until its deadline, on
  [`thread.now_ns`](#thread)'s clock, so wakeups that change nothing do not
  make it wait longer.

A timeout in a loop, such as a heartbeat while waiting for work:

```jihoo
let i = channel.wait_ms(&sel, 1000)
while i == -1 {
    libc.printf("still waiting\n")     // once a second
    i = channel.wait_ms(&sel, 1000)
}
```
