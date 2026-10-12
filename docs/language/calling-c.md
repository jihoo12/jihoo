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

Numbers, `bool`, pointers, function types made of those, and structs of those
cross into C, and `unit` as C's `void`. Enums, which C does not have, and arrays
go by pointer (`&p`).

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
| `struct { ... }` | a struct with the same fields, in order |

String literals are already NUL-terminated `*u8`s, so they can be passed
directly.

Arguments past `...` get C's default promotions: narrow integers and bools
widen to 32 bits, and `f32` to `f64`. So pass an `i64` with `%ld`, and an
`i32` with `%d`. Structs cannot be passed through `...`.

## Structs by value

A struct whose fields are numbers, `bool`s, pointers, function types, arrays of
those or such structs is a C struct: it has C's layout
([Structs](structs.md#layout)), and it is passed to and returned from C by
value, the way the target's C ABI says. A small struct goes in registers, a
large one in memory, and the backend does what a C compiler does for both
targets ([JIR](../jir.md#the-c-abi)).

```c
// geo.c
typedef struct { double x, y; } Vec2;
Vec2 vec2_scale(Vec2 v, double k) { return (Vec2){v.x * k, v.y * k}; }
```

```jihoo
#![native]
struct Vec2 { x: f64, y: f64 }

extern fn vec2_scale(v: Vec2, k: f64) -> Vec2

fn main() {
    let v = vec2_scale(Vec2 { x: 1.5, y: -2.0 }, 4.0)
    print(v.x + v.y)        // -2.0
}
```

A struct must have at least one field: C has no empty structs.

## Exporting functions

`export fn` defines a function that C code linked with the program can call, as
the C function of the same name:

```jihoo
#![native]
struct Vec2 { x: f64, y: f64 }

export fn vec2_len2(v: Vec2) -> f64 {
    return v.x * v.x + v.y * v.y
}
```

```c
typedef struct { double x, y; } Vec2;
double vec2_len2(Vec2 v);      // defined by the jihoo program
```

Its parameters and result are C types, as for `extern fn`, and it cannot have
`comptime` parameters. Inside the program it is an ordinary function. Each
symbol is exported once in a program, cannot also be declared `extern fn` and
called, and the entry point (`main`, `_start`) cannot be exported. `export fn`
works in freestanding programs too, whose objects can then be linked into
other code.

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
- A function value handed to C (a callback) takes and returns scalars only:
  it uses jihoo's own calling convention, which passes structs by pointer.
  Export a function to pass structs by value to it from C.
- C calls a function pointer and nothing else, so it cannot call a closure
  that captured values: passing one written right there is an error, and
  passing one from a variable stops the program. Pass the values another
  way, such as the `void *` argument most C callbacks get.
- A struct with a function value in it cannot go to C by value: natively a
  function value also holds what a closure captured.
- `print` and `libc.printf` write to the same buffered `stdout`, so they can be
  mixed.

See `examples/native.jh`.
