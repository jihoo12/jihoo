# Traits

A trait names functions that a type can have. An `impl` gives them for one
type, and a generic function can ask for a type that has them.

```jihoo
trait Shape {
    fn area(s: Self) -> i64
    fn sides() -> i64

    fn describe(s: Self) -> i64 {          // a default, which impls may replace
        return Self.sides() * 1000 + s.area()
    }
}

struct Sq { w: i64 }

impl Shape for Sq {
    fn area(s: Sq) -> i64 { return s.w * s.w }
    fn sides() -> i64 { return 4 }
}

fn larger(comptime T: Shape + Ord, x: T, y: T) -> i64 {
    if x.less(y) { return y.area() }
    return x.area()
}

fn main() {
    let s = Sq { w: 3 }
    print(s.area())              // 9: a method call
    print(Shape.describe(s))     // 4009: the trait spelled out
    print(Sq.sides())            // 4: a function without `Self` first
}
```

## Declaring a trait

`trait Name { ... }` lists functions. In them `Self` stands for the type that
implements the trait. A function with a body has a default: an impl that does
not give the function gets the body, with `Self` its own type. Trait functions
cannot have `comptime` parameters.

A trait is an item like a struct: it may be `pub`, and other modules name it as
`alias.Name`.

## Impls

`impl Trait for Type { ... }` gives the trait's functions for `Type`. It must
give every function that has no default, and no others, each with the
trait's signature (`Self` as `Type`, which an impl may write either way).
Every impl in a program is checked, used or not.

- A type has at most one impl of a trait.
- The impl must be in the module of the trait or the module of the type. So
  `impl Ord for i64` can only be in the trait's module; a module that wants
  another order for `i64` wraps it in a struct of its own.
- An impl can be for a concrete instance of a generic type,
  `impl Ord for Pair(u8)`, or [generic](#generic-impls).
- An impl has no name and no `pub`: it applies wherever the trait and the type
  can both be seen.

## Generic impls

An impl with parameters, written as a generic struct's are, is one impl for
every type of a shape. With `struct Pair(T: type) { a: T, b: T }`:

```jihoo
impl(T: Ord) Ord for Pair(T) {
    fn compare(x: Pair(T), y: Pair(T)) -> i64 {
        let c = x.a.compare(y.a)
        if c != 0 { return c }
        return x.b.compare(y.b)
    }
}
```

`Pair(i64)` has an `Ord` because `i64` has one, and `Pair(Pair(i64))` then too.
To find the impl for a type, its shape is matched against the impl's type,
`Pair(T)` against `Pair(i64)` giving `T = i64`, and the impl applies when the
parameters' bounds hold. So `Pair(Point)` has no `Ord` unless `Point` has.

- The parameters are types, `T: type` or `T: Trait`, and each must appear in
  the impl's type.
- An impl is for a shape, not for any type: `impl(T: Shape) Ord for T` is an
  error.
- Two impls of a trait must not be able to apply to one type: a concrete impl
  for `Pair(i64)` and a generic one for `Pair(T)` are an error, and so are two
  generic impls for `Pair(...)`, whatever their parameters.
- A generic impl's functions are checked against the trait once per type,
  as the bodies of generic functions are; its other rules are checked where
  it is declared.

## Calling trait functions

| call | means |
|------|-------|
| `x.area()` | `area` of the type of `x`, from a trait the module can see, with `x` first |
| `Shape.area(x)` | `area` of `Shape`, for the type of `Self` the arguments give |
| `Sq.sides()`, `T.sides()` | `sides` of a trait the module can see, for type `Sq` or `T` |

A method call `x.f(...)` works for trait functions whose first parameter is
`Self`. The traits it looks in are the module's own, the prelude's, and the
public ones of the modules it imports. If `x` has a field `f`, the field comes
first: `x.f(...)` calls the function the field holds. If two traits have an
`f` for the type, the call is an error that asks for `Shape.f(x)`.

Parameters are values, as everywhere in jihoo: there is no `self` keyword and
no borrowing. A function that changes a value returns the new one, and the
caller writes `v = v.grown()`.

## Bounds

`comptime T: Shape` is a type parameter that only takes types with an
`impl Shape`; `comptime T: Shape + Ord` asks for both. `comptime T: type`
takes any type, as before. The bound is checked where the function is
instantiated, so a missing impl is reported at the call:

```text
error: main.jh:12:15: Point does not implement `Ord`, which `T` of `sort` needs
```

The parameters of generic structs and enums can have bounds the same way.
Types are [inferred](generics.md#inferred-type-arguments) as for any generic
function, so `larger(s, Sq { w: 4 })` needs no `Sq`.

Bodies are still checked per instance, as for every generic function.

## The standard traits

The prelude declares three traits, so every module can use them without an
import.

### `Ord`

An order. The prelude implements it for every number type, `str`, and, when their parts have one, `Option(T)` (`None` first), lists
(element by element, the shorter first when one is the start of the other) and
tuples of two or three parts (part by part):

```jihoo
pub trait Ord {
    fn compare(a: Self, b: Self) -> i64     // < 0, 0 or > 0
    fn less(a: Self, b: Self) -> bool       // compare(a, b) < 0
}
```

Floats are ordered by IEEE 754's total order, `-inf < ... < -0.0 < 0.0 < ... <
inf < NaN`, so that sorting them always ends; `<` on floats keeps its IEEE
meaning, in which NaN is unordered and `-0.0 == 0.0`.

A type with an `Ord` and no order of its own can be compared with `<`, `<=`,
`>` and `>=`: `a < b` is `a.compare(b) < 0`. So generic code written with
operators, such as `max` in [Generics](generics.md), works for it unchanged.
`==` stays [structural](equality.md) whatever `compare` says.

[`lists.sort`](../reference/standard-library.md#lists) sorts by it, so
`lists.sort(xs)` works for any element type with an `Ord`.

### `Hash`

Values that can be hashed, for [`maps`](../reference/standard-library.md#maps).
A hasher is a value that `hash` folds a value into and returns, so hashing
needs no pointers and works in every profile:

```jihoo
pub trait Hash {
    fn hash(x: Self, h: Hasher) -> Hasher
}

struct Point { x: i64, y: i64 }

impl Hash for Point {
    fn hash(p: Point, h: Hasher) -> Hasher {
        return p.y.hash(p.x.hash(h))       // the fields in turn
    }
}

print(hash_of(Point { x: 1, y: 2 }))       // a u64, the same in every profile
```

Equal values (by `==`) must hash the same, which folding every field that
`==` compares gives. The prelude implements `Hash` for integers, `bool`, `str`,
and lists, `Option` and tuples of hashable parts; not for floats, whose `==`
holds for `0.0` and `-0.0` and never for NaN.

### `Show`

Values that can be written as text:

```jihoo
pub trait Show {
    fn show(x: Self) -> str
}
```

`print(x)` prints `x.show()` for a value that is not a number, a `bool` or a
`str`, so a type with a `Show` prints as itself. The prelude implements it for
numbers, `bool` and `str` (the text itself), and for lists (`[1, 2]`),
`Option` (`Some(1)`, `None`), `Result` (`Ok(1)`, `Err(why)`) and tuples
(`(1, a)`) whose parts have one.

## Deriving

Writing `Show`, `Hash` or `Ord` field by field is mechanical, so the
[`meta`](../reference/standard-library.md#meta) library has an item macro that
writes the impls:

```jihoo
import meta

struct Version { major: i64, minor: i64 }
enum Shape { Circle(i64), Rect(i64, i64), Empty }

meta.derive!(Show, Hash, Ord, Version)
meta.derive!(Show, Ord, Shape)
```

- `Show` writes a struct as `Version { major: 1, minor: 2 }` and a variant as
  `Rect(1, 2)` or `Empty`.
- `Hash` folds in the fields in order, or the number of the variant and then
  its values.
- `Ord` compares the fields in order; variants come in the order they are
  declared, and the same variant compares its values in order.

Each part needs the trait itself; a part without one is an error where the
impl is first used, naming the part's type. The impls are written in the
calling module, as if by hand, so they follow the same rules as one written
there. `T` may be an instance of a generic type, `meta.derive!(Show, Pair(i64))`;
a generic impl for every `Pair(T)` is written by hand. The macro reads the
type with `meta.shape`, which works where the macro is called, and since
`meta` only runs while compiling, deriving costs a program nothing and works
in freestanding ones for `Hash` and `Ord`.

## How it works

Each function of an impl, and each default body, is compiled like a generic
function whose comptime parameter is `Self`: once per type, named
`impl.N.name.M` or `trait.Name.name.M` in JIR. A call through a trait is a
direct call of that instance, so traits add nothing to JIR, the VM or the
backend (`crates/jihoo-sema/src/traits.rs`). The design and what comes next
are in [Design: traits](../internals/traits.md).
