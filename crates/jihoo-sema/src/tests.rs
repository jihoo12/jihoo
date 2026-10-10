use super::*;
use jihoo_ir::FloatTy;

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
    // `!` is bitwise on integers, logical on bools, and nothing else.
    check("fn main() { print(!1) }").unwrap();
    assert!(err("fn main() { print(!\"s\") }").contains("cannot apply `!` to str"));
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
fn compound_assignment() {
    check("fn main() { let x = 1\n x += 2\n x <<= 1\n let s = \"a\"\n s += \"b\"\n let f = 1.5\n f *= 2 }").unwrap();
    // The place is evaluated once: one call to `i`, one read and one write.
    let m = check("fn i() -> i64 { return 0 }\nfn main() { let a = [1, 2]\n a[i()] += 1 }").unwrap();
    let main = m.func("main").unwrap().to_string();
    assert_eq!(main.matches("call @i").count(), 1, "{main}");
    // The value takes the target's type; the result must fit back into it.
    check("fn main() { let x: u8 = 1\n x += 200 }").unwrap();
    assert!(err("fn main() { let x: u8 = 1\n x += 300 }").contains("300 does not fit in u8"));
    assert!(err("fn main() { let x: i32 = 1\n let y = 2\n x += y }").contains("cannot apply `+=` to i32 and i64"));
    assert!(err("fn main() { let b = true\n b += true }").contains("cannot apply `+=` to bool and bool"));
    check(&fs("fn f(p: *i64) { p += 1\n *p -= 1 }")).unwrap();
    // The same targets as `=`.
    assert!(err("const N = 1\nfn main() { N += 1 }").contains("cannot assign to constant `N`"));
    assert!(err("fn main() { let n = 1\n let f = fn() { n += 1 } }").contains("captured variable"));
    // A statement, not an expression; and the operator must be on the target's line.
    assert!(err("fn main() { let x = 1\n let y = x += 1 }").contains("end of statement"));
    assert!(err("fn main() { let x = 1\n x\n += 1 }").contains("found `+=`"));
}

#[test]
fn break_and_continue() {
    let m = check("fn main() {\n  let i = 0\n  while true {\n    i = i + 1\n    if i < 3 { continue }\n    break\n  }\n}").unwrap();
    // `continue` jumps to the condition block, `break` past the loop.
    let main = m.func("main").unwrap().to_string();
    assert!(main.contains("jmp bb1"), "{main}");
    assert_eq!(err("fn main() { break }"), "1:13: `break` outside of a `while` loop");
    assert!(err("fn main() { if true { continue } }").contains("`continue` outside of a `while` loop"));
    // A closure body is a function of its own: the loop around it does not count.
    assert!(err("fn main() { while true {\n  let f = fn() { break }\n  break\n} }").contains("`break` outside"));
    // Code after `break` is unreachable; what follows the loop is not.
    assert!(err("fn f() -> i64 { while true { break } }\nfn main() {}").contains("missing `return`"));
    check("fn f() -> i64 { while true { break\n return 2 }\n return 1 }\nfn main() {}").unwrap();
    // `break` and `continue` are keywords.
    assert!(err("fn main() { let break = 1 }").contains("variable name"));
}

#[test]
fn profile_rules() {
    assert!(err("#![freestanding]\nfn _start() { print(1) }").contains("not available in freestanding"));
    assert!(err("fn main() { syscall(60, 0) }").contains("only available in native and freestanding"));
    assert!(err(&fs("fn f(s: str) {}")).contains("garbage collected"));
    assert!(err("fn f(p: *u8) {}\nfn main() {}").contains("only available in native and freestanding"));
    assert!(err("fn main() { let x = 1\n let p = &x }").contains("only available in native and freestanding"));
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
fn literals_reach_the_limits_of_64_bit_types() {
    check("fn main() { let x: u64 = 0xffff_ffff_ffff_ffff\n let y: u64 = 18446744073709551615 }").unwrap();
    check("fn main() { let x = -9223372036854775808\n let y: i64 = -0x8000_0000_0000_0000 }").unwrap();
    assert!(err("fn main() { let x = 9223372036854775808 }").contains("9223372036854775808 does not fit in i64"));
    assert!(err("fn main() { let x: i64 = -9223372036854775809 }").contains("does not fit in i64"));
    assert!(err("fn main() { let a: [u8; 9223372036854775808] = [0; 1] }").contains("array length"));
    // u64 values keep their bit pattern in JIR.
    let m = check("fn top() -> u64 { return 0xffff_ffff_ffff_ffff }\nfn main() {}").unwrap();
    assert!(m.funcs[0].to_string().contains("const -1"), "{m}");
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
fn nested_field_assignment_is_one_setpath() {
    let m = check(&format!("{POINT}fn main() {{\n let l = Line {{ a: Point {{ x: 0, y: 0 }}, b: Point {{ x: 0, y: 0 }} }}\n l.b.y = 5\n print(l.b.y)\n l.a = l.b\n}}"))
        .unwrap();
    let text = m.funcs[0].to_string();
    // l.b.y = 5  =>  l = setpath l, (field 1, field 1), 5; one level is a setfield.
    assert!(text.contains(" = setpath %"), "{text}");
    assert!(text.contains("(field 1, field 1)"), "{text}");
    assert!(text.contains(" = getpath %"), "{text}");
    assert_eq!(text.matches("setfield").count(), 1, "{text}");
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
    // s.xs[i] = v  =>  s = setpath s, (field 0, elem i), v
    assert!(text.contains("(field 0, elem %"), "{text}");
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

#[test]
fn comptime_runs_share_what_they_have_compiled() {
    // Runs keep the functions earlier ones needed. One whose callee is broken
    // adds nothing, so a later run of the same function fails the same way
    // instead of finding half of it; runs that need only good functions work.
    let src = "fn bad() -> i64 { return true }\nfn uses_bad() -> i64 { return bad() }\n\
               fn good() -> i64 { return 2 }\nfn twice() -> i64 { return good() + good() }\n\
               const A = uses_bad()\nconst B = twice()\nconst C = uses_bad() + twice()\nconst D = twice() + 1\n\
               fn main() { print(B + D) }";
    let errs = analyze(&jihoo_syntax::parse(src).unwrap()).unwrap_err();
    let msgs: Vec<String> = errs.iter().map(|e| e.to_string()).collect();
    assert_eq!(msgs.iter().filter(|m| m.contains("`bad` has errors")).count(), 2, "{msgs:?}");
    assert!(!msgs.iter().any(|m| m.contains("twice") || m.contains("good")), "{msgs:?}");
    assert_eq!(run(&src.replace("bad() -> i64 { return true }", "bad() -> i64 { return 1 }")), "9\n");

    // A function that is compiled because a run needs it can itself need runs
    // (here for `N`, its array length), which add to the same module.
    let src = "fn three() -> i64 { return 3 }\nconst N = three()\n\
               fn make() -> [i64; N] { return [N; N] }\nconst X = make()[1] + N + comptime three()\n\
               fn main() { print(X) }";
    assert_eq!(run(src), "9\n");
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

// ---- function values ----

#[test]
fn function_values() {
    let m = check(
        "fn inc(x: i64) -> i64 { return x + 1 }\n\
         fn apply(f: fn(i64) -> i64, x: i64) -> i64 { return f(x) }\n\
         fn twice(comptime f: fn(i64) -> i64, x: i64) -> i64 { return f(f(x)) }\n\
         const F = inc\n\
         fn main() { let len = 3\n print(apply(inc, len) + twice(inc, 1) + F(0)) }",
    )
    .unwrap();
    let text = m.to_string();
    assert!(text.contains("funcref @inc"), "{text}");
    assert!(text.contains("fn @apply(fn(i64) -> i64, i64) -> i64"), "{text}");
    assert!(text.contains("= call %0(%1)"), "{text}");
    // Functions known at compile time are called directly.
    assert!(text.contains("fn @twice.0(i64) -> i64") && !text.contains("call %0(%0)"), "{text}");

    let e = err("fn inc(x: i64) -> i64 { return x }\nfn main() { let f: fn(i64) = inc }");
    assert!(e.contains("must be fn(i64), found fn(i64) -> i64"), "{e}");
    assert!(err("fn main() { let x = 1\n print(x(2)) }").contains("`x` is not a function; it has type i64"));
    assert!(err("fn main() { let f = print }").contains("`print` is a builtin"));
    assert!(err("fn g(comptime T: type) {}\nfn main() { let f = g }").contains("cannot be used as a value"));
    assert!(err("macro m() -> expr { return quote(1) }\nfn main() { let f = m }").contains("macros cannot be used as values"));
    assert!(err("fn inc(x: i64) -> i64 { return x }\nfn main() { print(inc(1)(2)) }").contains("this function is not a function"));
    assert!(err("fn inc(x: i64) -> i64 { return x }\nfn main() { let f = inc\n print(f(1, 2)) }").contains("`f` takes 1 arguments, 2 given"));
    assert!(err("fn main() { let t = fn(i64) }").contains("is a type, not a value"));
    // Function values have no equality, so closures can be added later without
    // having to define what comparing them means.
    assert!(err("fn inc(x: i64) -> i64 { return x }\nfn main() { print(inc == inc) }").contains("cannot apply `==`"));
}

#[test]
fn function_values_in_freestanding_code() {
    let m = check(&fs("struct S { f: fn(*u8) -> i64 }\nfn g(p: *u8) -> i64 { return 0 }\nfn h() -> i64 { let s = S { f: g }\n return s.f(\"x\") }")).unwrap();
    assert!(m.to_string().contains("struct $S { f: fn(*u8) -> i64 } size 8 align 8"), "{m}");
}

// ---- enums and match ----

const SHAPE: &str = "enum Shape {\n  Circle(i64)\n  Rect(i64, i64)\n  Empty\n}\nenum Option(T: type) { Some(T), None }\n";

#[test]
fn enums_and_match() {
    let m = check(&format!(
        "{SHAPE}fn area(s: Shape) -> i64 {{\n match s {{\n Circle(r) => return r * r\n Rect(w, h) => return w * h\n Empty => return 0\n }}\n}}\n\
         fn main() {{ print(area(Shape.Rect(2, 3)))\n let o = Option.Some(1)\n let n: Option(u8) = Option.None }}"
    ))
    .unwrap();
    let text = m.to_string();
    assert!(text.contains("enum $Shape { Circle(i64), Rect(i64, i64), Empty } size 24 align 8"), "{text}");
    assert!(text.contains("enum $\"Option(i64)\"") && text.contains("enum $\"Option(u8)\""), "{text}");
    assert!(text.contains("= tag %0") && text.contains("= payload %0, 1, 1"), "{text}");
    // Every arm returns, so there is no missing `return` and no fall-through.
    assert!(text.contains("unreachable"), "{text}");
}

#[test]
fn enum_errors() {
    let m = |body: &str| err(&format!("{SHAPE}fn f(s: Shape) {{\n{body}\n}}\nfn main() {{}}"));
    assert!(m("match s {\n Circle(r) => {}\n}").contains("`match` does not cover `Rect(_, _)`, `Empty`"));
    assert!(m("match s {\n Square => {}\n _ => {}\n}").contains("has no variant `Square` (it has Circle, Rect, Empty)"));
    assert!(m("match s {\n Rect(w) => {}\n _ => {}\n}").contains("`Rect` holds 2 values, 1 given"));
    assert!(m("match s {\n Rect => {}\n _ => {}\n}").contains("write `Rect(_, _)` to ignore them"));
    assert!(m("match s {\n Empty() => {}\n _ => {}\n}").contains("`Empty` holds no values"));
    assert!(m("match s {\n _ => {}\n Empty => {}\n}").contains("unreachable arm: an arm above matches every value"));
    assert!(m("match s {\n Empty => {}\n Empty => {}\n _ => {}\n}").contains("the arms above already match every value"));
    assert!(m("match s {\n Rect(a, a) => {}\n _ => {}\n}").contains("`a` is bound twice"));
    assert!(m("match s {\n Circle(r) => {}\n _ => {}\n}\nprint(r)").contains("unknown variable `r`"));
    assert!(m("match 3 {\n 1 => {}\n}").contains("does not cover every other integer"));
    assert!(m("match true {\n true => {}\n}").contains("does not cover `false`"));
    assert!(m("match 3 {\n Empty => {}\n _ => {}\n}").contains("`Empty` looks like a variant, but the value is i64"));
    assert!(m("let x: u8 = 1\nmatch x {\n 300 => {}\n _ => {}\n}").contains("300 does not fit in u8"));
    assert!(m("match \"a\" {\n _ => {}\n}").contains("cannot `match` on str"));
    assert!(m("let t = Shape.Square").contains("enum `Shape` has no variant `Square`"));
    assert!(m("let t = Shape.Circle").contains("`Shape.Circle` holds 1 values; write `Shape.Circle(...)`"));
    assert!(m("let t = Shape.Empty()").contains("`Shape.Empty` holds no values"));
    assert!(m("let t = Shape.Circle(true)").contains("argument 1 of `Shape.Circle` must be i64, found bool"));
    assert!(m("let t = Option.None").contains("cannot infer `T` of `Option` here"));
    assert!(m("let t = s.x").contains("type Shape has no fields"));
    assert!(err("enum L { Cons(i64, L), Nil }\nfn main() {}").contains("enum `L` contains itself (L -> L)"));
    assert!(err("enum E { A, A }\nfn main() {}").contains("variant `A` is declared twice"));
    assert!(err("struct E { x: i64 }\nenum E { A }\nfn main() {}").contains("type `E` is defined twice"));
}

// ---- nested patterns and match expressions ----

const NESTED: &str = "struct P { x: i64, y: i64 }\nenum O(T: type) { Some(T), None }\nenum S { Dot(P), Pair(O(bool), O(bool)) }\n";

#[test]
fn nested_patterns() {
    let m = |body: &str| err(&format!("{NESTED}fn f(s: S) -> i64 {{\n{body}\n}}\nfn main() {{}}"));
    // Missing values are spelled out as patterns, nested ones included.
    let e = m("match s {\n Dot(P { x: 0, y }) => return y\n Pair(Some(true), _) => return 1\n Pair(None, None) => return 2\n}");
    assert!(e.contains("does not cover `Dot(P { x: 1, y: _ })`, `Pair(Some(false), Some(_))`"), "{e}");
    let e = m("match s {\n Dot(_) => return 0\n Pair(_, Some(_)) => return 1\n Pair(Some(b), None) if b => return 2\n Pair(_, None) => return 3\n Pair(None, None) => return 4\n}");
    assert!(e.contains("unreachable arm: the arms above already match every value"), "{e}");
    // A guard does not count towards covering values.
    let e = m("match s {\n Dot(p) if p.x > 0 => return 0\n Pair(_, _) => return 1\n}");
    assert!(e.contains("does not cover `Dot(_)`"), "{e}");
    assert!(m("match s {\n Dot(P { x }) => return x\n _ => return 0\n}").contains("missing fields in this pattern: y (add `..`"));
    assert!(m("match s {\n Dot(P { z, .. }) => return 0\n _ => return 0\n}").contains("struct `P` has no field `z`"));
    assert!(m("match s {\n Dot(Q { .. }) => return 0\n _ => return 0\n}").contains("this pattern matches `Q`, but the value is P"));
    assert!(m("match s {\n Pair(Some(Nonee), _) => return 0\n _ => return 0\n}").contains("`Nonee` looks like a variant, but the value is bool"));
    assert!(m("match s {\n Pair(Sone(x), _) => return 0\n _ => return 0\n}").contains("enum `O(bool)` has no variant `Sone`"));
    assert!(m("match s {\n Pair(x, x) => return 0\n}").contains("`x` is bound twice"));
    assert!(m("match s {\n Dot(p) if p => return 0\n _ => return 0\n}").contains("a guard must be bool, found P"));
    // A lowercase name binds; it covers everything.
    check(&format!("{NESTED}fn f(s: S) -> i64 {{\n match s {{\n Dot(P {{ x, .. }}) => return x\n other => return 0\n }}\n}}\nfn main() {{}}")).unwrap();
}

#[test]
fn patterns_read_through_refs() {
    let list = "enum L { Cons(i64, ref L), Nil }\n";
    let m = check(&format!(
        "{list}fn two(l: L) -> bool {{ return match l {{ Cons(_, Cons(_, Nil)) => true, _ => false }} }}\nfn main() {{}}"
    ))
    .unwrap();
    assert!(m.to_string().contains("= deref %"), "{m}");
    // A name binds the ref itself.
    check(&format!("{list}fn tail(l: L) -> ref L {{ return match l {{ Cons(_, rest) => rest, Nil => ref L.Nil }} }}\nfn main() {{}}")).unwrap();
    // Coverage looks through refs too.
    let e = err(&format!("{list}fn f(l: L) -> i64 {{ return match l {{ Cons(_, Nil) => 1, Nil => 0 }} }}\nfn main() {{}}"));
    assert!(e.contains("does not cover `Cons(_, Cons(_, _))`"), "{e}");
}

#[test]
fn or_patterns() {
    let shapes = "enum S { A(i64), B(i64), C(bool), D }\n";
    let m = |arms: &str| err(&format!("{shapes}fn f(s: S) -> i64 {{ return match s {{ {arms} }} }}\nfn main() {{}}"));
    assert!(m("A(x) | B(y) => 1, _ => 0").contains("`x` is bound in some alternatives of this `|` pattern but not in others"));
    assert!(m("A(x) | C(x) => 1, _ => 0").contains("`x` is i64 in one alternative of this `|` pattern and bool in another"));
    assert!(m("A(_) | D => 1, D | B(_) => 2, _ => 0").contains("unreachable alternative: `D` is already matched above"));
    assert!(m("A(_) | B(_) => 1, C(true) => 2").contains("does not cover `C(false)`, `D`"));
    assert!(m("A(1 | 2) => 1, A(_) | B(_) | C(_) | D => 2, A(3) => 3").contains("unreachable arm"));
    let m = check(&format!("{shapes}fn f(s: S) -> i64 {{ return match s {{ A(n) | B(n) => n, C(true | false) | D => 0 }} }}\nfn main() {{}}")).unwrap();
    assert!(m.to_string().contains("fn @f($S) -> i64"), "{m}");
}

#[test]
fn match_expressions() {
    let m = check(&format!(
        "{NESTED}fn f(o: O(i64)) -> u8 {{ return match o {{ Some(n) if n > 0 => 1, Some(_) => 2, None => 3 }} }}\nfn main() {{ let k = match 3 > 2 {{ true => \"y\", false => \"n\" }}\n print(k) }}"
    ))
    .unwrap();
    assert!(m.to_string().contains("fn @f($\"O(i64)\") -> u8"), "{m}");
    let e = err("fn main() { let k = match 1 { 0 => 1, _ => \"x\" } }");
    assert!(e.contains("the value of this arm (like the arms before it) must be i64, found str"), "{e}");
    assert!(err("fn main() { let k = match 1 { 0 => 1 } }").contains("does not cover every other integer"));
    assert!(err("enum E {}\nfn f(e: E) -> i64 { return match e {} }\nfn main() {}").contains("needs at least one arm"));
}

// ---- cells ----

#[test]
fn cells() {
    let m = check(
        "struct S { n: i64, xs: [i64; 2] }\n\
         fn bump(c: cell S) { c.n = c.n + 1\n c.xs[1] = c.n }\n\
         fn main() { let c = cell(S { n: 0, xs: [0, 0] })\n bump(c)\n let k = cell(0)\n let f = fn() { *k = *k + 1 }\n f()\n print(*k + c.xs[1]) }",
    )
    .unwrap();
    let text = m.to_string();
    assert!(text.contains("cellset %0, (field 1, elem %"), "{text}");
    assert!(text.contains("= cellget %0, (field 0)"), "{text}");
    assert!(text.contains("= cell %"), "{text}");
    // The captured `k` cannot be reassigned, but what it holds can change.
    assert!(err("fn main() { let k = cell(0)\n let f = fn() { k = cell(1) } }").contains("cannot assign to a captured variable"));
    assert!(err("fn main() { let c = cell(1)\n print(c == c) }").contains("cells (cell i64) cannot be compared; compare what they hold"));
    assert!(err("fn main() { let c = cell(1)\n *c = true }").contains("must be i64, found bool"));
    assert!(err("const C = cell(1)\nfn main() {}").contains("a cell cannot be computed at compile time"));
    assert!(err(&fs("fn f() { let c = cell(1) }")).contains("only available in hosted mode"));
    assert!(err(&fs("fn f(c: cell i64) {}")).contains("only available in hosted mode"));
}

// ---- equality ----

#[test]
fn structural_equality() {
    let m = check(
        "struct P { x: i64, s: str }\nenum L { Cons(P, ref L), Nil }\n\
         fn main() { let p = P { x: 1, s: \"a\" }\n print(p == p && [p] != [p] && L.Cons(p, ref L.Nil) == L.Nil) }",
    )
    .unwrap();
    let text = m.to_string();
    // One helper per type compared, called for nested parts; `L` and `ref L` call each other.
    assert!(text.contains("fn @fn.eq.0($P, $P) -> bool"), "{text}");
    assert!(text.contains("call @fn.eq.0("), "{text}");
    assert!(text.matches("fn @fn.eq.").count() == 4, "{text}");

    let e = err("struct S { f: fn(i64) }\nfn g(x: i64) {}\nfn main() { let s = S { f: g }\n print(s == s) }");
    assert!(e.contains("cannot apply `==` to S: function values (fn(i64)) cannot be compared"), "{e}");
    let e = err("enum E { A(chan i64), B }\nfn main() { let c = chan(i64)\n print(E.A(c) != E.B) }");
    assert!(e.contains("cannot apply `!=` to E: channels (chan i64) cannot be compared"), "{e}");
    assert!(err("struct P { x: i64 }\nfn main() { print(P { x: 1 } == 1) }").contains("cannot apply `==` to P and i64"));
    assert!(err("struct P { x: i64 }\nfn main() { let p = P { x: 1 }\n print(p < p) }").contains("cannot apply `<` to P and P"));
    // Works natively too, with pointers compared as addresses.
    check(&fs("struct N { v: i64, next: *N }\nfn f(a: N, b: N) -> bool { return a == b }")).unwrap();
}

// ---- refs ----

#[test]
fn refs() {
    let m = check(
        "enum List(T: type) { Cons(T, ref List(T)), Nil }\n\
         struct P { x: i64 }\n\
         fn sum(l: List(i64)) -> i64 {\n match l {\n Cons(x, rest) => return x + sum(*rest)\n Nil => return 0\n }\n}\n\
         fn main() { let l = List.Cons(1, ref List.Cons(2, ref List.Nil))\n let p = ref P { x: 3 }\n print(sum(l) + p.x) }",
    )
    .unwrap();
    let text = m.to_string();
    // A ref breaks the cycle, and a type holding a ref has no fixed layout.
    assert!(text.contains("enum $\"List(i64)\" { Cons(i64, ref $\"List(i64)\"), Nil }\n"), "{text}");
    assert!(text.contains("= ref %") && text.contains("= deref %"), "{text}");

    let e = err("struct P { x: i64 }\nfn main() { let p = ref P { x: 1 }\n p.x = 2 }");
    assert!(e.contains("cannot assign to a value behind a `ref`"), "{e}");
    assert!(err("fn main() { let r = ref 1\n *r = 2 }").contains("behind a `ref`"));
    // Refs compare by value: which object a ref is cannot be observed.
    check("fn main() { let r = ref 1\n print(r == ref 1) }").unwrap();
    assert!(err(&fs("fn f() { let r = ref 1 }")).contains("only available in hosted mode"));
    assert!(err(&fs("fn f(r: ref i64) {}")).contains("use a pointer (`*T`)"));
    assert!(err("fn main() { let r = ref [1, 2]\n r[0] = 5 }").contains("behind a `ref`"));
    assert!(err("fn main() { let x = 1\n let r = ref x\n print(r + 1) }").contains("cannot apply `+` to ref i64 and i64"));
}

// ---- closures ----

#[test]
fn closures() {
    let m = check(
        "fn apply(f: fn(i64) -> i64, x: i64) -> i64 { return f(x) }\n\
         fn main() {\n let k = 2\n let unused = \"x\"\n let f = fn(x: i64) -> i64 { return x * k }\n k = 3\n print(apply(f, 5) + apply(fn(x) { return x }, 1)) }",
    )
    .unwrap();
    let text = m.to_string();
    // Only `k` is captured; `unused` and `f` are not.
    assert!(text.contains("fn @fn.0(i64, i64) -> i64"), "{text}");
    let closure = text.lines().find(|l| l.contains("= closure @fn.0(")).expect("a closure");
    assert!(!closure.contains(','), "{closure}");
    // Capturing nothing makes a plain function value.
    assert!(text.contains("= funcref @fn.1"), "{text}");

    let e = err("fn main() { let k = 1\n let f = fn() { k = 2 } }");
    assert!(e.contains("cannot assign to a captured variable"), "{e}");
    assert!(err("fn main() { let f = fn(x) { return x } }").contains("cannot infer the type of parameter `x`; write `x: T`"));
    assert!(err("fn main() { let f = fn(x: i64, x: i64) {} }").contains("duplicate parameter `x`"));
    let e = err("fn g(f: fn(i64) -> i64) {}\nfn main() { g(fn(x) { print(x) }) }");
    assert!(e.contains("missing `return`"), "{e}");
    let e = err(&fs("fn f(k: i64) -> i64 { let g = fn(x: i64) -> i64 { return x + k }\n return g(1) }"));
    assert!(e.contains("captures `k`") && e.contains("only hosted mode has"), "{e}");
    let e = err("fn adder(n: i64) -> fn(i64) -> i64 { return fn(x) { return x + n } }\nconst A = adder(1)\nfn main() {}");
    assert!(e.contains("closure that captures values cannot be computed at compile time"), "{e}");
    assert!(err("fn main() { let f = fn(x: i64) }").contains("expected the body of the anonymous function"));
    assert!(err("fn main() { let f = fn(*u8) {} }").contains("needs a name"));
}

#[test]
fn closures_in_freestanding_code() {
    // Capturing nothing is fine without a GC, and works at compile time too.
    let m = check(&fs("const SQ = fn(x: i64) -> i64 { return x * x }\nfn f() -> i64 { let g = fn(x: i64) -> i64 { return x + 1 }\n return g(SQ(3)) }")).unwrap();
    // `SQ` is a constant, so calling it is a direct call.
    assert!(m.to_string().contains("call @fn.0(") && m.to_string().contains("funcref @fn.1"), "{m}");
}

// ---- select ----

#[test]
fn select() {
    let m = check(
        "fn main() { let a = chan(i64)\n let b = chan(str, 1)\n select {\n let x = recv(a) => print(x)\n send(b, \"s\") => {}\n _ => {}\n }\n}",
    )
    .unwrap();
    assert!(m.to_string().contains("= select [recv %") && m.to_string().contains("; send %"), "{m}");
    assert!(m.to_string().contains("; default]"), "{m}");

    let s = |arms: &str| err(&format!("fn main() {{ let a = chan(i64)\n select {{\n{arms}\n}}\n}}"));
    assert!(s("let x = recv(a) => {}\n_ => {}\n_ => {}").contains("at most one `_` arm"));
    assert!(s("send(a, \"no\") => {}").contains("the value sent must be i64, found str"));
    assert!(s("recv(1) => {}").contains("`recv` needs a channel, found i64"));
    assert!(s("print(1) => {}").contains("a `select` arm starts with"));
    assert!(s("let x = send(a, 1) => {}").contains("a `select` arm starts with"));
    assert!(s("let x = recv(a) => {}\nrecv(a) => print(x)").contains("unknown variable `x`"));
    assert!(err("fn main() { select {} }").contains("needs at least one arm"));
    assert!(err(&fs("fn f() { select { _ => {} } }")).contains("only available in hosted mode"));
}

// ---- comptime closures ----

#[test]
fn comptime_closures() {
    let src = "fn each(comptime f: fn(i64), xs: [i64; 2]) { f(xs[0])\n f(xs[1]) }\n\
               fn twice(comptime f: fn(i64), xs: [i64; 2]) { each(f, xs)\n each(fn(x) { f(x + 1) }, xs) }\n\
               fn f(k: i64, s: i64) { let xs = [1, 2]\n each(fn(x) { g(x * k) }, xs)\n each(fn(x) { g(x + k + s) }, xs)\n twice(fn(x) { g(x + s) }, xs) }\n\
               fn g(x: i64) {}";
    let m = check(&fs(src)).unwrap();
    let text = m.to_string();
    // One instance per closure; captured values are hidden arguments, and calls
    // are direct: no closure values, no indirect calls.
    assert!(text.contains("fn @each.0([2 x i64], i64) -> unit"), "{text}");
    assert!(text.contains("fn @each.1([2 x i64], i64, i64) -> unit"), "{text}");
    assert!(!text.contains("closure") && !text.contains("call %"), "{text}");

    // Hosted code may keep a comptime closure as a value; freestanding code may not.
    let keep = "fn keep(comptime f: fn(i64) -> i64) -> fn(i64) -> i64 { return f }\n";
    check(&format!("{keep}fn main() {{ let k = 1\n print(keep(fn(x) {{ return x + k }})(1)) }}")).unwrap();
    let e = err(&fs(&format!("{keep}fn h(k: i64) -> i64 {{ return keep(fn(x) {{ return x + k }})(1) }}")));
    assert!(e.contains("`f` was given a function that captures values") && e.contains("in `keep` with f = fn."), "{e}");
    // The closure's type must match the parameter's.
    let e = err("fn each(comptime f: fn(i64), x: i64) { f(x) }\nfn main() { let k = 1\n each(fn(x: u8) { print(k) }, 1) }");
    assert!(e.contains("comptime argument `f` of `each` must be fn(i64), found fn(u8)"), "{e}");
}

// ---- tasks and channels ----

#[test]
fn tasks_and_channels() {
    let m = check(
        "fn worker(c: chan i64, n: i64) { send(c, n * 2) }\n\
         fn main() { let c: chan i64 = chan(i64, 2)\n go worker(c, 1)\n let k = 5\n go fn() { send(c, k) }()\n print(recv(c) + recv(c)) }",
    )
    .unwrap();
    let text = m.to_string();
    assert!(text.contains("= chan %") && text.contains("spawn %") && text.contains("send %") && text.contains("= recv %"), "{text}");

    assert!(err("fn main() { let c = chan(i64)\n send(c, true) }").contains("the value sent must be i64, found bool"));
    assert!(err("fn main() { print(recv(1)) }").contains("`recv` needs a channel, found i64"));
    assert!(err("fn main() { let c = chan(i64, true) }").contains("capacity of a channel must be i64"));
    assert!(err("fn main() { go print(1) }").contains("`print` is a builtin"));
    assert!(err("fn main() { let x = 1\n go x(2) }").contains("`x` is not a function"));
    assert!(err("fn f(a: i64) {}\nfn main() { go f() }").contains("`f` takes 1 arguments, 0 given"));
    assert!(err("fn main() { go 1 + 2 }").contains("`go` needs a function call"));
    assert!(err("fn main() { let c = chan(i64)\n print(c == c) }").contains("cannot apply `==`"));
    assert!(err("const C = chan(i64)\nfn main() {}").contains("channel cannot be computed at compile time"));
    assert!(err(&fs("fn f() { let c = chan(i64) }")).contains("only available in hosted mode"));
    assert!(err(&fs("fn g() {}\nfn f() { go g() }")).contains("only available in hosted mode"));
    assert!(err(&fs("fn f(c: chan u8) {}")).contains("only available in hosted mode"));
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
    assert!(err("fn main() { asm(\"nop\") }").contains("only available in native and freestanding"));
    assert!(err(&fs("fn f() -> bool { return asm(\"nop\", out(reg) bool) }")).contains("output must be an integer or a pointer"));
    assert!(err(&fs("fn f() { asm(\"mov {0}, {1}\", in(reg) 1) }")).contains("only 1 inputs"));
    // A function value is a code pointer, so asm can call it.
    let m = check(&fs("fn g() {}\nfn f() { asm(\"call {0}\", in(reg) g, clobber(\"memory\")) }")).unwrap();
    assert!(m.to_string().contains("funcref @g"), "{m}");
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

// ---- modules ----

/// Checks a program made of in-memory files; `main.jh` is the root.
fn check_files(files: &[(&str, &str)]) -> Result<ir::Module, String> {
    use std::path::{Path, PathBuf};
    let read = |p: &Path| files.iter().find(|(n, _)| Path::new(n) == p).map(|(_, s)| s.to_string());
    let loaded = jihoo_syntax::loader::load_with(Path::new("main.jh"), &[PathBuf::from("lib")], "x86_64", &read)
        .map_err(|(e, files)| format!("{}:{e}", files[e.pos.file as usize].display()))?;
    let m = analyze_modules(&loaded.modules)
        .map_err(|es| format!("{}:{}", loaded.files[es[0].pos.file as usize].display(), es[0]))?;
    ir::verify(&m).expect("sema produced invalid IR");
    Ok(m)
}

#[test]
fn modules_have_their_own_namespaces() {
    let m = check_files(&[
        ("main.jh", "import shapes\nimport util as u\nfn area() -> i64 { return 1 }\n\
                     fn main() {\n  let s = shapes.Square { side: u.SIDE }\n  print(shapes.area(s) + area() + u.area())\n}"),
        ("shapes.jh", "pub struct Square { side: i64 }\npub fn area(s: Square) -> i64 { return s.side * s.side }"),
        // The same names in another module do not clash.
        ("lib/util.jh", "pub const SIDE = 3\npub fn area() -> i64 { return 100 }"),
    ])
    .unwrap();
    let names: Vec<&str> = m.funcs.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"area") && names.contains(&"shapes.area") && names.contains(&"util.area"), "{names:?}");
    assert_eq!(m.structs[0].name, "shapes.Square");
    let mut out = Vec::new();
    jihoo_vm::run(&m, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "110\n");
}

#[test]
fn generics_and_macros_across_modules() {
    let m = check_files(&[
        ("main.jh", "import box\nstruct P { x: i64 }\n\
                     fn main() {\n  let b = box.wrap(P, P { x: 7 })\n  let c = box.Box(i64) { item: box.twice!(21) }\n  print(b.item.x + c.item)\n}"),
        // Inside `box`, its own names need no prefix; `T` may be a type of the caller.
        ("box.jh", "pub struct Box(T: type) { item: T }\n\
                    pub fn wrap(comptime T: type, x: T) -> Box(T) { return Box(T) { item: x } }\n\
                    pub macro twice(e: expr) -> expr { return quote($e * 2) }"),
    ])
    .unwrap();
    assert!(m.structs.iter().any(|s| s.name == "box.Box(P)"), "{m}");
    let mut out = Vec::new();
    jihoo_vm::run(&m, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "49\n");
}

#[test]
fn modules_may_import_each_other() {
    check_files(&[
        ("main.jh", "import even\nfn main() { print(even.is_even(10)) }"),
        ("even.jh", "import odd\npub fn is_even(n: i64) -> bool {\n  if n == 0 { return true }\n  return odd.is_odd(n - 1)\n}"),
        ("odd.jh", "import even\npub fn is_odd(n: i64) -> bool {\n  if n == 0 { return false }\n  return even.is_even(n - 1)\n}"),
    ])
    .unwrap();
}

#[test]
fn module_errors() {
    // Other modules' items need the prefix, and only imported modules exist.
    let e = check_files(&[("main.jh", "import util\nfn main() { print(twice(1)) }"), ("util.jh", "pub fn twice(x: i64) -> i64 { return x }")])
        .unwrap_err();
    assert!(e.contains("unknown function `twice`"), "{e}");
    let e = check_files(&[("main.jh", "fn main() { print(nope.f(1)) }")]).unwrap_err();
    assert!(e.contains("unknown module `nope`"), "{e}");
    let e = check_files(&[("main.jh", "fn main() { let x: nope.T = 1 }")]).unwrap_err();
    assert!(e.contains("unknown module `nope`"), "{e}");
    // Errors carry the file they are in.
    let e = check_files(&[("main.jh", "import util\nfn main() {}"), ("util.jh", "fn f() -> i64 { return true }")])
        .unwrap_err();
    assert!(e.starts_with("util.jh:1:"), "{e}");
    // A freestanding library cannot be used by a hosted program.
    let e = check_files(&[("main.jh", "import io\nfn main() {}"), ("lib/io.jh", "#![freestanding]\nfn f() {}")])
        .unwrap_err();
    assert!(e.contains("module `io` is freestanding-only"), "{e}");
}

#[test]
fn private_items_stay_in_their_module() {
    let lib = "fn helper() -> i64 { return 1 }\nstruct Inner { x: i64 }\nconst K = 2\nmacro m() -> expr { return quote(3) }\n\
               pub struct Outer { x: i64 }\n\
               pub fn api() -> i64 { let i = Inner { x: K }\n return helper() + i.x + m!() }";
    // Inside the module, private items are fine; outside, only `pub` ones.
    let ok = check_files(&[("main.jh", "import lib\nfn main() { let o = lib.Outer { x: 1 }\n print(lib.api() + o.x) }"), ("lib.jh", lib)]);
    assert!(ok.is_ok(), "{}", ok.unwrap_err());
    for (use_, what) in [
        ("print(lib.helper())", "`lib.helper` is private to module `lib`"),
        ("let i = lib.Inner { x: 1 }", "`lib.Inner` is private"),
        ("let i: lib.Inner = lib.make()", "`lib.Inner` is private"),
        ("print(lib.K)", "`lib.K` is private"),
        ("print(lib.m!())", "`lib.m` is private"),
    ] {
        let e = check_files(&[("main.jh", &format!("import lib\nfn main() {{ {use_} }}")), ("lib.jh", lib)]).unwrap_err();
        assert!(e.contains(what), "{use_}: {e}");
    }
}

// ---- statement and item macros ----

#[test]
fn statement_macros_add_statements_to_the_block() {
    let src = "macro swap(a: expr, b: expr) -> stmts {\n  return quote {\n    let tmp = $a\n    $a = $b\n    $b = tmp\n  }\n}\n\
               macro repeat(n: i64, body: expr) -> stmts {\n  let out = quote {}\n  let i = 0\n  while i < n {\n    out = quote {\n      $out\n      $body\n    }\n    i = i + 1\n  }\n  return out\n}\n\
               fn main() {\n  let x = 1\n  let y = 2\n  swap!(x, y)\n  print(x * 10 + y)\n  swap!(x, y)\n  repeat!(3, print(x))\n}";
    assert_eq!(run(src), "21\n1\n1\n1\n");
    // The declarations a statement macro makes are visible after it.
    let src = "macro define(name: str, v: i64) -> stmts { return quote { let $name = $v } }\n\
               fn main() {\n  define!(\"answer\", 42)\n  print(answer)\n}";
    assert_eq!(run(src), "42\n");
}

#[test]
fn item_macros_define_functions_and_types() {
    let src = "macro adders(n: i64) -> items {\n  let out = quote items {}\n  let i = 1\n  while i <= n {\n    out = quote items {\n      $out\n      fn $(\"add\" + stringify(quote($i)))(x: i64) -> i64 { return x + $i }\n    }\n    i = i + 1\n  }\n  return out\n}\n\
               macro point_type(name: str) -> items { return quote items { struct $name { x: i64, y: i64 } } }\n\
               adders!(3)\npoint_type!(\"P\")\n\
               fn main() {\n  let p = P { x: add1(0), y: add3(10) }\n  print(p.x + p.y + add2(0))\n}";
    assert_eq!(run(src), "16\n");
    // Item macros can produce item macro calls, expanded in a later round.
    let src = "macro make_one() -> items { return quote items { fn one() -> i64 { return 1 } } }\n\
               macro both() -> items { return quote items {\n  make_one!()\n  fn two() -> i64 { return one() + 1 }\n} }\n\
               both!()\nfn main() { print(two()) }";
    assert_eq!(run(src), "2\n");
}

#[test]
fn item_macros_across_modules() {
    let m = check_files(&[
        ("main.jh", "import gen\ngen.getter!(\"seven\", 7)\nfn main() { print(seven()) }"),
        ("gen.jh", "pub macro getter(name: str, v: i64) -> items { return quote items { fn $name() -> i64 { return $v } } }"),
    ])
    .unwrap();
    // The produced function belongs to the module that called the macro.
    assert!(m.func("seven").is_some(), "{m}");
}

#[test]
fn statement_and_item_macro_errors() {
    let swap = "macro s() -> stmts { return quote { let a = 1 } }\n";
    assert!(err(&format!("{swap}fn main() {{ let x = s!() }}")).contains("produces stmts, so use it on a line of its own"));
    assert!(err(&format!("{swap}s!()\nfn main() {{}}")).contains("only `items` macros can be used at the top level"));
    let e = err("macro m() -> items { return quote items { fn $(\"no way\")() {} } }\nm!()\nfn main() {}");
    assert!(e.contains("`no way` is not a valid name"), "{e}");
    let e = err("macro m(x: expr) -> stmts { return quote { $(quote items {}) } }\nfn main() { m!(1) }");
    assert!(e.contains("this hole needs `stmts` or `expr`, not items"), "{e}");
    let e = err("macro m() -> items { return quote items { m!() } }\nm!()\nfn main() {}");
    assert!(e.contains("after 16 rounds"), "{e}");
    // Errors inside produced items point at the call.
    let e = err("macro m() -> items { return quote items { fn f() -> i64 { return true } } }\nm!()\nfn main() {}");
    assert!(e.starts_with("2:1:"), "{e}");
    assert!(err("fn f(x: stmts) {}\nfn main() {}").contains("only available in macros"));
}

#[test]
fn unique_names_and_to_str() {
    // `unique` gives each expansion its own temporary, so nested and repeated
    // swaps cannot capture each other's names (or the caller's `tmp`).
    // In a name position `$t` is the name; in an expression, `ident(t)` is.
    let src = "macro swap(a: expr, b: expr) -> stmts {\n  let t = unique(\"tmp\")\n  let tv = ident(t)\n  return quote {\n    let $t = $a\n    $a = $b\n    $b = $tv\n  }\n}\n\
               fn main() {\n  let tmp = 1\n  let other = 2\n  swap!(tmp, other)\n  swap!(other, tmp)\n  swap!(tmp, other)\n  print(tmp * 10 + other)\n  print(to_str(-42) + to_str(true))\n}";
    assert_eq!(run(src), "21\n-42true\n");
    let m = check(src).unwrap();
    let text = main_ir(&m);
    assert!(!text.contains("call"), "{text}");
    // Numbers keep counting across macro runs.
    let src = "macro name() -> items { let n = unique(\"f\")\n return quote items { fn $n() {} } }\nname!()\nname!()\nfn main() {}";
    let m = check(src).unwrap();
    let names: Vec<&str> = m.funcs.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"f__0") && names.contains(&"f__1"), "{names:?}");
    assert!(err("fn main() { let x = unique(\"a\") }").contains("only be used inside a macro"));
    assert!(err("macro m() -> expr { return ident(\"1x\") }\nfn main() { print(m!()) }").contains("`1x` is not a valid name"));
    assert!(err(&fs("fn f() { let s = to_str(1) }")).contains("only available in hosted programs and macros"));
    assert!(err("fn main() { print(to_str(\"s\")) }").contains("takes a number or a bool"));
}

/// Wraps `body` in a native program.
fn native(body: &str) -> String {
    format!("#![native]\n{body}\nfn main() {{}}")
}

#[test]
fn native_programs_call_c() {
    let m = check(&native("extern fn printf(fmt: *u8, ...) -> i32\nextern fn abs(n: i32) -> i32\n\
                           fn f() -> i32 { printf(\"%d %s\\n\", 1 as u8, \"x\")\n return abs(-3) }"))
    .unwrap();
    assert_eq!(m.profile, Profile::Native);
    let printf = m.extern_fn("printf").unwrap();
    assert!(printf.variadic && printf.params == [Type::ptr(Type::U8)]);
    let text = m.to_string();
    assert!(text.contains("extern @printf(*u8, ...) -> i32"), "{text}");
    assert!(text.contains("call @abs("), "{text}");
    // Pointers, `&`, syscall and asm work as in freestanding code; GC types do not.
    check(&native("fn f() -> i64 { let x = 1\n let p = &x\n return *p + syscall(39) }")).unwrap();
    assert!(err(&native("fn f(s: str) {}")).contains("not available in native mode"));
    assert!(err(&native("fn f() { print(1) }")).contains("not available in native mode"));
    // `main` may take C's argc and argv.
    check("#![native]\nfn main(argc: i32, argv: **u8) -> i64 { return argc as i64 }").unwrap();
    assert!(err("#![native]\nfn main(n: i64) {}").contains("(argc: i32, argv: **u8)"));
    assert!(err("#![freestanding]\nfn _start(argc: i32, argv: **u8) {}").contains("must not take parameters"));
}

#[test]
fn extern_rules() {
    assert!(err("extern fn puts(s: *u8) -> i32\nfn main() {}").contains("need native mode"));
    assert!(err("#![freestanding]\nextern fn puts(s: *u8) -> i32\nfn _start() {}").contains("need native mode"));
    let e = err(&native("struct P { x: i64 }\nextern fn f(p: P)"));
    assert!(e.contains("cannot be passed to C; pass a pointer"), "{e}");
    assert!(err(&native("extern fn f() -> [u8; 4]")).contains("cannot be returned from C"));
    assert!(err(&native("extern fn f(cb: fn([u8; 2]))")).contains("cannot be passed to C"));
    let e = err(&native("struct P { x: i64 }\nextern fn printf(fmt: *u8, ...) -> i32\nfn f() { printf(\"\", P { x: 1 }) }"));
    assert!(e.contains("cannot be passed to C"), "{e}");
    assert!(err(&native("extern fn puts(s: *u8) -> i32\nfn f() { puts(1) }")).contains("must be *u8, found i64"));
    let e = err(&native("extern fn printf(fmt: *u8, ...) -> i32\nfn f() { let p = printf }"));
    assert!(e.contains("variable arguments"), "{e}");
    // A non-variadic C function is a function value like any other.
    check(&native("extern fn abs(n: i32) -> i32\nfn f() -> i32 { let g = abs\n return g(-1) }")).unwrap();
    assert!(err(&native("extern fn f(comptime n: i64)")).contains("cannot have `comptime`"));
    let e = err(&native("extern fn abs(n: i32) -> i32\nconst X = abs(-1)"));
    assert!(e.contains("it is a C function"), "{e}");
    assert!(err("#![native]\nextern fn main()").contains("cannot be extern"));
    assert!(err("#![native]\n#![freestanding]\nfn main() {}").contains("already native"));
}

#[test]
fn extern_declarations_across_modules() {
    let libc = "#![native]\npub extern fn puts(s: *u8) -> i32";
    // Declared in a module, named by its C symbol in JIR.
    let m = check_files(&[("main.jh", "#![native]\nimport c\nfn main() { c.puts(\"hi\") }"), ("lib/c.jh", libc)]).unwrap();
    assert!(m.to_string().contains("call @puts("), "{m}");
    // Several declarations of one C function must agree.
    let same = "#![native]\nimport c\nextern fn puts(s: *u8) -> i32\nfn main() { puts(\"a\")\n c.puts(\"b\") }";
    assert_eq!(check_files(&[("main.jh", same), ("lib/c.jh", libc)]).unwrap().externs.len(), 1);
    let differ = "#![native]\nimport c\nextern fn puts(s: *u8) -> i64\nfn main() {}";
    let e = check_files(&[("main.jh", differ), ("lib/c.jh", libc)]).unwrap_err();
    assert!(e.contains("different signature"), "{e}");
    // Only the C functions the program uses are in it...
    let unused = "#![native]\nimport c\nfn puts(s: *u8) -> i32 { return 0 }\nfn main() { puts(\"a\") }";
    assert!(check_files(&[("main.jh", unused), ("lib/c.jh", libc)]).unwrap().externs.is_empty());
    // ...and a used one cannot share its name with one of the program's functions.
    let clash = "#![native]\nimport c\nfn puts(s: *u8) -> i32 { return 0 }\nfn main() { c.puts(\"a\") }";
    let e = check_files(&[("main.jh", clash), ("lib/c.jh", libc)]).unwrap_err();
    assert!(e.contains("same name as the function `puts`"), "{e}");
    // Freestanding libraries work in native programs; native ones need native.
    let io = "#![freestanding]\npub fn f() -> i64 { return syscall(39) }";
    check_files(&[("main.jh", "#![native]\nimport io\nfn main() { io.f() }"), ("lib/io.jh", io)]).unwrap();
    let e = check_files(&[("main.jh", "#![freestanding]\nimport c\nfn _start() {}"), ("lib/c.jh", libc)]).unwrap_err();
    assert!(e.contains("module `c` is native-only, but the program is freestanding"), "{e}");
}

#[test]
fn floats() {
    // A float literal is f64 unless f32 is expected; an integer literal becomes
    // a float where one is expected, if it is exact.
    let m = check("fn f(x: f32) -> f32 { return x * 2 + 0.5 }\nfn main() { let y = 1.5\n let z: f64 = 3\n print(y + z) }").unwrap();
    let f = m.func("f").unwrap();
    assert!(f.regs.iter().all(|t| *t == Type::Float(FloatTy::F32)), "{f}");
    let text = m.to_string();
    assert!(text.contains("fconst 2.0") && text.contains("fconst 0.5") && text.contains("fconst 3.0"), "{text}");
    // `1 + 2.5` is a float; f32 literals are rounded to f32.
    check("fn main() { let x: f64 = 1 + 2.5 }").unwrap();
    let m = check("fn main() { let x: f32 = 0.1 }").unwrap();
    assert!(m.to_string().contains(&format!("fconst {:?}", 0.1f32 as f64)), "{m}");
    assert!(err("fn main() { let x: f32 = 16777217 }").contains("not exactly representable as f32"));
    // No implicit conversions between float types, or floats and integers.
    assert!(err("fn f(a: f32, b: f64) -> f64 { return a + b }\nfn main() {}").contains("cannot apply `+` to f32 and f64"));
    assert!(err("fn f(a: f64, n: i64) -> f64 { return a * n }\nfn main() {}").contains("cannot apply `*` to f64 and i64"));
    check("fn f(a: f32, n: i64) -> f64 { return a as f64 * n as f64 }\nfn g(x: f64) -> u8 { return x as u8 }\nfn main() {}").unwrap();
    // Integer-only operators, and casts that make no sense.
    assert!(err("fn f(a: f64) -> f64 { return a & a }\nfn main() {}").contains("cannot apply `&`"));
    assert!(err("fn f(a: f64) -> f64 { return a << 1 }\nfn main() {}").contains("cannot apply `<<`"));
    assert!(err("fn f(a: f64) -> bool { return a as bool }\nfn main() {}").contains("cannot cast f64 to bool"));
    assert!(err("fn main() { let x = 1.5\n match x { _ => {} } }").contains("cannot `match` on f64"));
    // Floats work at compile time, in structs and arrays, and in every profile.
    check("const K = 2.5 * 4\nstruct P { x: f64, y: f32 }\nfn main() { let a = [K; 3]\n let p = P { x: a[0], y: 1 } }").unwrap();
    check(&fs("fn f(x: f64) -> f32 { return (x % 2.0) as f32 }")).unwrap();
    check(&native("extern fn sqrt(x: f64) -> f64\nfn f() -> f64 { return sqrt(2) }")).unwrap();
    assert!(err(&fs("fn f() { syscall(1, 1.5) }")).contains("must be integers or pointers"));
}
