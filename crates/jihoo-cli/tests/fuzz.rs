//! Differential fuzzing: random programs, run on the VM and compiled with LLVM,
//! must print the same values and exit the same way.
//!
//! Each seed makes one program in the core language that every profile shares:
//! integers of every width and signedness, bools and `f64`, casts, every
//! operator, arrays and structs nested in each other, copied, updated in part,
//! compared, and passed to and returned from functions; `if`, `match`, loops
//! with `break` and `continue`, compound assignment. The program prints values
//! with `out` as it goes and the state of its variables at the end (see
//! `tests/common/mod.rs`). Programs never trap: divisors are made odd and
//! indexes are taken modulo the length, so the VM and LLVM must agree on every
//! value.
//!
//!   cargo test --test fuzz                                  # DEFAULT_SEEDS seeds
//!   JIHOO_FUZZ_SEEDS=5000 cargo test --release --test fuzz  # more
//!   JIHOO_FUZZ_SEED=1234 cargo test --test fuzz             # one, to reproduce
//!
//! A failing seed's program is saved, wrapped for the VM, to a file the failure
//! names. Without `JIHOO_LLC` the programs only run on the VM, which still
//! checks that they compile and run without errors.

mod common;

use std::fmt::Write;

use common::Profile;

const DEFAULT_SEEDS: u64 = 64;

/// SplitMix64: small, fast, and the same everywhere, so a seed always makes
/// the same program.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Ty {
    Int { bits: u32, signed: bool },
    Bool,
    F64,
    Array(Box<Ty>, usize),
    /// An index into `Gen::structs`.
    Struct(usize),
}

impl Ty {
    const I64: Ty = Ty::Int { bits: 64, signed: true };

    fn is_aggregate(&self) -> bool {
        matches!(self, Ty::Array(..) | Ty::Struct(_))
    }
}

/// One step of a path into an aggregate.
#[derive(Debug, Clone)]
enum Step {
    /// An element of an array of this length.
    Elem(usize),
    Field(usize),
}

struct Var {
    name: String,
    ty: Ty,
    /// Loop counters are only read; the generator updates them itself.
    mutable: bool,
}

struct Gen {
    rng: Rng,
    /// Field types of `S0`, `S1`, ...; a struct only holds earlier ones.
    structs: Vec<Vec<Ty>>,
    /// Parameter and result types of `f0`, `f1`, ...; a function only calls
    /// earlier ones, so nothing recurses and every program ends.
    funcs: Vec<(Vec<Ty>, Ty)>,
    vars: Vec<Var>,
    names: usize,
    /// Loops around the statement being made.
    loops: usize,
    src: String,
    indent: usize,
}

impl Gen {
    fn new(seed: u64) -> Gen {
        Gen {
            rng: Rng(seed),
            structs: Vec::new(),
            funcs: Vec::new(),
            vars: Vec::new(),
            names: 0,
            loops: 0,
            src: String::new(),
            indent: 0,
        }
    }

    fn program(mut self) -> String {
        for _ in 0..self.rng.below(4) {
            let fields = (0..1 + self.rng.below(3)).map(|_| self.any_ty(2)).collect();
            self.structs.push(fields);
        }
        for (k, fields) in self.structs.clone().iter().enumerate() {
            self.line(&format!("struct S{k} {{"));
            for (i, t) in fields.iter().enumerate() {
                let t = self.ty_name(t);
                self.line(&format!("    f{i}: {t}"));
            }
            self.line("}");
        }
        for _ in 0..self.rng.below(5) {
            self.function();
        }

        self.line("fn entry() -> i64 {");
        self.indent += 1;
        let n = 6 + self.rng.below(10);
        self.stmts(n, 3);
        // The final state of every variable, every element and field, so that
        // a wrong update shows even where nothing printed the value.
        for v in 0..self.vars.len() {
            let name = self.vars[v].name.clone();
            for (suffix, t) in self.leaves(&self.vars[v].ty.clone()) {
                self.out(&format!("{name}{suffix}"), &t);
            }
        }
        let ret = self.expr(&Ty::I64, 2);
        self.line(&format!("return {ret}"));
        self.indent -= 1;
        self.line("}");
        self.src
    }

    fn function(&mut self) {
        let k = self.funcs.len();
        let params: Vec<Ty> = (0..self.rng.below(4)).map(|_| self.any_ty(2)).collect();
        let ret = self.any_ty(2);
        let list: Vec<String> = params.iter().enumerate().map(|(i, t)| format!("p{i}: {}", self.ty_name(t))).collect();
        let ret_name = self.ty_name(&ret);
        self.line(&format!("fn f{k}({}) -> {ret_name} {{", list.join(", ")));
        self.indent += 1;
        self.vars = params.iter().enumerate().map(|(i, t)| Var { name: format!("p{i}"), ty: t.clone(), mutable: true }).collect();
        let n = 1 + self.rng.below(5);
        self.stmts(n, 2);
        let e = self.expr(&ret, 3);
        self.line(&format!("return {e}"));
        self.indent -= 1;
        self.line("}");
        self.vars.clear();
        self.funcs.push((params, ret));
    }

    // ---- types ----

    fn int_ty(&mut self) -> Ty {
        Ty::Int { bits: *self.rng.pick(&[8, 16, 32, 64]), signed: self.rng.chance(50) }
    }

    fn scalar_ty(&mut self) -> Ty {
        match self.rng.below(10) {
            0 => Ty::Bool,
            1 => Ty::F64,
            _ => self.int_ty(),
        }
    }

    fn any_ty(&mut self, depth: u32) -> Ty {
        match self.rng.below(10) {
            0..=2 if depth > 0 => Ty::Array(Box::new(self.any_ty(depth - 1)), 1 + self.rng.below(4)),
            3 if !self.structs.is_empty() => Ty::Struct(self.rng.below(self.structs.len())),
            _ => self.scalar_ty(),
        }
    }

    fn ty_name(&self, t: &Ty) -> String {
        match t {
            Ty::Int { bits, signed } => format!("{}{bits}", if *signed { "i" } else { "u" }),
            Ty::Bool => "bool".into(),
            Ty::F64 => "f64".into(),
            Ty::Array(e, n) => format!("[{}; {n}]", self.ty_name(e)),
            Ty::Struct(k) => format!("S{k}"),
        }
    }

    /// Every path into a value of type `t`, up to `depth` steps, with the type
    /// it leads to; the empty path first.
    fn paths(&self, t: &Ty, depth: u32) -> Vec<(Vec<Step>, Ty)> {
        let mut out = vec![(Vec::new(), t.clone())];
        if depth == 0 {
            return out;
        }
        let parts: Vec<(Step, Ty)> = match t {
            Ty::Array(e, n) => vec![(Step::Elem(*n), (**e).clone())],
            Ty::Struct(k) => self.structs[*k].iter().cloned().enumerate().map(|(i, f)| (Step::Field(i), f)).collect(),
            _ => Vec::new(),
        };
        for (step, part) in parts {
            for (mut rest, end) in self.paths(&part, depth - 1) {
                rest.insert(0, step.clone());
                out.push((rest, end));
            }
        }
        out
    }

    /// `path` as source; element indexes are computed (always in bounds) unless
    /// `constant`.
    fn path(&mut self, path: &[Step], constant: bool) -> String {
        let mut s = String::new();
        for step in path {
            match step {
                Step::Field(i) => write!(s, ".f{i}").unwrap(),
                Step::Elem(n) if constant || self.rng.chance(40) => write!(s, "[{}]", self.rng.below(*n)).unwrap(),
                Step::Elem(n) => {
                    let t = self.int_ty();
                    let i = self.expr(&t, 1);
                    write!(s, "[(({i}) as u64 % {n}) as i64]").unwrap()
                }
            }
        }
        s
    }

    /// Every scalar inside a value of type `t`: its path, with constant
    /// indexes, and its type.
    fn leaves(&self, t: &Ty) -> Vec<(String, Ty)> {
        let parts: Vec<(String, Ty)> = match t {
            Ty::Array(e, n) => (0..*n).map(|i| (format!("[{i}]"), (**e).clone())).collect(),
            Ty::Struct(k) => self.structs[*k].iter().enumerate().map(|(i, f)| (format!(".f{i}"), f.clone())).collect(),
            _ => return vec![(String::new(), t.clone())],
        };
        let mut out = Vec::new();
        for (step, part) in parts {
            for (rest, end) in self.leaves(&part) {
                out.push((format!("{step}{rest}"), end));
            }
        }
        out
    }

    /// Variable `v` followed by `path`.
    fn place(&mut self, v: usize, path: &[Step], constant: bool) -> String {
        let path = self.path(path, constant);
        format!("{}{path}", self.vars[v].name)
    }

    /// The variables and paths in them that lead to a `t`.
    fn places(&self, t: &Ty, mutable: bool) -> Vec<(usize, Vec<Step>)> {
        let mut out = Vec::new();
        for (v, var) in self.vars.iter().enumerate() {
            if mutable && !var.mutable {
                continue;
            }
            for (path, end) in self.paths(&var.ty, 3) {
                if end == *t {
                    out.push((v, path));
                }
            }
        }
        out
    }

    /// A read of some variable, or part of one, of type `t`.
    fn projection(&mut self, t: &Ty) -> Option<String> {
        let places = self.places(t, false);
        if places.is_empty() {
            return None;
        }
        let (v, path) = &places[self.rng.below(places.len())];
        let path = self.path(path, false);
        Some(format!("{}{path}", self.vars[*v].name))
    }

    // ---- expressions ----

    fn expr(&mut self, t: &Ty, depth: u32) -> String {
        if depth == 0 || self.rng.chance(20) {
            return self.leaf(t, depth);
        }
        let d = depth - 1;
        match self.rng.below(8) {
            // A call to an earlier function that returns a `t`.
            0 => {
                let callees: Vec<usize> = (0..self.funcs.len()).filter(|&k| self.funcs[k].1 == *t).collect();
                if callees.is_empty() {
                    return self.leaf(t, depth);
                }
                let k = *self.rng.pick(&callees);
                let params = self.funcs[k].0.clone();
                let args: Vec<String> = params.iter().map(|p| self.expr(p, d)).collect();
                return format!("f{k}({})", args.join(", "));
            }
            1 => {
                if let Some(p) = self.projection(t) {
                    return p;
                }
            }
            _ => {}
        }
        match t {
            Ty::Int { signed, .. } => match self.rng.below(6) {
                0..=2 => {
                    let op = *self.rng.pick(&["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>"]);
                    let (a, b) = (self.expr(t, d), self.expr(t, d));
                    match op {
                        // Odd, so never zero.
                        "/" | "%" => format!("({a} {op} ({b} | 1))"),
                        _ => format!("({a} {op} {b})"),
                    }
                }
                3 => {
                    let a = self.expr(t, d);
                    if *signed && self.rng.chance(50) {
                        format!("(-{a})")
                    } else {
                        format!("(!{a})")
                    }
                }
                4 => {
                    let from = self.scalar_ty();
                    let e = self.expr(&from, d);
                    format!("(({e}) as {})", self.ty_name(t))
                }
                _ => self.leaf(t, depth),
            },
            Ty::Bool => match self.rng.below(6) {
                0 | 1 => {
                    let operand = if self.rng.chance(80) { self.int_ty() } else { Ty::F64 };
                    let op = *self.rng.pick(&["<", "<=", ">", ">=", "==", "!="]);
                    let (a, b) = (self.expr(&operand, d), self.expr(&operand, d));
                    format!("({a} {op} {b})")
                }
                2 => {
                    let op = *self.rng.pick(&["&&", "||", "&", "|", "^"]);
                    let (a, b) = (self.expr(t, d), self.expr(t, d));
                    format!("({a} {op} {b})")
                }
                3 => format!("(!{})", self.expr(t, d)),
                4 => {
                    // Equality of whole structs and arrays, by value.
                    let aggregates: Vec<Ty> = self.vars.iter().map(|v| v.ty.clone()).filter(Ty::is_aggregate).collect();
                    if aggregates.is_empty() {
                        return self.leaf(t, depth);
                    }
                    let operand = self.rng.pick(&aggregates).clone();
                    let op = *self.rng.pick(&["==", "!="]);
                    let (a, b) = (self.expr(&operand, d), self.expr(&operand, d));
                    format!("({a} {op} {b})")
                }
                _ => self.leaf(t, depth),
            },
            Ty::F64 => match self.rng.below(5) {
                0 | 1 => {
                    let op = *self.rng.pick(&["+", "-", "*", "/", "%"]);
                    let (a, b) = (self.expr(t, d), self.expr(t, d));
                    format!("({a} {op} {b})")
                }
                2 => format!("(-{})", self.expr(t, d)),
                3 => {
                    let from = self.int_ty();
                    format!("(({}) as f64)", self.expr(&from, d))
                }
                _ => self.leaf(t, depth),
            },
            Ty::Array(e, n) => {
                if self.rng.chance(30) {
                    let x = self.expr(e, d);
                    format!("[{x}; {n}]")
                } else {
                    let xs: Vec<String> = (0..*n).map(|_| self.expr(e, d)).collect();
                    format!("[{}]", xs.join(", "))
                }
            }
            Ty::Struct(k) => {
                let fields = self.structs[*k].clone();
                let xs: Vec<String> = fields.iter().enumerate().map(|(i, f)| format!("f{i}: {}", self.expr(f, d))).collect();
                format!("S{k} {{ {} }}", xs.join(", "))
            }
        }
    }

    /// A variable (or part of one) or a literal.
    fn leaf(&mut self, t: &Ty, depth: u32) -> String {
        if self.rng.chance(60) {
            if let Some(p) = self.projection(t) {
                return p;
            }
        }
        match t {
            Ty::Int { .. } => {
                let v: i64 = match self.rng.below(8) {
                    0..=2 => self.rng.below(17) as i64 - 4,
                    3..=5 => *self.rng.pick(&[
                        0,
                        1,
                        -1,
                        127,
                        -128,
                        255,
                        32767,
                        -32768,
                        65535,
                        i32::MAX as i64,
                        i32::MIN as i64,
                        u32::MAX as i64,
                        i64::MAX,
                        i64::MIN + 1,
                    ]),
                    // Any bits; MIN has no literal of its own.
                    _ => (self.rng.next() as i64).max(i64::MIN + 1),
                };
                format!("({v} as {})", self.ty_name(t))
            }
            Ty::Bool => (if self.rng.chance(50) { "true" } else { "false" }).into(),
            Ty::F64 => {
                let x = *self.rng.pick(&["0.0", "-0.0", "1.0", "-1.5", "0.1", "2.5e-3", "1e10", "-3.75", "1e300", "123456.789"]);
                format!("({x})")
            }
            // Aggregates are built from leaves.
            _ => self.expr(t, depth.max(1)),
        }
    }

    // ---- statements ----

    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.src.push_str("    ");
        }
        self.src.push_str(s);
        self.src.push('\n');
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.names += 1;
        format!("{prefix}{}", self.names)
    }

    fn out(&mut self, e: &str, t: &Ty) {
        match t {
            Ty::Int { .. } | Ty::Bool | Ty::F64 => self.line(&format!("out(({e}) as i64)")),
            _ => unreachable!("only scalars are printed"),
        }
    }

    fn stmts(&mut self, n: usize, depth: u32) {
        for _ in 0..n {
            self.stmt(depth);
        }
    }

    /// Statements in braces, whose variables go out of scope at the end.
    fn block(&mut self, depth: u32) {
        let scope = self.vars.len();
        self.indent += 1;
        let n = 1 + self.rng.below(3);
        self.stmts(n, depth);
        self.indent -= 1;
        self.vars.truncate(scope);
    }

    fn stmt(&mut self, depth: u32) {
        match self.rng.below(24) {
            // A copy of an aggregate, then updates deep inside it and the
            // original, one after the other: what goes wrong when the VM updates
            // a part in place that the copy shares.
            21 | 22 => self.aliasing(),
            // A new variable.
            0..=4 => {
                let t = self.any_ty(2);
                let e = self.expr(&t, 3);
                self.declare(t, e);
            }
            // A copy of a whole aggregate, to be updated apart from the original.
            5 => {
                let aggregates: Vec<usize> = (0..self.vars.len()).filter(|&v| self.vars[v].ty.is_aggregate()).collect();
                if let Some(&v) = aggregates.get(self.rng.below(aggregates.len().max(1))) {
                    let (t, e) = (self.vars[v].ty.clone(), self.vars[v].name.clone());
                    self.declare(t, e);
                }
            }
            // Assignment to a variable or a part of one.
            6..=9 => {
                let candidates: Vec<usize> = (0..self.vars.len()).filter(|&v| self.vars[v].mutable).collect();
                if candidates.is_empty() {
                    return;
                }
                let v = *self.rng.pick(&candidates);
                let paths = self.paths(&self.vars[v].ty.clone(), 3);
                let (path, t) = self.rng.pick(&paths).clone();
                let target = self.place(v, &path, false);
                let e = self.expr(&t, 3);
                self.line(&format!("{target} = {e}"));
            }
            // Compound assignment to an integer or float.
            10 | 11 => {
                let t = if self.rng.chance(85) { self.int_ty() } else { Ty::F64 };
                let places = self.places(&t, true);
                if places.is_empty() {
                    return;
                }
                let (v, path) = &places[self.rng.below(places.len())];
                let target = self.place(*v, path, false);
                let ops: &[&str] = if t == Ty::F64 {
                    &["+", "-", "*", "/", "%"]
                } else {
                    &["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>"]
                };
                let op = *self.rng.pick(ops);
                let e = self.expr(&t, 2);
                let e = if t != Ty::F64 && (op == "/" || op == "%") { format!("({e} | 1)") } else { e };
                self.line(&format!("{target} {op}= {e}"));
            }
            12 | 13 => {
                let t = self.scalar_ty();
                let e = self.expr(&t, 3);
                self.out(&e, &t);
            }
            14 | 15 if depth > 0 => {
                let c = self.expr(&Ty::Bool, 3);
                self.line(&format!("if {c} {{"));
                self.block(depth - 1);
                if self.rng.chance(60) {
                    self.line("} else {");
                    self.block(depth - 1);
                }
                self.line("}");
            }
            16 if depth > 0 && self.loops < 2 => {
                // The counter goes up first, so `continue` cannot skip it.
                let i = self.fresh("i");
                let n = 1 + self.rng.below(3);
                self.line(&format!("let {i}: i64 = 0"));
                self.vars.push(Var { name: i.clone(), ty: Ty::I64, mutable: false });
                self.line(&format!("while {i} < {n} {{"));
                self.indent += 1;
                self.line(&format!("{i} = {i} + 1"));
                self.indent -= 1;
                self.loops += 1;
                self.block(depth - 1);
                self.loops -= 1;
                self.line("}");
            }
            17 if depth > 0 => {
                let t = self.int_ty();
                let subject = self.expr(&t, 2);
                self.line(&format!("match {subject} {{"));
                let Ty::Int { signed, .. } = t else { unreachable!() };
                let mut arms: Vec<i64> = (0..1 + self.rng.below(3)).map(|_| self.rng.below(6) as i64 - if signed { 2 } else { 0 }).collect();
                arms.sort();
                arms.dedup();
                for a in arms {
                    self.line(&format!("    {a} => {{"));
                    self.indent += 1;
                    self.block(depth - 1);
                    self.indent -= 1;
                    self.line("    }");
                }
                self.line("    _ => {");
                self.indent += 1;
                self.block(depth - 1);
                self.indent -= 1;
                self.line("    }");
                self.line("}");
            }
            // A call, whose result is kept: so every function runs.
            19 | 20 if !self.funcs.is_empty() => {
                let k = self.rng.below(self.funcs.len());
                let (params, ret) = self.funcs[k].clone();
                let args: Vec<String> = params.iter().map(|p| self.expr(p, 2)).collect();
                self.declare(ret, format!("f{k}({})", args.join(", ")));
            }
            18 if self.loops > 0 => {
                let c = self.expr(&Ty::Bool, 2);
                let jump = if self.rng.chance(50) { "break" } else { "continue" };
                self.line(&format!("if {c} {{ {jump} }}"));
            }
            _ => {
                let t = self.scalar_ty();
                let e = self.expr(&t, 2);
                self.out(&e, &t);
            }
        }
    }

    fn aliasing(&mut self) {
        let nested: Vec<usize> = (0..self.vars.len())
            .filter(|&v| self.vars[v].mutable && self.paths(&self.vars[v].ty, 3).iter().any(|(p, _)| p.len() >= 2))
            .collect();
        if nested.is_empty() {
            // Make one: an array of arrays.
            let inner = self.int_ty();
            let t = Ty::Array(Box::new(Ty::Array(Box::new(inner), 1 + self.rng.below(3))), 2 + self.rng.below(2));
            let e = self.expr(&t, 2);
            self.declare(t, e);
            return;
        }
        let a = *self.rng.pick(&nested);
        let t = self.vars[a].ty.clone();
        self.update_deep(a);
        let name = self.vars[a].name.clone();
        self.declare(t, name);
        let copy = self.vars.len() - 1;
        for _ in 0..2 + self.rng.below(3) {
            let v = if self.rng.chance(50) { a } else { copy };
            self.update_deep(v);
        }
    }

    /// An assignment to a part of `v` at least two steps in, if it has one.
    fn update_deep(&mut self, v: usize) {
        let paths: Vec<(Vec<Step>, Ty)> = self.paths(&self.vars[v].ty.clone(), 3).into_iter().filter(|(p, _)| p.len() >= 2).collect();
        if paths.is_empty() {
            return;
        }
        let (path, t) = self.rng.pick(&paths).clone();
        let constant = self.rng.chance(70);
        let target = self.place(v, &path, constant);
        let e = self.expr(&t, 1);
        self.line(&format!("{target} = {e}"));
    }

    fn declare(&mut self, t: Ty, e: String) {
        let name = self.fresh("v");
        let ty = self.ty_name(&t);
        self.line(&format!("let {name}: {ty} = {e}"));
        self.vars.push(Var { name, ty: t, mutable: true });
    }
}

/// Runs one seed: `Err` describes how the VM and the compiled program differ,
/// or what went wrong, and where the program was saved.
fn check(seed: u64, work: &std::path::Path) -> Result<(), String> {
    let body = Gen::new(seed).program();
    if std::env::var_os("JIHOO_FUZZ_SHOW").is_some() {
        eprintln!("---- seed {seed}\n{body}");
    }
    let name = format!("seed-{seed}");
    let fail = |what: String| {
        let saved = std::env::temp_dir().join(format!("jihoo-fuzz-{seed}.jh"));
        std::fs::write(&saved, common::wrap(&body, Profile::Hosted)).unwrap();
        format!("seed {seed}: {what}\n  the program: {}", saved.display())
    };
    let vm = common::run(&body, Profile::Hosted, work, &name).map_err(|e| fail(format!("on the VM: {e}")))?;
    if common::compiled_enabled() {
        // Freestanding for every seed; native, which links with the C compiler
        // and is slower to build, for some.
        let profiles: &[Profile] = if seed.is_multiple_of(4) { &[Profile::Freestanding, Profile::Native] } else { &[Profile::Freestanding] };
        for &profile in profiles {
            let nat = common::run(&body, profile, work, &name).map_err(|e| fail(format!("{} build: {e}", profile.name())))?;
            if nat != vm {
                let (a, b): (Vec<&str>, Vec<&str>) = (vm.stdout.lines().collect(), nat.stdout.lines().collect());
                let line = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
                return Err(fail(format!(
                    "the VM and the {} build differ: output line {} is {:?} on the VM and {:?} compiled \
                     ({} and {} lines); exit status {:?} and {:?}",
                    profile.name(),
                    line + 1,
                    a.get(line),
                    b.get(line),
                    a.len(),
                    b.len(),
                    vm.status,
                    nat.status
                )));
            }
        }
    }
    Ok(())
}

#[test]
fn vm_and_llvm_agree_on_random_programs() {
    let env = |name: &str| std::env::var(name).ok().map(|v| v.parse::<u64>().unwrap_or_else(|_| panic!("{name} is not a number")));
    let seeds: Vec<u64> = match env("JIHOO_FUZZ_SEED") {
        Some(seed) => vec![seed],
        None => (0..env("JIHOO_FUZZ_SEEDS").unwrap_or(DEFAULT_SEEDS)).collect(),
    };
    if !common::compiled_enabled() {
        eprintln!("JIHOO_LLC is not set: running the programs on the VM only");
    }
    let work = common::work_dir("fuzz");

    // Seeds in parallel; each has its own file names.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let failures: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let (seeds, work) = (&seeds, &work);
                s.spawn(move || seeds.iter().skip(t).step_by(threads).filter_map(|&seed| check(seed, work).err()).collect::<Vec<_>>())
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });

    std::fs::remove_dir_all(&work).unwrap();
    assert!(failures.is_empty(), "{} of {} seeds failed:\n{}", failures.len(), seeds.len(), failures.join("\n"));
}

#[test]
fn programs_depend_only_on_the_seed() {
    assert_eq!(Gen::new(7).program(), Gen::new(7).program());
    assert_ne!(Gen::new(7).program(), Gen::new(8).program());
}
