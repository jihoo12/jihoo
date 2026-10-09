//! Semantic analysis: type checks the AST and lowers it to JIR in one pass.
//!
//! Types: function signatures are written out, local variable types are inferred
//! from their initializers. Integer literals take their type from context
//! (`let x: u8 = 1`, `p[i] == 0`), defaulting to `i64`. The operator typing rules
//! are shared with the IR verifier (`jihoo_ir::types`), so well-typed programs
//! always produce valid IR.
//!
//! Errors are collected per function: one error stops the current function, but
//! the remaining functions are still checked.

mod place;

use std::collections::{HashMap, VecDeque};

use jihoo_ir as ir;
use jihoo_ir::{layout, types};
use jihoo_ir::{BlockId, Inst, IntTy, Profile, Reg, Terminator, Type};
use jihoo_syntax::ast::*;
use jihoo_syntax::{Error, Pos};

pub fn analyze(prog: &Program) -> Result<ir::Module, Vec<Error>> {
    let mut errors = Vec::new();

    let mut profile = Profile::Hosted;
    for (pos, attr) in &prog.attrs {
        match attr.as_str() {
            "freestanding" => profile = Profile::Freestanding,
            other => errors.push(Error::new(*pos, format!("unknown attribute `#![{other}]`"))),
        }
    }

    let mut env = Env { profile, structs: HashMap::new(), sigs: HashMap::new() };

    // Struct names first, so field and parameter types can refer to any struct.
    for s in &prog.structs {
        if Type::from_name(&s.name).is_some() {
            errors.push(Error::new(s.pos, format!("`{}` is a builtin type name", s.name)));
        } else if env.structs.insert(s.name.clone(), Vec::new()).is_some() {
            errors.push(Error::new(s.pos, format!("struct `{}` is defined twice", s.name)));
        }
    }
    let mut structs = Vec::new();
    for s in &prog.structs {
        match env.struct_fields(s) {
            Ok(fields) => {
                env.structs.insert(s.name.clone(), fields.clone());
                structs.push(ir::StructDef { name: s.name.clone(), fields });
            }
            Err(e) => errors.push(e),
        }
    }
    for s in &prog.structs {
        if let Err(e) = env.check_acyclic(s, &s.name, &mut Vec::new()) {
            errors.push(e);
        }
    }

    for f in &prog.funcs {
        match env.signature(f) {
            Ok(sig) => {
                if is_builtin(&f.name) {
                    errors.push(Error::new(f.pos, format!("`{}` is a builtin and cannot be redefined", f.name)));
                } else if env.sigs.insert(f.name.clone(), sig).is_some() {
                    errors.push(Error::new(f.pos, format!("function `{}` is defined twice", f.name)));
                }
            }
            Err(e) => errors.push(e),
        }
    }
    if let Err(e) = env.check_entry(prog) {
        errors.push(e);
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let mut funcs = Vec::new();
    for f in &prog.funcs {
        let sig = &env.sigs[&f.name];
        match FnCx::new(&env, sig).lower_fn(f) {
            Ok(func) => funcs.push(func),
            Err(e) => errors.push(e),
        }
    }

    if errors.is_empty() {
        Ok(ir::Module { profile, structs, funcs })
    } else {
        Err(errors)
    }
}

#[derive(Debug, Clone)]
struct Sig {
    params: Vec<Type>,
    ret: Type,
}

/// Module-level information shared by all functions.
struct Env {
    profile: Profile,
    /// Fields of each struct, in declaration order.
    structs: HashMap<String, Vec<(String, Type)>>,
    sigs: HashMap<String, Sig>,
}

fn is_builtin(name: &str) -> bool {
    matches!(name, "print" | "syscall" | "len" | "size_of" | "align_of")
}

impl Env {
    fn resolve(&self, t: &TypeExpr) -> Result<Type, Error> {
        match &t.kind {
            TypeExprKind::Ptr(inner) => {
                if self.profile != Profile::Freestanding {
                    return Err(Error::new(t.pos, "pointer types are only available in freestanding mode"));
                }
                Ok(Type::ptr(self.resolve(inner)?))
            }
            TypeExprKind::Array(elem, n) => Ok(Type::array(self.resolve(elem)?, *n)),
            TypeExprKind::Named(name) => {
                if let Some(ty) = Type::from_name(name) {
                    if ty == Type::Str && self.profile == Profile::Freestanding {
                        return Err(Error::new(
                            t.pos,
                            "type `str` is garbage collected and is not available in freestanding mode (use `*u8`)",
                        ));
                    }
                    Ok(ty)
                } else if self.structs.contains_key(name) {
                    Ok(Type::Struct(name.clone()))
                } else if name == "ptr" {
                    Err(Error::new(t.pos, "unknown type `ptr`; byte pointers are written `*u8`"))
                } else {
                    Err(Error::new(t.pos, format!("unknown type `{name}`")))
                }
            }
        }
    }

    fn struct_fields(&self, s: &StructDecl) -> Result<Vec<(String, Type)>, Error> {
        let mut fields: Vec<(String, Type)> = Vec::new();
        for f in &s.fields {
            if fields.iter().any(|(n, _)| *n == f.name) {
                return Err(Error::new(f.pos, format!("field `{}` is declared twice", f.name)));
            }
            fields.push((f.name.clone(), self.resolve(&f.ty)?));
        }
        Ok(fields)
    }

    /// A struct may not contain itself by value; use a pointer instead.
    fn check_acyclic(&self, decl: &StructDecl, name: &str, stack: &mut Vec<String>) -> Result<(), Error> {
        if stack.iter().any(|s| s == name) {
            let path = stack.join(" -> ");
            return Err(Error::new(
                decl.pos,
                format!("struct `{}` contains itself ({path} -> {name}); use a pointer", decl.name),
            ));
        }
        stack.push(name.to_string());
        for (_, t) in self.structs.get(name).into_iter().flatten() {
            // Arrays hold their elements by value; pointers break the cycle.
            let mut t = t;
            while let Type::Array(elem, _) = t {
                t = elem;
            }
            if let Type::Struct(inner) = t {
                self.check_acyclic(decl, inner, stack)?;
            }
        }
        stack.pop();
        Ok(())
    }

    fn signature(&self, f: &FnDecl) -> Result<Sig, Error> {
        let params = f.params.iter().map(|p| self.resolve(&p.ty)).collect::<Result<_, _>>()?;
        let ret = match &f.ret {
            Some(t) => self.resolve(t)?,
            None => Type::Unit,
        };
        Ok(Sig { params, ret })
    }

    fn check_entry(&self, prog: &Program) -> Result<(), Error> {
        let entry = self.profile.entry();
        let Some(decl) = prog.funcs.iter().find(|f| f.name == entry) else {
            let pos = Pos { line: 1, col: 1 };
            return Err(Error::new(pos, format!("{} program needs `fn {entry}()`", self.profile.as_str())));
        };
        match self.sigs.get(entry) {
            Some(sig) if !sig.params.is_empty() => {
                Err(Error::new(decl.pos, format!("`{entry}` must not take parameters")))
            }
            Some(sig) if !matches!(sig.ret, Type::Unit | Type::I64) => Err(Error::new(
                decl.pos,
                format!("`{entry}` must return nothing or i64, not {}", sig.ret),
            )),
            _ => Ok(()),
        }
    }

    /// Index and type of field `name` of struct type `t`.
    fn field(&self, pos: Pos, t: &Type, name: &str) -> Result<(u32, Type), Error> {
        let Type::Struct(s) = t else {
            return Err(Error::new(pos, format!("type {t} has no fields")));
        };
        self.structs[s]
            .iter()
            .enumerate()
            .find(|(_, (n, _))| n == name)
            .map(|(i, (_, t))| (i as u32, t.clone()))
            .ok_or_else(|| Error::new(pos, format!("struct `{s}` has no field `{name}`")))
    }

    fn layout(&self, pos: Pos, t: &Type) -> Result<layout::Layout, Error> {
        layout::of(t, &|name| self.structs.get(name).map(|f| &f[..]))
            .ok_or_else(|| Error::new(pos, format!("type {t} has no fixed memory layout (it contains a GC reference)")))
    }

    fn field_type(&self, t: &Type, index: u32) -> Type {
        let Type::Struct(s) = t else { unreachable!("not a struct: {t}") };
        self.structs[s][index as usize].1.clone()
    }
}

struct BlockBuf {
    insts: Vec<Inst>,
    term: Option<Terminator>,
}

struct FnCx<'a> {
    env: &'a Env,
    sig: &'a Sig,
    blocks: Vec<BlockBuf>,
    cur: BlockId,
    regs: Vec<Type>,
    scopes: Vec<HashMap<String, Reg>>,
}

/// An integer literal, possibly negated: its type comes from context.
fn is_int_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) => true,
        ExprKind::Unary(UnOp::Neg, inner) => matches!(inner.kind, ExprKind::Int(_)),
        _ => false,
    }
}

impl<'a> FnCx<'a> {
    fn new(env: &'a Env, sig: &'a Sig) -> Self {
        FnCx {
            env,
            sig,
            blocks: vec![BlockBuf { insts: vec![], term: None }],
            cur: BlockId(0),
            regs: Vec::new(),
            scopes: vec![HashMap::new()],
        }
    }

    fn profile(&self) -> Profile {
        self.env.profile
    }

    fn lower_fn(mut self, f: &FnDecl) -> Result<ir::Function, Error> {
        let sig = self.sig;
        for (p, ty) in f.params.iter().zip(&sig.params) {
            let r = self.new_reg(ty.clone());
            if self.scopes[0].insert(p.name.clone(), r).is_some() {
                return Err(Error::new(p.pos, format!("duplicate parameter `{}`", p.name)));
            }
        }

        self.block(&f.body)?;

        let reachable = self.reachable();
        // A reachable block that is still open falls off the end of the function.
        for &b in &reachable {
            if self.blocks[b].term.is_some() {
                continue;
            }
            if self.sig.ret != Type::Unit {
                return Err(Error::new(
                    f.body.end,
                    format!("missing `return`: `{}` must return {}", f.name, self.sig.ret),
                ));
            }
            let unit = self.new_reg(Type::Unit);
            self.blocks[b].insts.push(Inst::Unit { dst: unit });
            self.blocks[b].term = Some(Terminator::Ret(unit));
        }

        // Drop unreachable blocks and renumber the rest.
        let mut new_id = vec![None; self.blocks.len()];
        for (i, &b) in reachable.iter().enumerate() {
            new_id[b] = Some(BlockId(i as u32));
        }
        let mut old: Vec<Option<BlockBuf>> = self.blocks.into_iter().map(Some).collect();
        let blocks = reachable
            .iter()
            .map(|&b| {
                let buf = old[b].take().unwrap();
                let mut term = buf.term.unwrap();
                for s in term.successors_mut() {
                    *s = new_id[s.0 as usize].unwrap();
                }
                ir::Block { insts: buf.insts, term }
            })
            .collect();

        Ok(ir::Function {
            name: f.name.clone(),
            params: self.sig.params.clone(),
            ret: self.sig.ret.clone(),
            regs: self.regs,
            blocks,
        })
    }

    /// Blocks reachable from the entry, in breadth-first order (entry first).
    fn reachable(&self) -> Vec<usize> {
        let mut seen = vec![false; self.blocks.len()];
        let mut order = Vec::new();
        let mut queue = VecDeque::from([0usize]);
        seen[0] = true;
        while let Some(b) = queue.pop_front() {
            order.push(b);
            if let Some(t) = &self.blocks[b].term {
                for s in t.successors() {
                    let s = s.0 as usize;
                    if !seen[s] {
                        seen[s] = true;
                        queue.push_back(s);
                    }
                }
            }
        }
        order
    }

    // ---- builder ----

    fn new_reg(&mut self, ty: Type) -> Reg {
        self.regs.push(ty);
        Reg(self.regs.len() as u32 - 1)
    }

    fn ty(&self, r: Reg) -> &Type {
        &self.regs[r.0 as usize]
    }

    fn new_block(&mut self) -> BlockId {
        self.blocks.push(BlockBuf { insts: vec![], term: None });
        BlockId(self.blocks.len() as u32 - 1)
    }

    fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
    }

    fn emit(&mut self, inst: Inst) {
        self.blocks[self.cur.0 as usize].insts.push(inst);
    }

    /// Allocates a register of type `ty` and emits `make(dst)` into it.
    fn emit_to(&mut self, ty: Type, make: impl FnOnce(Reg) -> Inst) -> Reg {
        let dst = self.new_reg(ty);
        self.emit(make(dst));
        dst
    }

    /// Terminates the current block. No-op if it already ended (e.g. after `return`).
    fn terminate(&mut self, t: Terminator) {
        let b = &mut self.blocks[self.cur.0 as usize];
        if b.term.is_none() {
            b.term = Some(t);
        }
    }

    fn konst(&mut self, ty: Type, value: i64) -> Reg {
        self.emit_to(ty, |dst| Inst::Const { dst, value })
    }

    fn unit(&mut self) -> Reg {
        self.emit_to(Type::Unit, |dst| Inst::Unit { dst })
    }

    fn lookup(&self, pos: Pos, name: &str) -> Result<Reg, Error> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
            .ok_or_else(|| Error::new(pos, format!("unknown variable `{name}`")))
    }

    fn expect(&self, pos: Pos, r: Reg, want: &Type, what: &str) -> Result<(), Error> {
        let got = self.ty(r);
        if got == want {
            Ok(())
        } else {
            Err(Error::new(pos, format!("{what} must be {want}, found {got}")))
        }
    }

    // ---- statements ----

    fn block(&mut self, b: &Block) -> Result<(), Error> {
        self.scopes.push(HashMap::new());
        let r = b.stmts.iter().try_for_each(|s| self.stmt(s));
        self.scopes.pop();
        r
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), Error> {
        match s {
            Stmt::Let { pos, name, ty, value } => {
                let want = ty.as_ref().map(|t| self.env.resolve(t)).transpose()?;
                let v = self.expr(value, want.as_ref())?;
                if let Some(want) = &want {
                    self.expect(value.pos, v, want, &format!("the value of `{name}`"))?;
                }
                let vty = self.ty(v).clone();
                if vty == Type::Unit {
                    return Err(Error::new(*pos, format!("`{name}` would have type unit; this expression has no value")));
                }
                let dst = self.emit_to(vty, |dst| Inst::Copy { dst, src: v });
                self.scopes.last_mut().unwrap().insert(name.clone(), dst);
            }
            Stmt::Assign { target, value } => {
                let place = self.place(target)?;
                let want = place.ty().clone();
                let v = self.expr(value, Some(&want))?;
                self.expect(value.pos, v, &want, "the assigned value")?;
                self.write(target.pos, place, v)?;
            }
            Stmt::Return { pos, value } => {
                let ret = self.sig.ret.clone();
                let r = match value {
                    Some(e) => {
                        let r = self.expr(e, Some(&ret))?;
                        self.expect(e.pos, r, &ret, "the return value")?;
                        r
                    }
                    None if ret == Type::Unit => self.unit(),
                    None => return Err(Error::new(*pos, format!("missing return value of type {ret}"))),
                };
                self.terminate(Terminator::Ret(r));
                // Anything after this goes into an unreachable block.
                let dead = self.new_block();
                self.switch_to(dead);
            }
            Stmt::If { cond, then, els } => {
                let c = self.expr(cond, Some(&Type::Bool))?;
                self.expect(cond.pos, c, &Type::Bool, "an `if` condition")?;
                let then_bb = self.new_block();
                let end_bb = self.new_block();
                let else_bb = if els.is_some() { self.new_block() } else { end_bb };
                self.terminate(Terminator::Branch { cond: c, then: then_bb, els: else_bb });

                self.switch_to(then_bb);
                self.block(then)?;
                self.terminate(Terminator::Jump(end_bb));

                if let Some(els) = els {
                    self.switch_to(else_bb);
                    self.block(els)?;
                    self.terminate(Terminator::Jump(end_bb));
                }
                self.switch_to(end_bb);
            }
            Stmt::While { cond, body } => {
                let cond_bb = self.new_block();
                let body_bb = self.new_block();
                let end_bb = self.new_block();
                self.terminate(Terminator::Jump(cond_bb));

                self.switch_to(cond_bb);
                let c = self.expr(cond, Some(&Type::Bool))?;
                self.expect(cond.pos, c, &Type::Bool, "a `while` condition")?;
                self.terminate(Terminator::Branch { cond: c, then: body_bb, els: end_bb });

                self.switch_to(body_bb);
                self.block(body)?;
                self.terminate(Terminator::Jump(cond_bb));

                self.switch_to(end_bb);
            }
            Stmt::Expr(e) => {
                self.expr(e, None)?;
            }
        }
        Ok(())
    }

    // ---- expressions ----

    /// Lowers `e`. `expected` is only a hint for integer literals; callers still
    /// check the resulting type themselves.
    fn expr(&mut self, e: &Expr, expected: Option<&Type>) -> Result<Reg, Error> {
        Ok(match &e.kind {
            ExprKind::Int(n) => self.int_literal(e.pos, *n as i128, expected)?,
            ExprKind::Unary(UnOp::Neg, inner) if matches!(inner.kind, ExprKind::Int(_)) => {
                let ExprKind::Int(n) = inner.kind else { unreachable!() };
                self.int_literal(e.pos, -(n as i128), expected)?
            }
            ExprKind::Bool(b) => self.konst(Type::Bool, *b as i64),
            ExprKind::Str(s) => {
                let ty = types::str_literal(self.profile());
                self.emit_to(ty, |dst| Inst::Str { dst, value: s.clone() })
            }
            ExprKind::Var(name) => self.lookup(e.pos, name)?,
            ExprKind::Unary(op, inner) => {
                let (op, sym, hint) = match op {
                    UnOp::Neg => (ir::UnOp::Neg, "-", expected),
                    UnOp::Not => (ir::UnOp::Not, "!", Some(&Type::Bool)),
                };
                let src = self.expr(inner, hint)?;
                let ty = types::unary(op, self.ty(src)).ok_or_else(|| {
                    Error::new(e.pos, format!("cannot apply `{sym}` to {}", self.ty(src)))
                })?;
                self.emit_to(ty, |dst| Inst::Unary { dst, op, src })
            }
            ExprKind::Binary(BinOp::And, l, r) => self.short_circuit(true, l, r)?,
            ExprKind::Binary(BinOp::Or, l, r) => self.short_circuit(false, l, r)?,
            ExprKind::Binary(op, l, r) => self.binary(e.pos, *op, l, r, expected)?,
            ExprKind::Call(name, args) => self.call(e.pos, name, args)?,
            ExprKind::StructLit(name, inits) => self.struct_literal(e.pos, name, inits)?,
            ExprKind::ArrayLit(items) => self.array_literal(e.pos, items, expected)?,
            ExprKind::ArrayRepeat(value, n) => {
                let hint = match expected {
                    Some(Type::Array(elem, _)) => Some(&**elem),
                    _ => None,
                };
                let v = self.expr(value, hint)?;
                let ty = Type::array(self.ty(v).clone(), *n);
                self.emit_to(ty, |dst| Inst::Splat { dst, value: v })
            }
            ExprKind::SizeOf(t) | ExprKind::AlignOf(t) => {
                let ty = self.env.resolve(t)?;
                let l = self.env.layout(t.pos, &ty)?;
                let n = if matches!(e.kind, ExprKind::SizeOf(_)) { l.size } else { l.align };
                let n = i64::try_from(n).map_err(|_| Error::new(e.pos, format!("{ty} is too large")))?;
                self.konst(Type::I64, n)
            }
            ExprKind::Field(..) | ExprKind::Index(..) | ExprKind::Deref(_) => {
                let place = self.place(e)?;
                self.read(place)
            }
            ExprKind::AddrOf(inner) => {
                if self.profile() != Profile::Freestanding {
                    return Err(Error::new(e.pos, "`&` makes a pointer; pointers are only available in freestanding mode"));
                }
                let place = self.place(inner)?;
                self.addr_of(e.pos, place)?
            }
            ExprKind::Cast(inner, ty) => {
                let to = self.env.resolve(ty)?;
                let src = self.expr(inner, None)?;
                let from = self.ty(src).clone();
                if !types::can_cast(&from, &to) {
                    return Err(Error::new(e.pos, format!("cannot cast {from} to {to}")));
                }
                if from == to {
                    src
                } else {
                    self.emit_to(to, |dst| Inst::Cast { dst, src })
                }
            }
        })
    }

    fn int_literal(&mut self, pos: Pos, n: i128, expected: Option<&Type>) -> Result<Reg, Error> {
        let t = expected.and_then(Type::as_int).unwrap_or(IntTy::I64);
        if n < t.min() || n > t.max() {
            return Err(Error::new(pos, format!("integer literal {n} does not fit in {}", t.name())));
        }
        Ok(self.konst(Type::Int(t), n as i64))
    }

    fn binary(&mut self, pos: Pos, op: BinOp, l: &Expr, r: &Expr, expected: Option<&Type>) -> Result<Reg, Error> {
        let (op, sym) = match op {
            BinOp::Add => (ir::BinOp::Add, "+"),
            BinOp::Sub => (ir::BinOp::Sub, "-"),
            BinOp::Mul => (ir::BinOp::Mul, "*"),
            BinOp::Div => (ir::BinOp::Div, "/"),
            BinOp::Rem => (ir::BinOp::Rem, "%"),
            BinOp::Eq => (ir::BinOp::Eq, "=="),
            BinOp::Ne => (ir::BinOp::Ne, "!="),
            BinOp::Lt => (ir::BinOp::Lt, "<"),
            BinOp::Le => (ir::BinOp::Le, "<="),
            BinOp::Gt => (ir::BinOp::Gt, ">"),
            BinOp::Ge => (ir::BinOp::Ge, ">="),
            BinOp::And | BinOp::Or => unreachable!(),
        };
        let is_cmp = matches!(
            op,
            ir::BinOp::Eq | ir::BinOp::Ne | ir::BinOp::Lt | ir::BinOp::Le | ir::BinOp::Gt | ir::BinOp::Ge
        );
        // Arithmetic passes the expected type down; comparisons produce bool, so
        // their operands get no hint from outside.
        let hint = if is_cmp { None } else { expected };

        let (lhs, rhs) = if is_int_literal(l) && !is_int_literal(r) {
            // `1 + x`: type the literal after `x`. Literals have no side effects,
            // so evaluating the right side first is not observable.
            let rhs = self.expr(r, hint)?;
            let rt = self.ty(rhs).clone();
            (self.expr(l, Some(&rt))?, rhs)
        } else {
            let lhs = self.expr(l, hint)?;
            let rt = match self.ty(lhs) {
                Type::Ptr(_) => Type::I64, // pointer offsets are i64
                t => t.clone(),
            };
            (lhs, self.expr(r, Some(&rt))?)
        };

        let (lt, rt) = (self.ty(lhs).clone(), self.ty(rhs).clone());
        let ty = types::binary(op, &lt, &rt)
            .ok_or_else(|| Error::new(pos, format!("cannot apply `{sym}` to {lt} and {rt}")))?;
        Ok(self.emit_to(ty, |dst| Inst::Binary { dst, op, lhs, rhs }))
    }

    /// `a && b` / `a || b` on bools, evaluating `b` only when needed.
    fn short_circuit(&mut self, is_and: bool, l: &Expr, r: &Expr) -> Result<Reg, Error> {
        let sym = if is_and { "&&" } else { "||" };
        let lhs = self.expr(l, Some(&Type::Bool))?;
        self.expect(l.pos, lhs, &Type::Bool, &format!("the left side of `{sym}`"))?;
        // Result when the right side is skipped: false for `&&`, true for `||`.
        let res = self.konst(Type::Bool, (!is_and) as i64);
        let rhs_bb = self.new_block();
        let end_bb = self.new_block();
        let (then, els) = if is_and { (rhs_bb, end_bb) } else { (end_bb, rhs_bb) };
        self.terminate(Terminator::Branch { cond: lhs, then, els });

        self.switch_to(rhs_bb);
        let rhs = self.expr(r, Some(&Type::Bool))?;
        self.expect(r.pos, rhs, &Type::Bool, &format!("the right side of `{sym}`"))?;
        self.emit(Inst::Copy { dst: res, src: rhs });
        self.terminate(Terminator::Jump(end_bb));

        self.switch_to(end_bb);
        Ok(res)
    }

    /// `[a, b, c]`: the element type comes from the expected type if there is one,
    /// otherwise from the first element.
    fn array_literal(&mut self, pos: Pos, items: &[Expr], expected: Option<&Type>) -> Result<Reg, Error> {
        let mut elem = match expected {
            Some(Type::Array(elem, _)) => Some((**elem).clone()),
            _ => None,
        };
        let mut regs = Vec::with_capacity(items.len());
        for item in items {
            let r = self.expr(item, elem.as_ref())?;
            match &elem {
                Some(t) => self.expect(item.pos, r, t, "an array element")?,
                None => elem = Some(self.ty(r).clone()),
            }
            regs.push(r);
        }
        let Some(elem) = elem else {
            return Err(Error::new(pos, "cannot infer the element type of `[]`; give the variable a type"));
        };
        let ty = Type::array(elem, regs.len() as u64);
        Ok(self.emit_to(ty, |dst| Inst::Array { dst, items: regs }))
    }

    fn struct_literal(&mut self, pos: Pos, name: &str, inits: &[FieldInit]) -> Result<Reg, Error> {
        let env = self.env;
        let Some(fields) = env.structs.get(name) else {
            return Err(Error::new(pos, format!("unknown struct `{name}`")));
        };
        let mut values: Vec<Option<Reg>> = vec![None; fields.len()];
        // Evaluate in source order, store in declaration order.
        for init in inits {
            let (index, ty) = env.field(init.pos, &Type::Struct(name.to_string()), &init.name)?;
            if values[index as usize].is_some() {
                return Err(Error::new(init.pos, format!("field `{}` is given twice", init.name)));
            }
            let v = self.expr(&init.value, Some(&ty))?;
            self.expect(init.value.pos, v, &ty, &format!("field `{}`", init.name))?;
            values[index as usize] = Some(v);
        }
        let missing: Vec<&str> = fields
            .iter()
            .zip(&values)
            .filter(|(_, v)| v.is_none())
            .map(|((n, _), _)| n.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(Error::new(pos, format!("missing fields in `{name}`: {}", missing.join(", "))));
        }
        let fields = values.into_iter().map(Option::unwrap).collect();
        Ok(self.emit_to(Type::Struct(name.to_string()), |dst| Inst::Struct {
            dst,
            name: name.to_string(),
            fields,
        }))
    }

    fn call(&mut self, pos: Pos, name: &str, args: &[Expr]) -> Result<Reg, Error> {
        match name {
            "print" => {
                if self.profile() != Profile::Hosted {
                    return Err(Error::new(pos, "`print` needs std and is not available in freestanding mode"));
                }
                let [arg] = args else {
                    return Err(Error::new(pos, "`print` takes exactly 1 argument"));
                };
                let r = self.expr(arg, None)?;
                if !self.ty(r).is_printable() {
                    return Err(Error::new(arg.pos, format!("cannot print a value of type {}", self.ty(r))));
                }
                self.emit(Inst::Print { src: r });
                Ok(self.unit())
            }
            "len" => {
                let [arg] = args else {
                    return Err(Error::new(pos, "`len` takes exactly 1 argument"));
                };
                let r = self.expr(arg, None)?;
                let n = match self.ty(r) {
                    Type::Array(_, n) => *n,
                    Type::Ptr(inner) if matches!(**inner, Type::Array(..)) => {
                        let Type::Array(_, n) = **inner else { unreachable!() };
                        n
                    }
                    t => return Err(Error::new(arg.pos, format!("`len` needs an array, found {t}"))),
                };
                let n = i64::try_from(n).map_err(|_| Error::new(arg.pos, "array is too long"))?;
                Ok(self.konst(Type::I64, n))
            }
            "syscall" => {
                if self.profile() != Profile::Freestanding {
                    return Err(Error::new(pos, "`syscall` is only available in freestanding mode"));
                }
                if args.is_empty() || args.len() > 7 {
                    return Err(Error::new(pos, "`syscall` takes 1 to 7 arguments"));
                }
                let mut regs = Vec::with_capacity(args.len());
                for a in args {
                    let r = self.expr(a, None)?;
                    if !self.ty(r).is_syscall_arg() {
                        return Err(Error::new(
                            a.pos,
                            format!("syscall arguments must be integers or pointers, found {}", self.ty(r)),
                        ));
                    }
                    regs.push(r);
                }
                Ok(self.emit_to(Type::I64, |dst| Inst::Syscall { dst, args: regs }))
            }
            _ => {
                let env = self.env;
                let sig = env
                    .sigs
                    .get(name)
                    .ok_or_else(|| Error::new(pos, format!("unknown function `{name}`")))?;
                if sig.params.len() != args.len() {
                    return Err(Error::new(
                        pos,
                        format!("`{name}` takes {} arguments, {} given", sig.params.len(), args.len()),
                    ));
                }
                let mut regs = Vec::with_capacity(args.len());
                for (i, (a, want)) in args.iter().zip(&sig.params).enumerate() {
                    let r = self.expr(a, Some(want))?;
                    self.expect(a.pos, r, want, &format!("argument {} of `{name}`", i + 1))?;
                    regs.push(r);
                }
                let ret = sig.ret.clone();
                Ok(self.emit_to(ret, |dst| Inst::Call { dst, func: name.to_string(), args: regs }))
            }
        }
    }
}

#[cfg(test)]
mod tests;
