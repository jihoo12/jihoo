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
