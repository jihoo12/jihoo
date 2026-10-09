use super::*;

fn check(src: &str) -> Result<ir::Module, String> {
    let prog = jihoo_syntax::parse(src).map_err(|e| e.to_string())?;
    let m = analyze(&prog).map_err(|es| es[0].to_string())?;
    if let Err(e) = ir::verify(&m) {
        panic!("sema produced invalid IR: {e}\n{m}");
    }
    Ok(m)
}

fn err(src: &str) -> String {
    check(src).expect_err("expected a type error")
}

/// Wraps `body` in a freestanding program.
fn fs(body: &str) -> String {
    format!("#![freestanding]\n{body}\nfn _start() {{}}")
}

#[test]
fn well_typed_programs_pass() {
    check(
        "fn fib(n: i64) -> i64 {\n  if n < 2 { return n }\n  return fib(n - 1) + fib(n - 2)\n}\n\
         fn main() { let s = \"a\" + \"b\"\n print(s == \"ab\" && fib(3) == 2) }",
    )
    .unwrap();
    check("#![freestanding]\nfn _start() -> i64 { let p = \"hi\" + 1\n return syscall(1, 1, p, 1) }")
        .unwrap();
}

#[test]
fn infers_let_types() {
    assert!(err("fn main() { let x = 1\n x = \"s\" }").contains("must be i64, found str"));
    assert!(err("fn main() { let x: bool = 1 }").contains("must be bool, found i64"));
}

#[test]
fn operator_errors() {
    assert_eq!(err("fn main() { print(1 + true) }"), "1:21: cannot apply `+` to i64 and bool");
    assert!(err("fn main() { print(!1) }").contains("must be bool") || err("fn main() { print(!1) }").contains("`!`"));
    assert!(err("fn main() { print(1 && true) }").contains("left side of `&&` must be bool"));
}

#[test]
fn conditions_must_be_bool() {
    assert!(err("fn main() { if 1 { } }").contains("`if` condition must be bool"));
    assert!(err("fn main() { while 0 { } }").contains("`while` condition must be bool"));
}

#[test]
fn calls_are_checked() {
    let src = "fn f(a: i64, b: str) {}\nfn main() { f(1, 2) }";
    assert!(err(src).contains("argument 2 of `f` must be str, found i64"));
    assert!(err("fn main() { let x = main() }").contains("type unit"));
}

#[test]
fn returns_are_checked() {
    assert!(err("fn f() -> i64 { return true }\nfn main() {}").contains("return value must be i64"));
    assert!(err("fn f() -> i64 { return }\nfn main() {}").contains("missing return value"));
    assert_eq!(
        err("fn f(x: bool) -> i64 {\n  if x { return 1 }\n}\nfn main() {}"),
        "3:1: missing `return`: `f` must return i64"
    );
    // Both branches return, so the end is unreachable.
    check("fn f(x: bool) -> i64 {\n  if x { return 1 } else { return 2 }\n}\nfn main() {}").unwrap();
}

#[test]
fn profile_rules() {
    assert!(err("#![freestanding]\nfn _start() { print(1) }").contains("not available in freestanding"));
    assert!(err("fn main() { syscall(60, 0) }").contains("only available in freestanding"));
    assert!(err(&fs("fn f(s: str) {}")).contains("garbage collected"));
    assert!(err("fn f(p: *u8) {}\nfn main() {}").contains("only available in freestanding"));
    assert!(err("fn main() { let x = 1\n let p = &x }").contains("only available in freestanding"));
    assert!(err(&fs("fn f(p: ptr) {}")).contains("written `*u8`"));
    assert!(err("fn start() {}").contains("needs `fn main()`"));
    assert!(err("fn main() -> bool { return true }").contains("must return nothing or i64"));
}

#[test]
fn reports_errors_from_every_function() {
    let prog = jihoo_syntax::parse("fn a() { 1 + true }\nfn b() { if 1 {} }\nfn main() {}").unwrap();
    assert_eq!(analyze(&prog).unwrap_err().len(), 2);
}

#[test]
fn unreachable_blocks_are_removed() {
    let m = check("fn main() {\n  return\n  print(1)\n}").unwrap();
    assert_eq!(m.funcs[0].blocks.len(), 1);
}

#[test]
fn text_format_snapshot() {
    let m = check("fn add(a: i64, b: i64) -> i64 {\n  return a + b\n}\nfn main() {}").unwrap();
    assert_eq!(
        m.funcs[0].to_string(),
        "fn @add(i64, i64) -> i64 {\n  regs i64 i64 i64\nbb0:\n  %2 = add %0, %1\n  ret %2\n}\n"
    );
}

// ---- sized integers ----

#[test]
fn literals_take_their_type_from_context() {
    check("fn f(x: u8) -> u8 { return x + 1 }\nfn main() { let y: i16 = -300\n let z = f(2) * 3 }").unwrap();
    check("fn main() { let x: u32 = 7\n print(1 + x == 8) }").unwrap();
    assert!(err("fn main() { let x: u8 = 256 }").contains("integer literal 256 does not fit in u8"));
    assert!(err("fn main() { let x: i8 = -129 }").contains("-129 does not fit in i8"));
    check("fn main() { let x: i8 = -128 }").unwrap();
}

#[test]
fn integer_types_do_not_mix() {
    assert!(err("fn main() { let a: u8 = 1\n let b = 2\n print(a + b) }").contains("cannot apply `+` to u8 and i64"));
    assert!(err("fn main() { let a: u8 = 1\n print(-a) }").contains("cannot apply `-` to u8"));
}

#[test]
fn casts() {
    check("fn main() { let a: u8 = 200\n let b = a as i64 + 1\n let c = true as u8\n print(b) }").unwrap();
    check(&fs("fn f(p: *u8) -> i64 { let q = p as *i64\n return p as i64 }")).unwrap();
    assert!(err("fn main() { let s = \"x\" as i64 }").contains("cannot cast str to i64"));
    assert!(err(&fs("fn f(p: *u8) -> i32 { return p as i32 }")).contains("cannot cast *u8 to i32"));
}

// ---- structs ----

const POINT: &str = "struct Point { x: i64, y: i64 }\nstruct Line { a: Point, b: Point }\n";

#[test]
fn struct_literals_and_fields() {
    check(&format!(
        "{POINT}fn len2(l: Line) -> i64 {{\n  let dx = l.b.x - l.a.x\n  let dy = l.b.y - l.a.y\n  return dx * dx + dy * dy\n}}\n\
         fn main() {{ print(len2(Line {{ a: Point {{ x: 0, y: 0 }}, b: Point {{ y: 4, x: 3 }} }})) }}"
    ))
    .unwrap();
    assert!(err(&format!("{POINT}fn main() {{ let p = Point {{ x: 1 }} }}")).contains("missing fields in `Point`: y"));
    assert!(err(&format!("{POINT}fn main() {{ let p = Point {{ x: 1, x: 2, y: 3 }} }}")).contains("given twice"));
    assert!(err(&format!("{POINT}fn main() {{ let p = Point {{ x: 1, z: 2 }} }}")).contains("has no field `z`"));
    assert!(err(&format!("{POINT}fn main() {{ let p = Point {{ x: true, y: 2 }} }}")).contains("field `x` must be i64"));
    assert!(err("fn main() { let x = 1\n print(x.y) }").contains("type i64 has no fields"));
}

#[test]
fn nested_field_assignment_rebuilds_the_struct() {
    let m = check(&format!("{POINT}fn main() {{\n let l = Line {{ a: Point {{ x: 0, y: 0 }}, b: Point {{ x: 0, y: 0 }} }}\n l.b.y = 5\n}}"))
        .unwrap();
    let text = m.funcs[0].to_string();
    // l.b.y = 5  =>  t = field l, 1; t2 = setfield t, 1, 5; l = setfield l, 1, t2
    assert!(text.contains("= field %"), "{text}");
    assert_eq!(text.matches("setfield").count(), 2, "{text}");
    assert!(err(&format!("{POINT}fn p() -> Point {{ return Point {{ x: 0, y: 0 }} }}\nfn main() {{ p().x = 1 }}"))
        .contains("cannot assign"));
}

#[test]
fn structs_cannot_contain_themselves() {
    assert!(err("struct A { b: B }\nstruct B { a: A }\nfn main() {}").contains("contains itself"));
    check(&fs("struct Node { value: i64, next: *Node }")).unwrap();
    assert!(err("struct i64 { x: bool }\nfn main() {}").contains("builtin type name"));
    assert!(err("struct P { x: i64, x: i64 }\nfn main() {}").contains("declared twice"));
}

#[test]
fn struct_literal_needs_parens_in_conditions() {
    // `P { ... }` in an `if` condition is not a struct literal: `P` is the condition
    // and `{ x: 1 }` the body, which does not parse.
    assert!(check("struct P { x: i64 }\nfn main() { if P { x: 1 }.x == 1 {} }").is_err());
    check("struct P { x: i64 }\nfn main() { if (P { x: 1 }).x == 1 {} }").unwrap();
}

// ---- pointers ----

#[test]
fn loads_and_stores() {
    let m = check(&fs(
        "fn strlen(s: *u8) -> i64 {\n  let n = 0\n  while s[n] != 0 { n = n + 1 }\n  return n\n}\n\
         fn fill(p: *u8, len: i64, c: u8) {\n  let i = 0\n  while i < len {\n    p[i] = c\n    i = i + 1\n  }\n}\n\
         fn swap(a: *i64, b: *i64) {\n  let t = *a\n  *a = *b\n  *b = t\n}",
    ))
    .unwrap();
    let text = m.to_string();
    assert!(text.contains("= load %"), "{text}");
    assert!(text.contains("store %"), "{text}");
    assert!(err(&fs("fn f(x: i64) -> i64 { return *x }")).contains("cannot dereference i64"));
    assert!(err(&fs("fn f(x: i64) -> i64 { return x[0] }")).contains("cannot index into i64"));
    assert!(err(&fs("fn f(p: *u8) { *p = 300 }")).contains("does not fit in u8"));
}

#[test]
fn address_of() {
    let m = check(&fs(
        "struct P { x: i64, y: i64 }\n\
         fn bump(n: *i64) { *n = *n + 1 }\n\
         fn f() -> i64 {\n  let p = P { x: 1, y: 2 }\n  bump(&p.y)\n  let a = 5\n  bump(&a)\n  return p.y + a\n}",
    ))
    .unwrap();
    let text = m.to_string();
    assert!(text.contains("= addr %"), "{text}");
    assert!(text.contains("= fieldptr %"), "{text}");
    assert!(err(&fs("fn g() -> i64 { return 1 }\nfn f() { let p = &g() }")).contains("temporary"));
}

#[test]
fn field_access_through_pointers() {
    check(&fs(
        "struct Node { value: i64, next: *Node }\n\
         fn sum(n: *Node) -> i64 {\n  let total = 0\n  while n != 0 as *Node {\n    total = total + n.value\n    n = n.next\n  }\n  return total\n}\n\
         fn set(n: *Node, v: i64) { n.value = v\n (*n).value = v }",
    ))
    .unwrap();
}

// ---- arrays ----

#[test]
fn array_literals() {
    check("fn main() { let a = [1, 2, 3]\n let b: [u8; 2] = [250, 5]\n let c = [true; 8]\n let d: [i64; 0] = []\n print(a[0] + len(c)) }")
        .unwrap();
    assert!(err("fn main() { let a = [1, true] }").contains("array element must be i64, found bool"));
    assert!(err("fn main() { let a: [u8; 2] = [1, 256] }").contains("does not fit in u8"));
    assert!(err("fn main() { let a: [i64; 3] = [1, 2] }").contains("must be [i64; 3], found [i64; 2]"));
    assert!(err("fn main() { let a = [] }").contains("cannot infer the element type"));
    assert!(err("fn main() { print(len(5)) }").contains("`len` needs an array"));
}

#[test]
fn array_element_assignment() {
    let m = check(
        "struct S { xs: [i64; 4], n: i64 }\n\
         fn main() {\n  let s = S { xs: [0; 4], n: 0 }\n  let i = 0\n  while i < len(s.xs) {\n    s.xs[i] = i * i\n    i = i + 1\n  }\n  print(s.xs[3])\n}",
    )
    .unwrap();
    let text = m.funcs[0].to_string();
    // s.xs[i] = v  =>  t = field s, 0; t2 = setelem t, i, v; s = setfield s, 0, t2
    assert!(text.contains("setelem"), "{text}");
    assert!(text.contains("splat"), "{text}");
    assert!(err("fn main() { let a = [1, 2]\n a[true] = 3 }").contains("an index must be i64, found bool"));
    assert!(err("fn main() { let a = [1, 2]\n a[0] = \"x\" }").contains("must be i64, found str"));
}

#[test]
fn pointers_to_arrays() {
    let m = check(&fs(
        "fn zero(buf: *[u8; 16]) {\n  let i = 0\n  while i < len(buf) {\n    buf[i] = 0\n    i = i + 1\n  }\n}\n\
         fn first(buf: *[u8; 16]) -> *u8 { return &buf[0] }\n\
         fn f() -> u8 {\n  let b = [7 as u8; 16]\n  zero(&b)\n  let p = &b[3]\n  *p = 9\n  return b[3] + first(&b)[0]\n}",
    ))
    .unwrap();
    let text = m.to_string();
    assert!(text.contains("elemptr"), "{text}");
    // Arrays of structs that contain themselves are still cycles.
    assert!(err("struct A { xs: [A; 2] }\nfn main() {}").contains("contains itself"));
    check(&fs("struct N { kids: [*N; 2] }")).unwrap();
}

#[test]
fn size_and_align() {
    let m = check(&fs(
        "struct A { a: u8, b: i64, c: bool }\n\
         fn f() -> i64 { return size_of(A) * 100 + align_of(A) * 10 + size_of([u16; 3]) }",
    ))
    .unwrap();
    let text = m.to_string();
    for c in ["= const 24\n", "= const 8\n", "= const 6\n"] {
        assert!(text.contains(c), "{c} missing in {text}");
    }
    assert!(m.to_string().contains("struct $A { a: u8, b: i64, c: bool } size 24 align 8"), "{m}");
    assert!(err("fn main() { print(size_of(str)) }").contains("no fixed memory layout"));
    assert!(err("struct S { s: str }\nfn main() { print(size_of(S)) }").contains("no fixed memory layout"));
    check("fn main() { print(size_of(i32) == 4) }").unwrap();
}

// ---- comptime ----

const FIB: &str = "fn fib(n: i64) -> i64 {\n  if n < 2 { return n }\n  return fib(n - 1) + fib(n - 2)\n}\n";

fn main_ir(m: &ir::Module) -> String {
    m.func("main").or_else(|| m.func("_start")).unwrap().to_string()
}

#[test]
fn comptime_expressions_become_constants() {
    let m = check(&format!("{FIB}fn main() {{ print(comptime fib(20) + 1) }}")).unwrap();
    let text = main_ir(&m);
    assert!(text.contains("= const 6765"), "{text}");
    assert!(!text.contains("call @fib"), "{text}");
}

#[test]
fn consts() {
    let m = check(&format!(
        "const A = 6\nconst B: u8 = A as u8 * 7\nconst F = comptime fib(10)\n{FIB}\
         fn main() {{ print(B)\n print(F) }}"
    ))
    .unwrap();
    let text = main_ir(&m);
    assert!(text.contains("= const 42") && text.contains("= const 55"), "{text}");
    // Consts may be used before they are declared, and from any function.
    check("fn main() { print(LATE) }\nconst LATE = 1").unwrap();
    assert!(err("const X: bool = 1\nfn main() {}").contains("the value of `X` must be bool"));
    assert!(err("const X = Y\nconst Y = X\nfn main() {}").contains("depends on itself"));
    assert!(err("const X = 1\nfn main() { X = 2 }").contains("cannot assign to constant `X`"));
    // Locals shadow constants.
    check("const X = 1\nfn main() { let X = true\n print(X) }").unwrap();
}

#[test]
fn comptime_aggregates_and_strings() {
    let m = check(
        "struct P { x: i64, name: str }\n\
         fn squares() -> [i64; 5] {\n  let a = [0; 5]\n  let i = 0\n  while i < 5 {\n    a[i] = i * i\n    i = i + 1\n  }\n  return a\n}\n\
         fn label(n: i64) -> str {\n  if n > 1 { return \"many\" }\n  return \"one\"\n}\n\
         const SQ = squares()\nconst ORIGIN = P { x: 0, name: label(2) + \"!\" }\nconst ZEROS = [0; 64]\n\
         fn main() { print(SQ[4])\n print(ORIGIN.name)\n print(ZEROS[3]) }",
    )
    .unwrap();
    let text = main_ir(&m);
    assert!(text.contains("str \"many!\""), "{text}");
    assert!(text.contains("= array(") && text.contains("= const 16"), "{text}");
    // A uniform array is spliced as one `splat`, not 64 constants.
    assert!(text.contains("splat"), "{text}");
    assert!(text.matches("= const 0").count() < 10, "{text}");
}

#[test]
fn array_lengths_are_evaluated_at_compile_time() {
    let m = check(&fs(
        "const CAP = 4 * 4\nstruct Node { v: i64, next: *Node }\n\
         struct Buf { data: [u8; CAP + size_of(Node)], len: i64 }\n\
         fn f() -> i64 { let b = Buf { data: [0; len_of_data()], len: 0 }\n return len(b.data) }\n\
         fn len_of_data() -> i64 { return CAP + 16 }",
    ))
    .unwrap();
    assert!(m.to_string().contains("[32 x u8]"), "{m}");
    assert!(err("fn main() { let a = [0; 0 - 1] }").contains("between 0 and"));
    assert!(err("fn main() { let a: [u8; true] = [] }").contains("must be an integer"));
}

#[test]
fn comptime_runs_on_the_vm_even_when_freestanding() {
    // Integer work is fine...
    check(&fs(&format!("{FIB}const N = fib(15)\nfn g() -> i64 {{ return N }}"))).unwrap();
    // ...but there are no pointers or syscalls while compiling.
    let e = err(&fs("fn first(p: *u8) -> u8 { return p[0] }\nconst C = first(\"hi\")"));
    assert!(e.contains("compile-time evaluation failed in `first`"), "{e}");
    assert!(err(&fs("fn w() -> i64 { return syscall(39) }\nconst P = w()")).contains("compile-time evaluation failed"));
    assert!(err(&fs("const S = \"hi\"")).contains("pointers do not exist while compiling"));
}

#[test]
fn comptime_errors() {
    assert!(err("fn main() { let x = 1\n print(comptime x + 1) }").contains("not known at compile time"));
    // `f` cannot run at compile time while `f` itself is being compiled.
    let e = err("fn f() -> i64 { return comptime f() }\nfn main() {}");
    assert!(e.contains("still being compiled"), "{e}");
    let e = err("fn spin() -> i64 { while true {}\n return 0 }\nconst X = spin()\nfn main() {}");
    assert!(e.contains("step limit"), "{e}");
    let e = err("fn div(a: i64) -> i64 { return 10 / a }\nconst X = div(0)\nfn main() {}");
    assert!(e.contains("compile-time evaluation failed in `div`: division by zero"), "{e}");
    // A broken callee is reported once, plus where it was needed.
    let prog = jihoo_syntax::parse("fn bad() -> i64 { return true }\nconst X = bad()\nfn main() {}").unwrap();
    let errs = analyze(&prog).unwrap_err();
    assert_eq!(errs.len(), 2, "{errs:?}");
}

// ---- generics (comptime parameters) ----

const MAX: &str = "fn max(comptime T: type, a: T, b: T) -> T {\n  if a > b { return a }\n  return b\n}\n";

#[test]
fn one_instance_per_type() {
    let m = check(&format!(
        "{MAX}fn main() {{\n  let a: u8 = 3\n  print(max(u8, a, 200))\n  print(max(i64, -1, 2))\n  print(max(u8, 1, 2))\n}}"
    ))
    .unwrap();
    let names: Vec<&str> = m.funcs.iter().map(|f| f.name.as_str()).collect();
    // `max` itself is not compiled; `u8` is instantiated once, `i64` once.
    assert_eq!(names, ["main", "max.0", "max.1"], "{m}");
    assert_eq!(m.func("max.0").unwrap().params, [Type::U8, Type::U8]);
    assert_eq!(m.func("max.1").unwrap().params, [Type::I64, Type::I64]);
}

#[test]
fn comptime_value_parameters() {
    let m = check(
        "fn zeros(comptime N: i64) -> [i64; N] { return [0; N] }\n\
         fn sum(comptime N: i64, xs: [i64; N]) -> i64 {\n  let s = 0\n  let i = 0\n  while i < N {\n    s = s + xs[i]\n    i = i + 1\n  }\n  return s\n}\n\
         const K = 3\n\
         fn main() { print(sum(4, zeros(4)) + sum(K, [1, 2, 3]) + len(zeros(K * 2))) }",
    )
    .unwrap();
    let text = m.to_string();
    // `sum`'s comptime argument is handled before its runtime argument `zeros(4)`.
    assert!(text.contains("fn @sum.0([4 x i64]) -> i64"), "{text}");
    assert!(text.contains("fn @zeros.1() -> [4 x i64]"), "{text}");
    assert!(text.contains("fn @sum.2([3 x i64]) -> i64"), "{text}");
    assert!(err("fn zeros(comptime N: i64) -> [i64; N] { return [0; N] }\nfn main() { let x = 1\n let z = zeros(x) }")
        .contains("not known at compile time"));
    assert!(err("fn f(comptime N: u8) -> u8 { return N }\nfn main() { print(f(true)) }")
        .contains("comptime argument `N` of `f` must be u8, found bool"));
}

#[test]
fn generic_types_in_bodies() {
    check(&fs(
        "fn swap(comptime T: type, a: *T, b: *T) {\n  let t = *a\n  *a = *b\n  *b = t\n}\n\
         fn fill(comptime T: type, comptime N: i64, buf: *[T; N], v: T) {\n  let i = 0\n  while i < N {\n    buf[i] = v\n    i = i + 1\n  }\n}\n\
         fn bytes(comptime T: type) -> i64 { return size_of(T) * 8 }\n\
         fn f() -> i64 {\n  let x = 1\n  let y = 2\n  swap(i64, &x, &y)\n  let b = [0 as u8; 8]\n  fill(u8, 8, &b, 7)\n  let p = [0 as *u8; 2]\n  fill(*u8, 2, &p, &b[0])\n  return x + bytes([u8; 3]) + bytes(*u8)\n}",
    ))
    .unwrap();
}

#[test]
fn generic_recursion_and_comptime() {
    // An instance may call itself: callers only need its signature.
    let src = "fn pow(comptime T: type, x: T, n: i64) -> T {\n  if n == 0 { return 1 }\n  return x * pow(T, x, n - 1)\n}\n\
               const P = pow(u32, 3, 4)\nfn main() { print(P + pow(u32, 2, 3)) }";
    let m = check(src).unwrap();
    assert!(main_ir(&m).contains("= const 81"), "{m}");
    assert_eq!(m.funcs.iter().filter(|f| f.name.starts_with("pow.")).count(), 1);
    // Different comptime values recursing forever do not hang the compiler.
    let e = err("fn f(comptime n: i64) -> i64 { return f(n + 1) }\nfn main() { print(f(0)) }");
    assert!(e.contains("too many instances"), "{e}");
}

#[test]
fn generic_errors() {
    // The body is checked per instance, and errors say which one.
    let e = err(&format!("{MAX}fn main() {{ print(max(str, \"a\", \"b\")) }}"));
    assert!(e.contains("cannot apply `>` to str and str (in `max` with T = str)"), "{e}");
    assert!(err(&format!("{MAX}fn main() {{ print(max(i64, 1)) }}")).contains("takes 3 arguments, 2 given"));
    assert!(err(&format!("{MAX}fn main() {{ print(max(5, 1, 2)) }}")).contains("expected a type"));
    assert!(err(&format!("{MAX}fn main() {{ print(max(u8, 1, true)) }}")).contains("argument `b` of `max` must be u8"));
    assert!(err("fn f(comptime T: type) -> i64 { return T }\nfn main() { print(f(i64)) }").contains("is a type (i64), not a value"));
    assert!(err("fn f(comptime N: i64) { N = 1 }\nfn main() { f(1) }").contains("cannot assign to comptime parameter"));
    assert!(err("fn f(x: type) {}\nfn main() {}").contains("`type` can only be the type of a `comptime` parameter"));
    assert!(err("fn main(comptime T: type) {}").contains("comptime parameters"));
}

// ---- inline asm ----

#[test]
fn inline_asm_lowering() {
    let m = check(&fs(
        "fn bswap(x: u64) -> u64 { return asm(\"mov {out}, {0}\", \"bswap {out}\", out(reg) u64, in(reg) x) }\n\
         fn exit(code: i64) { asm(\"syscall\", in(\"rax\") 60, in(\"rdi\") code, clobber(\"rcx\", \"r11\", \"cc\", \"memory\")) }",
    ))
    .unwrap();
    let text = m.to_string();
    assert!(text.contains(r#"asm "mov ${0}, ${1}\nbswap ${0}", "=r,r"(%0)"#), "{text}");
    // `cc` is implied; the rest become LLVM clobbers.
    assert!(text.contains(r#""{rax},{rdi},~{rcx},~{r11},~{memory}""#), "{text}");
    assert!(err("fn main() { asm(\"nop\") }").contains("only available in freestanding"));
    assert!(err(&fs("fn f() -> bool { return asm(\"nop\", out(reg) bool) }")).contains("output must be an integer or a pointer"));
    assert!(err(&fs("fn f() { asm(\"mov {0}, {1}\", in(reg) 1) }")).contains("only 1 inputs"));
    let m = check(&fs("fn inc(x: i64) -> i64 { return asm(\"inc {out}\", out(reg) i64, in(out) x) }")).unwrap();
    assert!(m.to_string().contains(r#""=r,0"(%0)"#), "{m}");
    assert!(err(&fs("fn f() { asm(\"nop\", in(out) 1) }")).contains("needs an `out(...)`"));
    // Compile-time code runs on the VM, which has no asm.
    let e = err(&fs("fn f() -> i64 { return asm(\"mov {out}, 1\", out(reg) i64) }\nconst X = f()"));
    assert!(e.contains("inline asm is not available"), "{e}");
}

// ---- macros ----

/// Compiles and runs a hosted program, returning what it printed.
fn run(src: &str) -> String {
    let m = check(src).unwrap_or_else(|e| panic!("{e}"));
    let mut out = Vec::new();
    jihoo_vm::run(&m, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

const POWER: &str = "macro power(x: expr, n: i64) -> expr {\n  let e = quote(1)\n  let i = 0\n  while i < n {\n    e = quote($e * $x)\n    i = i + 1\n  }\n  return e\n}\n";

#[test]
fn macros_expand_to_code() {
    let src = format!("{POWER}fn main() {{\n  let y = 3\n  print(power!(y, 4))\n  print(power!(y + 1, 2))\n}}");
    // `y + 1` is inserted in parentheses: (1 * (y + 1)) * (y + 1), not 1 * y + 1 * y + 1.
    assert_eq!(run(&src), "81\n16\n");
    let m = check(&src).unwrap();
    // The macro runs while compiling and is not part of the program.
    assert!(m.func("power").is_none());
    assert!(!main_ir(&m).contains("call"), "{m}");
}

#[test]
fn macro_value_parameters_and_literals() {
    let src = "const N = 3\n\
               macro sum_to(n: i64) -> expr {\n  let e = quote(0)\n  let i = 1\n  while i <= n {\n    e = quote($e + $i)\n    i = i + 1\n  }\n  return e\n}\n\
               macro greet(who: str, loud: bool) -> expr {\n  if loud { return quote($(\"HELLO \" + who) + \"!\") }\n  return quote($(\"hello \" + who))\n}\n\
               macro neg() -> expr { return quote($(0 - 5) * 2) }\n\
               fn main() {\n  print(sum_to!(N * 2))\n  print(greet!(\"jihoo\", true))\n  print(greet!(\"vm\", 1 == 2))\n  print(neg!())\n}";
    assert_eq!(run(src), "21\nHELLO jihoo!\nhello vm\n-10\n");
}

#[test]
fn stringify_and_nested_macros() {
    let src = "fn check(ok: bool, what: str) -> i64 {\n  if ok { return 0 }\n  print(\"failed: \" + what)\n  return 1\n}\n\
               macro expect(cond: expr) -> expr { return quote(check($cond, $(stringify(cond)))) }\n\
               macro square(x: expr) -> expr { return quote($x * $x) }\n\
               fn main() {\n  let a = 4\n  let failures = expect!(square!(a) == 16) + expect!(a + 1 == 6)\n  print(failures)\n}";
    assert_eq!(run(src), "failed: a + 1 == 6\n1\n");
}

#[test]
fn macros_in_freestanding_programs() {
    // Macro bodies run on the VM, so they may use `str` even when freestanding;
    // a `str` inserted into code becomes a string literal (a `*u8` here).
    let m = check(&fs(
        "macro msg(s: str) -> expr { return quote($(s + \"\\n\")) }\n\
         fn f() -> i64 { let p = msg!(\"hi\")\n return syscall(1, 1, p, 3) }",
    ))
    .unwrap();
    assert!(m.to_string().contains(r#"str "hi\n""#), "{m}");
}

#[test]
fn macro_errors() {
    assert!(err("fn main() { let x = quote(1) }").contains("only be used inside a macro"));
    assert!(err("fn f(x: expr) {}\nfn main() {}").contains("only available in macros"));
    assert!(err(&format!("{POWER}fn main() {{ print(power(1, 2)) }}")).contains("call it as `power!(...)`"));
    assert!(err("fn main() { print(nope!(1)) }").contains("`nope` is not a macro"));
    assert!(err("macro m(x: expr) -> i64 { return 1 }\nfn main() {}").contains("must return `expr`"));
    assert!(err("macro m(x: [i64; 2]) -> expr { return quote(1) }\nfn main() {}").contains("macro parameters must be"));
    assert!(err(&format!("{POWER}fn main() {{ print(power!(1)) }}")).contains("takes 2 arguments, 1 given"));
    assert!(err(&format!("{POWER}fn main() {{ let k = 2\n print(power!(1, k)) }}")).contains("not known at compile time"));
    // Errors in the produced code point at the call and name the macro.
    let e = err(&format!("{POWER}fn main() {{ print(power!(true, 2)) }}"));
    assert!(e.contains("cannot apply `*` to i64 and bool (in code produced by `power!`)"), "{e}");
    assert!(e.starts_with("10:"), "{e}");
    let e = err("macro forever(x: expr) -> expr { return quote(forever!($x)) }\nfn main() { print(forever!(1)) }");
    assert!(e.contains("too deep"), "{e}");
    // Macro bodies are checked even if the macro is never used.
    assert!(err("macro bad() -> expr { return 1 }\nfn main() {}").contains("return value must be expr"));
}

// ---- generic structs ----

const PAIR: &str = "struct Pair(T: type) {\n  a: T\n  b: T\n}\n";

#[test]
fn generic_struct_instances() {
    let src = format!(
        "{PAIR}fn sum(comptime T: type, p: Pair(T)) -> T {{ return p.a + p.b }}\n\
         fn main() {{\n  let p = Pair(i64) {{ a: 1, b: 2 }}\n  let q = Pair(u8) {{ a: 200, b: 100 }}\n  print(sum(i64, p))\n  print(sum(u8, q))\n}}"
    );
    assert_eq!(run(&src), "3\n44\n"); // 300 wraps to 44 in u8
    let m = check(&src).unwrap();
    let names: Vec<&str> = m.structs.iter().map(|s| s.name.as_str()).collect();
    // The generic declaration itself is not a type; each instance is.
    assert_eq!(names, ["Pair(i64)", "Pair(u8)"]);
    assert!(m.to_string().contains(r#"struct $"Pair(u8)" { a: u8, b: u8 } size 2 align 1"#), "{m}");
}

#[test]
fn generic_struct_value_parameters() {
    let m = check(
        "struct Buf(N: i64) {\n  data: [u8; N]\n  len: i64\n}\nconst CAP = 8\n\
         fn cap(comptime N: i64, b: Buf(N)) -> i64 { return len(b.data) }\n\
         fn main() {\n  let b = Buf(CAP * 2) { data: [0; CAP * 2], len: 0 }\n  print(cap(16, b) + size_of(Buf(4)))\n}",
    )
    .unwrap();
    let text = m.to_string();
    // `Buf(CAP * 2)` and `Buf(16)` are the same type.
    assert!(text.contains(r#"struct $"Buf(16)" { data: [16 x u8], len: i64 }"#), "{text}");
    assert_eq!(m.structs.len(), 2, "{text}"); // Buf(16) and Buf(4)
}

#[test]
fn generic_structs_nest_and_point_to_themselves() {
    check(&fs(&format!(
        "{PAIR}struct Node(T: type) {{\n  value: T\n  next: *Node(T)\n}}\n\
         fn f() -> i64 {{\n  let n = Node(Pair(u8)) {{ value: Pair(u8) {{ a: 1, b: 2 }}, next: 0 as *Node(Pair(u8)) }}\n  return n.value.b as i64\n}}"
    )))
    .unwrap();
    assert!(err("struct Bad(T: type) { inner: Bad(T) }\nfn main() { let x = size_of(Bad(i64)) }").contains("contains itself"));
}

#[test]
fn generic_struct_errors() {
    assert!(err(&format!("{PAIR}fn main() {{ let p: Pair = Pair(i64) {{ a: 1, b: 2 }} }}")).contains("is generic; write `Pair(...)`"));
    assert!(err("struct P { x: i64 }\nfn main() { let p: P(i64) = P { x: 1 } }").contains("takes no arguments"));
    assert!(err(&format!("{PAIR}fn main() {{ let p: Pair(i64, u8) = 1 }}")).contains("takes 1 arguments, 2 given"));
    assert!(err(&format!("{PAIR}fn main() {{ let p = Pair(i64) {{ a: 1, b: true }} }}")).contains("field `b` must be i64, found bool"));
    // Errors in a generic struct's fields are reported per instance.
    let e = err("struct S(N: i64) { data: [u8; N] }\nfn main() { let s: S(0 - 1) = S(0 - 1) { data: [] } }");
    assert!(e.contains("array length must be between 0 and"), "{e}");
    assert!(e.contains("(in `S(-1)`)"), "{e}");
}
