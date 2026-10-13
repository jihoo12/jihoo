# Design: traits

Status: proposed; nothing is implemented yet.

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

- **In the body.** On a value of a bounded type parameter, trait functions
  resolve only through the declared bounds. `x.less(y)` with `x: T` and
  `T: Ord` finds `Ord.less`; `x.show()` is an error even when this instance's
  `T` happens to have a `Show`, so a body cannot depend on an impl it did not
  ask for. Built-in operators on `T` are still checked per instance, as now.

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
meaning: a field wins. Otherwise, `f` is looked up among:

- the bounds of `x`'s type, if it is a type parameter; otherwise
- the traits in the prelude and those the calling module declares or imports,
  that have an impl for `x`'s type and a function `f` whose first parameter is
  `Self`.

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

### 1. Inferring type arguments

Today every call to a generic function writes its types:
`lists.map(i64, i64, xs, f)`. Traits make more functions generic, so this comes
first.

When a call to a function with `comptime T: type` parameters gives exactly as
many arguments as it has runtime parameters, the type parameters are inferred:
each argument with a type of its own is unified with its parameter's type
(`[T]` with `[i64]` gives `T = i64`; `fn(T) -> U` with the type of an annotated
closure gives `U`); the remaining arguments, literals and closures without
annotations, are then checked against the parameter types with those bindings
as their expected types, as enum arguments are now
([Generic enums](../language/enums.md#generic-enums)). A parameter nothing
determines is an error that says so. Value parameters (`comptime N: i64`) are
never inferred. Writing all arguments out keeps working.

This needs no traits and shortens most of `lists` and `alloc` by itself. The
unifier is what step 3 matches generic impls with.

### 2. Traits, impls and bounds

`trait`, `impl`, `Self`, bounds on function parameters, default bodies, method
calls and `Trait.f(x)` / `Type.f(x)` calls, impls for concrete types only.
`trait` and `impl` become keywords; nothing in `lib/`, `examples/` or `tests/`
uses either as a name.

Done when `lists.sort(xs)` takes no order, `Ord` has impls for every integer
type and `str`, and a missing impl is reported at the call.

### 3. Generic impls and bounded structs

`impl(T: Ord) Ord for Option(T)`, impls for lists and tuples, bounds on
struct and enum parameters, and the overlap check.

Done when `lists.sort` sorts lists of `Option(i64)` and of tuples.

### 4. The standard traits

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

### 5. Comparison operators through `Ord`

`<`, `<=`, `>` and `>=` on a struct or enum with an `impl Ord` become calls of
`compare`, so `max(a, b)` works for `Version` with its body unchanged. This is sema
only: operator typing in `crates/jihoo-ir/src/types.rs` is unchanged, because
the verifier sees calls. Arithmetic operators are not overloaded.

### 6. Deriving

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

## Later

- **Associated types**, such as an `Iterator` with an `Item`, and `for` over
  any type with an `Iterator` impl. The value-only rule above shapes it:
  `fn next(it: Self) -> Option((Self, Item))`. Worth doing once the standard
  traits show whether associated types are needed elsewhere.
- **Supertraits** (`trait Ord: Hash`), when a standard trait needs one.
- **Trait functions with comptime parameters of their own.** Allowed by the
  checking model, but not needed by any trait above; left out until one is.
- **Dynamic dispatch.** A value of "some type with `Show`" chosen at run time.
  It can already be written as a struct of closures that capture the value,
  in hosted and native code; a `dyn` type would generate that struct. Not
  before there is a program that needs it.

## Done when

- `lists.sort(xs)`, a new `lists.max(xs)` and `Map(K, V)` work with user types in
  hosted and native programs, and `Ord` and `Hash` in freestanding ones;
- `tests/diff/` has programs with traits and impls that print the same in all
  three profiles, and `tests/diff-lists/` the same for `Map` and `Show`;
- the fuzzer writes random impls and calls through bounds;
- the JIR version is unchanged.

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
