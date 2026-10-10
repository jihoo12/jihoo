# Calling C

Native programs can call any C function: those of libc (declared in the
standard library's `libc` module), and any other declared with `extern fn`.

```jihoo
#![native]
import libc                               // lib/libc.jh: printf, malloc, qsort, ...

extern fn labs(n: i64) -> i64             // or declare a C function yourself
extern fn printf(fmt: *u8, ...) -> i32    // `...`: variable arguments

fn main(argc: i32, argv: **u8) -> i64 {
    libc.printf("%s has %d arguments\n", argv[0], argc - 1)
    return labs(-3)
}
```

## `extern fn`

`extern fn name(params) -> R` declares a C function by its symbol name, with no
body; calls use the C calling convention. `...` after the parameters makes it
variadic, like `printf`. `pub extern fn` makes the declaration usable from other
modules.

Several modules may declare the same C function if they agree on its signature.
Only the C functions a program uses become part of it, so a library of
declarations such as `lib/libc.jh` costs nothing.

## Types across the boundary

Only numbers, `bool`, pointers and function types made of those cross into C,
and `unit` as C's `void`. Structs, enums and arrays go by pointer (`&p`), since
each C ABI passes them by value differently.

| C | jihoo |
|---|-------|
| `int` | `i32` |
| `long`, `ssize_t` | `i64` |
| `size_t` | `u64` |
| `float`, `double` | `f32`, `f64` |
| `char`, `unsigned char` | `u8` |
| `T *` | `*T` |
| `void *` | `*u8` |
| `void` (result) | no `-> R` |

String literals are already NUL-terminated `*u8`s, so they can be passed
directly.

Arguments past `...` get C's default promotions: narrow integers and bools
widen to 32 bits, and `f32` to `f64`. So pass an `i64` with `%ld`, and an
`i32` with `%d`.

## Callbacks

A jihoo function is a C function pointer:

```jihoo
fn compare(a: *u8, b: *u8) -> i32 {
    let x = *(a as *i64)
    let y = *(b as *i64)
    if x < y { return -1 }
    if x > y { return 1 }
    return 0
}

libc.qsort(&xs[0] as *u8, 8, 8, compare)
```

Callbacks should take and return floats, integers of at least 32 bits,
pointers, or nothing: jihoo's own calling convention matches C's for those
([JIR](../jir.md#native-abi)).

## Linking

`jihoo build` links native programs with the C compiler, which adds libc and
libm (`sqrt`, `sin`, ... are declared in `lib/libc.jh`). C files and libraries
can be linked in too:

```sh
jihoo build prog.jh util.c -lz -L/opt/lib
```

`.c`, `.o`, `.a` and `.so` files and `-l`/`-L` options are passed on to the C
compiler, `cc` unless `JIHOO_CC` names another ([Command line](../reference/command-line.md)).

## Limits

- C functions cannot run at compile time: `comptime` and macros run on the VM.
- Structs cannot be passed to or returned from C by value yet, and jihoo
  functions cannot be exported under C names; both are on the
  [roadmap](../internals/roadmap.md).
- There is no `print` in native programs; use `libc.printf`.

See `examples/native.jh`.
