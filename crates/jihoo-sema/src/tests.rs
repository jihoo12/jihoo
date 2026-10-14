use super::*;
use jihoo_ir::FloatTy;

/// Checks `src`, as a hosted program unless it names a profile: most tests
/// here are about the language itself, and run what they compile on the VM.
/// It compiles on the stack the compiler always has (`on_compiler_stack`), not
/// a test thread's.
fn check(src: &str) -> Result<ir::Module, String> {
    jihoo_syntax::on_compiler_stack(|| check_here(src))
}

fn check_here(src: &str) -> Result<ir::Module, String> {
    let mut prog = jihoo_syntax::parse(src).map_err(|e| e.to_string())?;
    if prog.attrs.is_empty() {
        prog.attrs.push((Default::default(), "hosted".to_string()));
    }
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
    assert_eq!(err("fn main() { break }"), "1:13: `break` outside of a loop");
    assert!(err("fn main() { if true { continue } }").contains("`continue` outside of a loop"));
    // A closure body is a function of its own: the loop around it does not count.
    assert!(err("fn main() { while true {\n  let f = fn() { break }\n  break\n} }").contains("`break` outside"));
    // Code after `break` is unreachable; what follows the loop is not.
    assert!(err("fn f() -> i64 { while true { break } }\nfn main() {}").contains("missing `return`"));
    check("fn f() -> i64 { while true { break\n return 2 }\n return 1 }\nfn main() {}").unwrap();
    // `break` and `continue` are keywords.
    assert!(err("fn main() { let break = 1 }").contains("variable name"));
}

#[test]
fn for_loops() {
    // A literal bound takes the other bound's type.
    check("fn main() { let n: u8 = 3\n for i in 0..n { let b: u8 = i } }").unwrap();
    check("fn main() { for i, c in \"ab\" { let b: u8 = c\n let k: i64 = i } }").unwrap();
    check("fn main() { let xs: [str] = [\"a\"]\n for s in xs { print(s) }\n for x in [1, 2] { print(x) } }").unwrap();
    assert!(err("fn main() { let n: u8 = 3\n let m: i32 = 1\n for i in m..n { } }").contains("end of the range must be i32, found u8"));
    assert!(err("fn main() { for i in 0.5..2.0 { } }").contains("a range needs integers, found f64"));
    assert!(err("fn main() { for i, x in 0..3 { } }").contains("a range gives one value"));
    assert!(err("fn main() { for x in 3 { } }").contains("cannot loop over i64"));
    assert!(err("fn main() { for x of [1] { } }").contains("expected `in`"));
    // The loop's names are its own.
    assert!(err("fn main() { for i in 0..3 { }\n print(i) }").contains("unknown"));
    // `continue` goes to the test, after the counter has stepped.
    check("fn main() { for i in 0..3 { if i == 1 { continue }\n print(i) } }").unwrap();
    assert!(err("fn main() { let for = 1 }").contains("variable name"));
}

#[test]
fn profile_rules() {
    assert!(err("#![comptime]\nfn main() {}").contains("`#![comptime]` is for libraries"));
    assert!(err("#![freestanding]\nfn _start() { print(1) }").contains("not available in freestanding"));
    assert!(err("fn main() { syscall(60, 0) }").contains("only available in native and freestanding"));
    assert!(err(&fs("fn f(s: str) {}")).contains("type `str` needs an allocator"));
    assert!(err("fn f(p: *u8) {}\nfn main() {}").contains("only available in native and freestanding"));
    assert!(err("fn main() { let x = 1\n let p = &x }").contains("only available in native and freestanding"));
    assert!(err(&fs("fn f(p: ptr) {}")).contains("written `*u8`"));
    assert!(err("fn start() {}").contains("needs `fn main()`"));
    assert!(err("fn main() -> bool { return true }").contains("must return nothing or i64"));
    assert!(err("#![hosted]\n#![native]\nfn main() {}").contains("already hosted; it cannot also be native"));
}

#[test]
fn programs_are_native_by_default() {
    // Not through `check`, which makes programs without an attribute hosted.
    let prog = jihoo_syntax::parse("fn main() { let x = 1\n let p = &x }").unwrap();
    assert_eq!(analyze(&prog).unwrap().profile, Profile::Native);
    let prog = jihoo_syntax::parse("#![hosted]\nfn main() {}").unwrap();
    assert_eq!(analyze(&prog).unwrap().profile, Profile::Hosted);
    // A hosted library needs a hosted program.
    let e = check_files(&[("main.jh", "#![native]\nimport vm\nfn main() {}"), ("vm.jh", "#![hosted]\npub fn f() {}")])
        .unwrap_err();
    assert!(e.contains("module `vm` is hosted-only, but the program is native"), "{e}");
    check_files(&[("main.jh", "import vm\nfn main() {}"), ("vm.jh", "#![hosted]\npub fn f() {}")]).unwrap();
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
    let e = err(&format!("{MAX}fn main() {{ print(max(bool, true, false)) }}"));
    assert!(e.contains("cannot apply `>` to bool and bool (in `max` with T = bool)"), "{e}");
    assert!(err(&format!("{MAX}fn main() {{ print(max(i64, 1)) }}")).contains("takes 3 arguments, 2 given"));
    assert!(err(&format!("{MAX}fn main() {{ print(max(5, 1, 2)) }}")).contains("expected a type"));
    assert!(err(&format!("{MAX}fn main() {{ print(max(u8, 1, true)) }}")).contains("argument `b` of `max` must be u8"));
    assert!(err("fn f(comptime T: type) -> i64 { return T }\nfn main() { print(f(i64)) }").contains("is a type (i64), not a value"));
    assert!(err("fn f(comptime N: i64) { N = 1 }\nfn main() { f(1) }").contains("cannot assign to comptime parameter"));
    assert!(err("fn f(x: type) {}\nfn main() {}").contains("`type` can only be the type of a `comptime` parameter"));
    assert!(err("fn main(comptime T: type) {}").contains("comptime parameters"));
}

#[test]
fn inferred_type_arguments() {
    let src = format!(
        "{MAX}{PAIR}fn first(comptime T: type, p: Pair(T)) -> T {{ return p.a }}\n\
         fn swap(comptime A: type, comptime B: type, t: (A, B)) -> (B, A) {{ return (t.1, t.0) }}\n\
         fn get(comptime T: type, o: Option(T), d: T) -> T {{\n  match o {{\n    Some(x) => return x\n    None => return d\n  }}\n}}\n\
         fn map(comptime T: type, comptime U: type, xs: [T], f: fn(T) -> U) -> [U] {{\n  let out: [U] = []\n  for x in xs {{ push(out, f(x)) }}\n  return out\n}}\n\
         fn fold(comptime T: type, comptime A: type, xs: [T], init: A, f: fn(A, T) -> A) -> A {{\n  let acc = init\n  for x in xs {{ acc = f(acc, x) }}\n  return acc\n}}\n\
         fn main() {{\n  let x: u8 = 7\n  print(max(x, 200))\n  print(max(200, x))\n  print(max(3, -5))\n  print(max(1.5, 2.5))\n  print(max(\"app\", \"apple\"))\n\
           print(first(Pair(i64) {{ a: 4, b: 5 }}))\n  print(swap((1, \"one\")).0)\n  let o: Option(u8) = Option.None\n  print(get(o, 255))\n\
           let xs: [u8] = [1, 2]\n  let ys = map(xs, fn(v) -> i64 {{ return v as i64 * 300 }})\n  print(ys[1])\n\
           print(fold(xs, 0, fn(a, v) {{ return a + v as i64 }}))\n  print(max(i64, 1, 2))\n}}"
    );
    assert_eq!(run(&src), "200\n200\n3\n2.5\napple\n4\none\n255\n600\n3\n2\n");
    // Written out or inferred, the types make the same instance.
    let m = check(&format!("{MAX}fn main() {{ print(max(1, 2) + max(i64, 3, 4)) }}")).unwrap();
    assert_eq!(m.funcs.iter().filter(|f| f.name.starts_with("max.")).count(), 1);
}

#[test]
fn inferred_type_argument_errors() {
    let e = err(&format!("{MAX}fn main() {{ let x: u8 = 1\n print(max(x, \"a\")) }}"));
    assert!(e.contains("`T` of `max` is str here, but u8 from an earlier argument"), "{e}");
    let e = err(&format!("{MAX}fn main() {{ print(max(1)) }}"));
    assert!(e.contains("`max` takes 3 arguments (2 without its types), 1 given"), "{e}");
    let e = err("fn f(comptime T: type) -> i64 { return 0 }\nfn main() { print(f()) }");
    assert!(e.contains("cannot infer `T` of `f` from its arguments"), "{e}");
    let e = err("fn f(comptime N: i64, x: i64) -> i64 { return x }\nfn main() { print(f(1)) }");
    assert!(e.contains("`N` is not a type, so it cannot be inferred"), "{e}");
    let e = err("fn ap(comptime T: type, comptime U: type, x: T, f: fn(T) -> U) -> U { return f(x) }\n\
                 fn main() { print(ap(1, fn(v) { return v })) }");
    assert!(e.contains("cannot infer `U` of `ap`: annotate this function's types"), "{e}");
}

// ---- traits ----

const SHAPES: &str = "trait Shape {\n  fn area(s: Self) -> i64\n  fn sides() -> i64\n  fn describe(s: Self) -> i64 { return Self.sides() * 1000 + s.area() }\n}\n\
                      struct Sq { w: i64 }\nstruct Rect { w: i64, h: i64, area: fn(i64) -> i64 }\n\
                      impl Shape for Sq {\n  fn area(s: Sq) -> i64 { return s.w * s.w }\n  fn sides() -> i64 { return 4 }\n}\n\
                      impl Shape for Rect {\n  fn area(s: Self) -> i64 { return s.w * s.h }\n  fn sides() -> i64 { return 4 }\n  fn describe(s: Self) -> i64 { return -1 }\n}\n";

#[test]
fn traits_and_impls() {
    let src = format!(
        "{SHAPES}{PAIR}fn total(comptime T: Shape + Ord, a: T, b: T) -> i64 {{\n  if a.less(b) {{ return a.area() }}\n  return T.sides() + b.describe()\n}}\n\
         impl Ord for Sq {{\n  fn compare(a: Sq, b: Sq) -> i64 {{ return a.w.compare(b.w) }}\n}}\n\
         impl Ord for Pair(u8) {{\n  fn compare(a: Pair(u8), b: Pair(u8)) -> i64 {{ return (a.a + a.b).compare(b.a + b.b) }}\n}}\n\
         fn main() {{\n  let s = Sq {{ w: 3 }}\n  print(s.area())\n  print(s.describe())\n  print(Shape.describe(Sq {{ w: 2 }}))\n  print(Sq.sides())\n\
           let r = Rect {{ w: 2, h: 5, area: fn(x: i64) -> i64 {{ return x + 1 }} }}\n  print(r.area(7))\n  print(Shape.area(r))\n  print(r.describe())\n\
           print(total(s, Sq {{ w: 4 }}))\n  print(total(Sq {{ w: 4 }}, s))\n  let ss = [s, Sq {{ w: 1 }}]\n  print(ss[1].less(ss[0]))\n\
           print(Pair(u8) {{ a: 1, b: 2 }}.compare(Pair(u8) {{ a: 3, b: 0 }}))\n  let x: u8 = 200\n  print(x.compare(7))\n  print(\"b\".less(\"ab\"))\n  print(i64.compare(-1, 1))\n}}"
    );
    assert_eq!(run(&src), "9\n4009\n4004\n4\n8\n10\n-1\n9\n4013\ntrue\n0\n1\nfalse\n-1\n");
    // An impl's functions are instances, called directly; JIR has nothing new.
    let m = check(&src).unwrap();
    let text = m.to_string();
    assert!(text.contains("call @impl.0.area."), "{text}");
    assert!(text.contains("fn @trait.Shape.describe."), "{text}");
}

#[test]
fn trait_errors() {
    let e = err(&format!("{SHAPES}fn f(comptime T: Shape, x: T) -> i64 {{ return x.area() }}\nfn main() {{ print(f(1)) }}"));
    assert!(e.contains("i64 does not implement `Shape`, which `T` of `f` needs"), "{e}");
    let e = err(&format!("{SHAPES}impl Shape for i64 {{\n  fn area(s: i64) -> bool {{ return true }}\n  fn sides() -> i64 {{ return 0 }}\n}}\nfn main() {{}}"));
    assert!(e.contains("`area` of `Shape` for i64 must be fn(i64) -> i64, not fn(i64) -> bool"), "{e}");
    let e = err(&format!("{SHAPES}impl Shape for Sq {{\n  fn area(s: Sq) -> i64 {{ return 0 }}\n  fn sides() -> i64 {{ return 0 }}\n}}\nfn main() {{}}"));
    assert!(e.contains("`Shape` is implemented for Sq twice"), "{e}");
    let e = err(&format!("{SHAPES}struct T {{ x: i64 }}\nimpl Shape for T {{\n  fn sides() -> i64 {{ return 0 }}\n  fn other() {{}}\n}}\nfn main() {{}}"));
    assert!(e.contains("`Shape` has no function `other`"), "{e}");
    let e = err(&format!("{SHAPES}struct T {{ x: i64 }}\nimpl Shape for T {{\n  fn sides() -> i64 {{ return 0 }}\n}}\nfn main() {{}}"));
    assert!(e.contains("this impl of `Shape` for T lacks `area`, which has no default"), "{e}");
    assert!(err("impl Nope for i64 {}\nfn main() {}").contains("unknown trait `Nope`"));
    let e = err(&format!("{SHAPES}fn f(s: Shape) {{}}\nfn main() {{}}"));
    assert!(e.contains("`Shape` is a trait, not a type"), "{e}");
    let e = err(&format!("{SHAPES}fn f(comptime T: i64 + Shape) {{}}\nfn main() {{}}"));
    assert!(e.contains("only traits can be joined with `+`"), "{e}");
    let e = err("fn main() { print(true.less(false)) }");
    assert!(e.contains("bool does not implement `Ord`, which has `less`"), "{e}");
    let e = err("fn main() { print(Ord.compare(1, true)) }");
    assert!(e.contains("`Self` of `Ord.compare` is i64 here, but bool from an earlier argument"), "{e}");
    let e = err(&format!("{SHAPES}fn main() {{ print(Shape.sides()) }}"));
    assert!(e.contains("cannot infer `Self` of `Shape.sides`"), "{e}");
    let e = err(&format!("{SHAPES}trait Two {{\n  fn area(s: Self) -> i64\n}}\nimpl Two for Sq {{\n  fn area(s: Sq) -> i64 {{ return 0 }}\n}}\nfn main() {{ print(Sq {{ w: 1 }}.area()) }}"));
    assert!(e.contains("`area` of Sq is in both `Shape` and `Two`"), "{e}");
    let e = err("trait T {\n  fn f(comptime N: i64)\n}\nfn main() {}");
    assert!(e.contains("trait functions cannot have `comptime` parameters"), "{e}");
}

const SHOW: &str = "trait Show {\n  fn show(s: Self) -> i64\n}\nimpl Show for i64 {\n  fn show(s: i64) -> i64 { return s }\n}\nstruct P { x: i64 }\n";

#[test]
fn generic_impls() {
    let src = format!(
        "{SHOW}{PAIR}impl(T: Show) Show for Pair(T) {{\n  fn show(p: Pair(T)) -> i64 {{ return p.a.show() * 10 + p.b.show() }}\n}}\n\
         impl(A: Show, B: Show) Show for (A, B) {{\n  fn show(t: Self) -> i64 {{ return t.0.show() * 100 + t.1.show() }}\n}}\n\
         struct Sorted(T: Ord) {{ xs: [T] }}\n\
         fn main() {{\n  print(Pair(i64) {{ a: 1, b: 2 }}.show())\n  print(Show.show(Pair(Pair(i64)) {{ a: Pair(i64) {{ a: 1, b: 2 }}, b: Pair(i64) {{ a: 3, b: 4 }} }}))\n\
           print((Pair(i64) {{ a: 5, b: 6 }}, 7).show())\n\
           let o: Option(i64) = Option.None\n  print(o.compare(Option.Some(-9)))\n  print(Option.Some(2).compare(Option.Some(1)))\n\
           print((1, \"b\").compare((1, \"a\")))\n  print((1, 2, 3).less((1, 2, 4)))\n  let xs: [u8] = [1, 2]\n  let ys: [u8] = [1]\n  print(xs.compare(ys))\n\
           let s = Sorted(str) {{ xs: [\"a\"] }}\n  print(len(s.xs))\n}}"
    );
    assert_eq!(run(&src), "12\n154\n5607\n-1\n1\n1\ntrue\n1\n1\n");
}

#[test]
fn generic_impl_errors() {
    let pair_show = "impl(T: Show) Show for Pair(T) {\n  fn show(p: Pair(T)) -> i64 { return 0 }\n}\n";
    let e = err(&format!("{SHOW}{PAIR}{pair_show}impl Show for Pair(i64) {{\n  fn show(p: Pair(i64)) -> i64 {{ return 0 }}\n}}\nfn main() {{}}"));
    assert!(e.contains("`Show` is implemented for Pair(i64) twice"), "{e}");
    let e = err(&format!("{SHOW}{PAIR}{pair_show}impl(U: type) Show for Pair(U) {{\n  fn show(p: Pair(U)) -> i64 {{ return 0 }}\n}}\nfn main() {{}}"));
    assert!(e.contains("`Show` is implemented for Pair(U) twice"), "{e}");
    // A generic impl's signatures are checked per instance.
    let e = err(&format!("{SHOW}{PAIR}impl(T: Show) Show for Pair(T) {{\n  fn show(p: Pair(T)) -> bool {{ return true }}\n}}\nfn main() {{ print(Pair(i64) {{ a: 1, b: 2 }}.show()) }}"));
    assert!(e.contains("`show` of `Show` for Pair(i64) must be fn(Pair(i64)) -> i64, not fn(Pair(i64)) -> bool"), "{e}");
    // An impl applies only when its parameters' bounds hold.
    let e = err(&format!("{SHOW}{PAIR}{pair_show}fn main() {{ print(Pair(P) {{ a: P {{ x: 1 }}, b: P {{ x: 2 }} }}.show()) }}"));
    assert!(e.contains("Pair(P) does not implement `Show`, which has `show`"), "{e}");
    let e = err(&format!("{SHOW}impl(T: Show) Show for T {{\n  fn show(p: T) -> i64 {{ return 0 }}\n}}\nfn main() {{}}"));
    assert!(e.contains("an impl must be for a type, not for any `T`"), "{e}");
    let e = err(&format!("{SHOW}{PAIR}impl(T: Show, U: type) Show for Pair(T) {{\n  fn show(p: Pair(T)) -> i64 {{ return 0 }}\n}}\nfn main() {{}}"));
    assert!(e.contains("`U` is not part of the impl's type"), "{e}");
    let e = err(&format!("{SHOW}impl(N: i64) Show for [i64; N] {{\n  fn show(p: [i64; N]) -> i64 {{ return 0 }}\n}}\nfn main() {{}}"));
    assert!(e.contains("the parameters of an impl are types"), "{e}");
    let e = err("impl(T: Ord) Ord for [[T]] {\n  fn compare(a: [[T]], b: [[T]]) -> i64 { return 0 }\n}\nfn main() {}");
    assert!(e.contains("an impl of `Ord` for [[T]] must be in the module of `Ord`"), "{e}");
    let e = err("struct P { x: i64 }\nstruct Sorted(T: Ord) { xs: [T] }\nfn main() { let s = Sorted(P) { xs: [] } }");
    assert!(e.contains("P does not implement `Ord`, which `T` of `Sorted` needs"), "{e}");
}


const COUNT: &str = "struct Count { n: i64 }\n\
                     impl Iterator for Count {\n  type Item = i64\n  fn next(c: Count) -> Option((Count, i64)) {\n    if c.n == 0 { return Option.None }\n    return Option.Some((Count { n: c.n - 1 }, c.n))\n  }\n}\n";

#[test]
fn associated_types_and_iterators() {
    let src = format!(
        "{COUNT}{PAIR}impl(T: type) Iterator for Pair(T) {{\n  type Item = T\n  fn next(p: Self) -> Option((Self, Self.Item)) {{ return Option.None }}\n}}\n\
         fn total(comptime I: Iterator, it: I) -> I.Item {{\n  let s: I.Item = 0\n  for x in it {{ s = s + x }}\n  return s\n}}\n\
         fn main() {{\n  for x in (Count {{ n: 3 }}) {{ print(x) }}\n  for i, x in (Count {{ n: 2 }}) {{\n    print(i * 10 + x)\n  }}\n\
           print(total(Count {{ n: 100 }}))\n  let n: Count.Item = 7\n  print(n)\n\
           for x in (Pair(str) {{ a: \"a\", b: \"b\" }}) {{ print(x) }}\n\
           let c = Count {{ n: 5 }}\n  for x in c {{\n    if x == 4 {{ continue }}\n    if x == 2 {{ break }}\n    print(x)\n  }}\n  print(c.n)\n}}"
    );
    assert_eq!(run(&src), "3\n2\n1\n2\n11\n5050\n7\n5\n3\n5\n");
    // A trait's default body can use its associated types.
    let src = "trait Box {\n  type In\n  fn get(b: Self) -> Self.In\n  fn twice(b: Self) -> (Self.In, Self.In) { return (b.get(), b.get()) }\n}\n\
               struct B { x: u8 }\nimpl Box for B {\n  type In = u8\n  fn get(b: B) -> u8 { return b.x }\n}\n\
               fn main() {\n  let t = B { x: 4 }.twice()\n  print(t.0 + t.1)\n}";
    assert_eq!(run(src), "8\n");
}

#[test]
fn associated_type_errors() {
    let e = err("struct C { n: i64 }\nimpl Iterator for C {\n  fn next(c: C) -> Option((C, i64)) { return Option.None }\n}\nfn main() {}");
    assert!(e.contains("this impl of `Iterator` for C lacks type `Item`"), "{e}");
    let e = err(&format!("{COUNT}fn main() {{}}").replace("type Item = i64", "type Item = i64\n  type Other = i64"));
    assert!(e.contains("`Iterator` has no type `Other`"), "{e}");
    let e = err(&format!("{COUNT}fn main() {{}}").replace("type Item = i64", "type Item = i64\n  type Item = u8"));
    assert!(e.contains("type `Item` is given twice"), "{e}");
    let e = err(&format!("{COUNT}fn main() {{}}").replace("Option((Count, i64))", "Option((Count, u8))"));
    assert!(e.contains("`next` of `Iterator` for Count must be fn(Count) -> prelude.Option((Count, i64)), not fn(Count) -> prelude.Option((Count, u8))"), "{e}");
    let e = err("trait T {\n  type A\n  type A\n}\nfn main() {}");
    assert!(e.contains("`T` has two types `A`"), "{e}");
    let e = err("struct P { x: i64 }\nfn main() { let x: P.Item = 1 }");
    assert!(e.contains("P does not implement `Iterator`, which has a type `Item`"), "{e}");
    let e = err("fn main() { let x: i64.Nope = 1 }");
    assert!(e.contains("no trait has a type `Nope`"), "{e}");
    let e = err("struct P { x: i64 }\nfn main() {\n  for x in (P { x: 1 }) {}\n}");
    assert!(e.contains("cannot loop over P; `for` walks a list, an array, a str, a range `lo..hi` or a type with an `Iterator`"), "{e}");
    let e = err(&format!("{COUNT}fn f(comptime I: Iterator, it: I) -> I.Item {{ return 0 }}\nfn main() {{ print(f(3)) }}"));
    assert!(e.contains("i64 does not implement `Iterator`, which `I` of `f` needs"), "{e}");
}

#[test]
fn supertraits() {
    let src = "trait Named {\n  fn name(x: Self) -> str\n}\n\
               trait Greet: Named {\n  fn greet(x: Self) -> str { return \"hi \" + x.name() }\n}\n\
               trait Loud: Greet + Ord {\n  fn shout(x: Self) -> str { return x.greet() + \"!\" }\n}\n\
               struct P { n: str }\n\
               impl Named for P {\n  fn name(p: P) -> str { return p.n }\n}\n\
               impl Greet for P {}\n\
               impl Ord for P {\n  fn compare(a: P, b: P) -> i64 { return a.n.compare(b.n) }\n}\n\
               impl Loud for P {}\n\
               fn both(comptime T: Loud, a: T, b: T) -> str {\n  if a < b { return a.shout() }\n  return b.name()\n}\n\
               fn main() {\n  print(both(P { n: \"a\" }, P { n: \"b\" }))\n  print(both(P { n: \"d\" }, P { n: \"c\" }))\n}";
    assert_eq!(run(src), "hi a!\nc\n");
    // A generic impl is checked per type.
    let src = format!(
        "{PAIR}trait A {{\n  fn ay(x: Self) -> i64\n}}\ntrait B: A {{\n  fn bee(x: Self) -> i64 {{ return x.ay() + 1 }}\n}}\n\
         impl(T: type) A for Pair(T) {{\n  fn ay(p: Pair(T)) -> i64 {{ return 1 }}\n}}\nimpl(T: type) B for Pair(T) {{}}\n\
         fn main() {{ print(Pair(u8) {{ a: 1, b: 2 }}.bee()) }}"
    );
    assert_eq!(run(&src), "2\n");
}

#[test]
fn supertrait_errors() {
    let base = "trait A {\n  fn a(x: Self) -> i64\n}\ntrait B: A {\n  fn bee(x: Self) -> i64 { return 0 }\n}\nstruct P { x: i64 }\n";
    let e = err(&format!("{base}impl B for P {{}}\nfn main() {{}}"));
    assert!(e.contains("P implements `B` but not `A`, which `B` needs"), "{e}");
    let e = err(&format!("{base}{PAIR}impl(T: type) B for Pair(T) {{}}\nfn main() {{ print(Pair(u8) {{ a: 1, b: 2 }}.bee()) }}"));
    assert!(e.contains("Pair(u8) implements `B` but not `A`, which `B` needs"), "{e}");
    let e = err("trait A: Nope {}\nfn main() {}");
    assert!(e.contains("unknown trait `Nope`"), "{e}");
    let e = err("trait A: B {}\ntrait B: A {}\nfn main() {}");
    assert!(e.contains("`A` cannot ask for itself"), "{e}");
}

#[test]
fn standard_traits() {
    let src = "struct P { x: i64, y: i64 }\n\
               impl Show for P {\n  fn show(p: P) -> str { return \"P\" + p.x.show() + p.y.show() }\n}\n\
               impl Hash for P {\n  fn hash(p: P, h: Hasher) -> Hasher { return p.y.hash(p.x.hash(h)) }\n}\n\
               fn main() {\n  print(P { x: 1, y: 2 })\n  let xs: [u8] = [3, 1]\n  print(xs)\n  print((1.5, \"a\", true))\n\
                 let o: Option(P) = Option.Some(P { x: 0, y: 9 })\n  print(o)\n  let r: Result(i64, str) = Result.Err(\"no\")\n  print(r)\n\
                 print(hash_of(P { x: 1, y: 2 }) == hash_of(P { x: 1, y: 2 }))\n  print(hash_of(P { x: 1, y: 2 }) == hash_of(P { x: 2, y: 1 }))\n\
                 print(hash_of(\"ab\") == hash_of(\"a\" + \"b\"))\n  print((-0.0).compare(0.0))\n  print((0.0 / 0.0).less(1.0 / 0.0))\n}";
    assert_eq!(run(src), "P12\n[3, 1]\n(1.5, a, true)\nSome(P09)\nErr(no)\ntrue\nfalse\ntrue\n-1\nfalse\n");
    let e = err("struct P { x: i64 }\nfn main() { print(P { x: 1 }) }");
    assert!(e.contains("cannot print a value of type P; give the type a `Show`"), "{e}");
    // The prelude's plain functions and types are only in programs that use them.
    let m = check("fn main() { print(1) }").unwrap();
    assert!(m.structs.is_empty() && m.funcs.len() == 1, "{m}");
    let m = check("fn main() { print(hash_of(1)) }").unwrap();
    assert!(m.structs.iter().any(|s| s.name == "prelude.Hasher") && m.funcs.iter().any(|f| f.name == "prelude.mix"), "{m}");
    // Hashing works in freestanding programs too; showing needs `str`.
    check(&fs("struct P { x: i64 }\nfn f() -> u64 { return hash_of((1, true)) ^ hash_of(Option.Some(2)) }")).unwrap();
}

#[test]
fn orderings_through_ord() {
    let src = "struct V { major: i64, minor: i64 }\n\
               impl Ord for V {\n  fn compare(a: V, b: V) -> i64 {\n    if a.major != b.major { return a.major.compare(b.major) }\n    return a.minor.compare(b.minor)\n  }\n}\n\
               fn max(comptime T: type, a: T, b: T) -> T {\n  if a > b { return a }\n  return b\n}\n\
               fn main() {\n  let a = V { major: 1, minor: 2 }\n  let b = V { major: 1, minor: 5 }\n  print(a < b)\n  print(a >= b)\n  print(max(a, b).minor)\n\
                 print((1, \"b\") <= (1, \"b\"))\n  let xs: [i64] = [1, 2]\n  print(xs > [1])\n  let o: Option(i64) = Option.None\n  print(o < Option.Some(-5))\n}";
    assert_eq!(run(src), "true\nfalse\n5\ntrue\ntrue\ntrue\n");
    // Through `compare`: JIR has a call and an integer comparison, nothing new.
    let m = check(src).unwrap();
    assert!(m.to_string().contains("call @impl."), "{m}");
    let e = err("struct P { x: i64 }\nfn main() { print(P { x: 1 } < P { x: 2 }) }");
    assert!(e.contains("cannot apply `<` to P and P; give the type an `Ord`"), "{e}");
    // `==` stays structural, whatever `compare` says.
    let e = run("struct P { x: i64, y: i64 }\nimpl Ord for P {\n  fn compare(a: P, b: P) -> i64 { return a.x.compare(b.x) }\n}\n\
                 fn main() {\n  let a = P { x: 1, y: 1 }\n  let b = P { x: 1, y: 2 }\n  print(a == b)\n  print(a <= b && a >= b)\n}");
    assert_eq!(e, "false\ntrue\n");
}

#[test]
fn type_shape_and_to_items_are_for_macros() {
    assert!(err("struct P { x: i64 }\nfn main() { print(type_shape(P)) }").contains("`type_shape` can only be used inside a macro"));
    assert!(err("fn main() { let i = to_items(\"\") }").contains("`to_items` can only be used inside a macro"));
    // An item macro may build its items as text.
    let src = "struct P { x: i64, y: i64 }\n\
               macro getters(t: expr) -> items {\n  let s = type_shape(t)\n  return to_items(\"fn shape_text() -> str { return \\\"\" + s + \"\\\" }\")\n}\n\
               getters!(P)\nfn main() { print(shape_text()) }";
    assert_eq!(run(src), "struct x y\n");
}

#[test]
fn types_made_while_compiling_stay_there() {
    // A macro may use types the program cannot have, or does not use.
    let m = check(&fs("macro pick(e: expr) -> expr {\n  let t = (\"x\", 1)\n  let o: Option(str) = Option.Some(\"y\")\n  return quote($e + 1)\n}\nfn f() -> i64 { return pick!(2) }")).unwrap();
    assert!(m.structs.is_empty() && m.enums.is_empty(), "{m}");
    let m = check("macro pick(e: expr) -> expr {\n  let t = (1.5, true)\n  return quote($e + 1)\n}\nfn main() { print(pick!(2)) }").unwrap();
    assert!(m.structs.is_empty(), "{m}");
    // Types the program's code uses are still there, through their parts too.
    let m = check("struct S { p: (i64, Option(u8)) }\nfn main() { let s = S { p: (1, Option.None) }\n print(s.p.0) }").unwrap();
    let names: Vec<&str> = m.structs.iter().map(|s| s.name.as_str()).chain(m.enums.iter().map(|e| e.name.as_str())).collect();
    assert!(names.contains(&"(i64, prelude.Option(u8))") && names.contains(&"prelude.Option(u8)"), "{names:?}");
}

#[test]
fn deep_code_fits_the_compiler_stack() {
    // Close to the parser's bound on nesting, which `COMPILER_STACK` is
    // meant for, even in a debug build.
    let depth = 900;
    let src = format!("fn main() {{ print({}1{}) }}", "(".repeat(depth), ")".repeat(depth));
    assert_eq!(run(&src), "1\n");
    let src = format!("fn main() {{ print({}1) }}", "1 + ".repeat(depth));
    assert_eq!(run(&src), format!("{}\n", depth + 1));
}

#[test]
fn tail_calls_to_itself_are_jumps() {
    let src = "fn count(n: i64, acc: i64) -> i64 {\n  if n == 0 { return acc }\n  return count(n - 1, acc + 1)\n}\n\
               fn swap(a: i64, b: i64, n: i64) -> i64 {\n  if n == 0 { return a * 10 + b }\n  return swap(b, a, n - 1)\n}\n\
               fn depth(n: i64) -> i64 {\n  if n == 0 { return 0 }\n  return 1 + depth(n - 1)\n}\n\
               fn main() {\n  print(count(1000000, 0))\n  print(swap(1, 2, 3))\n  print(depth(10))\n}";
    // A million rounds: more than the VM's call stack holds.
    assert_eq!(run(src), "1000000\n21\n10\n");
    let m = check(src).unwrap();
    let calls = |f: &str| m.func(f).unwrap().blocks.iter().flat_map(|b| &b.insts).filter(|i| matches!(i, Inst::Call { .. })).count();
    assert_eq!(calls("count"), 0, "{m}");
    assert_eq!(calls("swap"), 0, "{m}");
    // `1 + depth(n - 1)` uses the result: still a call.
    assert_eq!(calls("depth"), 1, "{m}");
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

// ---- tuples ----

#[test]
fn tuples() {
    // Tuple types with the same parts are one type, wherever they are written.
    let m = check(
        "fn f() -> (i64, bool) { return (1, true) }\n\
         fn main() { let t: (i64, bool) = f()\n let (a, b) = t\n print(a + t.0)\n print(b) }",
    )
    .unwrap();
    assert_eq!(m.structs.iter().filter(|s| s.name == "(i64, bool)").count(), 1);
    // A part takes its type from the expected tuple type.
    check("fn main() { let t: (u8, (i64, f32)) = (1, (2, 3.0))\n print(t.1.1) }").unwrap();
    // Free of freestanding's limits when the parts are.
    check(&fs("fn f(p: (u8, *u8)) -> u8 { return p.0 }")).unwrap();
}

#[test]
fn tuple_errors() {
    let e = err("fn main() { let t = (1, 2)\n print(t.2) }");
    assert!(e.contains("tuple type (i64, i64) has no part `.2`; it has `.0`, `.1`"), "{e}");
    assert!(err("fn main() { let t = (1, 2)\n print(t.01) }").contains("plain number, such as `.1`"));
    assert!(err("fn main() { let t = (1,) }").contains("at least two parts"));
    assert!(err("fn f(t: (i64)) {}\nfn main() {}").contains("a tuple type has at least two parts"));
    assert!(err("fn main() { let t = (1, print(2)) }").contains("a part of a tuple must have a value"));
    assert!(err("fn f(t: (i64, unit)) {}\nfn main() {}").contains("part 1 of a tuple type cannot be unit"));
    assert!(err("fn main() { let t: (u8, i64) = (1, 2)\n let u: (i64, i64) = t }").contains("must be (i64, i64), found (u8, i64)"));
    assert!(err("fn main() { let x = (1, 2)\n print(x.y) }").contains("has no part `.y`"));
    // `let` patterns must match every value.
    let e = err("fn main() { let (1, b) = (1, 2) }");
    assert!(e.contains("does not match every value of type (i64, i64) (not `(0, _)`); use `match`"), "{e}");
    assert!(err("fn main() { let (a, a) = (1, 2) }").contains("`a` is bound twice"));
    assert!(err("fn main() { let (a, b) = 5 }").contains("a tuple pattern does not match values of type i64"));
    assert!(err("fn main() { let (a, b, c) = (1, 2) }").contains("this pattern has 3 parts, but the value is (i64, i64), which has 2"));
    let e = err("fn main() { match (true, 1) { (true, _) => print(1) } }");
    assert!(e.contains("does not cover `(false, _)`"), "{e}");
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
    assert!(err(&fs("fn f() { let c = cell(1) }")).contains("which freestanding programs cannot"));
    assert!(err(&fs("fn f(c: cell i64) {}")).contains("not available in freestanding mode"));
    // Natively a cell is a counted box; one never reaches another thread.
    check(&native("fn f() -> i64 { let c = cell(1)\n *c = *c + 1\n return *c }")).unwrap();
    let e = err(&native("struct S { c: cell i64 }\nfn w(s: S) {}\nfn f() { go w(S { c: cell(0) }) }"));
    assert!(e.contains("a thread cannot share a cell"), "{e}");
    // A recursive type through `ref` is fine to check.
    check(&native("enum L { Cons(i64, ref L)\n Nil }\nfn w(l: ref L) {}\nfn f() { go w(ref L.Nil) }")).unwrap();
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
    assert!(err(&fs("fn f() { let r = ref 1 }")).contains("which freestanding programs cannot"));
    check(&native("fn f() -> i64 { let r = ref [1, 2]\n return r[1] }")).unwrap();
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
    assert!(e.contains("captures `k`") && e.contains("freestanding programs do not have"), "{e}");
    // Natively a closure owns what it captured, as a list owns its elements.
    let m = check(&native("fn adder(n: [i64]) -> fn(i64) -> i64 { return fn(x: i64) -> i64 { return x + len(n) } }\n\
                           fn g() -> i64 { let f = adder([1, 2])\n return f(1) }"))
    .unwrap();
    assert!(m.to_string().contains(" = closure @fn."), "{m}");
    // C calls function pointers, which cannot carry captured values.
    let e = err(&native("extern fn qsort(b: *u8, n: u64, s: u64, cmp: fn(*u8, *u8) -> i32)\n\
                         fn g(k: i32) { qsort(\"\", 0, 1, fn(a: *u8, b: *u8) -> i32 { return k }) }"));
    assert!(e.contains("C cannot call a closure"), "{e}");
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
    assert!(m.to_string().contains("= select [recv %") && m.to_string().contains(" | send %"), "{m}");
    assert!(m.to_string().contains(" | default]"), "{m}");

    let s = |arms: &str| err(&format!("fn main() {{ let a = chan(i64)\n select {{\n{arms}\n}}\n}}"));
    assert!(s("let x = recv(a) => {}\n_ => {}\n_ => {}").contains("at most one `_` arm"));
    assert!(s("send(a, \"no\") => {}").contains("the value sent must be i64, found str"));
    assert!(s("recv(1) => {}").contains("`recv` needs a channel, found i64"));
    assert!(s("print(1) => {}").contains("a `select` arm starts with"));
    assert!(s("let x = send(a, 1) => {}").contains("a `select` arm starts with"));
    assert!(s("let x = recv(a) => {}\nrecv(a) => print(x)").contains("unknown variable `x`"));
    assert!(err("fn main() { select {} }").contains("needs at least one arm"));
    assert!(err(&fs("fn f() { select { _ => {} } }")).contains("not freestanding"));
    // Natively, select runs on lib/channel.jh, which takes 16 cases at most.
    let many: String = (0..17).map(|_| "recv(c) => {}\n").collect();
    assert!(err(&native(&format!("fn f() {{ let c = chan(i64)\n select {{\n{many} }} }}"))).contains("at most 16 cases"));
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
    assert!(err(&fs("fn f() { let c = chan(i64) }")).contains("not freestanding"));
    assert!(err(&fs("fn g() {}\nfn f() { go g() }")).contains("hosted and native only"));
    assert!(err(&fs("fn f(c: chan u8) {}")).contains("not freestanding"));
    // Natively channels connect threads, so they carry no cells.
    check(&native("fn f() -> i64 { let c = chan([str], 1)\n send(c, [\"a\"])\n return len(recv(c)) }")).unwrap();
    let e = err(&native("fn f() { let c = chan(cell i64) }"));
    assert!(e.contains("a channel cannot carry a cell"), "{e}");
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
fn functions_on_code() {
    // A function whose signature mentions code runs while compiling, and the
    // code it builds is hygienic like the macro's own: its `t` is the macro's.
    let src = "fn twice(e: expr) -> expr { return quote($e + $e) }\n\
               fn with_tmp(e: expr) -> stmts { return quote { let t = $e } }\n\
               fn nest(n: i64, e: expr) -> expr {\n  if n == 0 { return e }\n  return nest(n - 1, twice(e))\n}\n\
               macro quad(x: expr) -> expr { return nest(2, x) }\n\
               macro keep(x: expr) -> stmts { return quote {\n  $(with_tmp(x))\n  print(t * 10)\n} }\n\
               fn main() {\n  let t = 1\n  print(quad!(t + 2))\n  keep!(t)\n  print(t)\n}";
    assert_eq!(run(src), "12\n10\n1\n");
    let m = check(src).unwrap();
    assert!(m.func("twice").is_none() && m.func("nest").is_none(), "{m}");
    // Only macros and other functions on code may use one.
    let f = "fn f(e: expr) -> expr { return e }\n";
    assert!(err(&format!("{f}fn main() {{ let y = f(1) }}")).contains("`f` works on code, so it only runs while compiling"));
    assert!(err(&format!("{f}fn main() {{ let g = f }}")).contains("works on code"));
    assert!(err(&format!("{f}fn g(x: i64) -> i64 {{ f(1)\n return x }}\nfn main() {{}}")).contains("works on code"));
    check(&format!("{f}fn g(e: [expr]) -> expr {{ return f(e[0]) }}\nfn main() {{}}")).unwrap();
}

#[test]
fn block_expressions() {
    assert_eq!(run("fn main() {\n  let x = 1\n  let y = {\n    let x = 5\n    x + 1\n  } * 2\n  print(y)\n  print(x)\n}"), "12\n1\n");
    // A block whose last statement is not an expression is unit.
    assert!(err("fn main() { let u = { let a = 1 } }").contains("would have type unit"));
    assert!(err("macro m() -> stmts { return quote { print(1) } }\nfn main() { let u = { m!() } }").contains("would have type unit"));
    // The value takes the expected type, as literals do.
    check("fn main() { let b: u8 = { 200 } }").unwrap();
    // Names declared in a block end with it.
    assert!(err("fn main() { let a = { let t = 1\n t }\n print(t) }").contains("unknown"));
    // Where a block follows, as after `if`, a `{` is not a block expression.
    assert!(err("fn main() { if { true } { print(1) } }").contains("expected an expression, found `{`"));
    // An expression macro may make statements with a block.
    let src = "macro sq(e: expr) -> expr { return quote({\n  let t = $e\n  t * t\n}) }\nfn main() {\n  let t = 3\n  print(sq!(t + 1) + t)\n}";
    assert_eq!(run(src), "19\n");
}

#[test]
fn types_that_hold_code() {
    // A struct or enum may hold code; it then only exists while compiling,
    // and so do the functions that take or return it.
    let base = "enum Part { Lit(expr), Many([expr]) }\n\
                fn parts(rest: [expr]) -> Part { return Part.Many(rest) }\n\
                fn first(p: Part) -> expr {\n  match p {\n    Lit(e) => return e\n    Many(es) => return es[0]\n  }\n}\n";
    let src = format!("{base}macro head(rest: [expr]) -> expr {{ return first(parts(rest)) }}\nfn main() {{ print(head!(7, 8)) }}");
    assert_eq!(run(&src), "7\n");
    let m = check(&src).unwrap();
    assert!(m.enums.iter().all(|e| e.name != "Part") && m.func("first").is_none(), "{m}");
    let e = err(&format!("{base}fn main() {{ let p = Part.Many([])\n print(1) }}"));
    assert!(e.contains("uses") && e.contains("holds code"), "{e}");
    assert!(err(&format!("{base}fn f(p: Part) -> i64 {{ return 1 }}\nfn main() {{ print(f(Part.Many([]))) }}")).contains("works on code"));
    // `code_text` keeps the hygiene marks that `stringify` drops, and
    // `to_code` turns such text back into code: here the macro's own `t`.
    let src = "macro m(x: expr) -> stmts {\n  let c = quote(t + $x)\n  let back = to_code(code_text(c))\n  return quote {\n    let t = 10\n    print($back)\n    print($(stringify(c)))\n  }\n}\n\
               fn main() {\n  let t = 1\n  m!(t)\n}";
    // A name goes into a hole as it is; anything longer in parentheses.
    assert_eq!(run(src), "11\nt + t\n");
    assert!(err("fn main() { print(code_text(1)) }").contains("`code_text` can only be used inside a macro"));
}

#[test]
fn type_of_in_macros() {
    // The type of code where the macro was called, as text a type hole takes.
    let src = "struct P { x: i64 }\n\
               macro name(e: expr) -> expr { return quote($(type_of(e))) }\n\
               macro copy(e: expr) -> expr {\n  let t = type_of(e)\n  return quote({\n    let c: $t = $e\n    c\n  })\n}\n\
               fn main() {\n  let b: u8 = 7\n  let xs: [str] = []\n  let f = fn(a: i64) -> bool { return a > 0 }\n  let p = P { x: 4 }\n\
               print(name!(b + 1))\n  print(name!(xs))\n  print(name!(f))\n  print(name!(1.5))\n  print(copy!(p).x + copy!(b) as i64)\n}";
    assert_eq!(run(src), "u8\n[str]\nfn(i64) -> bool\nf64\n11\n");
    // Typing the code adds nothing to the function: no multiplication is left.
    let m = check("macro name(e: expr) -> expr { return quote($(type_of(e))) }\nfn main() {\n  let b: u8 = 7\n  print(name!(b * 3))\n}").unwrap();
    assert!(!main_ir(&m).contains("mul "), "{m}");
    // Each `type_of` runs the macro again; `unique` gives the same names each time.
    let src = "macro twice(e: expr) -> expr {\n  let a = unique(\"a\")\n  let t = type_of(e)\n  let b = unique(\"b\")\n\
               return quote({\n    let $a: $t = $e\n    let $b: $t = $(ident(a))\n    $(ident(a)) + $(ident(b))\n  })\n}\n\
               fn main() { print(twice!(21)) }";
    assert_eq!(run(src), "42\n");
    // It sees the caller's names, not the ones the macro's code declares.
    let e = err("macro m() -> expr { return quote({\n  let tmp = 1\n  \"\" + $(type_of(quote(tmp)))\n}) }\nfn main() { print(m!()) }");
    assert!(e.contains("which sees the names where the macro is called") && e.contains("`tmp`"), "{e}");
    assert!(err("fn main() { print(type_of(1)) }").contains("only be used inside a macro"));
}

#[test]
fn macros_take_the_rest_of_their_arguments() {
    // `rest: [expr]` takes the remaining arguments as code, none included,
    // and functions on code take such lists too.
    let src = "fn total(es: [expr]) -> expr {\n  let e = quote(0)\n  for x in es { e = quote($e + $x) }\n  return e\n}\n\
               macro sum(rest: [expr]) -> expr { return total(rest) }\n\
               macro each(label: str, rest: [expr]) -> stmts {\n  let s = quote { print($label) }\n  for x in rest { s = quote {\n    $s\n    print($x)\n  } }\n  return s\n}\n\
               fn main() {\n  let x = 2\n  print(sum!(1, x, x * 10))\n  print(sum!())\n  each!(\"x\", x, x + 1)\n}";
    assert_eq!(run(src), "23\n0\nx\n2\n3\n");
    assert!(err(&format!("{src}\nfn f() {{ each!() }}")).contains("macro `each` takes at least 1 arguments, 0 given"));
    assert!(err("macro m(a: [expr], b: expr) -> expr { return b }\nfn main() {}").contains("must be the last parameter"));
    assert!(err("macro m(a: [i64]) -> expr { return quote(1) }\nfn main() {}").contains("or a last `[expr]`"));
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
    assert!(err("fn main() { let x: expr = 1 }").contains("only available in macros and functions on code"));
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
    // A hole parenthesizes what is not one operand already, and only that.
    let dbl = "macro dbl(x: expr) -> expr { return quote($x * 2) }\n";
    assert_eq!(run(&format!("{dbl}fn main() {{ print(dbl!(1 + 2))\n print(dbl!((1 + 2)))\n print(dbl!(len(\")\") + 1)) }}")), "6\n6\n4\n");
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
    // `Buf(4)` is only measured, so only `Buf(16)` is a type of the program.
    assert_eq!(m.structs.len(), 1, "{text}");
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

/// Checks a program made of in-memory files; `main.jh` is the root, hosted
/// unless it names a profile (written on its first line, so line numbers stay).
fn check_files(files: &[(&str, &str)]) -> Result<ir::Module, String> {
    jihoo_syntax::on_compiler_stack(|| check_files_here(files))
}

fn check_files_here(files: &[(&str, &str)]) -> Result<ir::Module, String> {
    use std::path::{Path, PathBuf};
    let read = |p: &Path| {
        let (name, text) = files.iter().find(|(n, _)| Path::new(n) == p)?;
        Some(if *name == "main.jh" && !text.starts_with("#![") { format!("#![hosted] {text}") } else { text.to_string() })
    };
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
        ("shapes.jh", "pub struct Square { pub side: i64 }\npub fn area(s: Square) -> i64 { return s.side * s.side }"),
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
        ("box.jh", "pub struct Box(T: type) { pub item: T }\n\
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
fn traits_across_modules_and_macros() {
    let run_files = |files: &[(&str, &str)]| {
        let m = check_files(files).unwrap_or_else(|e| panic!("{e}"));
        let mut out = Vec::new();
        jihoo_vm::run(&m, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    };
    let shapes = "pub trait Shape {\n  fn area(s: Self) -> i64\n}\ntrait Hidden {\n  fn h(s: Self) -> i64\n}\n\
                  pub fn twice(comptime T: Shape, x: T) -> i64 { return x.area() * 2 }";
    // An impl of another module's trait, in the type's module; one made by a
    // macro; method calls and spelled-out calls written in macros.
    let main = "import shapes\nstruct Sq { w: i64 }\n\
                impl shapes.Shape for Sq {\n  fn area(s: Sq) -> i64 { return s.w * s.w }\n}\n\
                macro make_ord(t: expr) -> items {\n  return quote items {\n    impl Ord for $t {\n      fn compare(a: $t, b: $t) -> i64 { return a.w.compare(b.w) }\n    }\n  }\n}\n\
                make_ord!(Sq)\n\
                macro smaller(x: expr, y: expr) -> expr {\n  return quote({\n    let a = $x\n    let b = $y\n    let r = Ord.compare(a, b) * 10 + i64.compare(1, 2)\n    if a.less(b) { r = 100 }\n    r\n  })\n}\n\
                fn main() {\n  let s = Sq { w: 3 }\n  print(s.area() + shapes.twice(s) + shapes.Shape.area(s))\n  print(s.less(Sq { w: 4 }))\n  print(smaller!(3, 9))\n  print(smaller!(9, 3))\n}";
    assert_eq!(run_files(&[("main.jh", main), ("shapes.jh", shapes)]), "36\ntrue\n100\n9\n");

    let e = check_files(&[("main.jh", "import shapes\nstruct Sq { w: i64 }\nimpl shapes.Hidden for Sq {\n  fn h(s: Sq) -> i64 { return 0 }\n}\nfn main() {}"), ("shapes.jh", shapes)])
        .unwrap_err();
    assert!(e.contains("`shapes.Hidden` is private to module `shapes`"), "{e}");
    let e = check_files(&[("main.jh", "impl Ord for bool {\n  fn compare(a: bool, b: bool) -> i64 { return 0 }\n}\nfn main() {}")]).unwrap_err();
    assert!(e.contains("an impl of `Ord` for bool must be in the module of `Ord`"), "{e}");
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
               pub struct Outer { pub x: i64 }\n\
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

#[test]
fn private_fields_stay_in_their_module() {
    // `id` is private: only `acct` itself can read it, write it, or build an
    // `Account`; `balance` is `pub`.
    let lib = "pub struct Account { id: i64, pub balance: i64 }\n\
               pub struct Box(T: type) { secret: T, pub open: T }\n\
               pub fn open(id: i64) -> Account { return Account { id: id, balance: 0 } }\n\
               pub fn id(a: Account) -> i64 { return a.id }\n\
               pub fn boxed(x: i64) -> Box(i64) { return Box(i64) { secret: x, open: x + 1 } }\n\
               pub macro id_of(a: expr) -> expr { return quote($a.id) }\n\
               pub macro fresh(n: expr) -> expr { return quote(Account { id: $n, balance: 5 }) }\n\
               pub macro secret_of(b: expr) -> expr { return quote(match $b { Box { secret, .. } => secret }) }";
    let ok = "import acct\nfn main() {\n  let a = acct.open(7)\n  a.balance += 10\n\
              print(a.balance + acct.id(a) + acct.boxed(1).open)\n\
              match a { Account { balance, .. } => print(balance) }\n\
              // A macro of `acct` may use the private fields; code given to it may not.\n\
              print(acct.id_of!(a) + acct.id_of!(acct.fresh!(3)) + acct.secret_of!(acct.boxed(9)))\n}";
    let m = check_files(&[("main.jh", ok), ("acct.jh", lib)]).unwrap();
    let mut out = Vec::new();
    jihoo_vm::run(&m, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "19\n10\n19\n");
    for (use_, what) in [
        ("print(acct.open(1).id)", "field `id` of `acct.Account` is private to module `acct`"),
        ("let a = acct.open(1)\n a.id = 2", "field `id` of `acct.Account` is private"),
        ("let a = acct.Account { id: 1, balance: 2 }", "field `id` of `acct.Account` is private"),
        ("match acct.open(1) { Account { id, .. } => print(id) }", "field `id` of `acct.Account` is private"),
        ("print(acct.boxed(1).secret)", "field `secret` of `acct.Box(i64)` is private"),
        ("print(acct.id_of!(acct.open(1)) + acct.boxed(1).secret)", "field `secret` of `acct.Box(i64)` is private"),
    ] {
        let e = check_files(&[("main.jh", &format!("import acct\nfn main() {{\n {use_} }}")), ("acct.jh", lib)]).unwrap_err();
        assert!(e.contains(what), "{use_}: {e}");
    }
    // Inside the struct's own module every field is in reach, `pub` or not.
    assert_eq!(run("struct P { x: i64, pub y: i64 }\nfn main() { let p = P { x: 1, y: 2 }\n p.x = 3\n print(p.x + p.y) }"), "5\n");
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
fn macros_are_hygienic() {
    // A local the template declares neither captures the caller's `tmp` nor
    // is seen by the caller after the macro.
    let add_tmp = "macro add_tmp(x: expr) -> stmts {\n  return quote {\n    let tmp = 100\n    print($x + tmp)\n  }\n}\n";
    assert_eq!(run(&format!("{add_tmp}fn main() {{\n  let tmp = 1\n  add_tmp!(tmp)\n  print(tmp)\n}}")), "101\n1\n");
    let e = err(&format!("{add_tmp}fn main() {{\n  add_tmp!(1)\n  print(tmp)\n}}"));
    assert!(e.contains("unknown variable `tmp`"), "{e}");
    // The template cannot use the caller's locals by name either, only items.
    let e = err("macro peek() -> expr { return quote(secret) }\nfn main() {\n  let secret = 1\n  print(peek!())\n}");
    assert!(e.contains("unknown variable `secret` (in code produced by `peek!`)"), "{e}");
    // Each expansion has its own locals, also when expansions nest: `t` of
    // `outer!` reaches `twice!` through its argument.
    let src = "macro twice(x: expr) -> expr { return quote(fn() -> i64 { let t = $x\n return t + t }()) }\n\
               macro outer() -> stmts { return quote {\n  let t = 5\n  print(twice!(t) + twice!(t + 1) + t)\n} }\n\
               fn main() {\n  let t = 1000\n  outer!()\n  outer!()\n  print(t)\n}";
    assert_eq!(run(src), "27\n27\n1000\n");
    // Names the template writes for items: the macro's module first, then the
    // caller's; builtins stay builtins; pattern bindings and parameters are locals.
    let src = "struct P { x: i64, y: i64 }\nfn helper(n: i64) -> i64 { return n * 10 }\n\
               macro m(v: expr) -> stmts { return quote {\n  let p = P { x: helper($v), y: callback() }\n  match p {\n    P { x, y: yy } => print(x + yy + len([1, 2]))\n  }\n  let f = fn(k: i64) -> i64 { return k + p.x }\n  print(f(1))\n} }\n\
               fn callback() -> i64 { return 5 }\n\
               fn main() {\n  let x = 7\n  m!(x)\n  print(x)\n}";
    assert_eq!(run(src), "77\n71\n7\n");
    // `stringify` gives the text as written, without hygiene marks.
    let src = "macro show(x: expr) -> expr {\n  let code = quote(helper($x) + total)\n  return quote($(stringify(code)))\n}\n\
               fn main() { print(show!(a + 1)) }";
    assert_eq!(run(src), "helper((a + 1)) + total\n");
}

#[test]
fn macros_use_the_items_of_their_module() {
    let files = [
        ("main.jh", "import lib.util\nfn helper(x: i64) -> i64 { return 0 }\n\
                     fn main() {\n  print(util.scaled!(4))\n  util.show!(util.make!(2))\n}"),
        // Private functions, types and imports of `util`, used by its public macros.
        ("lib/util.jh", "import fmt\nfn helper(x: i64) -> i64 { return x * 10 }\nstruct Pt { v: i64 }\n\
                         pub macro scaled(x: expr) -> expr { return quote(helper($x)) }\n\
                         pub macro make(v: expr) -> expr { return quote(Pt { v: $v }.v) }\n\
                         pub macro show(v: expr) -> stmts { return quote {\n  print(fmt.twice($v))\n} }"),
        ("lib/fmt.jh", "pub fn twice(x: i64) -> i64 { return x + x }"),
    ];
    let m = check_files(&files).unwrap();
    let mut out = Vec::new();
    jihoo_vm::run(&m, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "40\n4\n");
    // The caller still cannot use them itself.
    let mut files = files;
    files[0].1 = "import lib.util\nfn main() { print(util.helper(1)) }";
    let e = check_files(&files).unwrap_err();
    assert!(e.contains("`lib.util.helper` is private to module `lib.util`"), "{e}");
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
    assert!(err("fn main() { let s: stmts = 1 }").contains("only available in macros"));
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
    assert!(err(&fs("fn f() { let s = to_str(1) }")).contains("which freestanding programs do not have"));
    assert!(err("fn main() { print(to_str(\"s\")) }").contains("takes a number or a bool"));
}

/// Wraps `body` in a native program.
fn native(body: &str) -> String {
    format!("#![native]\n{body}\nfn main() {{}}")
}

/// A parameter replaced after it is read is not copied on entry, unless its
/// old and new values can meet (after a branch, or around a loop).
#[test]
fn replaced_parameters_are_not_copied() {
    let m = check(&native(
        "fn renamed(xs: [i64]) -> i64 { let n = len(xs)\n xs = [n]\n push(xs, 1)\n return len(xs) }\n\
         fn fresh(xs: [i64]) -> [i64] { let n = len(xs)\n xs = [n]\n return xs }\n\
         fn maybe(xs: [i64], c: bool) -> i64 { if c { xs = [len(xs)] }\n return len(xs) }\n\
         fn looped(xs: [i64]) -> i64 { for i in 0..3 { xs = xs + [i] }\n return len(xs) }",
    ))
    .unwrap();
    let copies = |name: &str| {
        let f = m.funcs.iter().find(|f| f.name == name).unwrap();
        f.blocks.iter().flat_map(|b| &b.insts).any(|i| matches!(i, ir::Inst::Clone { src, .. } if src.0 == 0))
    };
    assert!(!copies("renamed") && !copies("fresh"));
    assert!(copies("maybe") && copies("looped"));
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
    // Pointers, `&`, syscall and asm work as in freestanding code, and so do lists and `str`.
    check(&native("fn f() -> i64 { let x = 1\n let p = &x\n return *p + syscall(39) }")).unwrap();
    check(&native("fn f(s: str) -> i64 { return len(s) + syscall(1, 1, \"hi\", 2) }")).unwrap();
    // `print` works natively, where text is a C string (`*u8`); not freestanding.
    check(&native("fn f(p: *u8, q: *i32) { print(1)\n print(\"hi\")\n print(p)\n print(1.5)\n print(q != q) }")).unwrap();
    assert!(err(&native("fn f(q: *i32) { print(q) }")).contains("cannot print a value of type *i32"));
    assert!(err(&fs("fn f() { print(1) }")).contains("not available in freestanding mode"));
    // `main` may take C's argc and argv.
    check("#![native]\nfn main(argc: i32, argv: **u8) -> i64 { return argc as i64 }").unwrap();
    assert!(err("#![native]\nfn main(n: i64) {}").contains("(argc: i32, argv: **u8)"));
    assert!(err("#![freestanding]\nfn _start(argc: i32, argv: **u8) {}").contains("must not take parameters"));
}

#[test]
fn extern_rules() {
    assert!(err("extern fn puts(s: *u8) -> i32\nfn main() {}").contains("need native mode"));
    assert!(err("#![freestanding]\nextern fn puts(s: *u8) -> i32\nfn _start() {}").contains("need native mode"));
    // Structs go by value (see `structs_cross_into_c_by_value`); arrays and
    // enums by pointer.
    let e = err(&native("enum P { A }\nextern fn f(p: P)"));
    assert!(e.contains("cannot be passed to or from C; pass a pointer"), "{e}");
    assert!(err(&native("extern fn f() -> [u8; 4]")).contains("cannot be returned to or from C"));
    assert!(err(&native("extern fn f(cb: fn([u8; 2]))")).contains("cannot be passed to or from C"));
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

#[test]
fn float_builtins() {
    // The type follows the argument; a literal is f64 unless f32 is expected.
    let src = "fn main() {\n  let h: f32 = 2.5\n  print(sqrt(16) + floor(-1.5) + ceil(1.2) + trunc(-2.7) + round(2.5) + abs(-0.25))\n\
               print(round(h) + copysign(h, -1.0))\n  let s: f32 = sqrt(2)\n  print(s)\n\
               print(to_bits(1.0))\n  print(to_bits(h))\n  print(from_bits(to_bits(0.1)))\n  let one: f32 = from_bits(0x3f800000)\n  print(one)\n\
               print(to_bits(0.0 / 0.0) == to_bits(-(0.0 / 0.0)))\n  print(copysign(2.0, 0.0 / 0.0))\n}";
    assert_eq!(run(src), "5.25\n0.5\n1.4142135\n4607182418800017408\n1075838976\n0.1\n1.0\ntrue\n2.0\n");
    let m = check("fn f(x: f32) -> u32 { return to_bits(sqrt(x)) }\nfn main() {}").unwrap();
    let f = m.func("f").unwrap().to_string();
    assert!(f.contains("= sqrt %0") && f.contains("= bitcast %"), "{f}");
    // They take floats, and `from_bits` takes u32 or u64.
    assert!(err("fn main() { let n = 4\n print(sqrt(n)) }").contains("`sqrt` takes a float (f32 or f64), not i64"));
    assert!(err("fn main() { print(copysign(1.0)) }").contains("`copysign` takes 2 arguments"));
    assert!(err("fn main() { let a: f32 = 1\n print(copysign(a, 2.0 as f64)) }").contains("must be f32, found f64"));
    assert!(err("fn main() { print(from_bits(1 as i64)) }").contains("`from_bits` takes the bits of an f32 (u32)"));
    assert!(err("fn main() { let f = sqrt }").contains("`sqrt` is a builtin, not a function"));
    // A function of the same name hides the builtin in its module, also for
    // the code macros of other modules produce there.
    assert_eq!(run("fn abs(x: i64) -> i64 { if x < 0 { return -x }\n return x }\nfn main() { print(abs(-3)) }"), "3\n");
    let files = [
        ("main.jh", "import m\nfn round(x: f64) -> f64 { return 100.0 }\nfn main() { print(m.r!(2.5) + round(0.0)) }"),
        ("m.jh", "pub macro r(x: expr) -> expr { return quote(round($x)) }"),
    ];
    let m = check_files(&files).unwrap();
    let mut out = Vec::new();
    jihoo_vm::run(&m, &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "103.0\n");
}

#[test]
fn structs_cross_into_c_by_value() {
    // Structs of scalars, arrays and structs go by value, to C and back.
    let src = "struct P { x: f64, y: f64 }\nstruct Q { p: P, tag: [u8; 3], next: *Q }\n\
               extern fn move(p: P, q: Q) -> Q\n\
               export fn mid(a: P, b: P) -> P { return P { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 } }\n\
               fn f() { let p = mid(P { x: 1, y: 2 }, P { x: 3, y: 4 }) }";
    let m = check(&native(src)).unwrap();
    assert_eq!(m.func("mid").unwrap().export.as_deref(), Some("mid"));
    assert!(m.to_string().contains("fn @mid($P, $P) -> $P export @mid {"), "{m}");
    check(&fs("struct P { x: i32 }\nexport fn get(p: P) -> i32 { return p.x }")).unwrap();
    // A native function value in a struct is code and env, not C's function pointer.
    let e = err(&native("struct S { f: fn(i32) -> i32 }\nextern fn f(s: S)"));
    assert!(e.contains("S cannot be passed to or from C"), "{e}");
    // C has no enums or empty structs, and through `...` only scalars go.
    let e = err(&native("enum E { A, B }\nstruct S { e: E }\nextern fn f(s: S)"));
    assert!(e.contains("S cannot be passed to or from C"), "{e}");
    assert!(err(&native("struct S {}\nextern fn f() -> S")).contains("S cannot be returned to or from C"));
    assert!(err(&native("struct S { f: fn(S) }\nexport fn f(s: S) {}")).contains("cannot be passed to or from C"));
    let e = err(&native("struct P { x: i32 }\nextern fn printf(fmt: *u8, ...) -> i32\nfn f() { printf(\"\", P { x: 1 }) }"));
    assert!(e.contains("P cannot be passed to C as a variable argument"), "{e}");
    // Exported functions are compiled, named once, and not the entry point.
    assert!(err("export fn f() {}\nfn main() {}").contains("need native or freestanding mode"));
    assert!(err(&native("export fn g(comptime n: i64) {}")).contains("cannot have `comptime` parameters"));
    assert!(err("#![native]\nexport fn main() {}").contains("`main` is the program's entry point and cannot be exported"));
    let e = check_files(&[
        ("main.jh", "#![native]\nimport a\nexport fn twice(x: i32) -> i32 { return x * 2 }\nfn main() {}"),
        ("a.jh", "pub export fn twice(x: i32) -> i32 { return x + x }"),
    ])
    .unwrap_err();
    assert!(e.contains("C symbol `twice` is exported twice"), "{e}");
    let e = check_files(&[
        ("main.jh", "#![native]\nimport c\nexport fn puts(s: *u8) -> i32 { return 0 }\nfn main() { c.puts(\"hi\") }"),
        ("c.jh", "pub extern fn puts(s: *u8) -> i32"),
    ])
    .unwrap_err();
    assert!(e.contains("`puts` is exported here and declared as an extern C function the program calls"), "{e}");
}

// ---- strings, lists and the prelude ----

#[test]
fn string_operations() {
    check("fn main() { let s = \"abc\"\n let b: u8 = s[0]\n print(len(s) + len(s[1:]) + len(s[:2]))\n print(s < \"b\") }").unwrap();
    assert!(err("fn main() { let s = \"abc\"\n s[0] = 1 }").contains("cannot assign to a byte of a str"));
    assert!(err("fn main() { let a = [1, 2]\n print(len(a[1:])) }").contains("only a str or list can be sliced"));
    assert!(err("fn main() { print(\"a\"[true:]) }").contains("slice bound must be i64"));
    assert!(err("fn main() { print(read_line(1)) }").contains("`read_line` takes no arguments"));
    assert!(err(&fs("fn f() { let x = read_line() }")).contains("which freestanding programs do not have"));
    // Natively, a literal is a `str` unless a C string is expected.
    let m = check(&native("extern fn puts(s: *u8) -> i32\nfn g(s: str) -> i64 { return len(s) }\n\
                           fn f() -> i64 { puts(\"c\")\n let p: *u8 = \"c\"\n return g(\"abc\" + \"d\") }"))
    .unwrap();
    assert!(m.to_string().contains("str \"abc\""), "{m}");
}

#[test]
fn list_operations() {
    check(
        "struct S { xs: [i64] }\nfn main() {\n let xs: [i64] = [1, 2]\n push(xs, 3)\n let s = S { xs: xs }\n\
         push(s.xs, pop(xs))\n let ys: [[str]] = [[]; 2]\n push(ys[0], \"a\")\n print(len(ys[0]) + len(xs + s.xs))\n\
         print(xs == s.xs) }",
    )
    .unwrap();
    assert!(err("fn main() { let xs = [] }").contains("cannot infer the element type"));
    assert!(err("fn main() { let xs: [i64] = [1]\n push(xs, true) }").contains("value pushed must be i64, found bool"));
    assert!(err("fn main() { let a = [1, 2]\n push(a, 3) }").contains("`push` needs a list, found [i64; 2]"));
    assert!(err("fn f() -> [i64] { return [] }\nfn main() { push(f(), 1) }").contains("cannot `push` on a temporary value"));
    assert!(err("fn main() { let xs: [i64] = [1]\n let r = ref xs\n print(pop(*r)) }").contains("cannot `pop` on a value behind a `ref`"));
    assert!(err(&fs("fn f(xs: [i64]) {}")).contains("lists need an allocator and are not available in freestanding mode"));
    // Lists are ordered when their elements are (`Ord`), and not otherwise.
    assert!(err("fn main() { let xs: [bool] = [true]\n print(xs < xs) }").contains("cannot apply `<` to [bool] and [bool]; give the type an `Ord`"));
    assert!(err("fn main() { let xs: [fn()] = []\n print(xs == xs) }").contains("function values"));
}

#[test]
fn builtin_names_can_be_taken() {
    // A module's own `push` (as `alloc.push` is) wins over the builtin.
    check("fn push(a: i64, b: i64) -> i64 { return a + b }\nfn main() { print(push(1, 2)) }").unwrap();
    check("fn read_line() -> i64 { return 1 }\nfn main() { print(read_line()) }").unwrap();
}

#[test]
fn the_prelude_is_everywhere() {
    check("fn f() -> Option(i64) { return Option.Some(1) }\nfn main() {\n match f() {\n Some(x) => print(x)\n None => {}\n }\n\
           let r: Result(i64, str) = Result.Err(\"no\")\n print(r == Result.Ok(1)) }")
    .unwrap();
    check(&fs("fn f() -> Option(u8) { return Option.None }")).unwrap();
    // A module's own item of the same name comes first.
    check("enum Option { A, B }\nfn main() { let o = Option.A\n print(o == Option.B) }").unwrap();
    // Imported modules see the prelude too.
    check_files(&[
        ("main.jh", "import m\nfn main() { print(m.f() == Option.Some(2)) }"),
        ("m.jh", "pub fn f() -> Option(i64) { return Option.Some(2) }"),
    ])
    .unwrap();
}

#[test]
fn trap_ends_a_path() {
    // Code after `trap()` is unreachable, so the function needs no `return` there.
    let m = check("fn f(x: i64) -> i64 {\n if x > 0 { return x }\n trap()\n}\nfn main() { print(f(1)) }").unwrap();
    assert!(m.to_string().contains("  trap\n  unreachable\n"), "{m}");
    check(&fs("fn f() -> i64 { trap() }")).unwrap();
    assert!(err("fn main() { trap(1) }").contains("`trap` takes no arguments"));
    // A module's own `trap` wins over the builtin.
    check("fn trap() -> i64 { return 1 }\nfn main() { print(trap()) }").unwrap();
}

#[test]
fn atomics_take_pointers_to_integers() {
    let m = check(&fs("fn f(p: *u32, q: **u8) -> bool {\n\
        let old = atomic_add(p, 1)\n\
        atomic_store(p, atomic_load(p) | old)\n\
        let r = atomic_swap(q, 0 as *u8)\n\
        fence()\n\
        return atomic_cas(p, 2, 3) && atomic_cas(q, r, r)\n}"))
    .unwrap();
    let text = m.to_string();
    for inst in ["= atomic_add ", "= atomic_load ", "  atomic_store ", "= atomic_swap ", "  fence\n", "= atomic_cas "] {
        assert!(text.contains(inst), "{inst} missing in\n{text}");
    }
    // A literal takes the type pointed to.
    check(&fs("fn f(p: *u8) -> u8 { return atomic_sub(p, 255) }")).unwrap();
    assert!(err(&fs("fn f(p: *u8) -> u8 { return atomic_sub(p, 256) }")).contains("u8"));
    assert!(err(&fs("fn f(p: *i64) { atomic_add(p, 1 as i32) }")).contains("must be i64, found i32"));
    // Arithmetic is on integers only; loads, stores, swaps and CAS also take pointers.
    assert!(err(&fs("fn f(p: **u8) { atomic_add(p, 0 as *u8) }")).contains("works on integers, not *u8"));
    assert!(err(&fs("fn f(p: *bool) { atomic_store(p, true) }")).contains("integers and pointers, not bool"));
    assert!(err(&fs("fn f(x: i64) { atomic_load(x) }")).contains("needs a pointer, found i64"));
    assert!(err(&fs("fn f(p: *i64) { atomic_cas(p, 1) }")).contains("`atomic_cas` takes 3 arguments"));
    assert!(err(&fs("fn f() { fence(1) }")).contains("`fence` takes no arguments"));
    // Hosted programs have no pointers to share.
    assert!(err("fn main() { fence() }").contains("only available in native and freestanding mode"));
    // A module's own function of the name wins over the builtin.
    check("fn fence() -> i64 { return 1 }\nfn main() { print(fence()) }").unwrap();
}

#[test]
fn qualified_types_are_type_arguments() {
    check_files(&[
        ("main.jh", "import shapes\nfn main() {\n let o: Option(shapes.Dot) = Option.Some(shapes.Dot { x: 1 })\n print(o == Option.None) }"),
        ("shapes.jh", "pub struct Dot {\n pub x: i64\n}"),
    ])
    .unwrap();
}

#[test]
fn modules_may_take_send_and_recv() {
    // `channel.send` is the module's; plain `send` is still the builtin.
    check_files(&[
        ("main.jh", "import ch\nfn main() {\n let c = chan(i64, 1)\n send(c, ch.send(1))\n print(recv(c) + ch.recv(2)) }"),
        ("ch.jh", "pub fn send(x: i64) -> i64 { return x }\npub fn recv(x: i64) -> i64 { return x }"),
    ])
    .unwrap();
}

#[test]
fn go_starts_threads_natively() {
    let m = check("#![native]\nfn work(p: *i64, n: i64) { atomic_add(p, n) }\nfn main() -> i64 {\n let x = 0\n go work(&x, 1)\n return 0 }").unwrap();
    assert!(m.to_string().contains("  spawn %"), "{m}");
    assert!(err(&fs("fn f() {}\nfn g() { go f() }")).contains("`go` needs the VM's scheduler or the C library's threads"));
}

#[test]
fn path_references() {
    let ok = "struct N { v: i64, next: path(G).ns[_] }\nstruct G { ns: [N], first: path(G).ns[_] }\n\
              fn main() { let g = G { ns: [], first: path(G).ns[0] }\n\
              push(g.ns, N { v: 1, next: g.first })\n g[g[g.first].next].v = 2\n print(g.first == g[g.first].next) }";
    let m = check(ok).unwrap();
    // A path is a struct of its indices.
    assert!(m.structs.iter().any(|s| s.name == "path(G).ns[_]" && s.fields.len() == 1));
    let base = "struct A { xs: [i64], r: ref i64 }\nstruct B { a: A }\n";
    let e = |body: &str| err(&format!("{base}fn main() {{ {body} }}"));
    assert!(e("let b = B { a: A { xs: [], r: ref 1 } }\n print(b[path(A).xs[0]])").contains("path(A).xs[_] leads from `A`, not from `B`"));
    assert!(e("let p = path(A).r.x").contains("a path cannot take `.x` of ref i64"));
    assert!(e("let p = path(A).nope").contains("struct `A` has no field `nope`"));
    assert!(e("let p = path(i64)").contains("a path starts at a struct, not i64"));
    assert!(e("let b = B { a: A { xs: [], r: ref 1 } }\n print(b[3])").contains("a struct is indexed with a path"));
    assert!(err("struct A { x: i64 }\nfn f(p: path(A).x[_]) {}\nfn main() {}").contains("a path cannot take an element of i64"));
    // `path_index`: the last index, or index `k`.
    check("struct G { rows: [[i64]] }\nfn main() { let p = path(G).rows[1][2]\n print(path_index(p) * 10 + path_index(p, 0)) }").unwrap();
    assert!(e("print(path_index(path(A).xs[0], 1))").contains("must be a constant below 1"));
    assert!(e("print(path_index(3))").contains("`path_index` needs a path"));
}

#[test]
fn native_lists_are_owned() {
    let ir = |src: &str| check(&format!("#![native]\n{src}")).unwrap().to_string();
    let body = |m: &str, f: &str| -> String {
        let start = m.find(&format!("fn @{f}(")).unwrap();
        m[start..].split("\n}\n").next().unwrap().to_string()
    };
    // A copy of a list still used afterwards is a clone; the last use moves;
    // every value is freed after its last use.
    let m = ir("fn main() -> i64 {\n let a: [i64] = [1]\n let b = a\n push(b, 2)\n let c = b\n return len(a) + len(c) }");
    let main = body(&m, "main");
    assert_eq!(main.matches(" = clone ").count(), 1, "{main}");
    assert_eq!(main.matches("drop ").count(), 2, "{main}");
    // Parameters are borrowed: reading one copies nothing, changing one copies it once.
    let m = ir("fn n(xs: [i64]) -> i64 { return len(xs) }\nfn g(xs: [i64]) -> [i64] { push(xs, 1)\n return xs }\n\
                fn main() -> i64 { let a: [i64] = [1]\n return n(g(a)) }");
    assert!(!body(&m, "n").contains("clone") && !body(&m, "n").contains("drop"), "{m}");
    assert!(body(&m, "g").contains("%0 = clone %0"), "{m}");
    // Reading a part is a view: `len(s.items)` copies nothing.
    let m = ir("struct S { items: [i64] }\nfn main() -> i64 { let s = S { items: [1, 2] }\n return len(s.items) }");
    assert!(!body(&m, "main").contains("clone"), "{m}");
    // ...unless the value it was read from changes while the part is used.
    let m = ir("struct S { items: [i64] }\nfn main() -> i64 { let s = S { items: [1] }\n let x = s.items\n push(s.items, 2)\n return len(x) }");
    assert!(body(&m, "main").contains("clone"), "{m}");
    // A copy of a value still used afterwards is a view of it too: a `for`
    // loop walks the list it was given without copying it...
    let m = ir("fn t(xs: [i64]) -> i64 { let s = 0\n for x in xs { s += x }\n return s + len(xs) }\n\
                fn main() -> i64 { let a: [i64] = [1]\n for x in a { print(x) }\n return t(a) + len(a) }");
    assert!(!m.contains("clone"), "{m}");
    // ...unless the body changes the list, since the loop walks its old value.
    let m = ir("fn main() -> i64 { let a: [i64] = [1]\n for x in a { push(a, x) }\n return len(a) }");
    assert!(body(&m, "main").contains("clone"), "{m}");
    // `xs = f(xs)`: the argument is at its last use, so it moves into the
    // consuming version of `f`, which changes it in place.
    let m = ir("fn f(xs: [i64]) -> [i64] { push(xs, 1)\n return xs }\n\
                fn main() -> i64 { let a: [i64] = [1]\n a = f(a)\n let b = f(a)\n return len(a) + len(b) }");
    let main = body(&m, "main");
    assert!(main.contains("call @f.own(") && main.contains("call @f("), "{main}");
    assert!(!body(&m, "f.own").contains("clone"), "{m}");
    // Pointers never reach memory a value owns.
    let e = |src: &str| err(&format!("#![native]\n{src}"));
    assert!(e("fn main() { let a: [i64] = [1]\n let p = &a }").contains("cannot take the address of [i64]"));
    assert!(e("struct S { xs: [i64] }\nfn f(p: *S) -> S { return *p }\nfn main() {}").contains("cannot be read or written through a pointer"));
    // Fields that do not own memory can still be reached through a pointer.
    ir("struct S { n: i64, xs: [i64] }\nfn f(p: *i64) -> i64 { return *p }\nfn main() { let s = S { n: 1, xs: [] }\n print(s.n) }");
}
