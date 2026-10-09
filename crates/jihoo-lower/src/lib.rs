//! AST -> JIR lowering.
//!
//! TODO: no type checking yet; we only check that type names are known.

use std::collections::HashMap;

use jihoo_ir as ir;
use jihoo_ir::{BlockId, Inst, Reg, Terminator};
use jihoo_syntax::ast::*;
use jihoo_syntax::{Error, Pos};

const KNOWN_TYPES: &[&str] = &["i64", "bool", "str", "ptr"];

pub fn lower(prog: &Program) -> Result<ir::Module, Error> {
    let mut profile = ir::Profile::Hosted;
    for (pos, attr) in &prog.attrs {
        match attr.as_str() {
            "freestanding" => profile = ir::Profile::Freestanding,
            other => return Err(Error::new(*pos, format!("unknown attribute `#![{other}]`"))),
        }
    }

    let mut sigs = HashMap::new();
    for f in &prog.funcs {
        if is_builtin(&f.name) {
            return Err(Error::new(f.pos, format!("`{}` is a builtin and cannot be redefined", f.name)));
        }
        if sigs.insert(f.name.clone(), f.params.len()).is_some() {
            return Err(Error::new(f.pos, format!("function `{}` is defined twice", f.name)));
        }
    }

    let funcs = prog
        .funcs
        .iter()
        .map(|f| FnCx::new(&sigs).lower_fn(f))
        .collect::<Result<_, _>>()?;
    Ok(ir::Module { profile, funcs })
}

fn is_builtin(name: &str) -> bool {
    matches!(name, "print" | "syscall")
}

fn check_type(t: &TypeExpr) -> Result<(), Error> {
    if KNOWN_TYPES.contains(&t.name.as_str()) {
        Ok(())
    } else {
        Err(Error::new(t.pos, format!("unknown type `{}`", t.name)))
    }
}

struct BlockBuf {
    insts: Vec<Inst>,
    term: Option<Terminator>,
}

struct FnCx<'a> {
    sigs: &'a HashMap<String, usize>,
    blocks: Vec<BlockBuf>,
    cur: BlockId,
    num_regs: u32,
    scopes: Vec<HashMap<String, Reg>>,
}

impl<'a> FnCx<'a> {
    fn new(sigs: &'a HashMap<String, usize>) -> Self {
        FnCx {
            sigs,
            blocks: vec![BlockBuf { insts: vec![], term: None }],
            cur: BlockId(0),
            num_regs: 0,
            scopes: vec![HashMap::new()],
        }
    }

    fn lower_fn(mut self, f: &FnDecl) -> Result<ir::Function, Error> {
        for p in &f.params {
            check_type(&p.ty)?;
            let r = self.new_reg();
            if self.scopes[0].insert(p.name.clone(), r).is_some() {
                return Err(Error::new(p.pos, format!("duplicate parameter `{}`", p.name)));
            }
        }
        if let Some(t) = &f.ret {
            check_type(t)?;
        }

        self.block(&f.body)?;

        // Blocks that fall off the end without `return` return 0.
        let mut blocks = Vec::with_capacity(self.blocks.len());
        for i in 0..self.blocks.len() {
            if self.blocks[i].term.is_none() {
                let zero = self.new_reg();
                let b = &mut self.blocks[i];
                b.insts.push(Inst::Const { dst: zero, value: 0 });
                b.term = Some(Terminator::Ret(zero));
            }
        }
        for b in self.blocks {
            blocks.push(ir::Block { insts: b.insts, term: b.term.unwrap() });
        }

        Ok(ir::Function {
            name: f.name.clone(),
            params: f.params.len() as u32,
            num_regs: self.num_regs,
            blocks,
        })
    }

    // ---- builder ----

    fn new_reg(&mut self) -> Reg {
        self.num_regs += 1;
        Reg(self.num_regs - 1)
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

    fn konst(&mut self, value: i64) -> Reg {
        let dst = self.new_reg();
        self.emit(Inst::Const { dst, value });
        dst
    }

    fn lookup(&self, pos: Pos, name: &str) -> Result<Reg, Error> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name).copied())
            .ok_or_else(|| Error::new(pos, format!("unknown variable `{name}`")))
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
            Stmt::Let { name, ty, value, .. } => {
                if let Some(t) = ty {
                    check_type(t)?;
                }
                let v = self.expr(value)?;
                let dst = self.new_reg();
                self.emit(Inst::Copy { dst, src: v });
                self.scopes.last_mut().unwrap().insert(name.clone(), dst);
            }
            Stmt::Assign { pos, name, value } => {
                let dst = self.lookup(*pos, name)?;
                let v = self.expr(value)?;
                self.emit(Inst::Copy { dst, src: v });
            }
            Stmt::Return { value, .. } => {
                let r = match value {
                    Some(e) => self.expr(e)?,
                    None => self.konst(0),
                };
                self.terminate(Terminator::Ret(r));
                // Anything after this goes into an unreachable block.
                let dead = self.new_block();
                self.switch_to(dead);
            }
            Stmt::If { cond, then, els } => {
                let c = self.expr(cond)?;
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
            ExprKind::Int(n) => self.konst(*n),
            ExprKind::Bool(b) => self.konst(*b as i64),
            ExprKind::Str(s) => {
                let dst = self.new_reg();
                self.emit(Inst::Str { dst, value: s.clone() });
                dst
            }
            ExprKind::Var(name) => self.lookup(e.pos, name)?,
            ExprKind::Unary(op, inner) => {
                let src = self.expr(inner)?;
                let dst = self.new_reg();
                let op = match op {
                    UnOp::Neg => ir::UnOp::Neg,
                    UnOp::Not => ir::UnOp::Not,
                };
                self.emit(Inst::Unary { dst, op, src });
                dst
            }
            ExprKind::Binary(BinOp::And, l, r) => self.short_circuit(true, l, r)?,
            ExprKind::Binary(BinOp::Or, l, r) => self.short_circuit(false, l, r)?,
            ExprKind::Binary(op, l, r) => {
                let lhs = self.expr(l)?;
                let rhs = self.expr(r)?;
                let dst = self.new_reg();
                let op = match op {
                    BinOp::Add => ir::BinOp::Add,
                    BinOp::Sub => ir::BinOp::Sub,
                    BinOp::Mul => ir::BinOp::Mul,
                    BinOp::Div => ir::BinOp::Div,
                    BinOp::Rem => ir::BinOp::Rem,
                    BinOp::Eq => ir::BinOp::Eq,
                    BinOp::Ne => ir::BinOp::Ne,
                    BinOp::Lt => ir::BinOp::Lt,
                    BinOp::Le => ir::BinOp::Le,
                    BinOp::Gt => ir::BinOp::Gt,
                    BinOp::Ge => ir::BinOp::Ge,
                    BinOp::And | BinOp::Or => unreachable!(),
                };
                self.emit(Inst::Binary { dst, op, lhs, rhs });
                dst
            }
            ExprKind::Call(name, args) => self.call(e.pos, name, args)?,
        })
    }

    /// `a && b` / `a || b`. The result is always 0 or 1.
    fn short_circuit(&mut self, is_and: bool, l: &Expr, r: &Expr) -> Result<Reg, Error> {
        let res = self.new_reg();
        let lhs = self.expr(l)?;
        // Result when the right side is skipped: 0 for `&&`, 1 for `||`.
        self.emit(Inst::Const { dst: res, value: (!is_and) as i64 });
        let rhs_bb = self.new_block();
        let end_bb = self.new_block();
        let (then, els) = if is_and { (rhs_bb, end_bb) } else { (end_bb, rhs_bb) };
        self.terminate(Terminator::Branch { cond: lhs, then, els });

        self.switch_to(rhs_bb);
        let rhs = self.expr(r)?;
        let zero = self.konst(0);
        self.emit(Inst::Binary { dst: res, op: ir::BinOp::Ne, lhs: rhs, rhs: zero });
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
                if regs.len() != 1 {
                    return Err(Error::new(pos, "`print` takes exactly 1 argument"));
                }
                self.emit(Inst::Print { src: regs[0] });
                Ok(self.konst(0))
            }
            "syscall" => {
                if regs.is_empty() || regs.len() > 7 {
                    return Err(Error::new(pos, "`syscall` takes 1 to 7 arguments"));
                }
                let dst = self.new_reg();
                self.emit(Inst::Syscall { dst, args: regs });
                Ok(dst)
            }
            _ => {
                let &n = self
                    .sigs
                    .get(name)
                    .ok_or_else(|| Error::new(pos, format!("unknown function `{name}`")))?;
                if n != regs.len() {
                    return Err(Error::new(
                        pos,
                        format!("`{name}` takes {n} arguments, {} given", regs.len()),
                    ));
                }
                let dst = self.new_reg();
                self.emit(Inst::Call { dst, func: name.to_string(), args: regs });
                Ok(dst)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower_src(src: &str) -> ir::Module {
        let m = lower(&jihoo_syntax::parse(src).unwrap()).unwrap();
        ir::verify(&m).unwrap();
        m
    }

    #[test]
    fn freestanding_attr_sets_profile() {
        let m = lower_src("#![freestanding]\nfn _start() { syscall(60, 0) }");
        assert_eq!(m.profile, ir::Profile::Freestanding);
    }

    #[test]
    fn print_is_rejected_in_freestanding() {
        let m = lower(&jihoo_syntax::parse("#![freestanding]\nfn _start() { print(1) }").unwrap())
            .unwrap();
        assert!(ir::verify(&m).unwrap_err().contains("freestanding"));
    }

    #[test]
    fn text_format_snapshot() {
        let m = lower_src("fn main() -> i64 {\n  return 1 + 2\n}");
        let text = m.to_string();
        assert!(text.contains("fn @main params 0 regs"));
        assert!(text.contains("= add %0, %1"));
    }
}
