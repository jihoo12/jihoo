//! Semantic analysis: type checks the AST and lowers it to JIR in one pass.
//!
//! Types: function signatures are written out, local variable types are inferred
//! from their initializers. The operator typing rules are shared with the IR
//! verifier (`jihoo_ir::types`), so well-typed programs always produce valid IR.
//!
//! Errors are collected per function: one error stops the current function, but
//! the remaining functions are still checked.

use std::collections::{HashMap, VecDeque};

use jihoo_ir as ir;
use jihoo_ir::types;
use jihoo_ir::{BlockId, Inst, Profile, Reg, Terminator, Type};
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

    let mut sigs = HashMap::new();
    for f in &prog.funcs {
        match signature(profile, f) {
            Ok(sig) => {
                if is_builtin(&f.name) {
                    errors.push(Error::new(f.pos, format!("`{}` is a builtin and cannot be redefined", f.name)));
                } else if sigs.insert(f.name.clone(), sig).is_some() {
                    errors.push(Error::new(f.pos, format!("function `{}` is defined twice", f.name)));
                }
            }
            Err(e) => errors.push(e),
        }
    }
    if let Err(e) = check_entry(profile, prog, &sigs) {
        errors.push(e);
    }

    let mut funcs = Vec::new();
    for f in &prog.funcs {
        let Some(sig) = sigs.get(&f.name) else { continue };
        match FnCx::new(profile, &sigs, sig).lower_fn(f) {
            Ok(func) => funcs.push(func),
            Err(e) => errors.push(e),
        }
    }

    if errors.is_empty() {
        Ok(ir::Module { profile, funcs })
    } else {
        Err(errors)
    }
}

#[derive(Debug, Clone)]
struct Sig {
    params: Vec<Type>,
    ret: Type,
}

fn is_builtin(name: &str) -> bool {
    matches!(name, "print" | "syscall")
}

fn resolve(profile: Profile, t: &TypeExpr) -> Result<Type, Error> {
    let ty = Type::from_name(&t.name)
        .ok_or_else(|| Error::new(t.pos, format!("unknown type `{}`", t.name)))?;
    if !ty.available_in(profile) {
        let why = match ty {
            Type::Str => "is garbage collected and is not available in freestanding mode (use `ptr`)",
            Type::Ptr => "is only available in freestanding mode",
            _ => "is not available here",
        };
        return Err(Error::new(t.pos, format!("type `{ty}` {why}")));
    }
    Ok(ty)
}

fn signature(profile: Profile, f: &FnDecl) -> Result<Sig, Error> {
    let params = f.params.iter().map(|p| resolve(profile, &p.ty)).collect::<Result<_, _>>()?;
    let ret = match &f.ret {
        Some(t) => resolve(profile, t)?,
        None => Type::Unit,
    };
    Ok(Sig { params, ret })
}

fn check_entry(profile: Profile, prog: &Program, sigs: &HashMap<String, Sig>) -> Result<(), Error> {
    let entry = profile.entry();
    let Some(decl) = prog.funcs.iter().find(|f| f.name == entry) else {
        let pos = Pos { line: 1, col: 1 };
        return Err(Error::new(pos, format!("{} program needs `fn {entry}()`", profile.as_str())));
    };
    match sigs.get(entry) {
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

struct BlockBuf {
    insts: Vec<Inst>,
    term: Option<Terminator>,
}

struct FnCx<'a> {
    profile: Profile,
    sigs: &'a HashMap<String, Sig>,
    sig: &'a Sig,
    blocks: Vec<BlockBuf>,
    cur: BlockId,
    regs: Vec<Type>,
    scopes: Vec<HashMap<String, Reg>>,
}

impl<'a> FnCx<'a> {
    fn new(profile: Profile, sigs: &'a HashMap<String, Sig>, sig: &'a Sig) -> Self {
        FnCx {
            profile,
            sigs,
            sig,
            blocks: vec![BlockBuf { insts: vec![], term: None }],
            cur: BlockId(0),
            regs: Vec::new(),
            scopes: vec![HashMap::new()],
        }
    }

    fn lower_fn(mut self, f: &FnDecl) -> Result<ir::Function, Error> {
        for (p, &ty) in f.params.iter().zip(&self.sig.params) {
            let r = self.new_reg(ty);
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
            ret: self.sig.ret,
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

    fn ty(&self, r: Reg) -> Type {
        self.regs[r.0 as usize]
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

    /// Terminates the current block. No-op if it already ended (e.g. after `return`).
    fn terminate(&mut self, t: Terminator) {
        let b = &mut self.blocks[self.cur.0 as usize];
        if b.term.is_none() {
            b.term = Some(t);
        }
    }

    fn konst(&mut self, ty: Type, value: i64) -> Reg {
        let dst = self.new_reg(ty);
        self.emit(Inst::Const { dst, value });
        dst
    }

    fn unit(&mut self) -> Reg {
        let dst = self.new_reg(Type::Unit);
        self.emit(Inst::Unit { dst });
        dst
    }

    fn lookup(&self, pos: Pos, name: &str) -> Result<Reg, Error> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
            .ok_or_else(|| Error::new(pos, format!("unknown variable `{name}`")))
    }

    fn expect(&self, pos: Pos, r: Reg, want: Type, what: &str) -> Result<(), Error> {
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
                let v = self.expr(value)?;
                let vty = self.ty(v);
                if let Some(t) = ty {
                    let want = resolve(self.profile, t)?;
                    self.expect(value.pos, v, want, &format!("the value of `{name}`"))?;
                }
                if vty == Type::Unit {
                    return Err(Error::new(*pos, format!("`{name}` would have type unit; this expression has no value")));
                }
                let dst = self.new_reg(vty);
                self.emit(Inst::Copy { dst, src: v });
                self.scopes.last_mut().unwrap().insert(name.clone(), dst);
            }
            Stmt::Assign { pos, name, value } => {
                let dst = self.lookup(*pos, name)?;
                let v = self.expr(value)?;
                let want = self.ty(dst);
                self.expect(value.pos, v, want, &format!("a value assigned to `{name}`"))?;
                self.emit(Inst::Copy { dst, src: v });
            }
            Stmt::Return { pos, value } => {
                let ret = self.sig.ret;
                let r = match value {
                    Some(e) => {
                        let r = self.expr(e)?;
                        self.expect(e.pos, r, ret, "the return value")?;
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
                let c = self.expr(cond)?;
                self.expect(cond.pos, c, Type::Bool, "an `if` condition")?;
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
                let c = self.expr(cond)?;
                self.expect(cond.pos, c, Type::Bool, "a `while` condition")?;
                self.terminate(Terminator::Branch { cond: c, then: body_bb, els: end_bb });

                self.switch_to(body_bb);
                self.block(body)?;
                self.terminate(Terminator::Jump(cond_bb));

                self.switch_to(end_bb);
            }
            Stmt::Expr(e) => {
                self.expr(e)?;
            }
        }
        Ok(())
    }

    // ---- expressions ----

    fn expr(&mut self, e: &Expr) -> Result<Reg, Error> {
        Ok(match &e.kind {
            ExprKind::Int(n) => self.konst(Type::I64, *n),
            ExprKind::Bool(b) => self.konst(Type::Bool, *b as i64),
            ExprKind::Str(s) => {
                let dst = self.new_reg(types::str_literal(self.profile));
                self.emit(Inst::Str { dst, value: s.clone() });
                dst
            }
            ExprKind::Var(name) => self.lookup(e.pos, name)?,
            ExprKind::Unary(op, inner) => {
                let src = self.expr(inner)?;
                let (op, sym) = match op {
                    UnOp::Neg => (ir::UnOp::Neg, "-"),
                    UnOp::Not => (ir::UnOp::Not, "!"),
                };
                let ty = types::unary(op, self.ty(src)).ok_or_else(|| {
                    Error::new(e.pos, format!("cannot apply `{sym}` to {}", self.ty(src)))
                })?;
                let dst = self.new_reg(ty);
                self.emit(Inst::Unary { dst, op, src });
                dst
            }
            ExprKind::Binary(BinOp::And, l, r) => self.short_circuit(true, l, r)?,
            ExprKind::Binary(BinOp::Or, l, r) => self.short_circuit(false, l, r)?,
            ExprKind::Binary(op, l, r) => {
                let lhs = self.expr(l)?;
                let rhs = self.expr(r)?;
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
                let (lt, rt) = (self.ty(lhs), self.ty(rhs));
                let ty = types::binary(op, lt, rt).ok_or_else(|| {
                    Error::new(e.pos, format!("cannot apply `{sym}` to {lt} and {rt}"))
                })?;
                let dst = self.new_reg(ty);
                self.emit(Inst::Binary { dst, op, lhs, rhs });
                dst
            }
            ExprKind::Call(name, args) => self.call(e.pos, name, args)?,
        })
    }

    /// `a && b` / `a || b` on bools, evaluating `b` only when needed.
    fn short_circuit(&mut self, is_and: bool, l: &Expr, r: &Expr) -> Result<Reg, Error> {
        let sym = if is_and { "&&" } else { "||" };
        let lhs = self.expr(l)?;
        self.expect(l.pos, lhs, Type::Bool, &format!("the left side of `{sym}`"))?;
        // Result when the right side is skipped: false for `&&`, true for `||`.
        let res = self.konst(Type::Bool, (!is_and) as i64);
        let rhs_bb = self.new_block();
        let end_bb = self.new_block();
        let (then, els) = if is_and { (rhs_bb, end_bb) } else { (end_bb, rhs_bb) };
        self.terminate(Terminator::Branch { cond: lhs, then, els });

        self.switch_to(rhs_bb);
        let rhs = self.expr(r)?;
        self.expect(r.pos, rhs, Type::Bool, &format!("the right side of `{sym}`"))?;
        self.emit(Inst::Copy { dst: res, src: rhs });
        self.terminate(Terminator::Jump(end_bb));

        self.switch_to(end_bb);
        Ok(res)
    }

    fn call(&mut self, pos: Pos, name: &str, args: &[Expr]) -> Result<Reg, Error> {
        let mut regs = Vec::with_capacity(args.len());
        for a in args {
            regs.push(self.expr(a)?);
        }
        match name {
            "print" => {
                if self.profile != Profile::Hosted {
                    return Err(Error::new(pos, "`print` needs std and is not available in freestanding mode"));
                }
                if regs.len() != 1 {
                    return Err(Error::new(pos, "`print` takes exactly 1 argument"));
                }
                if !self.ty(regs[0]).is_printable() {
                    return Err(Error::new(args[0].pos, format!("cannot print a value of type {}", self.ty(regs[0]))));
                }
                self.emit(Inst::Print { src: regs[0] });
                Ok(self.unit())
            }
            "syscall" => {
                if self.profile != Profile::Freestanding {
                    return Err(Error::new(pos, "`syscall` is only available in freestanding mode"));
                }
                if regs.is_empty() || regs.len() > 7 {
                    return Err(Error::new(pos, "`syscall` takes 1 to 7 arguments"));
                }
                for (a, &r) in args.iter().zip(&regs) {
                    if !self.ty(r).is_syscall_arg() {
                        return Err(Error::new(
                            a.pos,
                            format!("syscall arguments must be i64 or ptr, found {}", self.ty(r)),
                        ));
                    }
                }
                let dst = self.new_reg(Type::I64);
                self.emit(Inst::Syscall { dst, args: regs });
                Ok(dst)
            }
            _ => {
                let sig = self
                    .sigs
                    .get(name)
                    .ok_or_else(|| Error::new(pos, format!("unknown function `{name}`")))?;
                if sig.params.len() != regs.len() {
                    return Err(Error::new(
                        pos,
                        format!("`{name}` takes {} arguments, {} given", sig.params.len(), regs.len()),
                    ));
                }
                for (i, ((a, &r), &want)) in args.iter().zip(&regs).zip(&sig.params).enumerate() {
                    self.expect(a.pos, r, want, &format!("argument {} of `{name}`", i + 1))?;
                }
                let dst = self.new_reg(sig.ret);
                self.emit(Inst::Call { dst, func: name.to_string(), args: regs });
                Ok(dst)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(src: &str) -> Result<ir::Module, String> {
        let prog = jihoo_syntax::parse(src).map_err(|e| e.to_string())?;
        let m = analyze(&prog).map_err(|es| es[0].to_string())?;
        ir::verify(&m).expect("sema produced invalid IR");
        Ok(m)
    }

    fn err(src: &str) -> String {
        check(src).expect_err("expected a type error")
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
        assert!(err("fn main() { print(!1) }").contains("cannot apply `!` to i64"));
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
        assert!(err("#![freestanding]\nfn f(s: str) {}\nfn _start() {}").contains("garbage collected"));
        assert!(err("fn f(p: ptr) {}\nfn main() {}").contains("only available in freestanding"));
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
}
