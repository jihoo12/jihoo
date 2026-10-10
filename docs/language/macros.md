# Macros

A macro is a function that runs at compile time and returns code. It is called
as `name!(...)`, and the code it returns replaces the call.

```jihoo
macro power(x: expr, n: i64) -> expr {
    let e = quote(1)
    let i = 0
    while i < n {
        e = quote($e * $x)
        i = i + 1
    }
    return e
}

macro expect(cond: expr) -> expr {
    return quote(report($cond, $(stringify(cond))))
}

fn main() {
    let y = 3
    print(power!(y + 1, 2))    // expands to ((1) * (y + 1)) * (y + 1): 16
}
```

## Declaring a macro

`macro name(params) -> kind { body }` declares a macro. The body is ordinary
jihoo, run on the VM while compiling (`crates/jihoo-sema/src/macros.rs`), and
the result kind says what code it returns:

| result | built with | used |
|--------|------------|------|
| `expr` | `quote(expression)` | as an expression |
| `stmts` | `quote { statements }` | as a statement of its own |
| `items` | `quote items { items }` | at the top level of a module |

Parameters of type `expr` receive the arguments as code, unevaluated. Integer,
bool and `str` parameters receive values computed at compile time.

Macros are module items like functions: `pub macro` exports one, and another
module calls it as `alias.name!(...)`. They are type checked even when unused,
and are never part of the compiled program.

## Quoting code

`quote(template)` builds code. The template is checked when the macro is
parsed, so it must be valid code of its kind. `$x` and `$(e)` are *holes*,
filled in when the macro runs:

- An `expr` is inserted in parentheses, so precedence cannot change.
- Integers, bools and strings are inserted as literals.
- `stringify(e)` gives the source text of code, as a `str`.

## Scope and hygiene

The code a macro returns is compiled where the macro was called, but macros are
hygienic: the names a template writes and the names the caller wrote are kept
apart.

- **Code from holes** (`$x`, `$(e)`) is the caller's, and means what it means
  where the macro was called.
- **Locals the template declares** (`let`, parameters of anonymous functions,
  pattern bindings) belong to that one expansion. They cannot capture the
  caller's variables of the same name, and the caller cannot see them after the
  macro. A template cannot use the caller's locals by name either: only through
  a hole.
- **Other names the template writes** (functions, constants, types, macros,
  module aliases) mean what they mean where the macro is *defined*. So a
  `pub macro` can use its own module's private functions and types and the
  modules it imports, wherever it is called. A name the macro's module does not
  have is looked up among the caller's items (not its locals), which lets item
  macros refer to the items they produce.
- **To declare a name the caller sees**, put it in a name position through a
  hole: `let $name`, `fn $name`, with a `str` or an `expr` that is a name.

```jihoo
macro add_tmp(x: expr) -> stmts {
    return quote {
        let tmp = 100                // the macro's own `tmp`
        print($x + tmp)              // `$x` is the caller's code
    }
}

macro define(name: str, v: i64) -> stmts {
    return quote { let $name = $v }  // a name for the caller
}

fn main() {
    let tmp = 1
    add_tmp!(tmp)                    // 101
    print(tmp)                       // 1
    define!("answer", 42)
    print(answer)                    // 42
}
```

```jihoo
// lib/util.jh
fn helper(x: i64) -> i64 { return x * 10 }     // private
pub macro scaled(x: expr) -> expr {
    return quote(helper($x))                    // `util`'s helper, wherever it is used
}
```

Field names, enum variant names, item names (`fn f` in an `items` template)
and the built-in types are not affected. How it works is in
[Compiler](../internals/compiler.md#macros).

Errors in produced code point at the call and say which macro produced it.
Expansion stops 64 levels deep, which catches a macro that expands to itself.

Macro bodies may use `str` even in native and freestanding programs, since they
only run on the VM; a `str` inserted into code becomes a string literal.

## Statement and item macros

```jihoo
macro swap(a: expr, b: expr) -> stmts {
    return quote {
        let tmp = $a                // the macro's own: `swap!(tmp, x)` works too
        $a = $b
        $b = tmp
    }
}

macro adders(n: i64) -> items {
    let out = quote items {}
    let i = 1
    while i <= n {
        out = quote items {
            $out
            fn $("add" + to_str(i))(x: i64) -> i64 { return x + $i }
        }
        i = i + 1
    }
    return out
}

adders!(3)            // at the top level: defines add1, add2, add3

fn main() {
    let x = 1
    let y = 2
    swap!(x, y)       // on a line of its own: three statements in this block
    print(add3(x))    // 5
}
```

- A `stmts` macro is used as a statement of its own. The variables it declares
  through name holes (`let $name`) stay visible after it; the ones its template
  names itself do not. An `items` macro is used at the top level of a module.
- A hole's position decides what it takes: in an expression, code
  (parenthesized) or a literal value; in a name position (`fn $name`, `let $v`,
  a struct name), a `str` that must be an identifier; as a statement of its own,
  `stmts` or `expr`; as an item of its own, `items`.
- Item macros are expanded before the program is analyzed, in rounds: each round
  analyzes lazily what the macros need, runs them, and adds the produced items
  to the module that called the macro. Produced code may call item macros, so
  this repeats, up to 16 rounds.

## Macro builtins

| builtin | gives |
|---------|-------|
| `quote(...)`, `quote { ... }`, `quote items { ... }` | code from a template |
| `stringify(e)` | the source text of code `e`, a `str` |
| `unique(prefix)` | a name that is new in the whole compilation, a `str`, for items or variables a macro makes for the caller |
| `ident(name)` | a name as code, for expression positions |
| `to_str(x)` | a number or bool as a `str`, handy for building names |

See `examples/macros.jh`.
