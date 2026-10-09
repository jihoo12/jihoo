//! Tasks and channels: `go f(x)`, `chan(T, n)`, `send`, `recv` and `select`.
//!
//! They need the VM's scheduler (`crates/jihoo-vm/src/lib.rs`), so they are
//! hosted only. A task shares nothing with the one that started it except what
//! it is given: arguments are copied, closures capture by value, `ref`s are
//! immutable. Channels are the only way for tasks to communicate.

use jihoo_ir::{BinOp, Inst, Profile, Reg, SelectCase, Terminator, Type};
use jihoo_syntax::ast::{Expr, ExprKind, SelectArm, SelectOp, TypeExpr};
use jihoo_syntax::{Error, Pos};

use crate::FnCx;

impl FnCx<'_> {
    fn check_hosted(&self, pos: Pos, what: &str) -> Result<(), Error> {
        if self.profile() == Profile::Hosted || self.in_macro {
            Ok(())
        } else {
            Err(Error::new(pos, format!("{what} need the VM's scheduler and are only available in hosted mode")))
        }
    }

    /// `go f(x, y)`: evaluates the function and the arguments here, then runs
    /// the call in a new task.
    pub(crate) fn go_stmt(&mut self, pos: Pos, call: &Expr) -> Result<(), Error> {
        self.check_hosted(pos, "tasks (`go`)")?;
        let (callee, args, what) = match &call.kind {
            ExprKind::Call(name, args) => {
                // `s.f(x)` with a local `s` calls a function stored in a field.
                let f = match name.split_once('.') {
                    Some((base, field)) if self.local(base).is_some() => {
                        let base = Expr { pos: call.pos, kind: ExprKind::Var(base.to_string()) };
                        ExprKind::Field(Box::new(base), field.to_string())
                    }
                    _ => ExprKind::Var(name.clone()),
                };
                (self.expr(&Expr { pos: call.pos, kind: f }, None)?, args, format!("`{name}`"))
            }
            ExprKind::CallExpr(f, args) => (self.expr(f, None)?, args, "this function".to_string()),
            _ => unreachable!("the parser only accepts calls after `go`"),
        };
        let Type::Fn(params, _) = self.ty(callee).clone() else {
            return Err(Error::new(call.pos, format!("{what} is not a function; it has type {}", self.ty(callee))));
        };
        let args = self.call_args(call.pos, &what, &params, args)?;
        self.emit(Inst::Spawn { callee, args });
        Ok(())
    }

    /// `select { ... }`: evaluates every channel and value to send, in order,
    /// then lets the VM pick the case that goes ahead, and runs its arm.
    pub(crate) fn select_stmt(&mut self, pos: Pos, arms: &[SelectArm]) -> Result<(), Error> {
        self.check_hosted(pos, "`select` statements")?;
        if arms.is_empty() {
            return Err(Error::new(pos, "a `select` needs at least one arm"));
        }
        let mut cases = Vec::new();
        let mut default = false;
        for arm in arms {
            match &arm.op {
                SelectOp::Recv { chan, .. } => {
                    let c = self.channel(chan, "recv")?;
                    let Type::Chan(t) = self.ty(c).clone() else { unreachable!() };
                    let dst = self.new_reg(*t);
                    cases.push(SelectCase::Recv { dst, chan: c });
                }
                SelectOp::Send { chan, value } => {
                    let c = self.channel(chan, "send")?;
                    let Type::Chan(t) = self.ty(c).clone() else { unreachable!() };
                    let v = self.expr(value, Some(&t))?;
                    self.expect(value.pos, v, &t, "the value sent")?;
                    cases.push(SelectCase::Send { chan: c, value: v });
                }
                SelectOp::Default if default => {
                    return Err(Error::new(arm.pos, "a `select` has at most one `_` arm"));
                }
                SelectOp::Default => default = true,
            }
        }

        // Run the arm of the case the VM picked; `_` is the index after the cases.
        let n = cases.len() as i64;
        let received: Vec<Option<Reg>> = cases
            .iter()
            .map(|c| match c {
                SelectCase::Recv { dst, .. } => Some(*dst),
                SelectCase::Send { .. } => None,
            })
            .collect();
        let picked = self.emit_to(Type::I64, |dst| Inst::Select { dst, cases, default });
        let end = self.new_block();
        let mut next_case = 0;
        for arm in arms {
            let index = match arm.op {
                SelectOp::Default => n,
                _ => {
                    next_case += 1;
                    next_case - 1
                }
            };
            let k = self.konst(Type::I64, index);
            let cond = self.emit_to(Type::Bool, |dst| Inst::Binary { dst, op: BinOp::Eq, lhs: picked, rhs: k });
            let body = self.new_block();
            let next = self.new_block();
            self.terminate(Terminator::Branch { cond, then: body, els: next });
            self.switch_to(body);
            self.scopes.push(Default::default());
            if let SelectOp::Recv { bind: Some(name), .. } = &arm.op {
                let r = received[index as usize].unwrap();
                self.scopes.last_mut().unwrap().insert(name.clone(), r);
            }
            let r = self.block(&arm.body);
            self.scopes.pop();
            r?;
            self.terminate(Terminator::Jump(end));
            self.switch_to(next);
        }
        self.terminate(Terminator::Unreachable);
        self.switch_to(end);
        Ok(())
    }

    /// Evaluates `e`, which must be a channel; `op` names the operation.
    fn channel(&mut self, e: &Expr, op: &str) -> Result<Reg, Error> {
        let c = self.expr(e, None)?;
        if !matches!(self.ty(c), Type::Chan(_)) {
            return Err(Error::new(e.pos, format!("`{op}` needs a channel, found {}", self.ty(c))));
        }
        Ok(c)
    }

    /// `chan(T)` or `chan(T, cap)`.
    pub(crate) fn new_chan(&mut self, pos: Pos, t: &TypeExpr, cap: Option<&Expr>) -> Result<Reg, Error> {
        self.check_hosted(pos, "channels")?;
        let elem = self.resolve(t)?;
        let cap = match cap {
            Some(c) => {
                let r = self.expr(c, Some(&Type::I64))?;
                self.expect(c.pos, r, &Type::I64, "the capacity of a channel")?;
                r
            }
            None => self.konst(Type::I64, 0),
        };
        Ok(self.emit_to(Type::Chan(Box::new(elem)), |dst| Inst::NewChan { dst, cap }))
    }
}
