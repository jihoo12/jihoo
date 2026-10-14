# Design: traits

Status: the six steps are done, and so is [Done when](#done-when); so are two
of the later ones, [associated types and `Iterator`](#7-associated-types-and-iterator-done)
and [supertraits](#8-supertraits-done). What is left is in [Later](#later).

## Why

Generic code in jihoo has no way to say what it needs from a type. A
`comptime T: type` parameter accepts every type, and the body is checked per
instance, as C++ templates are. That works, and it is what made generics cheap
to add, but three things follow from it:

1. **Behaviour is passed by hand.** `lists.sort` cannot use `<` on any `T`, so
   every caller passes the order:

   ```jihoo
   lists.sort(i64, xs, fn(a: i64, b: i64) -> bool { return a < b })
   ```

   A hash map, which the roadmap asks for, would need a hash function and an
   equality at every call, or a struct of functions carried in every map.
2. **Errors point inside the library.** `max(Point, p, q)` fails at the
   `>` in `max`'s body, "in `max` with T = Point", not at the call that passed
   a type without an order.
3. **There is no printing, hashing or ordering of user types.** `print` takes
   numbers, `bool` and `str`; structs and enums have `==` (built in,
   structural, [Equality](../language/equality.md)) and nothing else.

Every library written before traits exist bakes in one of the workarounds
above, and changing them afterwards changes their signatures. That is why
traits come before v1.

The goal is to fix these three without giving up what generics are now:
monomorphized, so a call through a trait is a direct call; checked per
instance, so sema stays one pass; and invisible to JIR, so the VM, the backend
and the ownership pass do not change.

## The design in one example

```jihoo
// lib/prelude.jh
pub trait Ord {
    // Less than zero, zero or more than zero, as `a` is before, equal to or after `b`.
    fn compare(a: Self, b: Self) -> i64

    fn less(a: Self, b: Self) -> bool {         // a default, which impls may replace
        return Self.compare(a, b) < 0
    }
}

impl Ord for i64 {
    fn compare(a: i64, b: i64) -> i64 { ... }
}

// a user's module
struct Version { major: i64, minor: i64 }

impl Ord for Version {
    fn compare(a: Version, b: Version) -> i64 {
        if a.major != b.major { return a.major.compare(b.major) }
        return a.minor.compare(b.minor)
    }
}

// lib/lists.jh
pub fn sort(comptime T: Ord, xs: [T]) -> [T] {
    ...
    if src[j].less(src[i]) { ... }
    ...
}

let vs = lists.sort(versions)          // T = Version, inferred (step 1)
```

- `trait` declares functions over `Self`, the type that implements it, with
  optional default bodies.
- `impl Trait for Type { ... }` gives the functions for one type. It has no
  name and no `pub`: it applies wherever the trait and the type can both be
  seen.
- `comptime T: Ord` is a type parameter that only accepts types with an
  `impl Ord`. `comptime T: Ord + Hash` asks for both. `comptime T: type`
  stays the unconstrained form.
- `x.less(y)` calls the trait function whose first parameter is `Self`, for
  the type of `x`; `Ord.less(x, y)` and `Version.less(x, y)` spell the same
  call out. A function without a `Self` first parameter, such as
  `fn zero() -> Self`, is called as `T.zero()`.

## Semantics

### What an impl is

An impl is a set of ordinary functions. Sema gives each one a name that no
user function can have, `impl.N.compare`, as it does for equality helpers
(`fn.eq.N`): `impl` is a keyword, so the name never collides. A call through a
trait is a direct call to that function. There are no vtables, no dictionaries
passed at run time and no new instructions, so JIR does not change and needs no
version bump; the VM runs impls, the backend compiles them and the ownership
pass moves and frees their arguments like any other calls.

Since impls are ordinary functions, they also run at compile time: `const`,
`comptime` and macros can use them, within the usual limits of the VM.

### Checking

Bodies stay checked per instance (`crates/jihoo-sema/src/generic.rs`).
Checking a generic body once, against its bounds alone, as Rust does, would
need sema to type code without knowing its types, which a one-pass checker that
lowers as it goes cannot do. Traits instead add two checks around the
per-instance one:

- **At the call.** `Env::bind` turns a type argument into a binding; for a
  bounded parameter it first looks up an impl of each bound for that type, and
  fails at the call when one is missing:

  ```text
  error: main.jh:12:15: `Point` does not implement `Ord`, which `T` of `lists.sort` needs
  ```

- **In the body.** Trait functions resolve as anywhere else, through the
  traits the module can see; the bound only guarantees that the impl exists.
  The proposal was to resolve them on a value of type `T` only through `T`'s
  bounds, so that a body could not depend on an impl it did not ask for. Step 2
  found that this cannot be checked per instance: in `sort` with `T = i64`, a
  value of type `T` and any other `i64` have the same type, and nothing tells
  them apart. A body that uses a trait it did not ask for still works for the
  types that have it, and fails, inside its body, for the others, as a body
  using `<` on `T` does. Checking that would need sema to know which values
  have type `T`, not just `i64`.

The error for a missing impl is now at the call, with the bound named, which
fixes gap 2 for every library that states its bounds.

### Finding impls: coherence

There is at most one impl of a trait for a type in a program, and it must be
in the module of the trait or the module of the type (the orphan rule). For
built-in types, `i64`, `str`, `[T]`, tuples, the only home is the trait's
module.

This keeps lookup local, which matters because modules are analyzed lazily
(`crates/jihoo-sema/src/env.rs`): to find `impl Ord for Version`, sema looks in
two modules, the trait's and the type's, both already loaded because the code
names both. Nothing scans the whole program, and an import elsewhere can never
change which impl a call uses. Two impls of the same trait for the same type in
those two modules are an error at the second.

The cost is that a module cannot add `Show` to another module's type. That is
the same rule as Rust's, and a wrapper struct is the way around it.

### Generic impls

```jihoo
impl(T: Ord) Ord for Option(T) { ... }       // Option(i64) is Ord, Option(Point) is not
impl(T: Show) Show for [T] { ... }
impl(A: Show, B: Show) Show for (A, B) { ... }
```

The parameters are written as a generic struct's are. To find an impl for
`Option(i64)`, sema matches the impl's type, `Option(T)`, against it, binds
`T = i64`, checks `T`'s bounds, and instantiates the impl's functions as
generic functions with those bindings. Matching is the same unification that
infers type arguments (step 1). Two generic impls that could match the same
type are rejected when the second is declared, so the lookup never has to
choose.

Generic structs and enums may bound their parameters too,
`struct Map(K: Hash, V: type)`, checked where an instance is named.

### Method calls

`x.f(args)` already means calling a field `f` of function type. It keeps that
meaning: a field wins. Otherwise, `f` is looked up among the traits in the
prelude and those the calling module declares or imports (public ones), that
have an impl for `x`'s type and a function `f` whose first parameter is `Self`.
A bounded type parameter is no exception (see [Checking](#checking)).

None is an error naming the type; more than one is an error asking for the
spelled-out form, `Show.show(x)`. Methods are never found through traits the
module cannot see, so adding a trait to some other module never breaks a call.

There is no `self` keyword and no implicit borrowing. `Self` parameters are
values, as every parameter is. A function that changes a value returns the new
one and the caller writes `it = it.advance()`; natively the ownership pass
makes that a consuming call that copies nothing (commit "Replacing a parameter
copies nothing"), and on the VM copy-on-write does the same. So traits need no
`&mut self`, and none of the questions it raises.

Only trait functions get method syntax. Making every function callable as
`x.f()` (uniform call syntax) is a separate question and not part of this.

### Hygiene

A method call in a macro's template resolves by the type of the value, so it
needs nothing new from hygiene. The trait names it may consult are those of the
module where the macro is defined, as for every other name the template writes.

## Steps

Each step is useful alone, and the libraries are the measure: after each, some
signature in `lib/` gets shorter or some workaround goes away.

### 1. Inferring type arguments (done)

Every call to a generic function used to write its types:
`lists.map(i64, i64, xs, f)`. Traits make more functions generic, so this came
first.

When a call to a function with `comptime T: type` parameters gives exactly as
many arguments as it has runtime parameters, the type parameters are inferred:
each argument with a type of its own is unified with its parameter's type
(`[T]` with `[i64]` gives `T = i64`; `fn(T) -> U` with the type of an annotated
closure gives `U`); the remaining arguments, literals and closures without
annotations, are then checked against the parameter types with those bindings
as their expected types, as enum arguments are
([Generic enums](../language/enums.md#generic-enums)). Value parameters
(`comptime N: i64`) are never inferred. Writing all arguments out keeps working.

Done ([Inferred type arguments](../language/generics.md#inferred-type-arguments)),
in `crates/jihoo-sema/src/infer.rs`, without changing JIR. Two choices the
design left open: a literal that no other argument decides takes its default
type, as in `let`, so `max(3, -5)` is an `i64` instance rather than an error;
and an anonymous function may leave out exactly the types other arguments
decide, so `lists.sort_by(xs, fn(a, b) { return a < b })` needs no annotations
while `lists.map(xs, fn(v) { ... })` needs `-> U`. `lists`, `examples/words.jh`
and `examples/pipe.jh` call the short form; `tests/diff/inferred_types.jh`
checks it in all three profiles. The unifier (`FnCx::unify`) is what step 3
will match generic impls with.

### 2. Traits, impls and bounds (done)

`trait`, `impl`, `Self`, bounds on function parameters, default bodies, method
calls and `Trait.f(x)` / `Type.f(x)` calls, impls for concrete types only.
`trait` and `impl` become keywords; nothing in `lib/`, `examples/` or `tests/`
uses either as a name.

Done when `lists.sort(xs)` takes no order, `Ord` has impls for every integer
type and `str`, and a missing impl is reported at the call.

Done ([Traits](../language/traits.md)), in `crates/jihoo-sema/src/traits.rs`, with
no change to JIR. Each function of an impl, and each default body, is compiled
like a generic function whose one comptime parameter is `Self`, so instances,
their names and their compilation are the ones generic functions have. Choices
made on the way:

- Every impl is checked, used or not: its trait, the orphan rule, duplicates,
  missing and extra functions, and each function's signature against the
  trait's. The prelude's impls are the exception, since `impl Ord for str`
  cannot be checked in a freestanding program, which has no `str`; the
  compiler's tests check them instead.
- `Trait.f(args)` infers `Self` from the arguments with the unifier of step 1;
  `T.f(args)` and `Type.f(args)` name it.
- A bound on a struct or enum parameter is checked too, though generic impls
  (step 3) are what make it useful.
- `lists.sort(xs)` sorts by `Ord`, and the old `sort`, which takes an order,
  is `sort_by`.

`tests/diff/traits.jh` and `tests/diff-lists/traits.jh` check traits in every
profile, and `traits_and_impls` and `trait_errors` in
`crates/jihoo-sema/src/tests.rs` the rules and the messages.

### 3. Generic impls and bounded structs (done)

`impl(T: Ord) Ord for Option(T)`, impls for lists and tuples, bounds on
struct and enum parameters, and the overlap check.

Done when `lists.sort` sorts lists of `Option(i64)` and of tuples.

Done ([Generic impls](../language/traits.md#generic-impls)). The prelude orders
`Option(T)`, lists and tuples of two and three parts. Matching an impl's type
against a type is the unifier of step 1, moved to `Env` and given a strict
mode, in which a part that differs fails the match instead of being left for
later. Choices made on the way:

- A generic impl's signatures are checked per instance, like generic bodies:
  checking them once would need types for `T` that are not any type.
- The overlap check is conservative. A concrete impl overlaps a generic one
  when the generic one matches its type, bounds aside; two generic impls
  overlap when their types have the same outermost constructor, so
  `Pair(T, i64)` and `Pair(i64, T)` are rejected, but so are `Pair(T, i64)`
  and `Pair(T, u8)`, which could not meet.
- An impl's parameters must be types that appear in its type, and an impl
  cannot be for a bare parameter (`impl(T: A) B for T`), which would overlap
  every other impl of `B`.
- `E.name(...)` on an enum calls a trait function when the enum has no
  variant `name`, so a default body's `Self.compare(a, b)` works when `Self`
  is `Option(i64)`.

`generic_impls` and `generic_impl_errors` in `crates/jihoo-sema/src/tests.rs`,
`tests/diff/generic_impls.jh` and `tests/diff-lists/traits.jh` check them.

### 4. The standard traits (done)

- `Ord` in the prelude, as above. Floats get `compare` by IEEE 754
  `totalOrder` (`-NaN < -inf < ... < -0.0 < 0.0 < ... < inf < NaN`), so
  sorting floats always terminates; `<` on floats keeps its IEEE meaning.
- `Hash` in the prelude: `fn hash(x: Self, h: Hasher) -> Hasher`, folding
  values into a hasher that is itself a value, so it works without pointers in
  hosted code. Impls for integers, `bool`, `str`, lists, tuples, `Option`.
- `Show` in the prelude: `fn show(x: Self) -> str`. `print(x)` on a type that
  is not a number, `bool` or `str` becomes `print(x.show())`; since sema
  analyzes lazily, a freestanding program that never uses `Show` never
  compiles its `str` impls.
- `lib/maps.jh`: `Map(K: Hash, V: type)`, a hash map on lists, which is the
  roadmap's "map keyed by `str`" for any key that has `Hash` and `==`.

`==` stays built in and structural; there is no `Eq` trait, because there is
no type whose equality a user should be able to make disagree with its
structure without wrapping it.

Done ([The standard traits](../language/traits.md#the-standard-traits),
[`maps`](../reference/standard-library.md#maps)). As planned, with these
choices:

- `hash_of(x) -> u64` gives a hash: it starts the hasher, which only the
  prelude can make, and ends with splitmix64's finish. Each word is folded
  in by a multiply and a shift. The hashes are the same in every profile,
  which `tests/diff/hashing.jh` checks, so a map's order is too.
- Floats have no `Hash`, since `==` on them is not structural (`0.0 ==
  -0.0`, NaN unequal to itself).
- `Show` is display, not debugging: a `str` shows as itself, so `("a", 1)`
  shows as `(a, 1)`. `Result` has one too.
- The prelude's plain functions and types (`Hasher`, and `mix`, which folds a
  word in) are compiled only when a program uses them, as its generic items
  always were, so a program that does not hash has neither in its JIR. A
  module's own trait hides the prelude's of the same name, as its other items
  do.
- `maps` has `new`, `size`, `get`, `has`, `set`, `remove`, `keys`, `values`
  and `entries`, on buckets of `(K, V)` lists that double when there are
  more than two keys a bucket. `m = maps.set(m, k, v)` is a consuming call
  natively: 200 000 inserts and lookups take a fraction of a second.

`standard_traits` in `crates/jihoo-sema/src/tests.rs`, `tests/diff/hashing.jh`
and `tests/diff-lists/maps.jh` (also under valgrind) check them.

### 5. Comparison operators through `Ord` (done)

`<`, `<=`, `>` and `>=` on a struct or enum with an `impl Ord` become calls of
`compare`, so `max(a, b)` works for `Version` with its body unchanged. This is sema
only: operator typing in `crates/jihoo-ir/src/types.rs` is unchanged, because
the verifier sees calls. Arithmetic operators are not overloaded.

Done. A comparison whose operands have the same type and no order of their
own becomes a call of the prelude's `compare` and an `i64` comparison with 0;
such a type without an `Ord` gets an error that says to give it one. It
works for anything with an impl, generic ones included: tuples, lists and
options order when their parts do. `==` is not part of it, and stays
structural (`tests/diff/orderings.jh`, `orderings_through_ord`).

### 6. Deriving (done)

Writing `impl Show` and `impl Hash` field by field is what macros are for, but
a macro cannot yet read a struct's fields: `type_of` gives the type's name, not
its shape. One more compile-time function, `meta.fields(t: str) -> [(str, str)]`
(names and type spellings, `#type:N` as `type_of` gives them), lets an item
macro write the impl:

```jihoo
derive!(Show, Hash, Version)
```

This belongs to [code as data](code-as-data.md) as much as to traits: it is
the "deriving equality, printing or serialization" that design names as a
reason for macros to see types.

Done ([Deriving](../language/traits.md#deriving)), as `meta.derive!(Show, Hash,
Ord, T)`, for structs and enums. Field types turned out not to be needed:
an impl only names the parts (`x.a.show()`), and each part's own impl is
found where the impl is used. So the compiler gives less than proposed:

- `type_shape(t)`, a builtin for macros, gives the field names of a struct,
  or the variants of an enum with how many values each holds, as text. It
  asks the compiler the way `type_of` does, with the code marked `#shape `,
  so neither the VM nor JIR changed. `meta.shape(t)` reads the text into a
  `Shape`.
- `to_items(s)` turns text into items, which an item macro returns. The
  impls are built as text rather than with `quote`, so their names are
  resolved in the calling module: the traits are the ones it sees, as if the
  impl were written there.
- Deriving for a generic struct (`impl(T: Show) Show for Pair(T)`) was left
  out at first; an instance (`Pair(i64)`) worked. It came later:
  `type_shape` of a generic type's bare name reads its declaration and
  gives its parameters too (`struct(A,B) a b`), `meta.params` reads them,
  and `meta.derive!` writes the impl with every parameter bounded by the
  trait, as Rust's `derive` does. The functions write their parameters as
  `Self`, so the impl's body is the same either way.

On the way, `print` became usable in compile-time code of freestanding
programs, which runs on the VM, so `meta.derive!` can report its errors
there; and a method called on a variant (`Shape.Empty.compare(x)`) takes the
variant as a value.

### 7. Associated types and `Iterator` (done)

`type Item` in a trait, `type Item = T` in an impl, `Self.Item` and `T.Item`
as types; the prelude's `Iterator`, and `for` over any type with one.

```jihoo
pub trait Iterator {
    type Item
    fn next(it: Self) -> Option((Self, Self.Item))
}
```

Done ([Associated types](../language/traits.md#associated-types),
[`Iterator`](../language/traits.md#iterator)), without changing JIR. Choices
made on the way:

- `T.Item` is resolved where a type is, in `Env::resolve`: the part before
  the dot names a type (a parameter, `Self` or a type's name, but not a
  module), and the item is looked up among the traits the module can see
  that have such a type and an impl for it, as a method is. The impl's
  `type Item = ...` is resolved with the impl's parameters bound, so a
  generic impl gives `Item` per instance. Two traits that could give it are
  an error; there is no `<T as Trait>.Item` yet.
- Inference treats `I.Item` as unknown until `I` is known, so
  `fn total(comptime I: Iterator, it: I) -> I.Item` infers `I` from `it`
  alone.
- An impl's types are checked before its functions, whose signatures may
  name them, and a missing one is reported as such rather than as a
  signature that does not resolve.
- `for x in it` calls `next` on `it`, and on the rest each round gives, until
  `None`; `for i, x in it` counts the rounds. The loop starts from a copy, so
  the variable `it` is unchanged, as with lists.
- An iterator that holds a list must not copy it each round. The value
  `next` returns, `Some((rest, x))`, is only taken apart, so the ownership
  pass now moves parts out of a value that is only taken apart: each of its
  owning parts read once, in one block, after which the value is dead. Only
  the parts left in it are freed. Before, the parts were views of the value
  read from, and became deep copies once they outlived it, as `rest` does:
  walking 200 000 items took 25 s natively, and now takes a few milliseconds.
  The same holds for a `match` written by hand.
- An adapter (`lib/iters.jh`) holds another iterator, which its `next` reads
  out of its parameter. A parameter is borrowed, so that part was copied
  every round. A function whose parameter is only taken apart, and that
  would copy a part, now clones the parameter on entry instead, which makes
  it a function with a consuming version (`f.own`); a caller whose argument
  is at its last use, as a `for` loop's is, hands it over, and the parts move
  out of it as out of any value taken apart. The adapters take their
  iterator apart first (`let inner = m.inner`) and build the rest anew, which
  gives the pass that shape.

`associated_types_and_iterators` and `associated_type_errors` in
`crates/jihoo-sema/src/tests.rs`, `tests/diff/iterators.jh` and
`tests/diff-lists/iterators.jh` (under valgrind) check them, and the fuzzer
walks an iterator, `Rep(T)`, holding values of random types.

### 8. Supertraits (done)

`trait Greet: Named + Ord` asks every type with `Greet` to have `Named` and
`Ord`.

Done ([Supertraits](../language/traits.md#supertraits)). The check is where
impls are checked: an impl for a type is an error without impls of the
supertraits for it; a generic impl is checked per type where it is used, as
its signatures are. A module that can see a trait can also use its
supertraits' functions, so a bound `T: Greet` is enough to call `x.name()`.
Bounds are not widened otherwise, since impls are found by type and not by
bound (see [Checking](#checking)). A trait that asks for itself, directly or
through others, is an error.

`supertraits` and `supertrait_errors` check them.

## Later

- **Trait functions with comptime parameters of their own.** Allowed by the
  checking model, but not needed by any trait above; left out until one is.
- **Dynamic dispatch.** A value of "some type with `Show`" chosen at run time.
  It can already be written as a struct of closures that capture the value,
  in hosted and native code; a `dyn` type would generate that struct. Not
  before there is a program that needs it.

## Done when

- `lists.sort(xs)`, a new `lists.max(xs)` and `Map(K, V)` work with user types in
  hosted and native programs, and `Ord` and `Hash` in freestanding ones (done);
- `tests/diff/` has programs with traits and impls that print the same in all
  three profiles, and `tests/diff-lists/` the same for `Map` and `Show` (done);
- the fuzzer writes random impls and calls through bounds (done);
- the JIR version is unchanged (done: still 15).

## Costs and risks

- **Name lookup.** Method calls make `x.f` depend on the type of `x`, on fields
  and on the traits in scope. The lookup order above is the whole rule; every
  ambiguity is an error with the spelled-out form in the message.
- **Error messages in instances.** Bodies are still checked per instance, so
  an error that is not about a bound still says "in `sort` with T = Version".
  The bounds move the common errors to the call; they do not remove the
  others.
- **Compile time.** Lookup touches two modules, and impl functions are
  instantiated like generic functions already are; no new kind of work.
- **Keywords.** `trait` and `impl` stop being usable as names.

Not goals: checking generic bodies before they are instantiated; run-time
dispatch tables; overloading arithmetic; impls outside the trait's or the
type's module.
