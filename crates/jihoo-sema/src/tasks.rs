//! Tasks and channels: `go f(x)`, `chan(T, n)`, `send` and `recv`.
//!
//! They need the VM's scheduler (`crates/jihoo-vm/src/lib.rs`), so they are
//! hosted only. A task shares nothing with the one that started it except what
//! it is given: arguments are copied, closures capture by value, `ref`s are
//! immutable. Channels are the only way for tasks to communicate.

use jihoo_ir::{Inst, Profile, Reg, Type};
use jihoo_syntax::ast::{Expr, ExprKind, TypeExpr};
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
