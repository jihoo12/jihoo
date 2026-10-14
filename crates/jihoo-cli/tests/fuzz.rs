//! Differential fuzzing: random programs, run on the VM and compiled with LLVM,
//! must print the same values and exit the same way.
//!
//! Each seed makes one program in the core language that every profile shares:
//! integers of every width and signedness, bools and `f64`, casts, every
//! operator, arrays, structs and tuples nested in each other, copied, updated
//! in part, compared, and passed to and returned from functions; `if`, `match`
//! (on integers, and on tuples with nested patterns), `let` patterns, `while`
//! and `for` loops with `break` and `continue`, compound assignment, and
//! functions that return a part of a value of their own, and tail calls
//! between functions of different signatures; and, in most
//! programs, a trait `Weigh` with a default, impls of it for random structs
//! and scalar types and a generic impl for pairs, derived `Ord` and `Hash`
//! (`meta.derive!`) and `heavier`, a function bounded by `Weigh`, called as
//! methods (`x.weigh()`), through the trait (`Weigh.twice(x)`) and the type
//! (`S0.unit()`), with inferred type arguments, and through `<`, `compare`
//! and `hash_of`; and an `Iterator`, `Rep(T)`, that `for` loops walk. The
//! program prints values with `out` as it goes and the state of its variables
//! at the end (see `tests/common/mod.rs`). Programs never trap: divisors are
//! made odd and indexes are taken modulo the length, so the VM and LLVM must
//! agree on every value.
//!
//!   cargo test --test fuzz                                  # DEFAULT_SEEDS seeds
//!   JIHOO_FUZZ_SEEDS=5000 cargo test --release --test fuzz  # more
//!   JIHOO_FUZZ_SEED=1234 cargo test --test fuzz             # one, to reproduce
//!
//! With lists (`vm_and_native_agree_on_random_list_programs`), programs also
//! hold lists `[T]` and strings in variables, fields and elements, push to and
//! pop from lists, slice, join and compare them, make text with `to_str`, and
//! pass them to functions that change their copy, also as `v.f = grown(v.f)`;
//! they also hold `ref`s and `cell`s, whose copies share (and, for cells, see
//! each other's changes).
//! They run `lists.fuse!` pipelines of `map` and `filter` closures ending in
//! `sum`, `count` or `collect`, and the same pipelines as loops written out,
//! whose results must be the same.
//! Lists never become empty, so indexes (taken modulo the length) are in
//! bounds. Those run on the VM and natively only, where the ownership pass
//! decides where lists are copied and freed; with `JIHOO_VALGRIND` set to
//! valgrind's path, any leak or invalid access fails the seed (memory C threads
//! keep until they finish is not a leak).
//!
//!   JIHOO_FUZZ_LIST_SEEDS=500 cargo test --release --test fuzz list
//!
//! A failing seed's program is saved, wrapped for the VM, to a file the failure
//! names. Without `JIHOO_LLC` the programs only run on the VM, which still
//! checks that they compile and run without errors.

mod common;

use std::fmt::Write;

use common::Profile;

const DEFAULT_SEEDS: u64 = 64;

/// What a program prints when a `lists.fuse!` pipeline and the same pipeline
/// written out as a loop give different results; it fails the seed.
const PIPELINE_DIFFERS: i64 = -6_148_914_691_236_517_206;

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
    /// Never empty.
    List(Box<Ty>),
    Str,
    Ref(Box<Ty>),
    Cell(Box<Ty>),
    /// Two or three parts.
    Tuple(Vec<Ty>),
}

impl Ty {
    const I64: Ty = Ty::Int { bits: 64, signed: true };

    fn is_aggregate(&self) -> bool {
        matches!(self, Ty::Array(..) | Ty::Struct(_) | Ty::List(_) | Ty::Ref(_) | Ty::Cell(_) | Ty::Tuple(_))
    }
}

/// One step of a path into an aggregate.
#[derive(Debug, Clone)]
enum Step {
    /// An element of an array of this length.
    Elem(usize),
    Field(usize),
    /// An element of a list.
    Item,
    /// A part of a tuple.
    Part(usize),
}

struct Var {
    name: String,
    ty: Ty,
    /// Loop counters are only read; the generator updates them itself.
    mutable: bool,
}

struct Gen {
    rng: Rng,
    /// Whether to make lists (hosted and native programs only).
    lists: bool,
    /// Field types of `S0`, `S1`, ...; a struct only holds earlier ones.
    structs: Vec<Vec<Ty>>,
    /// Parameter and result types of `f0`, `f1`, ...; a function only calls
    /// earlier ones, so nothing recurses and every program ends.
    funcs: Vec<(Vec<Ty>, Ty)>,
    vars: Vec<Var>,
    names: usize,
    /// Loops around the statement being made.
    loops: usize,
    /// Whether the program has the trait `Weigh` and its impls (see `traits`).
    traits: bool,
    /// The types with an `impl Weigh` of their own; 2-tuples of such types
    /// have one too when `tuple_impl`.
    weigh: Vec<Ty>,
    tuple_impl: bool,
    /// Which structs have a derived `Ord` and a derived `Hash`.
    ord: Vec<bool>,
    hash: Vec<bool>,
    /// Making the body of an impl, which must not call the trait again.
    in_impl: bool,
    src: String,
    indent: usize,
}

impl Gen {
    fn new(seed: u64) -> Gen {
        Gen::with_lists(seed, false)
    }

    fn with_lists(seed: u64, lists: bool) -> Gen {
        Gen {
            rng: Rng(seed),
            lists,
            structs: Vec::new(),
            funcs: Vec::new(),
            vars: Vec::new(),
            names: 0,
            loops: 0,
            traits: false,
            weigh: Vec::new(),
            tuple_impl: false,
            ord: Vec::new(),
            hash: Vec::new(),
            in_impl: false,
            src: String::new(),
            indent: 0,
        }
    }

    fn program(mut self) -> String {
        if self.lists {
            self.line("import lists");
            // Changes its copy of a list: `v.f = grown(v.f)` moves the field
            // out and back in natively (`take`).
            for l in ["fn grown(comptime T: type, xs: [T]) -> [T] {", "    push(xs, xs[0])", "    return xs", "}"] {
                self.line(l);
            }
        }
        self.traits = self.rng.chance(60);
        if self.traits {
            self.line("import meta");
        }
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
        if self.traits {
            self.trait_items();
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
            let t = self.vars[v].ty.clone();
            self.dump(&name, &t);
        }
        let ret = self.expr(&Ty::I64, 2);
        self.line(&format!("return {ret}"));
        self.indent -= 1;
        self.line("}");
        self.src
    }

    fn function(&mut self) {
        let k = self.funcs.len();
        // Some parameters have the type of the one before, so that a call can
        // pass one value for both.
        let mut params: Vec<Ty> = Vec::new();
        for _ in 0..self.rng.below(4) {
            let t = match params.last() {
                Some(last) if self.rng.chance(30) => last.clone(),
                _ => self.any_ty(2),
            };
            params.push(t);
        }
        let ret = self.any_ty(2);
        let list: Vec<String> = params.iter().enumerate().map(|(i, t)| format!("p{i}: {}", self.ty_name(t))).collect();
        let ret_name = self.ty_name(&ret);
        self.line(&format!("fn f{k}({}) -> {ret_name} {{", list.join(", ")));
        self.indent += 1;
        self.vars = params.iter().enumerate().map(|(i, t)| Var { name: format!("p{i}"), ty: t.clone(), mutable: true }).collect();
        let n = 1 + self.rng.below(5);
        self.stmts(n, 2);
        let e = if self.rng.chance(30) { self.returned_part(&ret) } else { self.expr(&ret, 3) };
        self.line(&format!("return {e}"));
        self.indent -= 1;
        self.line("}");
        self.vars.clear();
        self.funcs.push((params.clone(), ret.clone()));
        if self.rng.chance(50) {
            self.tail_calls(k, &params, &ret);
        }
    }

    /// `f{j}`, which gives what `f{k}` does through a few tail calls between
    /// `t{j}` and `u{j}`, functions with other signatures: natively the
    /// arguments are handed over at each step.
    fn tail_calls(&mut self, k: usize, params: &[Ty], ret: &Ty) {
        let j = self.funcs.len();
        let ret_name = self.ty_name(ret);
        let typed: Vec<String> = params.iter().enumerate().map(|(i, t)| format!("p{i}: {}", self.ty_name(t))).collect();
        let names: Vec<String> = (0..params.len()).map(|i| format!("p{i}")).collect();
        let (typed, names) = (typed.join(", "), names.join(", "));
        let sep = if params.is_empty() { "" } else { ", " };
        let rounds = self.rng.below(5);
        for l in [
            format!("fn f{j}({typed}) -> {ret_name} {{"),
            format!("    return t{j}({rounds}{sep}{names})"),
            "}".to_string(),
            format!("fn t{j}(n: i64{sep}{typed}) -> {ret_name} {{"),
            "    if n > 0 {".to_string(),
            format!("        return u{j}({names}{sep}n - 1)"),
            "    }".to_string(),
            format!("    return f{k}({names})"),
            "}".to_string(),
            format!("fn u{j}({typed}{sep}n: i64) -> {ret_name} {{"),
            format!("    return t{j}(n{sep}{names})"),
            "}".to_string(),
        ] {
            self.line(&l);
        }
        self.funcs.push((params.to_vec(), ret.clone()));
    }

    /// A part of a value of the function's own, to be returned: the value is
    /// freed at the `return`, after the part is copied out of it.
    fn returned_part(&mut self, ret: &Ty) -> String {
        let (t, part) = match self.rng.below(3) {
            0 => {
                let other = self.any_ty(1);
                let k = self.rng.below(2);
                let parts = if k == 0 { vec![ret.clone(), other] } else { vec![other, ret.clone()] };
                (Ty::Tuple(parts), format!(".{k}"))
            }
            1 if self.lists => (Ty::List(Box::new(ret.clone())), "[0]".to_string()),
            _ => {
                let n = 1 + self.rng.below(3);
                (Ty::Array(Box::new(ret.clone()), n), format!("[{}]", self.rng.below(n)))
            }
        };
        let e = self.expr(&t, 3);
        let name = self.fresh("w");
        let ty = self.ty_name(&t);
        self.line(&format!("let {name}: {ty} = {e}"));
        format!("{name}{part}")
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
            0..=2 if depth > 0 && self.lists && self.rng.chance(50) => Ty::List(Box::new(self.any_ty(depth - 1))),
            4 if self.lists => Ty::Str,
            5 if self.lists && depth > 0 && self.rng.chance(50) => {
                let inner = Box::new(self.any_ty(depth - 1));
                if self.rng.chance(50) {
                    Ty::Ref(inner)
                } else {
                    Ty::Cell(inner)
                }
            }
            0..=2 if depth > 0 => Ty::Array(Box::new(self.any_ty(depth - 1)), 1 + self.rng.below(4)),
            6 if depth > 0 => Ty::Tuple((0..2 + self.rng.below(2)).map(|_| self.any_ty(depth - 1)).collect()),
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
            Ty::List(e) => format!("[{}]", self.ty_name(e)),
            Ty::Str => "str".into(),
            Ty::Ref(e) => format!("ref {}", self.ty_name(e)),
            Ty::Cell(e) => format!("cell {}", self.ty_name(e)),
            Ty::Tuple(parts) => format!("({})", parts.iter().map(|p| self.ty_name(p)).collect::<Vec<_>>().join(", ")),
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
            Ty::List(e) => vec![(Step::Item, (**e).clone())],
            Ty::Struct(k) => self.structs[*k].iter().cloned().enumerate().map(|(i, f)| (Step::Field(i), f)).collect(),
            Ty::Tuple(parts) => parts.iter().cloned().enumerate().map(|(i, p)| (Step::Part(i), p)).collect(),
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

    /// `base` followed by `path`, as source; element indexes are computed
    /// (always in bounds) unless `constant`.
    fn path(&mut self, base: &str, path: &[Step], constant: bool) -> String {
        let mut s = base.to_string();
        for step in path {
            match step {
                Step::Field(i) => write!(s, ".f{i}").unwrap(),
                Step::Part(i) => write!(s, ".{i}").unwrap(),
                // Lists are never empty.
                Step::Item if constant || self.rng.chance(40) => s.push_str("[0]"),
                Step::Item => {
                    let t = self.int_ty();
                    let i = self.expr(&t, 1);
                    s = format!("{s}[(({i}) as u64 % (len({s}) as u64)) as i64]");
                }
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

    /// Variable `v` followed by `path`.
    fn place(&mut self, v: usize, path: &[Step], constant: bool) -> String {
        let name = self.vars[v].name.clone();
        self.path(&name, path, constant)
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

    /// A read of some variable, or part of one, of type `t`, possibly what a
    /// `ref` or `cell` holds.
    fn projection(&mut self, t: &Ty) -> Option<String> {
        let direct = self.places(t, false);
        let boxed: Vec<(usize, Vec<Step>)> = if self.lists {
            let (r, c) = (Ty::Ref(Box::new(t.clone())), Ty::Cell(Box::new(t.clone())));
            self.places(&r, false).into_iter().chain(self.places(&c, false)).collect()
        } else {
            Vec::new()
        };
        let n = direct.len() + boxed.len();
        if n == 0 {
            return None;
        }
        let k = self.rng.below(n);
        if k < direct.len() {
            let (v, path) = direct[k].clone();
            Some(self.place(v, &path, false))
        } else {
            let (v, path) = boxed[k - direct.len()].clone();
            Some(format!("(*{})", self.place(v, &path, false)))
        }
    }

    /// Prints every scalar in `e`, a `t`: the length and each element of a list.
    fn dump(&mut self, e: &str, t: &Ty) {
        match t {
            Ty::Int { .. } | Ty::Bool | Ty::F64 => self.out(e, t),
            Ty::Array(el, n) => {
                for i in 0..*n {
                    self.dump(&format!("{e}[{i}]"), el);
                }
            }
            Ty::Struct(k) => {
                for (i, f) in self.structs[*k].clone().iter().enumerate() {
                    self.dump(&format!("{e}.f{i}"), f);
                }
            }
            Ty::Str => {
                self.out(&format!("len({e})"), &Ty::I64);
                let i = self.fresh("d");
                self.line(&format!("let {i}: i64 = 0"));
                self.line(&format!("while {i} < len({e}) {{"));
                self.indent += 1;
                self.out(&format!("{e}[{i}]"), &Ty::Int { bits: 8, signed: false });
                self.line(&format!("{i} = {i} + 1"));
                self.indent -= 1;
                self.line("}");
            }
            Ty::Ref(el) | Ty::Cell(el) => self.dump(&format!("(*{e})"), el),
            Ty::Tuple(parts) => {
                for (i, p) in parts.clone().iter().enumerate() {
                    self.dump(&format!("{e}.{i}"), p);
                }
            }
            Ty::List(el) => {
                self.out(&format!("len({e})"), &Ty::I64);
                let i = self.fresh("d");
                self.line(&format!("let {i}: i64 = 0"));
                self.line(&format!("while {i} < len({e}) {{"));
                self.indent += 1;
                self.dump(&format!("{e}[{i}]"), el);
                self.line(&format!("{i} = {i} + 1"));
                self.indent -= 1;
                self.line("}");
            }
        }
    }

    // ---- expressions ----

    fn expr(&mut self, t: &Ty, depth: u32) -> String {
        if depth == 0 || self.rng.chance(20) {
            return self.leaf(t, depth);
        }
        let d = depth - 1;
        if self.traits && !self.in_impl && self.rng.chance(10) {
            if let Some(e) = self.trait_expr(t, d) {
                return e;
            }
        }
        match self.rng.below(8) {
            // A call to an earlier function that returns a `t`. Not at depth
            // 1, where `leaf` builds aggregates: a call's arguments may hold a
            // `t` again, and the generator would recurse without end.
            0 if depth > 1 => {
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
                    let operand = if self.lists && self.rng.chance(20) {
                        Ty::Str
                    } else if self.rng.chance(80) {
                        self.int_ty()
                    } else {
                        Ty::F64
                    };
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
                    // Cells are not compared: two cells are two, whatever they hold.
                    let aggregates: Vec<Ty> =
                        self.vars.iter().map(|v| v.ty.clone()).filter(|t| t.is_aggregate() && !self.has_cell(t)).collect();
                    if aggregates.is_empty() {
                        return self.leaf(t, depth);
                    }
                    let operand = self.rng.pick(&aggregates).clone();
                    let op = *self.rng.pick(&["==", "!="]);
                    // A list literal needs a list type from where it is used,
                    // which `==` does not give: compare variables.
                    let (a, b) = if self.has_list(&operand) {
                        match (self.projection(&operand), self.projection(&operand)) {
                            (Some(a), Some(b)) => (a, b),
                            _ => return self.leaf(t, depth),
                        }
                    } else {
                        (self.expr(&operand, d), self.expr(&operand, d))
                    };
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
            Ty::Tuple(parts) => {
                let xs: Vec<String> = parts.clone().iter().map(|p| self.expr(p, d)).collect();
                format!("({})", xs.join(", "))
            }
            Ty::Str => match self.rng.below(6) {
                0 => format!("({} + {})", self.expr(t, d), self.expr(t, d)),
                1 => {
                    let from = if self.rng.chance(70) { self.int_ty() } else { self.rng.pick(&[Ty::Bool, Ty::F64]).clone() };
                    format!("to_str({})", self.expr(&from, d))
                }
                2 => match self.projection(t) {
                    Some(p) => {
                        let it = self.int_ty();
                        let i = self.expr(&it, 1);
                        format!("{p}[(({i}) as u64 % (len({p}) as u64 + 1)) as i64:]")
                    }
                    None => self.leaf(t, d),
                },
                _ => self.leaf(t, d),
            },
            Ty::Ref(e) => match self.projection(t) {
                Some(p) if self.rng.chance(40) => p,
                _ => format!("ref {}", self.expr(e, d)),
            },
            // A copy of an existing cell shares it.
            Ty::Cell(e) => match self.projection(t) {
                Some(p) if self.rng.chance(40) => p,
                _ => format!("cell({})", self.expr(e, d)),
            },
            // Never empty. Literals only where a list type is expected, which
            // is everywhere `expr` is used for one.
            Ty::List(e) => match self.rng.below(6) {
                0 => {
                    let x = self.expr(e, d);
                    format!("[{x}; {}]", 1 + self.rng.below(3))
                }
                1 | 2 => match self.projection(t) {
                    // From a list somewhere in a variable: a slice from an
                    // index in range to the end, or joined with another.
                    Some(p) if self.rng.chance(50) => {
                        let it = self.int_ty();
                        let i = self.expr(&it, 1);
                        format!("{p}[(({i}) as u64 % (len({p}) as u64)) as i64:]")
                    }
                    Some(p) => match self.projection(t) {
                        Some(q) => format!("({p} + {q})"),
                        None => p,
                    },
                    None => self.list_literal(e, d),
                },
                _ => self.list_literal(e, d),
            },
        }
    }

    fn list_literal(&mut self, e: &Ty, d: u32) -> String {
        let xs: Vec<String> = (0..1 + self.rng.below(3)).map(|_| self.expr(e, d)).collect();
        format!("[{}]", xs.join(", "))
    }

    fn has_cell(&self, t: &Ty) -> bool {
        match t {
            Ty::Cell(_) => true,
            Ty::Array(e, _) | Ty::List(e) | Ty::Ref(e) => self.has_cell(e),
            Ty::Struct(k) => self.structs[*k].iter().any(|f| self.has_cell(f)),
            Ty::Tuple(parts) => parts.iter().any(|p| self.has_cell(p)),
            _ => false,
        }
    }

    fn has_list(&self, t: &Ty) -> bool {
        match t {
            Ty::List(_) => true,
            Ty::Ref(e) | Ty::Cell(e) => self.has_list(e),
            Ty::Array(e, _) => self.has_list(e),
            Ty::Struct(k) => self.structs[*k].iter().any(|f| self.has_list(f)),
            Ty::Tuple(parts) => parts.iter().any(|p| self.has_list(p)),
            _ => false,
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
            Ty::Str => format!("\"{}\"", *self.rng.pick(&["", "a", "ab", "hello", "a,b", "\\n", "zz", "Ab"])),
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
        if self.lists && self.rng.chance(25) && self.list_stmt() {
            return;
        }
        if self.lists && self.rng.chance(15) && self.cell_stmt() {
            return;
        }
        if self.lists && self.rng.chance(6) {
            self.pipeline();
            return;
        }
        if self.traits && depth > 0 && self.loops < 2 && self.rng.chance(4) && self.rep_loop(depth) {
            return;
        }
        match self.rng.below(27) {
            24 => self.let_pattern(),
            25 | 26 if depth > 0 => self.tuple_match(depth),
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
            16 if depth > 0 && self.loops < 2 && self.rng.chance(50) => self.for_loop(depth),
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
                // Sometimes aggregates go in through variables used for
                // nothing else, so the call is their last use and hands them
                // over (`f.own` natively), at times one for two parameters.
                let temps = self.rng.chance(40);
                let mut made: Vec<(Ty, String)> = Vec::new();
                let mut args = Vec::new();
                for p in &params {
                    if !(temps && p.is_aggregate()) {
                        args.push(self.expr(p, 2));
                        continue;
                    }
                    let again = made.iter().find(|(t, _)| t == p).map(|(_, n)| n.clone());
                    match again {
                        Some(n) if self.rng.chance(50) => args.push(n),
                        _ => {
                            let (n, e) = (self.fresh("t"), self.expr(p, 2));
                            self.line(&format!("let {n}: {} = {e}", self.ty_name(p)));
                            made.push((p.clone(), n.clone()));
                            args.push(n);
                        }
                    }
                }
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

    /// A `for` loop over a range, or over an array, list or str variable with
    /// the index, which the body may change: the loop walks the value the
    /// variable had when it started.
    fn for_loop(&mut self, depth: u32) {
        let seqs: Vec<usize> =
            (0..self.vars.len()).filter(|&v| matches!(self.vars[v].ty, Ty::Array(..) | Ty::List(_) | Ty::Str)).collect();
        let scope = self.vars.len();
        if seqs.is_empty() || self.rng.chance(30) {
            let i = self.fresh("i");
            let n = self.rng.below(4);
            self.line(&format!("for {i} in 0..{n} {{"));
            self.vars.push(Var { name: i, ty: Ty::I64, mutable: false });
        } else {
            let v = *self.rng.pick(&seqs);
            let (seq, ty) = (self.vars[v].name.clone(), self.vars[v].ty.clone());
            let el = match ty {
                Ty::Array(el, _) | Ty::List(el) => *el,
                _ => Ty::Int { bits: 8, signed: false },
            };
            let (i, x) = (self.fresh("i"), self.fresh("x"));
            self.line(&format!("for {i}, {x} in {seq} {{"));
            self.vars.push(Var { name: i, ty: Ty::I64, mutable: false });
            self.vars.push(Var { name: x, ty: el, mutable: true });
        }
        self.loops += 1;
        self.block(depth - 1);
        self.loops -= 1;
        self.vars.truncate(scope);
        self.line("}");
    }

    /// A `for` loop over an iterator, `Rep(T)`, that holds a copy of a
    /// variable's value and gives it with each item.
    fn rep_loop(&mut self, depth: u32) -> bool {
        // A type argument is parsed as an expression, which `cell T` is not.
        let held: Vec<usize> = (0..self.vars.len()).filter(|&v| !self.ty_name(&self.vars[v].ty).contains("cell ")).collect();
        if held.is_empty() {
            return false;
        }
        let scope = self.vars.len();
        let v = *self.rng.pick(&held);
        let (held, ty) = (self.vars[v].name.clone(), self.vars[v].ty.clone());
        let (i, x) = (self.fresh("i"), self.fresh("x"));
        let n = self.rng.below(4);
        self.line(&format!("for {i}, {x} in (Rep({}) {{ x: {held}, n: {n} }}) {{", self.ty_name(&ty)));
        self.vars.push(Var { name: i, ty: Ty::I64, mutable: false });
        self.vars.push(Var { name: x, ty: Ty::Tuple(vec![Ty::I64, ty]), mutable: true });
        self.loops += 1;
        self.block(depth - 1);
        self.loops -= 1;
        self.vars.truncate(scope);
        self.line("}");
        true
    }

    /// Replaces what a cell somewhere in a variable holds, or pushes to the
    /// list one holds, if there is one: every copy of the cell sees it.
    fn cell_stmt(&mut self) -> bool {
        let mut cells = Vec::new();
        for (v, var) in self.vars.iter().enumerate() {
            for (path, end) in self.paths(&var.ty, 3) {
                if let Ty::Cell(e) = end {
                    cells.push((v, path, *e));
                }
            }
        }
        if cells.is_empty() {
            return false;
        }
        let (v, path, e) = cells[self.rng.below(cells.len())].clone();
        let target = self.place(v, &path, true);
        match e {
            Ty::List(item) if self.rng.chance(50) => {
                let x = self.expr(&item, 2);
                self.line(&format!("push(*{target}, {x})"));
            }
            _ => {
                let x = self.expr(&e, 2);
                self.line(&format!("*{target} = {x}"));
            }
        }
        true
    }

    /// Pushes to or pops from a list somewhere in a variable, if there is one.
    fn list_stmt(&mut self) -> bool {
        let mut lists = Vec::new();
        for (v, var) in self.vars.iter().enumerate() {
            if !var.mutable {
                continue;
            }
            for (path, end) in self.paths(&var.ty, 3) {
                if let Ty::List(e) = end {
                    lists.push((v, path, *e));
                }
            }
        }
        if lists.is_empty() {
            return false;
        }
        let (v, path, e) = lists[self.rng.below(lists.len())].clone();
        let constant = self.rng.chance(50);
        let target = self.place(v, &path, constant);
        if self.rng.chance(25) {
            self.line(&format!("{target} = grown({target})"));
        } else if self.rng.chance(60) {
            let x = self.expr(&e, 2);
            self.line(&format!("push({target}, {x})"));
        } else {
            // Lists never become empty.
            let x = self.fresh("x");
            let t = self.ty_name(&e);
            self.line(&format!("if len({target}) > 1 {{"));
            self.indent += 1;
            self.line(&format!("let {x}: {t} = pop({target})"));
            if self.scalar(&e) {
                self.out(&x, &e);
            }
            self.indent -= 1;
            self.line("}");
        }
        true
    }

    /// `let (a, (b, _)) = e`: the parts of a tuple as variables of their own,
    /// some left out.
    fn let_pattern(&mut self) {
        let t = Ty::Tuple((0..2 + self.rng.below(2)).map(|_| self.any_ty(2)).collect());
        let e = self.expr(&t, 3);
        let mut binds = Vec::new();
        let pattern = self.binding_pattern(&t, &mut binds);
        let ty = self.ty_name(&t);
        self.line(&format!("let {pattern}: {ty} = {e}"));
        self.vars.extend(binds.into_iter().map(|(name, ty)| Var { name, ty, mutable: true }));
    }

    /// A pattern for any value of `t` that binds some of its parts to new
    /// names, which go to `binds`.
    fn binding_pattern(&mut self, t: &Ty, binds: &mut Vec<(String, Ty)>) -> String {
        match t {
            Ty::Tuple(parts) if self.rng.chance(70) => {
                let ps: Vec<String> = parts.iter().map(|p| self.binding_pattern(p, binds)).collect();
                format!("({})", ps.join(", "))
            }
            _ if self.rng.chance(20) => "_".into(),
            _ => {
                let name = self.fresh("b");
                binds.push((name.clone(), t.clone()));
                name
            }
        }
    }

    /// A `match` on a tuple of integers, bools and tuples of them, whose arms
    /// test some parts against literals and bind others. An arm that matches
    /// nothing new is an error, so the first part, an integer, is a different
    /// literal (or `|` of them) in every arm: arms never overlap, and the last
    /// one, `_`, is always reachable.
    fn tuple_match(&mut self, depth: u32) {
        let first = self.int_ty();
        let Ty::Tuple(rest) = self.match_ty(2) else { unreachable!() };
        let t = Ty::Tuple([vec![first.clone()], rest.clone()].concat());
        let subject = self.expr(&t, 2);
        self.line(&format!("match {subject} {{"));
        let mut keys: Vec<i64> = (0..6).collect();
        if matches!(first, Ty::Int { signed: true, .. }) && self.rng.chance(50) {
            keys.iter_mut().for_each(|k| *k -= 3);
        }
        for _ in 0..1 + self.rng.below(3) {
            let mut alts = Vec::new();
            for _ in 0..1 + usize::from(self.rng.chance(25)) {
                alts.push(keys.remove(self.rng.below(keys.len())).to_string());
            }
            let mut binds = Vec::new();
            let parts: Vec<String> = rest.iter().map(|p| self.test_pattern(p, &mut binds)).collect();
            let pattern = format!("({}, {})", alts.join(" | "), parts.join(", "));
            let scope = self.vars.len();
            self.vars.extend(binds.into_iter().map(|(name, ty)| Var { name, ty, mutable: false }));
            let guard = if self.rng.chance(20) { format!(" if {}", self.expr(&Ty::Bool, 1)) } else { String::new() };
            self.line(&format!("    {pattern}{guard} => {{"));
            self.indent += 1;
            self.block(depth - 1);
            self.indent -= 1;
            self.vars.truncate(scope);
            self.line("    }");
        }
        self.line("    _ => {");
        self.indent += 1;
        self.block(depth - 1);
        self.indent -= 1;
        self.line("    }");
        self.line("}");
    }

    /// A tuple type `match` can test: integers, bools and tuples of them.
    fn match_ty(&mut self, depth: u32) -> Ty {
        Ty::Tuple(
            (0..2 + self.rng.below(2))
                .map(|_| match self.rng.below(4) {
                    0 if depth > 0 => self.match_ty(depth - 1),
                    1 => Ty::Bool,
                    _ => self.int_ty(),
                })
                .collect(),
        )
    }

    /// A pattern for values of `t` (see `match_ty`): literals, `|` of them,
    /// `_`, names bound for the arm, and nested tuples.
    fn test_pattern(&mut self, t: &Ty, binds: &mut Vec<(String, Ty)>) -> String {
        match t {
            Ty::Tuple(parts) => {
                let ps: Vec<String> = parts.iter().map(|p| self.test_pattern(p, binds)).collect();
                format!("({})", ps.join(", "))
            }
            Ty::Bool if self.rng.chance(60) => (if self.rng.chance(50) { "true" } else { "false" }).into(),
            Ty::Int { signed, .. } if self.rng.chance(60) => {
                let k = self.rng.below(4) as i64 - if *signed { 1 } else { 0 };
                if self.rng.chance(20) {
                    format!("{k} | {}", k + 1)
                } else {
                    k.to_string()
                }
            }
            _ if self.rng.chance(50) => "_".into(),
            _ => {
                let name = self.fresh("b");
                binds.push((name.clone(), t.clone()));
                name
            }
        }
    }

    /// A `lists.fuse!` pipeline over a new list variable, and the same stages
    /// as a loop written out. The stages are closures, which may read other
    /// variables; both results are kept, and the program prints
    /// `PIPELINE_DIFFERS` if they are not the same.
    fn pipeline(&mut self) {
        let number = |g: &mut Gen| if g.rng.chance(80) { g.int_ty() } else { Ty::F64 };
        let mut t = number(self);
        let list = Ty::List(Box::new(t.clone()));
        let src = self.expr(&list, 2);
        let xs = self.fresh("xs");
        self.line(&format!("let {xs}: {} = {src}", self.ty_name(&list)));
        let (mut stages, mut steps) = (Vec::new(), Vec::new());
        for _ in 0..1 + self.rng.below(3) {
            let (g, p) = (self.fresh("g"), self.fresh("p"));
            self.vars.push(Var { name: p.clone(), ty: t.clone(), mutable: false });
            let param = self.ty_name(&t);
            if self.rng.chance(60) {
                let u = number(self);
                let e = self.expr(&u, 2);
                // Mostly a function of the value, not only of what it captured.
                let un = self.ty_name(&u);
                let e = if self.rng.chance(60) { format!("(({p} as {un}) + {e})") } else { e };
                self.line(&format!("let {g} = fn({p}: {param}) -> {un} {{ return {e} }}"));
                stages.push(format!("map({g})"));
                steps.push(format!("let s = {g}(s)"));
                t = u;
            } else {
                let e = self.expr(&Ty::Bool, 2);
                let k = 2 + self.rng.below(3);
                let e = if self.rng.chance(60) { format!("((({p} as i64) % {k} == 0) != {e})") } else { e };
                self.line(&format!("let {g} = fn({p}: {param}) -> bool {{ return {e} }}"));
                stages.push(format!("filter({g})"));
                steps.push(format!("if !{g}(s) {{ continue }}"));
            }
            self.vars.pop();
        }
        let (end, result, init, step) = match self.rng.below(3) {
            0 => ("count", Ty::I64, "0".to_string(), "acc += 1"),
            1 => ("collect", Ty::List(Box::new(t.clone())), "[]".to_string(), "push(acc, s)"),
            _ => ("sum", t.clone(), if t == Ty::F64 { "0.0".into() } else { format!("0 as {}", self.ty_name(&t)) }, "acc += s"),
        };
        let r = self.ty_name(&result);
        let (fused, looped) = (self.fresh("v"), self.fresh("v"));
        self.line(&format!("let {fused}: {r} = lists.fuse!({xs}, {}, {end}())", stages.join(", ")));
        self.line(&format!("let {looped}: {r} = {{"));
        self.indent += 1;
        self.line(&format!("let acc: {r} = {init}"));
        let x = self.fresh("x");
        self.line(&format!("for {x} in {xs} {{"));
        self.indent += 1;
        self.line(&format!("let s = {x}"));
        for step in steps {
            self.line(&step);
        }
        self.line(step);
        self.indent -= 1;
        self.line("}");
        self.line("acc");
        self.indent -= 1;
        self.line("}");
        // Floats compare by their bits, so that a NaN is the same as itself.
        let differ = match &result {
            Ty::F64 => format!("to_bits({fused}) != to_bits({looped})"),
            Ty::List(e) if **e == Ty::F64 => format!("len({fused}) != len({looped})"),
            _ => format!("{fused} != {looped}"),
        };
        self.line(&format!("if {differ} {{ out({PIPELINE_DIFFERS}) }}"));
        self.vars.push(Var { name: xs, ty: list, mutable: true });
        // `collect` may leave nothing, and lists here are never empty.
        if end != "collect" {
            self.vars.push(Var { name: fused, ty: result.clone(), mutable: true });
            self.vars.push(Var { name: looped, ty: result, mutable: true });
        }
    }

    fn scalar(&self, t: &Ty) -> bool {
        matches!(t, Ty::Int { .. } | Ty::Bool | Ty::F64)
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


    // ---- traits ----

    /// The trait `Weigh` with a default, impls of it for some structs and
    /// scalar types (and, generically, for 2-tuples of types with one),
    /// derived `Ord` and `Hash` for the structs whose fields have them, and
    /// `heavier`, a function bounded by `Weigh`.
    fn trait_items(&mut self) {
        for l in [
            "trait Weigh {",
            "    fn weigh(x: Self) -> i64",
            "    fn unit() -> i64",
            "    fn twice(x: Self) -> i64 {",
            "        return x.weigh() * 2 + Self.unit()",
            "    }",
            "}",
        ] {
            self.line(l);
        }
        let mut types: Vec<Ty> = (0..self.structs.len()).filter(|_| self.rng.chance(60)).map(Ty::Struct).collect();
        for _ in 0..1 + self.rng.below(3) {
            let t = self.scalar_ty();
            if !types.contains(&t) {
                types.push(t);
            }
        }
        for t in types {
            self.weigh_impl(&t);
            self.weigh.push(t);
        }
        if self.rng.chance(50) {
            self.tuple_impl = true;
            for l in [
                "impl(A: Weigh, B: Weigh) Weigh for (A, B) {",
                "    fn weigh(x: (A, B)) -> i64 {",
                "        return x.0.weigh() * 31 + x.1.twice()",
                "    }",
                "    fn unit() -> i64 {",
                "        return 7",
                "    }",
                "}",
            ] {
                self.line(l);
            }
        }
        for k in 0..self.structs.len() {
            let ord = self.structs[k].iter().all(|f| self.has_ord(f));
            let hash = self.structs[k].iter().all(|f| self.has_hash(f));
            self.ord.push(ord);
            self.hash.push(hash);
            let derived: Vec<&str> = [(ord, "Ord"), (hash, "Hash")].iter().filter(|(on, _)| *on).map(|(_, t)| *t).collect();
            if !derived.is_empty() {
                self.line(&format!("meta.derive!({}, S{k})", derived.join(", ")));
            }
        }
        // An iterator over any type, which gives its value away with each item
        // and keeps it: natively, a part taken out of `next`'s result.
        for l in [
            "struct Rep(T: type) {",
            "    x: T",
            "    n: i64",
            "}",
            "impl(T: type) Iterator for Rep(T) {",
            "    type Item = (i64, T)",
            "    fn next(r: Rep(T)) -> Option((Rep(T), Self.Item)) {",
            "        if r.n <= 0 {",
            "            return Option.None",
            "        }",
            "        return Option.Some((Rep(T) { x: r.x, n: r.n - 1 }, (r.n, r.x)))",
            "    }",
            "}",
        ] {
            self.line(l);
        }
        for l in [
            "fn heavier(comptime T: Weigh, a: T, b: T) -> i64 {",
            "    if a.weigh() < b.weigh() {",
            "        return b.twice()",
            "    }",
            "    return a.twice() + T.unit()",
            "}",
        ] {
            self.line(l);
        }
    }

    /// `impl Weigh for t`, whose `weigh` is a random `i64` made from `x`.
    fn weigh_impl(&mut self, t: &Ty) {
        let name = self.ty_name(t);
        self.line(&format!("impl Weigh for {name} {{"));
        self.indent += 1;
        self.line(&format!("fn weigh(x: {name}) -> i64 {{"));
        self.indent += 1;
        self.vars = vec![Var { name: "x".into(), ty: t.clone(), mutable: false }];
        self.in_impl = true;
        let e = self.expr(&Ty::I64, 3);
        self.in_impl = false;
        self.vars.clear();
        self.line(&format!("return {e}"));
        self.indent -= 1;
        self.line("}");
        let unit = self.rng.below(100);
        self.line(&format!("fn unit() -> i64 {{ return {unit} }}"));
        // Sometimes the default is replaced.
        if self.rng.chance(30) {
            self.line("fn twice(x: Self) -> i64 { return x.weigh() - 1 }");
        }
        self.indent -= 1;
        self.line("}");
    }

    fn has_weigh(&self, t: &Ty) -> bool {
        match t {
            Ty::Tuple(parts) if self.tuple_impl && parts.len() == 2 && !self.weigh.contains(t) => {
                parts.iter().all(|p| self.has_weigh(p))
            }
            _ => self.weigh.contains(t),
        }
    }

    /// Whether `t` has an `Ord`: the prelude's, or a derived one.
    fn has_ord(&self, t: &Ty) -> bool {
        match t {
            Ty::Int { .. } | Ty::F64 | Ty::Str => true,
            Ty::Tuple(parts) => parts.iter().all(|p| self.has_ord(p)),
            Ty::Struct(k) => self.ord.get(*k) == Some(&true),
            Ty::List(e) => self.has_ord(e),
            _ => false,
        }
    }

    /// Whether `t` has a `Hash`: the prelude's, or a derived one.
    fn has_hash(&self, t: &Ty) -> bool {
        match t {
            Ty::Int { .. } | Ty::Bool | Ty::Str => true,
            Ty::Tuple(parts) => parts.iter().all(|p| self.has_hash(p)),
            Ty::Struct(k) => self.hash.get(*k) == Some(&true),
            Ty::List(e) => self.has_hash(e),
            _ => false,
        }
    }

    /// A type that `has` holds for: a variable's, a struct, or a tuple of two.
    fn trait_ty(&mut self, has: fn(&Gen, &Ty) -> bool) -> Option<Ty> {
        let mut pool: Vec<Ty> = self.vars.iter().map(|v| v.ty.clone()).collect();
        pool.extend((0..self.structs.len()).map(Ty::Struct));
        pool.extend(self.weigh.clone());
        let (a, b) = (self.scalar_ty(), self.scalar_ty());
        pool.push(a.clone());
        pool.push(Ty::Tuple(vec![a, b]));
        pool.retain(|t| has(self, t));
        if pool.is_empty() {
            return None;
        }
        Some(self.rng.pick(&pool).clone())
    }

    /// Two values of type `t`. A list literal needs a list type from where it
    /// is used, which a receiver does not give: those come from variables.
    fn operands(&mut self, t: &Ty, d: u32) -> Option<(String, String)> {
        if self.has_list(t) {
            return Some((self.projection(t)?, self.projection(t)?));
        }
        Some((self.expr(t, d), self.expr(t, d)))
    }

    /// An integer or bool made through a trait: `Weigh`'s functions, `heavier`,
    /// or `Ord` and `Hash` (operators on ordered types included).
    fn trait_expr(&mut self, t: &Ty, d: u32) -> Option<String> {
        match t {
            Ty::Int { .. } => {
                let tn = self.ty_name(t);
                let e = match self.rng.below(6) {
                    0 | 1 => {
                        let w = self.trait_ty(Gen::has_weigh)?;
                        let (a, _) = self.operands(&w, d)?;
                        format!("({a}).weigh()")
                    }
                    2 => {
                        let w = self.trait_ty(Gen::has_weigh)?;
                        let (a, _) = self.operands(&w, d)?;
                        format!("Weigh.twice({a})")
                    }
                    3 => {
                        // `S0.unit()`, `i16.unit()`: a type that has a name, which no
                        // variable in scope hides (loop counters may be called `i8`).
                        let named: Vec<Ty> = self
                            .weigh
                            .iter()
                            .filter(|t| !matches!(t, Ty::Tuple(_)) && !self.vars.iter().any(|v| v.name == self.ty_name(t)))
                            .cloned()
                            .collect();
                        if named.is_empty() {
                            return None;
                        }
                        let w = self.rng.pick(&named).clone();
                        format!("{}.unit()", self.ty_name(&w))
                    }
                    4 => {
                        let w = self.trait_ty(Gen::has_weigh)?;
                        let (a, b) = self.operands(&w, d)?;
                        format!("heavier({a}, {b})")
                    }
                    _ => {
                        let o = self.trait_ty(Gen::has_ord)?;
                        let (a, b) = self.operands(&o, d)?;
                        format!("({a}).compare({b})")
                    }
                };
                Some(format!("(({e}) as {tn})"))
            }
            Ty::Bool => match self.rng.below(3) {
                0 => {
                    let o = self.trait_ty(Gen::has_ord)?;
                    let (a, b) = self.operands(&o, d)?;
                    let op = *self.rng.pick(&["<", "<=", ">", ">="]);
                    Some(format!("({a} {op} {b})"))
                }
                1 => {
                    let o = self.trait_ty(Gen::has_ord)?;
                    let (a, b) = self.operands(&o, d)?;
                    Some(format!("({a}).less({b})"))
                }
                _ => {
                    let h = self.trait_ty(Gen::has_hash)?;
                    let (a, b) = self.operands(&h, d)?;
                    Some(format!("(hash_of({a}) == hash_of({b}))"))
                }
            },
            _ => None,
        }
    }

    fn declare(&mut self, t: Ty, e: String) {
        let name = self.fresh("v");
        let ty = self.ty_name(&t);
        self.line(&format!("let {name}: {ty} = {e}"));
        self.vars.push(Var { name, ty: t, mutable: true });
    }
}

/// Runs one seed, with lists or without: `Err` describes how the VM and the compiled program differ,
/// or what went wrong, and where the program was saved.
fn check_with(seed: u64, work: &std::path::Path, lists: bool) -> Result<(), String> {
    let body = Gen::with_lists(seed, lists).program();
    if std::env::var_os("JIHOO_FUZZ_SHOW").is_some() {
        eprintln!("---- seed {seed}\n{body}");
    }
    let name = format!("{}seed-{seed}", if lists { "lists-" } else { "" });
    let fail = |what: String| {
        let saved = std::env::temp_dir().join(format!("jihoo-fuzz-{name}.jh"));
        std::fs::write(&saved, common::wrap(&body, Profile::Hosted)).unwrap();
        format!("seed {seed}: {what}\n  the program: {}", saved.display())
    };
    let vm = common::run(&body, Profile::Hosted, work, &name).map_err(|e| fail(format!("on the VM: {e}")))?;
    if vm.stdout.lines().any(|l| l == PIPELINE_DIFFERS.to_string()) {
        return Err(fail("a `lists.fuse!` pipeline and the same loop written out give different results".into()));
    }
    if common::compiled_enabled() {
        // Freestanding for every seed; native, which links with the C compiler
        // and is slower to build, for some.
        let profiles: &[Profile] = if lists {
            &[Profile::Native]
        } else if seed.is_multiple_of(4) {
            &[Profile::Freestanding, Profile::Native]
        } else {
            &[Profile::Freestanding]
        };
        let valgrind: Option<Vec<String>> = std::env::var("JIHOO_VALGRIND").ok().filter(|_| lists).map(|v| {
            [v.as_str(), "-q", "--leak-check=full", "--show-leak-kinds=definite,indirect", "--errors-for-leak-kinds=definite,indirect", "--error-exitcode=99"]
                .map(String::from)
                .to_vec()
        });
        for &profile in profiles {
            let nat = common::run_under(&body, profile, work, &name, valgrind.as_deref())
                .map_err(|e| fail(format!("{} build: {e}", profile.name())))?;
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
    fuzz("JIHOO_FUZZ_SEEDS", false);
}

#[test]
fn vm_and_native_agree_on_random_list_programs() {
    fuzz("JIHOO_FUZZ_LIST_SEEDS", true);
}

fn fuzz(seeds_var: &str, lists: bool) {
    let env = |name: &str| std::env::var(name).ok().map(|v| v.parse::<u64>().unwrap_or_else(|_| panic!("{name} is not a number")));
    let seeds: Vec<u64> = match env("JIHOO_FUZZ_SEED") {
        Some(seed) => vec![seed],
        None => (0..env(seeds_var).unwrap_or(DEFAULT_SEEDS)).collect(),
    };
    if !common::compiled_enabled() {
        eprintln!("JIHOO_LLC is not set: running the programs on the VM only");
    }
    let work = common::work_dir(if lists { "fuzz-lists" } else { "fuzz" });

    // Seeds in parallel; each has its own file names.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let failures: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let (seeds, work) = (&seeds, &work);
                s.spawn(move || {
                    seeds.iter().skip(t).step_by(threads).filter_map(|&seed| check_with(seed, work, lists).err()).collect::<Vec<_>>()
                })
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
