//! Tasks and channels: `go f(x)`, `chan(T, n)`, `send`, `recv` and `select`.
//!
//! On the VM they run on its scheduler (`crates/jihoo-vm/src/lib.rs`).
//! Natively `go` starts a thread of the C library's, and channels and
//! `select` run on `lib/channel.jh`, which the loader adds to a native program
//! that uses them (`backend-llvm/src/codegen.cpp` calls it). A task shares
//! nothing with the one that started it except what it is given: arguments
//! are copied, closures capture by value, `ref`s are immutable. Channels are
//! the only way for tasks to communicate; natively, a cell never goes through
//! one, since the receiving thread would share it.

use jihoo_ir::{BinOp, Inst, Profile, Reg, SelectCase, Terminator, Type};
use jihoo_syntax::ast::{Expr, ExprKind, SelectArm, SelectOp, TypeExpr};
use jihoo_syntax::{Error, Pos};

use crate::FnCx;

impl FnCx<'_> {
    fn check_tasks(&self, pos: Pos, what: &str) -> Result<(), Error> {
        if self.profile() != Profile::Freestanding || self.in_macro {
            Ok(())
        } else {
            Err(Error::new(pos, format!("{what} need the VM's scheduler or the C library's threads: not freestanding")))
        }
    }

    /// `go f(x, y)`: evaluates the function and the arguments here, then runs
    /// the call in a new task on the VM, or natively in a new thread of the C
    /// library's.
    pub(crate) fn go_stmt(&mut self, pos: Pos, call: &Expr) -> Result<(), Error> {
        if self.profile() == Profile::Freestanding && !self.in_macro {
            let msg = "`go` needs the VM's scheduler or the C library's threads: hosted and native only";
            return Err(Error::new(pos, msg));
        }
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
        // A thread shares nothing with the one that started it: a cell would
        // be changed by both at once. (A closure that captured one stops the
        // program when started; its type does not say what it captured.)
        if self.profile() == Profile::Native {
            for (p, a) in params.iter().zip(args) {
                if self.env.has_cell(p) {
                    let msg = format!("a thread cannot share a cell, and this is {p}; send values over a channel instead");
                    return Err(Error::new(a.pos, msg));
                }
            }
        }
        let args = self.call_args(call.pos, &what, &params, args)?;
        self.emit(Inst::Spawn { callee, args });
        Ok(())
    }

    /// `select { ... }`: evaluates every channel and value to send, in order,
    /// then lets the VM pick the case that goes ahead, and runs its arm.
    pub(crate) fn select_stmt(&mut self, pos: Pos, arms: &[SelectArm]) -> Result<(), Error> {
        self.check_tasks(pos, "`select` statements")?;
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
        if self.profile() == Profile::Native && cases.len() > 16 {
            return Err(Error::new(pos, "a native `select` has at most 16 cases (`channel.MAX_CASES`)"));
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
        self.check_tasks(pos, "channels")?;
        let elem = self.resolve(t)?;
        if self.profile() == Profile::Native && self.env.has_cell(&elem) {
            let msg = format!("a channel cannot carry a cell, and this is {elem}: the receiving thread would share it");
            return Err(Error::new(t.pos, msg));
        }
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

impl FnCx<'_> {
    /// `send(c, v)`: sends `v` on channel `c`, waiting while it is full.
    pub(crate) fn chan_send(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        let [c, v] = args else {
            return Err(Error::new(pos, "`send` takes 2 arguments: a channel and a value"));
        };
        let chan = self.expr(c, None)?;
        let Type::Chan(t) = self.ty(chan).clone() else {
            return Err(Error::new(c.pos, format!("`send` needs a channel, found {}", self.ty(chan))));
        };
        let value = self.expr(v, Some(&t))?;
        self.expect(v.pos, value, &t, "the value sent")?;
        self.emit(Inst::Send { chan, value });
        Ok(self.unit())
    }

    /// `recv(c)`: receives a value from channel `c`, waiting until there is one.
    pub(crate) fn chan_recv(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        let [c] = args else {
            return Err(Error::new(pos, "`recv` takes 1 argument: a channel"));
        };
        let chan = self.expr(c, None)?;
        let Type::Chan(t) = self.ty(chan).clone() else {
            return Err(Error::new(c.pos, format!("`recv` needs a channel, found {}", self.ty(chan))));
        };
        Ok(self.emit_to(*t, |dst| Inst::Recv { dst, chan }))
    }
}
