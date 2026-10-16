//! of the parts. So the functions must be inert and visible at the call,
//! and so must the parts that would move past code that is not inert, or
//! that `&&` and `||` may skip. A rule that calls the functions it is
//! given as its pattern does (`keeps_order`) may be given any function.//! Rewrite rules the compiler checks: functions that replace a pattern.
//!
//! ```jihoo
//! fn map_map(comptime T: type, xs: [T], g: fn(T) -> T, f: fn(T) -> T) -> [T]
//!     replaces lists.map(lists.map(xs, g), f)
//! {
//!     let out: [T] = []
//!     for x in xs { push(out, f(g(x))) }
//!     return out
//! }
//! ```
//!
//! A function with `replaces pattern` is a rule: its parameters are the
//! pattern's variables, each written once, and every call in the program that
//! matches the pattern calls the function instead (`try_rules`). The pattern
//! is a call, made of calls, operators, literals, names and the parameters. A
//! call matches when it calls the same functions, with the same literals and
//! names, where the pattern has them; whatever stands where the pattern has a
//! parameter is that parameter's argument. Those parts are evaluated once each,
//! in the order they are written, as the call would have evaluated them. If
//! their types do not fit the function's parameters (inferring its type
//! parameters, as a call would), the rule does not apply, and the call is
//! compiled as written. Rules do not apply inside rules.
//!
//! Nor does it apply where moving code could be seen (`effects.rs`): the
//! rule may call the functions it is given in another order, or another
//! number of times, and runs the pattern's own calls and operators after all
//! of the parts. So the functions must be inert and visible at the call,
//! and so must the parts that would move past code that is not inert, or
//! that `&&` and `||` may skip.
//!
//! A rule is a claim that the function computes what the pattern does, and
//! the compiler checks it before the program can run (`check_rules`): it runs
//! both on the VM, with the inputs contracts are tried with (`refute.rs`) and,
//! for a parameter of function type, with small functions it writes (`x`,
//! `x + 1`, `x * 2`, constants, ...), which are inert. Both must give the same
//! value, or both trap. A rule that does not is a compile error that shows the
//! smallest input where they differ; one whose parameters or result the
//! compiler cannot make up or compare, whose code cannot run while compiling,
//! or that reads input, is refused.
//!
//! Whether a rule keeps the order of calls is checked the same way, with
//! each function wrapped in one that prints that it was called, and with
//! what, and that traps at one of the first few calls. A rule that does not
//! is still a rule, for inert functions.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use jihoo_ir::{BinOp, Function, Inst, Reg, Terminator, Type};
use jihoo_syntax::ast::{Contract, Expr, ExprKind, FnDecl, Param};
use jihoo_syntax::{Error, Pos};
use jihoo_vm::{Program, Value, Vm};

use crate::comptime::{arg, ConstValue};
use crate::effects::Effects;
use crate::env::{Env, Sig};
use crate::generic::Bindings;
use crate::infer::Found;
use crate::refute::Gen;
use crate::{BlockBuf, FnCx};

/// Inputs tried per rule.
const CASES: usize = if cfg!(test) { 300 } else { 1000 };
/// Inputs tried per rule for the order of calls, each twice: the values
/// are checked already.
const ORDER_CASES: usize = CASES / 4;
/// VM steps for one side of one input; an input that needs more is skipped.
const CASE_FUEL: u64 = 100_000;
/// VM steps for all inputs of one rule.
const BUDGET: u64 = if cfg!(test) { 5_000_000 } else { 20_000_000 };
const SHRINK_RUNS: usize = 1000;
const DEPTH: u32 = 4;
/// When checking the order of calls, the wrapped functions trap at one of
/// the first this many calls.
const TRAP_AT: usize = 4;

impl<'p> Env<'p> {
    /// The rules whose pattern calls function `key`, in a fixed order.
    pub(crate) fn rules_for(&self, key: &str) -> &[String] {
        let index = self.rule_index.get_or_init(|| {
            let mut index: HashMap<String, Vec<String>> = HashMap::new();
            for (rule, (m, decl)) in self.fn_decls_iter() {
                let Some(Contract { cond: Expr { kind: ExprKind::Call(head, _), .. }, .. }) = &decl.replaces else { continue };
                if let Some(k) = self.key(m, head) {
                    index.entry(k).or_default().push(rule.to_string());
                }
            }
            index.values_mut().for_each(|v| v.sort());
            index
        });
        index.get(key).map_or(&[], Vec::as_slice)
    }
}

/// Where code lowered after some point starts (`FnCx::mark`).
struct Mark {
    /// How many instructions each block had.
    lens: Vec<usize>,
    hoisted: usize,
    /// The block being written then.
    cur: usize,
}

/// One step of what a call that matches a pattern evaluates, in order.
enum Step<'e> {
    /// The part that parameter `name` stands for; `guarded` right of `&&`
    /// or `||`, where the call as written may not evaluate it.
    Part(&'e str, bool),
    /// A call or operator of the pattern itself, which the rule takes over.
    Op(&'e Expr),
}

/// What `try_rules` puts back when a rule turns out not to apply.
struct Snapshot {
    blocks: Vec<BlockBuf>,
    regs: usize,
    cur: jihoo_ir::BlockId,
    hoisted: usize,
    const_regs: HashMap<String, Reg>,
    scopes: Vec<HashMap<String, Reg>>,
    last_loc: Option<(jihoo_ir::BlockId, Pos)>,
}

impl FnCx<'_> {
    /// Checks that `f`, which has a `replaces` clause, is a rule that can be
    /// applied: its pattern is a call of another function, made of what a
    /// pattern may hold, and names each runtime parameter once.
    pub(crate) fn check_rule(&mut self, f: &FnDecl) -> Result<(), Error> {
        let Some(rule) = &f.replaces else { return Ok(()) };
        self.in_rule = true;
        let ExprKind::Call(head, _) = &rule.cond.kind else {
            return Err(Error::new(rule.cond.pos, "a pattern is a call: `replaces f(x, g(y))`"));
        };
        if *head == f.name {
            return Err(Error::new(rule.cond.pos, format!("`{}` cannot replace calls of itself", f.name)));
        }
        let module = self.bindings.module;
        if let Some(p) = f.params.iter().find(|p| p.comptime && !self.env.is_type_param(module, p)) {
            let msg = format!("a rule's comptime parameters are types, which calls infer; `{}` is not one", p.name);
            return Err(Error::new(p.pos, msg));
        }
        let params: Vec<&str> = f.params.iter().filter(|p| !p.comptime).map(|p| p.name.as_str()).collect();
        let mut seen = HashMap::new();
        pattern_vars(&rule.cond, &params, &mut seen)?;
        if let Some(p) = params.iter().find(|p| seen.get(**p).copied().unwrap_or(0) != 1) {
            let n = seen.get(*p).copied().unwrap_or(0);
            let msg = if n == 0 {
                format!("the pattern does not use `{p}`; every parameter of a rule stands for a part of the call it replaces")
            } else {
                format!("the pattern uses `{p}` {n} times; a parameter stands for one part of the call")
            };
            return Err(Error::new(rule.pos, msg));
        }
        Ok(())
    }

    /// A call of `key`, written `name(args)`, that a rule replaces: the call of
    /// the rule, or `None` when no rule applies.
    pub(crate) fn try_rules(&mut self, pos: Pos, name: &str, key: &str, args: &[Expr]) -> Result<Option<Reg>, Error> {
        if self.in_rule {
            return Ok(None);
        }
        let rules = self.env.rules_for(key);
        if rules.is_empty() {
            return Ok(None);
        }
        let site = Expr { pos, kind: ExprKind::Call(name.to_string(), args.to_vec()) };
        for rule in rules {
            let Some((decl, _)) = self.env.decl_of(rule) else { continue };
            let Some(pattern) = &decl.replaces else { continue };
            let module = self.env.fn_module(rule);
            let params: Vec<&str> = decl.params.iter().filter(|p| !p.comptime).map(|p| p.name.as_str()).collect();
            let mut parts = Vec::new();
            if !self.matches(module, &params, &pattern.cond, &site, &mut parts) || parts.len() != params.len() {
                continue;
            }
            let snapshot = self.snapshot();
            match self.apply_rule(pos, rule, decl, module, &parts) {
                Ok(Some(r)) => return Ok(Some(r)),
                // Not for these types; or an error the call as written will
                // report again.
                Ok(None) | Err(_) => self.rollback(snapshot),
            }
        }
        Ok(None)
    }

    /// Whether code `site`, written here, matches `pattern`, written in
    /// `module` with variables `params`; adds what each variable stands for
    /// to `parts`, in the order they are written.
    fn matches<'e>(&self, module: usize, params: &[&str], pattern: &Expr, site: &'e Expr, parts: &mut Vec<(String, &'e Expr)>) -> bool {
        let here = self.bindings.module;
        // A name refers to the same item in both places, and no local hides it.
        let same_item = |p: &str, s: &str| {
            let base = s.split('.').next().unwrap_or(s);
            self.local(base).is_none() && self.env.key(module, p).is_some() && self.env.key(module, p) == self.env.key(here, s)
        };
        match (&pattern.kind, &site.kind) {
            (ExprKind::Var(v), _) if params.contains(&v.as_str()) => {
                parts.push((v.clone(), site));
                true
            }
            (ExprKind::Var(p), ExprKind::Var(s)) => same_item(p, s),
            (ExprKind::Int(a), ExprKind::Int(b)) => a == b,
            (ExprKind::Float(a), ExprKind::Float(b)) => a.to_bits() == b.to_bits(),
            (ExprKind::Bool(a), ExprKind::Bool(b)) => a == b,
            (ExprKind::Str(a), ExprKind::Str(b)) => a == b,
            (ExprKind::Unary(o, a), ExprKind::Unary(q, b)) => o == q && self.matches(module, params, a, b, parts),
            (ExprKind::Binary(o, a, x), ExprKind::Binary(q, b, y)) => {
                o == q && self.matches(module, params, a, b, parts) && self.matches(module, params, x, y, parts)
            }
            (ExprKind::Call(p, pa), ExprKind::Call(s, sa)) => {
                pa.len() == sa.len()
                    && same_item(p, s)
                    && pa.iter().zip(sa).all(|(a, b)| self.matches(module, params, a, b, parts))
            }
            _ => false,
        }
    }

    /// Calls rule `rule` with the matched `parts`, lowered in order, if their
    /// types fit its parameters and moving the code the rule moves is safe
    /// here (`effects.rs`):
    ///
    /// - a part that the call as written evaluates after one of the
    ///   pattern's own calls or operators, which the rule runs later, must
    ///   be inert, unless that call or operator is;
    /// - a part right of `&&` or `||`, which the call as written may not
    ///   evaluate at all, must be inert;
    /// - a function a part gives must be one the compiler can see (written
    ///   there, or named), and inert: the rule may call it in another order,
    ///   or another number of times. Unless the rule keeps the order
    ///   (`keeps_order`): then it may be any function;
    /// - no part may hold function values inside other values.
    ///
    /// Otherwise the call is compiled as written.
    fn apply_rule(&mut self, pos: Pos, rule: &str, decl: &FnDecl, module: usize, parts: &[(String, &Expr)]) -> Result<Option<Reg>, Error> {
        let runtime: Vec<&Param> = decl.params.iter().filter(|p| !p.comptime).collect();
        let type_params: Vec<&str> = decl.params.iter().filter(|p| p.comptime).map(|p| p.name.as_str()).collect();
        let generic = !type_params.is_empty();
        let mut found = Found { module, callee: &decl.name, params: type_params, types: HashMap::new(), strict: true };
        let mut regs: HashMap<&str, Reg> = HashMap::new();
        let names: Vec<&str> = runtime.iter().map(|p| p.name.as_str()).collect();
        let mut steps = Vec::new();
        pattern_steps(&decl.replaces.as_ref().unwrap().cond, &names, false, &mut steps);
        // The call itself comes last, after every part in both versions.
        steps.pop();
        // Whether an operation of the pattern ran before this point in the
        // call as written, that the rule runs after all of the parts.
        let mut moved = false;
        // Whether a function given may not be inert, which the rule must
        // then call as the pattern does.
        let mut in_order = false;
        for step in steps {
            let (name, guarded) = match step {
                Step::Op(op) => {
                    moved = moved || !self.op_inert(module, op, &regs);
                    continue;
                }
                Step::Part(name, guarded) => (name, guarded),
            };
            let (_, e) = parts.iter().find(|(n, _)| n == name).unwrap();
            let p = runtime.iter().find(|p| p.name == name).unwrap();
            let want = match found.unknown_in(&p.ty) {
                None => self.env.resolve(&p.ty, &Rc::new(found.bindings())).ok(),
                Some(_) => None,
            };
            let mark = self.mark();
            let r = self.expr(e, want.as_ref())?;
            let got = self.ty(r).clone();
            if !self.env.unify(&mut found, &p.ty, &got, e.pos)? {
                return Ok(None);
            }
            if (moved || guarded) && !self.effects_since(&mark).inert() {
                return Ok(None);
            }
            if matches!(got, Type::Fn(..)) {
                in_order |= !self.fn_value_effects(&mark, r).inert();
            } else if self.holds_fn(&got, &mut HashSet::new()) {
                return Ok(None);
            }
            regs.insert(name, r);
        }
        if found.params.iter().any(|t| !found.types.contains_key(t)) {
            return Ok(None);
        }
        let (func, sig) = if generic {
            self.env.instance(pos, rule, decl, found.bindings())?
        } else {
            (self.env.ir_name(rule), self.env.signature(rule).unwrap()?)
        };
        let args: Vec<Reg> = runtime.iter().map(|p| regs[p.name.as_str()]).collect();
        if args.iter().zip(&sig.params).any(|(r, want)| self.ty(*r) != want) {
            return Ok(None);
        }
        if in_order && !self.env.keeps_order(&func) {
            return Ok(None);
        }
        Ok(Some(self.emit_to(sig.ret.clone(), |dst| jihoo_ir::Inst::Call { dst, func, args })))
    }

    /// Whether operation `op` of a pattern written in `module`, a call or an
    /// operator, is inert. The parts it reads are in `regs` already.
    fn op_inert(&self, module: usize, op: &Expr, regs: &HashMap<&str, Reg>) -> bool {
        match &op.kind {
            // A generic function's instance is not known yet: assumed to do
            // anything.
            ExprKind::Call(name, _) => match self.env.key(module, name) {
                Some(key) if self.env.generic(&key).is_none() => self.env.effects_of(&key).inert(),
                _ => false,
            },
            // On numbers and bools, operators other than integer division
            // cannot fail; on anything else they may call code (`Ord`).
            ExprKind::Unary(_, x) => self.scalar_operand(module, x, regs),
            ExprKind::Binary(o, a, b) => {
                !matches!(o, jihoo_syntax::ast::BinOp::Div | jihoo_syntax::ast::BinOp::Rem)
                    && self.scalar_operand(module, a, regs)
                    && self.scalar_operand(module, b, regs)
            }
            _ => false,
        }
    }

    /// Whether `e`, an operand in a pattern, is a number or a bool: a
    /// literal, a part of that type, or an inert operator on such operands.
    fn scalar_operand(&self, module: usize, e: &Expr, regs: &HashMap<&str, Reg>) -> bool {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) => true,
            ExprKind::Var(v) => regs.get(v.as_str()).is_some_and(|r| matches!(self.ty(*r), Type::Int(_) | Type::Float(_) | Type::Bool)),
            ExprKind::Unary(..) | ExprKind::Binary(..) => self.op_inert(module, e, regs),
            _ => false,
        }
    }

    /// Where the code lowered from here on starts.
    fn mark(&self) -> Mark {
        Mark { lens: self.blocks.iter().map(|b| b.insts.len()).collect(), hoisted: self.hoisted, cur: self.cur.0 as usize }
    }

    /// What the code lowered since `mark` may do.
    fn effects_since(&self, mark: &Mark) -> Effects {
        let mut fin = true;
        let regs = |r: Reg| self.ty(r).clone();
        let mut e = self.env.insts_effects(self.insts_since(mark), &regs, &mut fin);
        for (i, b) in self.blocks.iter().enumerate().filter(|(i, _)| *i == mark.cur || *i >= mark.lens.len()) {
            if let Some(term) = &b.term {
                e |= self.env.term_effects(i, term, &mut fin);
            }
        }
        e
    }

    fn insts_since<'s>(&'s self, mark: &'s Mark) -> impl Iterator<Item = &'s Inst> + 's {
        self.blocks.iter().enumerate().flat_map(move |(i, b)| {
            // Constants hoisted to the front of the entry block meanwhile are
            // inert; what was added at its end is the new code.
            let from = match mark.lens.get(i) {
                Some(&n) if i == 0 => n + (self.hoisted - mark.hoisted),
                Some(&n) => n,
                None => 0,
            };
            &b.insts[from.min(b.insts.len())..]
        })
    }

    /// What calling function value `r`, made by the code lowered since `mark`,
    /// may do: the function it names, or anything if it was not made there.
    /// A named function is a hoisted constant, whose register is never
    /// written again.
    fn fn_value_effects(&self, mark: &Mark, r: Reg) -> Effects {
        let hoisted = self.blocks[0].insts[..self.hoisted].iter();
        let made = hoisted.chain(self.insts_since(mark)).find_map(|i| match i {
            Inst::FuncRef { dst, func } | Inst::Closure { dst, func, .. } if *dst == r => Some(func),
            _ => None,
        });
        match made {
            // Captured values are copies, read-only inside the closure.
            Some(func) => self.env.effects_of(func),
            None => Effects::UNKNOWN,
        }
    }

    /// Whether a value of `t` may hold a function value inside it; a pointer
    /// may point at anything.
    fn holds_fn(&self, t: &Type, seen: &mut HashSet<String>) -> bool {
        match t {
            Type::Fn(..) | Type::Ptr(_) => true,
            Type::List(e) | Type::Array(e, _) | Type::Ref(e) | Type::Cell(e) | Type::Chan(e) => self.holds_fn(e, seen),
            Type::Struct(n) | Type::Enum(n) if !seen.insert(n.clone()) => false,
            Type::Struct(n) => self.env.struct_fields(Pos::default(), n).map_or(true, |fs| fs.iter().any(|(_, t)| self.holds_fn(t, seen))),
            Type::Enum(n) => {
                self.env.enum_variants(Pos::default(), n).map_or(true, |vs| vs.iter().flat_map(|(_, ts)| ts).any(|t| self.holds_fn(t, seen)))
            }
            _ => false,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            blocks: self.blocks.clone(),
            regs: self.regs.len(),
            cur: self.cur,
            hoisted: self.hoisted,
            const_regs: self.const_regs.clone(),
            scopes: self.scopes.clone(),
            last_loc: self.last_loc,
        }
    }

    fn rollback(&mut self, s: Snapshot) {
        self.blocks = s.blocks;
        self.regs.truncate(s.regs);
        self.cur = s.cur;
        self.hoisted = s.hoisted;
        self.const_regs = s.const_regs;
        self.scopes = s.scopes;
        self.last_loc = s.last_loc;
    }
}

/// The steps of pattern `e` with variables `params`, in the order a call
/// that matches it runs them: operands, then the operation.
fn pattern_steps<'e>(e: &'e Expr, params: &[&str], guarded: bool, out: &mut Vec<Step<'e>>) {
    match &e.kind {
        ExprKind::Var(v) if params.contains(&v.as_str()) => out.push(Step::Part(v, guarded)),
        ExprKind::Unary(_, x) => {
            pattern_steps(x, params, guarded, out);
            out.push(Step::Op(e));
        }
        ExprKind::Binary(op, a, b) => {
            let short = matches!(op, jihoo_syntax::ast::BinOp::And | jihoo_syntax::ast::BinOp::Or);
            pattern_steps(a, params, guarded, out);
            pattern_steps(b, params, guarded || short, out);
            out.push(Step::Op(e));
        }
        ExprKind::Call(_, args) => {
            args.iter().for_each(|a| pattern_steps(a, params, guarded, out));
            out.push(Step::Op(e));
        }
        // Literals, and names of constants and functions.
        _ => {}
    }
}

/// Counts how often the pattern uses each of `params`, checking it holds
/// only what a pattern may.
fn pattern_vars(e: &Expr, params: &[&str], seen: &mut HashMap<String, usize>) -> Result<(), Error> {
    match &e.kind {
        ExprKind::Var(v) => {
            if params.contains(&v.as_str()) {
                *seen.entry(v.clone()).or_default() += 1;
            }
            Ok(())
        }
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) => Ok(()),
        ExprKind::Unary(_, x) => pattern_vars(x, params, seen),
        ExprKind::Binary(_, a, b) => {
            pattern_vars(a, params, seen)?;
            pattern_vars(b, params, seen)
        }
        ExprKind::Call(_, args) => args.iter().try_for_each(|a| pattern_vars(a, params, seen)),
        _ => Err(Error::new(e.pos, "a pattern holds calls, operators, literals, names and the rule's parameters")),
    }
}

/// How one side of a rule went for one input.
#[derive(Clone)]
enum Outcome {
    /// The result, and what it printed.
    Value(ConstValue, Vec<u8>),
    /// Why it trapped, and what it printed before.
    Trap(String, Vec<u8>),
    /// It did not finish within its steps.
    Unknown,
}

/// A rule being checked, ready to run.
struct Check {
    rule: String,
    pattern: String,
    params: Vec<Type>,
    ret: Type,
    module: Rc<jihoo_ir::Module>,
    program: Rc<Program>,
    fuel: u64,
    /// Whether two traps must also have the same cause and have printed the
    /// same, as when checking the order of calls.
    strict: bool,
    /// The inputs both sides finished.
    decided: usize,
}

impl<'p> Env<'p> {
    /// Checks every rule among `funcs`: each must compute what it replaces.
    /// A generic rule is also checked with `i64` for each of its type
    /// parameters, so that one the program never applies is checked too.
    pub(crate) fn check_rules(&self, funcs: &[Function]) -> Vec<Error> {
        let mut errors = Vec::new();
        let mut funcs: Vec<Function> = funcs.to_vec();
        let mut generic: Vec<(&str, &FnDecl)> =
            self.fn_decls_iter().filter(|(_, (_, d))| d.replaces.is_some() && d.params.iter().any(|p| p.comptime)).map(|(k, (_, d))| (k, d)).collect();
        generic.sort_by_key(|(k, _)| *k);
        for (key, decl) in generic {
            let mut b = Bindings::in_module(self.fn_module(key));
            for p in decl.params.iter().filter(|p| p.comptime) {
                b.push(&p.name, crate::generic::Binding::Type(Type::I64));
            }
            // A rule whose type parameters `i64` does not fit is checked
            // where it is used.
            let Ok((name, _)) = self.instance(decl.pos, key, decl, b) else { continue };
            match self.function(&name) {
                Ok(f) if !funcs.iter().any(|g| g.name == f.name) => funcs.push((*f).clone()),
                Ok(_) => {}
                Err(e) => errors.push(e),
            }
        }
        for f in &funcs {
            let Some((decl, bindings)) = self.decl_of(&f.name) else { continue };
            let Some(rule) = &decl.replaces else { continue };
            if let Err(e) = self.check_rule_fn(f, decl, rule, bindings) {
                errors.push(e);
            }
        }
        errors
    }

    fn check_rule_fn(&self, f: &Function, decl: &FnDecl, rule: &Contract, bindings: Rc<Bindings>) -> Result<(), Error> {
        let refuse = |why: String| Error::new(rule.pos, format!("cannot check that `{}` does what it replaces: {why}", decl.name));
        let (mut gen, made, pattern_name) = self.rule_sides(f, decl, rule, &bindings, &f.name).map_err(|e| match e {
            Refused::Why(why) => refuse(why),
            Refused::Error(e) => e,
        })?;
        let cannot = |e: Error| refuse(e.msg.trim_start_matches("cannot evaluate this at compile time: ").to_string());
        self.prepare(rule.pos, made, &pattern_name).map_err(cannot)?;
        let (vm_module, program) = self.prepare(rule.pos, vec![], &f.name).map_err(cannot)?;
        let mut c = Check {
            rule: f.name.clone(),
            pattern: pattern_name,
            params: f.params.clone(),
            ret: f.ret.clone(),
            module: vm_module,
            program,
            fuel: BUDGET,
            strict: false,
            decided: 0,
        };

        let specials: Vec<Vec<ConstValue>> = c.params.iter().map(|t| gen.specials(t, DEPTH)).collect();
        for i in 0..CASES {
            let Some(args) = case(&mut gen, &c.params, &specials, i, CASES) else { continue };
            if c.fuel == 0 {
                break;
            }
            if let Some((ours, theirs)) = self.differ(&mut c, &args) {
                let (args, ours, theirs) = self.shrink_rule(&mut c, &mut gen, args, ours, theirs);
                return Err(self.rule_error(&c, decl, rule, &gen, &args, &ours, &theirs));
            }
        }
        Ok(())
    }

    /// What both sides of the rule `f` need to run, named from `prefix`: the
    /// inputs, the functions written to pass for its parameters of function
    /// type, and the pattern as a function, with its name.
    fn rule_sides(
        &self,
        f: &Function,
        decl: &FnDecl,
        rule: &Contract,
        bindings: &Rc<Bindings>,
        prefix: &str,
    ) -> Result<(Gen<'_, 'p>, Vec<Function>, String), Refused> {
        let module = bindings.module;
        for t in &f.params {
            if !self.rule_makeable(t, module) {
                return Err(Refused::Why(format!("the compiler cannot make up values of type {t} to try it with")));
            }
        }
        if !self.comparable(&f.ret, &mut HashSet::new()) {
            return Err(Refused::Why(format!("values of type {} cannot be compared", f.ret)));
        }

        let mut gen = Gen::new(self, &f.name);
        let mut made = Vec::new();
        for t in &f.params {
            if let Type::Fn(ps, r) = t {
                if gen.fns.contains_key(&t.to_string()) {
                    continue;
                }
                let fs = self.sample_fns(prefix, made.len(), ps, r, bindings, &mut gen).map_err(Refused::Error)?;
                gen.fns.insert(t.to_string(), fs.iter().map(|(f, _)| f.name.clone()).collect());
                for (func, text) in fs {
                    gen.fn_text.insert(func.name.clone(), text);
                    made.push(func);
                }
            }
        }
        let pattern = self.pattern_fn(f, decl, rule, bindings.clone(), &format!("{prefix}.pattern")).map_err(Refused::Error)?;
        // Both sides run with no input; what they would read at run time is
        // not tried.
        if (self.effects_of(&f.name) | self.body_effects(&pattern)).has(Effects::READS_INPUT) {
            return Err(Refused::Why("it reads standard input, which the checks cannot give it".into()));
        }
        let pattern_name = pattern.name.clone();
        made.push(pattern);
        Ok((gen, made, pattern_name))
    }

    /// Whether rule `name` (a JIR name, of a generic rule's instance)
    /// calls the functions it is given as its pattern does: the same
    /// functions, with the same arguments, in the same order, as many times,
    /// and stops where the pattern does when one of them traps. Then any
    /// function can be given to it, whatever it does. Worked out once for
    /// each rule asked about, the first time it would apply to a function
    /// that is not inert.
    pub(crate) fn keeps_order(&self, name: &str) -> bool {
        if let Some(&kept) = self.order_kept.borrow().get(name) {
            return kept;
        }
        // A rule the checks cannot run, or one still being compiled, is
        // taken not to.
        let kept = self.check_order(name).unwrap_or(false);
        self.order_kept.borrow_mut().insert(name.to_string(), kept);
        kept
    }

    /// Runs rule `name` and its pattern side by side, as `check_rule_fn`
    /// does, but with each function given wrapped in one that first prints
    /// which parameter it was passed for and its arguments, and that traps
    /// on the `k`th call of any of them, for no `k` and for a few.
    fn check_order(&self, name: &str) -> Result<bool, Refused> {
        let f = self.function(name).map_err(Refused::Error)?;
        let Some((decl, bindings)) = self.decl_of(name) else { return Ok(false) };
        let Some(rule) = &decl.replaces else { return Ok(false) };
        let prefix = format!("{}.order", f.name);
        let (mut gen, mut made, pattern) = self.rule_sides(&f, decl, rule, &bindings, &prefix)?;
        let names = decl.params.iter().filter(|p| !p.comptime).map(|p| p.name.as_str());
        let mut tracers = Vec::new();
        for (i, (t, param)) in f.params.iter().zip(names).enumerate() {
            tracers.push(match t {
                Type::Fn(ps, r) => {
                    let tracer = self.tracer(&format!("{prefix}.trace.{i}"), param, ps, r, &bindings).map_err(Refused::Error)?;
                    let name = tracer.name.clone();
                    made.push(tracer);
                    Some(name)
                }
                _ => None,
            });
        }
        let (ours, theirs) = (format!("{prefix}.rule"), format!("{prefix}.of_pattern"));
        made.push(self.traced(&ours, &f.name, &f, &tracers, &bindings).map_err(Refused::Error)?);
        made.push(self.traced(&theirs, &pattern, &f, &tracers, &bindings).map_err(Refused::Error)?);
        let (vm_module, program) = self.prepare(rule.pos, made, &ours).map_err(Refused::Error)?;
        let mut params = f.params.clone();
        params.push(Type::I64);
        let mut c = Check { rule: ours, pattern: theirs, params, ret: f.ret.clone(), module: vm_module, program, fuel: BUDGET, strict: true, decided: 0 };

        let specials: Vec<Vec<ConstValue>> = f.params.iter().map(|t| gen.specials(t, DEPTH)).collect();
        for i in 0..ORDER_CASES {
            let Some(mut args) = case(&mut gen, &f.params, &specials, i, ORDER_CASES) else { continue };
            // No trap, then a trap at one of the first few calls.
            for k in [-1, (i % TRAP_AT) as i64] {
                if c.fuel == 0 {
                    break;
                }
                args.push(ConstValue::Int(k));
                if self.differ(&mut c, &args).is_some() {
                    return Ok(false);
                }
                args.pop();
            }
        }
        // Only what was tried counts.
        Ok(c.decided >= ORDER_CASES / 10)
    }

    /// `fn name(c: cell<i64>, k: i64, f: fn(params) -> ret, params) -> ret`,
    /// a closure to wrap function `f`, given for parameter `param`, in: it
    /// counts its call in `c`, prints `param` and the arguments it can
    /// print, traps if that call is the `k`th of all the wrapped functions'
    /// (from 0), and returns what `f` does.
    fn tracer(&self, name: &str, param: &str, params: &[Type], ret: &Type, bindings: &Rc<Bindings>) -> Result<Function, Error> {
        let inner_ty = Type::Fn(params.to_vec(), Box::new(ret.clone()));
        let cell_ty = Type::Cell(Box::new(Type::I64));
        let all = [cell_ty.clone(), Type::I64, inner_ty].into_iter().chain(params.iter().cloned()).collect();
        let sig = Rc::new(Sig { params: all, ret: ret.clone() });
        let mut cx = FnCx::new(self, sig, bindings.clone());
        let cell = cx.new_reg(cell_ty);
        let k = cx.new_reg(Type::I64);
        let inner = cx.new_reg(Type::Fn(params.to_vec(), Box::new(ret.clone())));
        let args: Vec<Reg> = params.iter().map(|t| cx.new_reg(t.clone())).collect();
        let n = cx.emit_to(Type::I64, |dst| Inst::CellGet { dst, cell, path: vec![] });
        let one = cx.konst(Type::I64, 1);
        let next = cx.emit_to(Type::I64, |dst| Inst::Binary { dst, op: BinOp::Add, lhs: n, rhs: one });
        cx.emit(Inst::CellSet { cell, path: vec![], value: next });
        let tag = cx.emit_to(Type::Str, |dst| Inst::Str { dst, value: format!("{param}:") });
        cx.emit(Inst::Print { src: tag });
        for (&a, t) in args.iter().zip(params) {
            if matches!(t, Type::Int(_) | Type::Float(_) | Type::Bool | Type::Str) {
                cx.emit(Inst::Print { src: a });
            }
        }
        let at_k = cx.emit_to(Type::Bool, |dst| Inst::Binary { dst, op: BinOp::Eq, lhs: n, rhs: k });
        let (trap, call) = (cx.new_block(), cx.new_block());
        cx.terminate(Terminator::Branch { cond: at_k, then: trap, els: call });
        cx.switch_to(trap);
        cx.emit(Inst::Trap);
        cx.terminate(Terminator::Unreachable);
        cx.switch_to(call);
        let r = cx.emit_to(ret.clone(), |dst| Inst::CallIndirect { dst, callee: inner, args });
        cx.terminate(Terminator::Ret(r));
        cx.finish(name, ret.clone(), Pos::default())
    }

    /// `fn name(f's params, k: i64) -> f's ret`: calls `target`, which has
    /// `f`'s signature, with each function among the parameters wrapped in
    /// its tracer, all counting in one cell.
    fn traced(&self, name: &str, target: &str, f: &Function, tracers: &[Option<String>], bindings: &Rc<Bindings>) -> Result<Function, Error> {
        let all = f.params.iter().cloned().chain([Type::I64]).collect();
        let sig = Rc::new(Sig { params: all, ret: f.ret.clone() });
        let mut cx = FnCx::new(self, sig, bindings.clone());
        let params: Vec<Reg> = f.params.iter().map(|t| cx.new_reg(t.clone())).collect();
        let k = cx.new_reg(Type::I64);
        let zero = cx.konst(Type::I64, 0);
        let cell = cx.emit_to(Type::Cell(Box::new(Type::I64)), |dst| Inst::NewCell { dst, value: zero });
        let mut args = Vec::new();
        for ((&p, t), tracer) in params.iter().zip(&f.params).zip(tracers) {
            args.push(match tracer {
                Some(func) => cx.emit_to(t.clone(), |dst| Inst::Closure { dst, func: func.clone(), captures: vec![cell, k, p] }),
                None => p,
            });
        }
        let func = target.to_string();
        let r = cx.emit_to(f.ret.clone(), |dst| Inst::Call { dst, func, args });
        cx.terminate(Terminator::Ret(r));
        cx.finish(name, f.ret.clone(), Pos::default())
    }

    /// `fn name(params) -> R { return pattern }`, with rules off.
    fn pattern_fn(&self, f: &Function, decl: &FnDecl, rule: &Contract, bindings: Rc<Bindings>, name: &str) -> Result<Function, Error> {
        let sig = Rc::new(Sig { params: f.params.clone(), ret: f.ret.clone() });
        let mut cx = FnCx::new(self, sig, bindings);
        cx.in_rule = true;
        for (p, ty) in decl.params.iter().filter(|p| !p.comptime).zip(&f.params) {
            let r = cx.new_reg(ty.clone());
            cx.scopes[0].insert(p.name.clone(), r);
        }
        let r = cx.expr(&rule.cond, Some(&f.ret))?;
        if *cx.ty(r) != f.ret {
            let msg = format!("the pattern gives {}, but `{}` returns {}", cx.ty(r), decl.name, f.ret);
            return Err(Error::new(rule.pos, msg));
        }
        cx.terminate(Terminator::Ret(r));
        cx.finish(name, f.ret.clone(), rule.pos)
    }

    /// Runs both sides with `args`: what each gave, when they differ.
    fn differ(&self, c: &mut Check, args: &[ConstValue]) -> Option<(Outcome, Outcome)> {
        let rule = c.rule.clone();
        let pattern = c.pattern.clone();
        let ours = self.run_side(c, &rule, args);
        let theirs = self.run_side(c, &pattern, args);
        match (&ours, &theirs) {
            (Outcome::Unknown, _) | (_, Outcome::Unknown) => return None,
            _ => c.decided += 1,
        }
        match (&ours, &theirs) {
            (Outcome::Trap(a, p), Outcome::Trap(b, q)) if !c.strict || (a == b && p == q) => None,
            (Outcome::Value(a, p), Outcome::Value(b, q)) if same(a, b) && p == q => None,
            _ => Some((ours, theirs)),
        }
    }

    fn run_side(&self, c: &mut Check, entry: &str, args: &[ConstValue]) -> Outcome {
        let module = c.module.clone();
        let fuel = c.fuel.min(CASE_FUEL);
        let mut vm = Vm::with_program(&module, c.program.clone()).with_fuel(fuel).with_input(jihoo_vm::Input::Closed);
        let values: Vec<Value> = args.iter().map(|a| arg(&mut vm, a)).collect();
        let mut printed = Vec::new();
        let r = vm.call_named(entry, &values, &mut printed);
        c.fuel = c.fuel.saturating_sub(fuel - vm.fuel().unwrap_or(0));
        match r {
            Ok(v) => match self.to_const(Pos::default(), v, &c.ret, &vm) {
                Ok(v) => Outcome::Value(v, printed),
                Err(_) => Outcome::Unknown,
            },
            Err(e) if e.msg.starts_with("evaluation did not finish") || e.msg == "stack overflow" => Outcome::Unknown,
            Err(e) => Outcome::Trap(e.msg, printed),
        }
    }
    fn shrink_rule(
        &self,
        c: &mut Check,
        gen: &mut Gen,
        mut args: Vec<ConstValue>,
        mut ours: Outcome,
        mut theirs: Outcome,
    ) -> (Vec<ConstValue>, Outcome, Outcome) {
        c.fuel = BUDGET;
        let mut runs = 0;
        'outer: while runs < SHRINK_RUNS {
            for i in 0..args.len() {
                for smaller in gen.shrinks(&c.params[i], &args[i]) {
                    runs += 1;
                    if runs >= SHRINK_RUNS || c.fuel == 0 {
                        break 'outer;
                    }
                    let mut tried = args.clone();
                    tried[i] = smaller;
                    if let Some((o, t)) = self.differ(c, &tried) {
                        (args, ours, theirs) = (tried, o, t);
                        continue 'outer;
                    }
                }
            }
            break;
        }
        (args, ours, theirs)
    }

    #[allow(clippy::too_many_arguments)]
    fn rule_error(&self, c: &Check, decl: &FnDecl, rule: &Contract, gen: &Gen, args: &[ConstValue], ours: &Outcome, theirs: &Outcome) -> Error {
        let names = decl.params.iter().filter(|p| !p.comptime).map(|p| p.name.as_str());
        let with: Vec<String> = names.zip(&c.params).zip(args).map(|((n, t), v)| format!("{n} = {}", gen.show(t, v))).collect();
        // When the values are the same, what differs is what they print.
        let printing = matches!((ours, theirs), (Outcome::Value(a, _), Outcome::Value(b, _)) if same(a, b));
        let gives = |o: &Outcome| match o {
            Outcome::Value(_, p) if printing => format!("prints {:?}", String::from_utf8_lossy(p)),
            Outcome::Value(v, _) => format!("gives {}", gen.show(&c.ret, v)),
            Outcome::Trap(msg, _) => format!("traps ({msg})"),
            Outcome::Unknown => "does not finish".into(),
        };
        let msg = format!(
            "`{}` does not do what it replaces: with {}, `{}` {}, but `{}` {}",
            decl.name,
            with.join(", "),
            rule.text,
            gives(theirs),
            decl.name,
            gives(ours)
        );
        Error::new(rule.pos, jihoo_ir::plain_names(&msg))
    }

    /// Whether the compiler can make up values of `t` for a rule: what
    /// contracts are tried with, and functions it can write.
    fn rule_makeable(&self, t: &Type, module: usize) -> bool {
        match t {
            // Functions that return values it can make up and compare.
            Type::Fn(_, r) => self.comparable(r, &mut HashSet::new()) && self.makeable(r, module, false, &mut HashSet::new()),
            _ => self.makeable(t, module, false, &mut HashSet::new()),
        }
    }

    /// Whether two values of `t` can be compared for the same result.
    fn comparable(&self, t: &Type, seen: &mut HashSet<String>) -> bool {
        match t {
            Type::Unit | Type::Int(_) | Type::Float(_) | Type::Bool | Type::Str => true,
            Type::List(e) | Type::Array(e, _) | Type::Ref(e) => self.comparable(e, seen),
            Type::Struct(n) | Type::Enum(n) if !seen.insert(n.clone()) => true,
            Type::Struct(n) if self.path_named(n).is_none() => {
                self.struct_fields(Pos::default(), n).is_ok_and(|fs| fs.iter().all(|(_, t)| self.comparable(t, seen)))
            }
            Type::Enum(n) => self.enum_variants(Pos::default(), n).is_ok_and(|vs| vs.iter().flat_map(|(_, ts)| ts).all(|t| self.comparable(t, seen))),
            _ => false,
        }
    }

    /// A few functions of type `fn(params) -> ret` to try a rule with,
    /// simplest first, each with how it reads.
    fn sample_fns(
        &self,
        rule: &str,
        first: usize,
        params: &[Type],
        ret: &Type,
        bindings: &Rc<Bindings>,
        gen: &mut Gen,
    ) -> Result<Vec<(Function, String)>, Error> {
        let names = ["x", "y", "z", "w", "v", "u"];
        let args = names.iter().take(params.len()).copied().collect::<Vec<_>>().join(", ");
        let mut bodies: Vec<Body> = Vec::new();
        let same: Vec<usize> = (0..params.len()).filter(|&i| params[i] == *ret).collect();
        match ret {
            Type::Int(it) => {
                bodies.push(Body::Const(ConstValue::Int(0)));
                for &i in &same {
                    bodies.extend([Body::Param(i), Body::Op(i, BinOp::Add, 1), Body::Op(i, BinOp::Mul, 2)]);
                    if it.signed() {
                        bodies.push(Body::Neg(i));
                    }
                    bodies.push(Body::Square(i));
                }
                if same.len() >= 2 {
                    bodies.push(Body::Sum(same[0], same[1]));
                }
                bodies.push(Body::Const(Gen::int(*it, 7)));
            }
            &Type::Float(ft) => {
                bodies.push(Body::Const(ConstValue::Float(0f64.to_bits())));
                for &i in &same {
                    bodies.extend([Body::Param(i), Body::FOp(i, BinOp::Add, 1.0), Body::FOp(i, BinOp::Mul, 2.0)]);
                }
                bodies.push(Body::Const(ConstValue::Float(ft.round(1.5).to_bits())));
            }
            Type::Bool => {
                bodies.extend([Body::Const(ConstValue::Bool(false)), Body::Const(ConstValue::Bool(true))]);
                for (i, p) in params.iter().enumerate() {
                    match p {
                        Type::Int(_) => bodies.extend([Body::Even(i), Body::Positive(i)]),
                        Type::Bool => bodies.extend([Body::Param(i), Body::Not(i)]),
                        _ => {}
                    }
                }
            }
            _ => {
                for &i in &same {
                    bodies.push(Body::Param(i));
                }
                for v in gen.specials(ret, 2).into_iter().take(3) {
                    bodies.push(Body::Const(v));
                }
            }
        }
        bodies.truncate(8);
        let mut out = Vec::new();
        for (k, body) in bodies.into_iter().enumerate() {
            let name = format!("{rule}.arg.{}", first + k);
            let sig = Rc::new(Sig { params: params.to_vec(), ret: ret.clone() });
            let mut cx = FnCx::new(self, sig, bindings.clone());
            let regs: Vec<Reg> = params.iter().map(|t| cx.new_reg(t.clone())).collect();
            let (r, text) = body.lower(&mut cx, &regs, ret, &names, gen);
            cx.terminate(Terminator::Ret(r));
            out.push((cx.finish(&name, ret.clone(), Pos::default())?, format!("fn({args}) {text}")));
        }
        Ok(out)
    }
}

/// The body of a function `sample_fns` writes.
enum Body {
    Const(ConstValue),
    Param(usize),
    Op(usize, BinOp, i64),
    FOp(usize, BinOp, f64),
    Neg(usize),
    Square(usize),
    Sum(usize, usize),
    Even(usize),
    Positive(usize),
    Not(usize),
}

impl Body {
    /// Emits the body into `cx`, whose parameters are `p`: the result and how
    /// it reads.
    fn lower(&self, cx: &mut FnCx, p: &[Reg], ret: &Type, names: &[&str], gen: &Gen) -> (Reg, String) {
        let bin = |cx: &mut FnCx, op: BinOp, a: Reg, b: Reg, t: Type| cx.emit_to(t, |dst| jihoo_ir::Inst::Binary { dst, op, lhs: a, rhs: b });
        match *self {
            Body::Const(ref v) => (cx.splice(ret, v), gen.show(ret, v)),
            Body::Param(i) => (p[i], names[i].to_string()),
            Body::Op(i, op, n) => {
                let k = cx.konst(ret.clone(), n);
                let sym = if op == BinOp::Add { "+" } else { "*" };
                (bin(cx, op, p[i], k, ret.clone()), format!("{} {sym} {n}", names[i]))
            }
            Body::FOp(i, op, x) => {
                let Type::Float(ft) = *ret else { unreachable!() };
                let k = cx.fconst(ft, x);
                let sym = if op == BinOp::Add { "+" } else { "*" };
                (bin(cx, op, p[i], k, ret.clone()), format!("{} {sym} {x:?}", names[i]))
            }
            Body::Neg(i) => {
                let z = cx.konst(ret.clone(), 0);
                (bin(cx, BinOp::Sub, z, p[i], ret.clone()), format!("-{}", names[i]))
            }
            Body::Square(i) => (bin(cx, BinOp::Mul, p[i], p[i], ret.clone()), format!("{0} * {0}", names[i])),
            Body::Sum(i, j) => (bin(cx, BinOp::Add, p[i], p[j], ret.clone()), format!("{} + {}", names[i], names[j])),
            Body::Even(i) => {
                let t = cx.ty(p[i]).clone();
                let two = cx.konst(t.clone(), 2);
                let rem = bin(cx, BinOp::Rem, p[i], two, t.clone());
                let zero = cx.konst(t, 0);
                (bin(cx, BinOp::Eq, rem, zero, Type::Bool), format!("{} % 2 == 0", names[i]))
            }
            Body::Positive(i) => {
                let t = cx.ty(p[i]).clone();
                let zero = cx.konst(t, 0);
                (bin(cx, BinOp::Gt, p[i], zero, Type::Bool), format!("{} > 0", names[i]))
            }
            Body::Not(i) => {
                let f = cx.konst(Type::Bool, 0);
                (bin(cx, BinOp::Eq, p[i], f, Type::Bool), format!("!{}", names[i]))
            }
        }
    }
}

/// Why a rule cannot be run side by side with its pattern.
enum Refused {
    /// What keeps the checks from making up its inputs or comparing it.
    Why(String),
    /// An error compiling what the checks run.
    Error(Error),
}

/// Input `i` of the `n` inputs tried for `params`: the first half combines
/// their `specials` while there are combinations left, the rest are random
/// and grow; `None` when one cannot be made.
fn case(gen: &mut Gen, params: &[Type], specials: &[Vec<ConstValue>], i: usize, n: usize) -> Option<Vec<ConstValue>> {
    let total: usize = specials.iter().map(Vec::len).try_fold(1usize, |a, n| a.checked_mul(n)).unwrap_or(usize::MAX);
    if i < n / 2 && i < total {
        let mut k = i;
        return Some(
            specials
                .iter()
                .map(|vs| {
                    let v = vs[k % vs.len()].clone();
                    k /= vs.len();
                    v
                })
                .collect(),
        );
    }
    let size = 1 + (i * 16 / n) as u32;
    params.iter().map(|t| gen.random(t, size, DEPTH)).collect()
}

/// Whether two results are the same, with every NaN the same as any other.
fn same(a: &ConstValue, b: &ConstValue) -> bool {
    match (a, b) {
        (ConstValue::Float(x), ConstValue::Float(y)) => x == y || (f64::from_bits(*x).is_nan() && f64::from_bits(*y).is_nan()),
        (ConstValue::Agg(xs), ConstValue::Agg(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| same(x, y)),
        _ => a == b,
    }
}
