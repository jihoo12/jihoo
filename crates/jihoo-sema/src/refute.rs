//! Trying to break contracts while compiling.
//!
//! After the program is lowered, every function with a contract
//! (`contracts.rs`) is run on the VM, as compile-time code is, with inputs the
//! compiler makes up: first the values where code tends to go wrong (0, -1,
//! the largest and smallest integers, the empty string and list, NaN, ...),
//! then random ones from a seed fixed by the function's name, so a build gives
//! the same answer every time. Inputs that a `requires` clause rules out are
//! skipped. The contract is broken when, for an input that meets every
//! `requires`,
//!
//! - an `ensures` clause is false for the value returned,
//! - the function calls a function with arguments that break its `requires`, or
//! - it traps: a bounds check, a division by zero, `trap()`, `pop` of an empty
//!   list.
//!
//! The first input that does is made smaller while it still fails the same way
//! (integers towards 0, strings and lists shorter), and becomes a compile
//! error that shows the call.
//!
//! This is testing, not proof: passing means no input that was tried breaks
//! the contract, and the checks still run in the program. A function whose
//! parameters the compiler cannot make up (pointers, function values, cells,
//! channels, structs with fields its module cannot set) or whose code cannot
//! run while compiling (C functions, `syscall`, `asm`) is only checked when it
//! runs.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use jihoo_ir::{FloatTy, Function, IntTy, Terminator, Type};
use jihoo_syntax::ast::{ContractKind, FnDecl};
use jihoo_syntax::{Error, Pos};
use jihoo_vm::{Program, Value, Vm};

use crate::comptime::{arg, ConstValue};
use crate::env::{Env, Sig};
use crate::generic::Bindings;
use crate::FnCx;

/// Inputs tried per function, the first third of them from the special values.
const CASES: usize = if cfg!(test) { 300 } else { 1000 };
/// VM steps for one input; an input that needs more is skipped.
const CASE_FUEL: u64 = 100_000;
/// VM steps for all inputs of one function.
const BUDGET: u64 = if cfg!(test) { 5_000_000 } else { 20_000_000 };
/// Runs spent making a failing input smaller.
const SHRINK_RUNS: usize = 2000;
/// How deep recursive types (`ref`) are made.
const DEPTH: u32 = 4;

/// How a run with one input went.
enum Outcome {
    Passed,
    /// Ruled out by `requires`, or it did not finish within its steps.
    Skipped,
    Failed(Failure),
}

#[derive(Clone)]
enum Failure {
    /// `ensures` clause `clause` was false for `result`, in the call tried or,
    /// when `nested`, in a call of the function inside it.
    Ensures { clause: usize, result: Option<ConstValue>, nested: bool },
    /// A call of `callee` with `args` broke its clause `clause` (`requires`,
    /// or `ensures` when another function broke its own).
    Callee { callee: String, kind: ContractKind, clause: usize, args: Option<Vec<ConstValue>> },
    /// The VM stopped in function `func` with `msg`.
    Trap { func: String, msg: String },
}

impl Failure {
    /// What has to stay the same while an input is made smaller.
    fn key(&self) -> String {
        match self {
            Failure::Ensures { clause, .. } => format!("ensures {clause}"),
            Failure::Callee { callee, kind, clause, .. } => format!("{callee} {} {clause}", kind.word()),
            Failure::Trap { func, msg } => format!("{func}: {}", msg.replace(|c: char| c.is_ascii_digit(), "")),
        }
    }
}

/// A function being tried, ready to run.
struct Subject<'p> {
    name: String,
    decl: &'p FnDecl,
    params: Vec<Type>,
    ret: Type,
    /// `name.pre`, which says whether arguments meet every `requires`.
    pre: Option<String>,
    module: Rc<jihoo_ir::Module>,
    program: Rc<Program>,
    fuel: u64,
}

impl<'p> Env<'p> {
    /// Tries to break the contract of every function of `funcs` that has one.
    pub(crate) fn refute_contracts(&self, funcs: &[Function]) -> Vec<Error> {
        let mut errors = Vec::new();
        for f in funcs {
            let Some((decl, bindings)) = self.decl_of(&f.name) else { continue };
            if decl.contracts.is_empty() || decl.is_compile_time() || bindings.closures().next().is_some() {
                continue;
            }
            if let Some(e) = self.refute(f, decl, bindings) {
                errors.push(e);
            }
        }
        errors
    }

    fn refute(&self, f: &Function, decl: &'p FnDecl, bindings: Rc<Bindings>) -> Option<Error> {
        let module = bindings.module;
        if !f.params.iter().all(|t| self.makeable(t, module, decl.is_pub, &mut HashSet::new())) {
            return None;
        }
        let pre = match decl.contracts.iter().any(|c| c.kind == ContractKind::Requires) {
            true => Some(self.pre_fn(f, decl, bindings).ok()?),
            false => None,
        };
        let pre_name = pre.as_ref().map(|p| p.name.clone());
        if let Some(pre) = pre {
            let name = pre.name.clone();
            self.prepare(decl.pos, vec![pre], &name).ok()?;
        }
        // Code that cannot run while compiling is only checked when it runs.
        let (vm_module, program) = self.prepare(decl.pos, vec![], &f.name).ok()?;
        let mut s = Subject {
            name: f.name.clone(),
            decl,
            params: f.params.clone(),
            ret: f.ret.clone(),
            pre: pre_name,
            module: vm_module,
            program,
            fuel: BUDGET,
        };

        let mut gen = Gen::new(self, &f.name);
        let specials: Vec<Vec<ConstValue>> = s.params.iter().map(|t| gen.specials(t, DEPTH)).collect();
        let total: usize = specials.iter().map(Vec::len).try_fold(1usize, |a, n| a.checked_mul(n)).unwrap_or(usize::MAX);
        for i in 0..CASES {
            let args: Vec<ConstValue> = if i < CASES / 3 && i < total {
                // Every combination while there are few, the first ones first.
                let mut k = i;
                specials
                    .iter()
                    .map(|vs| {
                        let v = vs[k % vs.len()].clone();
                        k /= vs.len();
                        v
                    })
                    .collect()
            } else {
                let size = 1 + (i * 16 / CASES) as u32;
                match s.params.iter().map(|t| gen.random(t, size, DEPTH)).collect::<Option<Vec<_>>>() {
                    Some(args) => args,
                    None => continue,
                }
            };
            if s.fuel == 0 {
                return None;
            }
            match self.try_input(&mut s, &args)? {
                Outcome::Passed | Outcome::Skipped => {}
                Outcome::Failed(failure) => {
                    let (args, failure) = self.shrink(&mut s, &mut gen, args, failure);
                    return Some(self.report(&s, &args, failure));
                }
            }
        }
        None
    }

    /// `fn name.pre(params) -> bool`: whether the arguments meet every
    /// `requires` clause, each tested only if those before it hold.
    fn pre_fn(&self, f: &Function, decl: &FnDecl, bindings: Rc<Bindings>) -> Result<Function, Error> {
        let sig = Rc::new(Sig { params: f.params.clone(), ret: Type::Bool });
        let mut cx = FnCx::new(self, sig, bindings);
        for (p, ty) in decl.params.iter().filter(|p| !p.comptime).zip(&f.params) {
            let r = cx.new_reg(ty.clone());
            cx.scopes[0].insert(p.name.clone(), r);
        }
        for c in decl.contracts.iter().filter(|c| c.kind == ContractKind::Requires) {
            let v = cx.expr(&c.cond, Some(&Type::Bool))?;
            cx.expect(c.cond.pos, v, &Type::Bool, "a `requires` clause")?;
            let (ok, bad) = (cx.new_block(), cx.new_block());
            cx.terminate(Terminator::Branch { cond: v, then: ok, els: bad });
            cx.switch_to(bad);
            let no = cx.konst(Type::Bool, 0);
            cx.terminate(Terminator::Ret(no));
            cx.switch_to(ok);
        }
        let yes = cx.konst(Type::Bool, 1);
        cx.terminate(Terminator::Ret(yes));
        cx.finish(&format!("{}.pre", f.name), Type::Bool, decl.pos)
    }

    /// Runs the subject with `args`; `None` if it cannot run while compiling.
    fn try_input(&self, s: &mut Subject, args: &[ConstValue]) -> Option<Outcome> {
        let module = s.module.clone();
        if let Some(pre) = s.pre.clone() {
            let (r, _) = self.call(&module, s, &pre, args);
            match r {
                Ok(Value::Bool(true)) => {}
                // A `requires` that is false, or that cannot be decided.
                Ok(_) | Err(_) => return Some(Outcome::Skipped),
            }
        }
        let name = s.name.clone();
        let (r, failed) = self.call(&module, s, &name, args);
        let Err(msg) = r else { return Some(Outcome::Passed) };
        let (trace, vm) = failed.unwrap();
        if msg.starts_with("evaluation did not finish") || msg == "stack overflow" {
            return Some(Outcome::Skipped);
        }
        if unsupported(&msg) {
            return None;
        }
        let Some((inner, inner_args)) = trace.last() else { return Some(Outcome::Skipped) };
        let failure = match breach(inner) {
            Some((func, ContractKind::Ensures, clause)) if func == s.name => {
                let result = inner_args.first().and_then(|v| self.to_const(Pos::default(), *v, &s.ret, &vm).ok());
                let nested = trace.iter().filter(|(n, _)| *n == s.name).count() > 1;
                Failure::Ensures { clause, result, nested }
            }
            Some((callee, kind, clause)) => {
                // The arguments of the call that broke it, from its frame,
                // which the clause was checked in before anything changed them.
                let args = trace.len().checked_sub(2).map(|i| &trace[i]).filter(|(n, _)| *n == callee).and_then(|(_, vals)| {
                    let tys = self.function(&callee).ok()?.params.clone();
                    vals.iter().zip(&tys).map(|(v, t)| self.to_const(Pos::default(), *v, t, &vm).ok()).collect()
                });
                Failure::Callee { callee, kind, clause, args }
            }
            None => Failure::Trap { func: inner.clone(), msg },
        };
        Some(Outcome::Failed(failure))
    }

    /// Calls `entry` with `args`: its result or error message and, when it
    /// failed, the calls that were running and the VM that ran them.
    #[allow(clippy::type_complexity)]
    fn call<'m>(
        &self,
        module: &'m jihoo_ir::Module,
        s: &mut Subject,
        entry: &str,
        args: &[ConstValue],
    ) -> (Result<Value, String>, Option<(Vec<(String, Vec<Value>)>, Vm<'m>)>) {
        let mut vm = Vm::with_program(module, s.program.clone())
            .with_fuel(s.fuel.min(CASE_FUEL))
            .with_input(jihoo_vm::Input::Closed);
        let values: Vec<Value> = args.iter().map(|a| arg(&mut vm, a)).collect();
        // What the function prints while it is tried is not shown.
        let r = vm.call_named(entry, &values, &mut std::io::sink());
        let used = s.fuel.min(CASE_FUEL) - vm.fuel().unwrap_or(0);
        s.fuel = s.fuel.saturating_sub(used);
        match r {
            Ok(v) => (Ok(v), None),
            Err(e) => {
                let trace = vm.trace().into_iter().map(|(n, vals)| (n.to_string(), vals)).collect();
                (Err(e.msg), Some((trace, vm)))
            }
        }
    }

    /// Makes `args` smaller while they still fail the way `failure` says.
    fn shrink(&self, s: &mut Subject, gen: &mut Gen, mut args: Vec<ConstValue>, mut failure: Failure) -> (Vec<ConstValue>, Failure) {
        let key = failure.key();
        let mut runs = 0;
        // Shrinking has its own budget, so a long search does not leave it none.
        s.fuel = BUDGET;
        'outer: while runs < SHRINK_RUNS {
            for i in 0..args.len() {
                for smaller in gen.shrinks(&s.params[i], &args[i]) {
                    runs += 1;
                    if runs >= SHRINK_RUNS || s.fuel == 0 {
                        break 'outer;
                    }
                    let mut tried = args.clone();
                    tried[i] = smaller;
                    if let Some(Outcome::Failed(f)) = self.try_input(s, &tried) {
                        if f.key() == key {
                            args = tried;
                            failure = f;
                            continue 'outer;
                        }
                    }
                }
            }
            break;
        }
        (args, failure)
    }

    fn report(&self, s: &Subject, args: &[ConstValue], failure: Failure) -> Error {
        let gen = Gen::new(self, "");
        let show_call = |name: &str, tys: &[Type], args: &[ConstValue]| {
            let shown: Vec<String> = tys.iter().zip(args).map(|(t, v)| gen.show(t, v)).collect();
            format!("{name}({})", shown.join(", "))
        };
        let call = show_call(&s.decl.name, &s.params, args);
        let clause = |func: &str, kind: ContractKind, i: usize| {
            let decl = self.decl_of(func).map(|(d, _)| d);
            let text = decl.and_then(|d| d.contracts.get(i)).map_or("...", |c| c.text.as_str());
            (decl, format!("`{} {text}`", kind.word()))
        };
        let user_name = |func: &str| self.decl_of(func).map_or(func.to_string(), |(d, _)| d.name.clone());
        let (pos, msg) = match failure {
            Failure::Ensures { clause: i, result, nested } => {
                let (_, text) = clause(&s.name, ContractKind::Ensures, i);
                let pos = s.decl.contracts[i].pos;
                let result = result.map(|r| gen.show(&s.ret, &r));
                match (result, nested) {
                    (Some(r), false) => (pos, format!("contract broken: {call} returns {r}, so {text} is false")),
                    (Some(r), true) => {
                        let name = &s.decl.name;
                        (pos, format!("contract broken: {call} breaks {text}: a call of `{name}` inside it returns {r}"))
                    }
                    (None, _) => (pos, format!("contract broken: {call} breaks {text}")),
                }
            }
            Failure::Callee { callee, kind, clause: i, args: callee_args } => {
                let (decl, text) = clause(&callee, kind, i);
                let name = user_name(&callee);
                let inner = match (&callee_args, self.function(&callee)) {
                    (Some(a), Ok(f)) => show_call(&name, &f.params, a),
                    _ => format!("`{name}`"),
                };
                let msg = match kind {
                    ContractKind::Requires => format!("contract broken: {call} calls {inner}, which breaks `{name}`'s {text}"),
                    ContractKind::Ensures | ContractKind::Replaces => format!("contract broken: {call} calls {inner}, which breaks its own {text}"),
                };
                (decl.map_or(s.decl.pos, |_| s.decl.pos), msg)
            }
            Failure::Trap { func, msg } => {
                let at = if func == s.name { String::new() } else { format!(" in `{}`", user_name(&func)) };
                let why = match s.pre {
                    Some(_) => "it meets every `requires`, but",
                    None => "it",
                };
                (s.decl.pos, format!("contract broken: {call} traps{at} ({msg}); {why} a function with a contract must not trap"))
            }
        };
        Error::new(pos, jihoo_ir::plain_names(&msg))
    }

    /// Whether the compiler can make up values of `t` for a function of
    /// `module`, `public` or not. `seen` breaks cycles through `ref`.
    pub(crate) fn makeable(&self, t: &Type, module: usize, public: bool, seen: &mut HashSet<String>) -> bool {
        match t {
            Type::Int(_) | Type::Float(_) | Type::Bool | Type::Str => true,
            Type::List(e) | Type::Array(e, _) | Type::Ref(e) => self.makeable(e, module, public, seen),
            Type::Struct(name) => {
                if !seen.insert(name.clone()) {
                    return true;
                }
                if self.holds_code(t) || !self.buildable_from(name, module, public) {
                    return false;
                }
                let Ok(fields) = self.struct_fields(Pos::default(), name) else { return false };
                fields.iter().all(|(_, ft)| self.makeable(ft, module, public, seen))
            }
            Type::Enum(name) => {
                if !seen.insert(name.clone()) {
                    return true;
                }
                if self.holds_code(t) {
                    return false;
                }
                let Ok(variants) = self.enum_variants(Pos::default(), name) else { return false };
                variants.iter().all(|(_, tys)| tys.iter().all(|vt| self.makeable(vt, module, public, seen)))
            }
            _ => false,
        }
    }
}

/// The function, kind and clause that a breach function (`contracts.rs`)
/// stands for, from its name: `f.requires.0`, `max.2.ensures.1`.
fn breach(name: &str) -> Option<(String, ContractKind, usize)> {
    let mut parts = name.rsplitn(3, '.');
    let clause = parts.next()?.parse().ok()?;
    let kind = match parts.next()? {
        "requires" => ContractKind::Requires,
        "ensures" => ContractKind::Ensures,
        _ => return None,
    };
    Some((parts.next()?.to_string(), kind, clause))
}

/// Whether a VM error says the code cannot run while compiling at all.
fn unsupported(msg: &str) -> bool {
    ["pointers", "`syscall`", "inline asm", "`read_line`", "cannot run at compile time", "deadlock", "channel", "`type_of`", "cannot bitcast"]
        .iter()
        .any(|w| msg.contains(w))
}

/// FNV-1a: the seed for a function's random inputs.
fn seed(name: &str) -> u64 {
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)) | 1
}

/// Makes up values of a type, shows them, and makes them smaller.
pub(crate) struct Gen<'e, 'p> {
    env: &'e Env<'p>,
    /// xorshift64 state.
    rng: u64,
    /// Functions to pass for a function type (by how the type is written),
    /// simplest first, which `rules.rs` makes; and how each one reads.
    pub(crate) fns: HashMap<String, Vec<String>>,
    pub(crate) fn_text: HashMap<String, String>,
}

impl<'e, 'p> Gen<'e, 'p> {
    pub(crate) fn new(env: &'e Env<'p>, seed_from: &str) -> Self {
        Gen { env, rng: seed(seed_from), fns: HashMap::new(), fn_text: HashMap::new() }
    }

    fn next(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    pub(crate) fn int(t: IntTy, v: i128) -> ConstValue {
        ConstValue::Int(t.wrap(v as i64))
    }

    fn float(t: FloatTy, x: f64) -> ConstValue {
        ConstValue::Float(t.round(x).to_bits())
    }

    /// The values where code tends to go wrong, simplest first. Empty when
    /// `depth` runs out on the way to a value.
    pub(crate) fn specials(&mut self, t: &Type, depth: u32) -> Vec<ConstValue> {
        match t {
            &Type::Int(it) => {
                let (lo, hi) = (it.min(), it.max());
                let vals: Vec<i128> = match it.signed() {
                    true => vec![0, 1, -1, 2, -2, 10, -10, hi, lo, hi - 1, lo + 1],
                    false => vec![0, 1, 2, 10, hi, hi - 1],
                };
                vals.into_iter().map(|v| Self::int(it, v)).collect()
            }
            Type::Bool => vec![ConstValue::Bool(false), ConstValue::Bool(true)],
            &Type::Float(ft) => {
                let max = if ft == FloatTy::F32 { f32::MAX as f64 } else { f64::MAX };
                let tiny = if ft == FloatTy::F32 { f32::MIN_POSITIVE as f64 } else { f64::MIN_POSITIVE };
                let vals = [0.0, 1.0, -1.0, 0.5, -0.0, 2.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN, max, -max, tiny];
                vals.into_iter().map(|x| Self::float(ft, x)).collect()
            }
            Type::Str => ["", "a", "ab", "aa", "ba", " ", "abc", "\n", "0"].into_iter().map(|s| ConstValue::Str(s.into())).collect(),
            Type::List(e) => {
                let mut out = vec![ConstValue::Agg(vec![])];
                if depth > 0 {
                    let es = self.specials(e, depth - 1);
                    out.extend(es.iter().take(3).map(|x| ConstValue::Agg(vec![x.clone()])));
                    if es.len() >= 2 {
                        out.push(ConstValue::Agg(vec![es[0].clone(), es[0].clone()]));
                        out.push(ConstValue::Agg(vec![es[1].clone(), es[0].clone()]));
                    }
                }
                out
            }
            Type::Array(e, n) => {
                let es = self.specials(e, depth);
                es.into_iter().take(3).map(|x| ConstValue::Agg(vec![x; *n as usize])).collect()
            }
            Type::Ref(inner) if depth > 0 => {
                self.specials(inner, depth - 1).into_iter().take(4).map(|x| ConstValue::Agg(vec![x])).collect()
            }
            Type::Ref(_) => vec![],
            Type::Fn(..) => self.fns.get(&t.to_string()).map_or(vec![], |fs| fs.iter().map(|f| ConstValue::Func(f.clone())).collect()),
            Type::Struct(name) => {
                let Ok(fields) = self.env.struct_fields(Pos::default(), name) else { return vec![] };
                let per: Vec<Vec<ConstValue>> = fields.iter().map(|(_, ft)| self.specials(ft, depth)).collect();
                if per.iter().any(Vec::is_empty) {
                    return vec![];
                }
                // Field by field: the k-th value takes each field's k-th.
                let n = per.iter().map(Vec::len).max().unwrap_or(1).min(6);
                (0..n).map(|k| ConstValue::Agg(per.iter().map(|vs| vs[k % vs.len()].clone()).collect())).collect()
            }
            Type::Enum(name) => {
                let Ok(variants) = self.env.enum_variants(Pos::default(), name) else { return vec![] };
                let mut out = Vec::new();
                for (tag, (_, tys)) in variants.iter().enumerate() {
                    let per: Vec<Vec<ConstValue>> = tys.iter().map(|vt| self.specials(vt, depth.saturating_sub(1))).collect();
                    if per.iter().any(Vec::is_empty) {
                        continue;
                    }
                    let n = if tys.is_empty() { 1 } else { per.iter().map(Vec::len).max().unwrap_or(1).min(2) };
                    for k in 0..n {
                        let payload = per.iter().map(|vs| vs[k % vs.len()].clone());
                        out.push(ConstValue::Agg(std::iter::once(ConstValue::Int(tag as i64)).chain(payload).collect()));
                    }
                }
                out
            }
            _ => vec![],
        }
    }

    /// A random value of `t`, of about `size`; `None` when `depth` runs out.
    pub(crate) fn random(&mut self, t: &Type, size: u32, depth: u32) -> Option<ConstValue> {
        let size = size as i128;
        Some(match t {
            &Type::Int(it) => {
                let v: i128 = match self.below(10) {
                    0..=3 => self.below(2 * size as u64 + 5) as i128 - size - 2,
                    4 | 5 => self.below(2001) as i128 - 1000,
                    6 | 7 => self.next() as i64 as i128,
                    8 => it.min() + self.below(4) as i128,
                    _ => it.max() - self.below(4) as i128,
                };
                // Small negative numbers are no use to an unsigned type.
                let v = if !it.signed() && v < 0 && v > -1000 { -v } else { v };
                Self::int(it, v)
            }
            Type::Bool => ConstValue::Bool(self.below(2) == 1),
            &Type::Float(ft) => {
                let x = match self.below(8) {
                    0..=3 => (self.below(16 * size as u64 + 1) as f64 - 8.0 * size as f64) / 4.0,
                    4 | 5 => (self.below(2_000_001) as f64 - 1_000_000.0) / 1000.0,
                    6 => f64::from_bits(self.next()),
                    _ => {
                        let s = self.specials(t, depth);
                        return Some(s[self.below(s.len() as u64) as usize].clone());
                    }
                };
                Self::float(ft, x)
            }
            Type::Str => {
                const CHARS: &[u8] = b"aabbcxyz 019\n-";
                let n = self.below(size.min(12) as u64 + 1);
                ConstValue::Str((0..n).map(|_| CHARS[self.below(CHARS.len() as u64) as usize] as char).collect())
            }
            Type::List(e) => {
                let n = if depth == 0 { 0 } else { self.below(size.min(8) as u64 + 1) };
                ConstValue::Agg((0..n).map(|_| self.random(e, size as u32, depth - 1)).collect::<Option<_>>()?)
            }
            Type::Array(e, n) if *n > 256 => ConstValue::Agg(vec![self.random(e, size as u32, depth)?; *n as usize]),
            Type::Array(e, n) => ConstValue::Agg((0..*n).map(|_| self.random(e, size as u32, depth)).collect::<Option<_>>()?),
            Type::Fn(..) => {
                let n = self.fns.get(&t.to_string())?.len();
                let i = self.below(n as u64) as usize;
                ConstValue::Func(self.fns[&t.to_string()].get(i)?.clone())
            }
            Type::Ref(_) if depth == 0 => return None,
            Type::Ref(inner) => ConstValue::Agg(vec![self.random(inner, size as u32, depth - 1)?]),
            Type::Struct(name) => {
                let fields = self.env.struct_fields(Pos::default(), name).ok()?;
                ConstValue::Agg(fields.iter().map(|(_, ft)| self.random(ft, size as u32, depth)).collect::<Option<_>>()?)
            }
            Type::Enum(name) => {
                let variants = self.env.enum_variants(Pos::default(), name).ok()?;
                // A few random variants; then the first that can be made,
                // which stops recursive types from growing without end.
                let picks: Vec<usize> = (0..4).map(|_| self.below(variants.len() as u64) as usize).chain(0..variants.len()).collect();
                for tag in picks {
                    let d = depth.saturating_sub(1);
                    let payload: Option<Vec<ConstValue>> = variants[tag].1.iter().map(|vt| self.random(vt, size as u32, d)).collect();
                    if let Some(p) = payload {
                        return Some(ConstValue::Agg(std::iter::once(ConstValue::Int(tag as i64)).chain(p).collect()));
                    }
                }
                return None;
            }
            _ => return None,
        })
    }

    /// Values a little simpler than `v`, to try in its place.
    pub(crate) fn shrinks(&mut self, t: &Type, v: &ConstValue) -> Vec<ConstValue> {
        match (t, v) {
            (&Type::Int(it), &ConstValue::Int(x)) => {
                let x = if it == IntTy::U64 { x as u64 as i128 } else { x as i128 };
                // 0, then ever closer to `x` from 0's side: the first that
                // still fails is the smallest of them, so a failure that starts
                // at a boundary (an overflow) is found in a few rounds.
                let mut out: Vec<i128> = vec![0];
                if x < 0 {
                    out.push(-x);
                }
                out.extend((1..64).rev().map(|k| x / (1i128 << k)));
                out.extend((1..64).map(|k| x - x / (1i128 << k)));
                out.push(x - x.signum());
                let mut seen = HashSet::new();
                out.retain(|&c| (c.abs() < x.abs() || (c > 0 && c == -x)) && seen.insert(c));
                out.into_iter().map(|c| Self::int(it, c)).collect()
            }
            (Type::Bool, ConstValue::Bool(true)) => vec![ConstValue::Bool(false)],
            // The functions before it are simpler.
            (Type::Fn(..), ConstValue::Func(f)) => {
                let fs = self.fns.get(&t.to_string()).cloned().unwrap_or_default();
                fs.iter().take_while(|g| *g != f).map(|g| ConstValue::Func(g.clone())).collect()
            }
            (&Type::Float(ft), &ConstValue::Float(bits)) => {
                let x = f64::from_bits(bits);
                let mut out = Vec::new();
                for c in [0.0, 1.0, x.trunc(), (x / 2.0).trunc(), -x] {
                    let simpler = c != x && (c.abs() < x.abs() || x.is_nan() || (x.is_sign_negative() && c == -x));
                    if simpler && !(c == -x && c.abs() == x.abs() && x.is_sign_positive()) {
                        out.push(Self::float(ft, c));
                    }
                }
                out
            }
            (Type::Str, ConstValue::Str(s)) => {
                let chars: Vec<char> = s.chars().collect();
                let mut out = Vec::new();
                if !chars.is_empty() {
                    out.push(String::new());
                    out.push(chars[..chars.len() / 2].iter().collect());
                    for i in 0..chars.len() {
                        out.push(chars.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, c)| c).collect());
                    }
                    for i in (0..chars.len()).filter(|&i| chars[i] != 'a') {
                        out.push(chars.iter().enumerate().map(|(j, &c)| if j == i { 'a' } else { c }).collect());
                    }
                }
                out.dedup();
                out.into_iter().filter(|c| c != s).map(ConstValue::Str).collect()
            }
            (Type::List(e), ConstValue::Agg(items)) => {
                let mut out = Vec::new();
                if !items.is_empty() {
                    out.push(ConstValue::Agg(vec![]));
                    out.push(ConstValue::Agg(items[..items.len() / 2].to_vec()));
                    for i in 0..items.len() {
                        let mut fewer = items.clone();
                        fewer.remove(i);
                        out.push(ConstValue::Agg(fewer));
                    }
                }
                out.extend(self.shrink_parts(&vec![(**e).clone(); items.len()], items, 0));
                out
            }
            (Type::Array(e, _), ConstValue::Agg(items)) => self.shrink_parts(&vec![(**e).clone(); items.len()], items, 0),
            (Type::Ref(inner), ConstValue::Agg(items)) => self.shrink_parts(&[(**inner).clone()], items, 0),
            (Type::Struct(name), ConstValue::Agg(items)) => {
                let Ok(fields) = self.env.struct_fields(Pos::default(), name) else { return vec![] };
                let tys: Vec<Type> = fields.iter().map(|(_, t)| t.clone()).collect();
                self.shrink_parts(&tys, items, 0)
            }
            (Type::Enum(name), ConstValue::Agg(items)) => {
                let ConstValue::Int(tag) = items[0] else { return vec![] };
                let tys = self.env.payload_types(t, tag as u32);
                let _ = name;
                let mut out = Vec::new();
                // A variant without a payload is simpler than one with.
                if !tys.is_empty() {
                    if let Ok(variants) = self.env.enum_variants(Pos::default(), name) {
                        if let Some(bare) = variants.iter().position(|(_, p)| p.is_empty()) {
                            out.push(ConstValue::Agg(vec![ConstValue::Int(bare as i64)]));
                        }
                    }
                }
                // Recursive data: a part of the same type in place of the whole.
                for (pt, pv) in tys.iter().zip(&items[1..]) {
                    if let (Type::Ref(inner), ConstValue::Agg(r)) = (pt, pv) {
                        if **inner == *t {
                            out.push(r[0].clone());
                        }
                    }
                }
                out.extend(self.shrink_parts(&tys, items, 1));
                out
            }
            _ => vec![],
        }
    }

    /// `items` (after the first `skip`, an enum's tag) with one part, of type
    /// `tys[i]`, made smaller.
    fn shrink_parts(&mut self, tys: &[Type], items: &[ConstValue], skip: usize) -> Vec<ConstValue> {
        let mut out = Vec::new();
        for (i, t) in tys.iter().enumerate() {
            for smaller in self.shrinks(t, &items[skip + i]) {
                let mut v = items.to_vec();
                v[skip + i] = smaller;
                out.push(ConstValue::Agg(v));
            }
        }
        out
    }

    /// `v` as it would be written in jihoo, more or less.
    pub(crate) fn show(&self, t: &Type, v: &ConstValue) -> String {
        let list = |tys: &mut dyn Iterator<Item = Type>, items: &[ConstValue]| {
            items.iter().zip(tys).map(|(x, t)| self.show(&t, x)).collect::<Vec<_>>().join(", ")
        };
        match (t, v) {
            (Type::Int(IntTy::U64), &ConstValue::Int(x)) => (x as u64).to_string(),
            (_, ConstValue::Int(x)) => x.to_string(),
            (_, ConstValue::Bool(b)) => b.to_string(),
            (&Type::Float(ft), &ConstValue::Float(bits)) => {
                let x = f64::from_bits(bits);
                match () {
                    _ if x.is_nan() => "nan".into(),
                    _ if x.is_infinite() => (if x > 0.0 { "inf" } else { "-inf" }).into(),
                    _ => jihoo_vm::float_text(ft, x),
                }
            }
            (_, ConstValue::Str(s)) => format!("{s:?}"),
            (_, ConstValue::Func(f)) => self.fn_text.get(f).cloned().unwrap_or_else(|| f.clone()),
            (Type::List(e) | Type::Array(e, _), ConstValue::Agg(items)) => {
                format!("[{}]", list(&mut std::iter::repeat((**e).clone()), items))
            }
            (Type::Ref(inner), ConstValue::Agg(items)) => format!("ref {}", self.show(inner, &items[0])),
            (Type::Struct(name), ConstValue::Agg(items)) => {
                let fields = self.env.struct_fields(Pos::default(), name).map(|f| (*f).clone()).unwrap_or_default();
                if self.env.tuple_named(name).is_some() {
                    return format!("({})", list(&mut fields.iter().map(|(_, t)| t.clone()), items));
                }
                let shown: Vec<String> = fields.iter().zip(items).map(|((n, ft), x)| format!("{n}: {}", self.show(ft, x))).collect();
                format!("{t} {{ {} }}", shown.join(", "))
            }
            (Type::Enum(name), ConstValue::Agg(items)) => {
                let ConstValue::Int(tag) = items[0] else { return "?".into() };
                let variant = self.env.enum_variants(Pos::default(), name).ok().map(|v| v[tag as usize].0.clone());
                let variant = variant.unwrap_or_else(|| "?".into());
                if items.len() == 1 {
                    return variant;
                }
                let tys = self.env.payload_types(t, tag as u32);
                format!("{variant}({})", list(&mut tys.into_iter(), &items[1..]))
            }
            _ => "?".into(),
        }
    }
}
