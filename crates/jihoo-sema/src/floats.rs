//! The float builtins: the operations IEEE 754 defines exactly (`sqrt`,
//! `floor`, `ceil`, `trunc`, `round`, `abs`, `copysign`), which every backend
//! computes the same, and a float's bits (`to_bits`, `from_bits`).
//!
//! Unlike `print` or `len`, these are *predeclared*: a module may define a
//! function of the same name, which then hides the builtin in that module.
//! So a program that has its own `sqrt` keeps working.

use jihoo_ir::{BinOp, FloatTy, Inst, IntTy, Reg, Type, UnOp};
use jihoo_syntax::ast::Expr;
use jihoo_syntax::{Error, Pos};

use crate::FnCx;

/// The names of the float builtins.
pub(crate) fn is_float_builtin(name: &str) -> bool {
    matches!(name, "sqrt" | "floor" | "ceil" | "trunc" | "round" | "abs" | "copysign" | "to_bits" | "from_bits")
}

impl FnCx<'_> {
    /// A float argument of `name`: a literal takes the expected float type, or
    /// `f64`.
    fn float_arg(&mut self, name: &str, arg: &Expr, hint: Option<&Type>) -> Result<Reg, Error> {
        let hint = match hint {
            Some(t @ Type::Float(_)) => t.clone(),
            _ => Type::F64,
        };
        let r = self.expr(arg, Some(&hint))?;
        match self.ty(r) {
            Type::Float(_) => Ok(r),
            t => Err(Error::new(arg.pos, format!("`{name}` takes a float (f32 or f64), not {t}"))),
        }
    }

    /// A call of float builtin `name`.
    pub(crate) fn float_builtin(&mut self, pos: Pos, name: &str, args: &[Expr], expected: Option<&Type>) -> Result<Reg, Error> {
        let arity = if name == "copysign" { 2 } else { 1 };
        if args.len() != arity {
            let what = if arity == 1 { "exactly 1 argument" } else { "2 arguments" };
            return Err(Error::new(pos, format!("`{name}` takes {what}")));
        }
        match name {
            "copysign" => {
                let x = self.float_arg(name, &args[0], expected)?;
                let t = self.ty(x).clone();
                let y = self.float_arg(name, &args[1], Some(&t))?;
                self.expect(args[1].pos, y, &t, "the sign argument of `copysign`")?;
                Ok(self.emit_to(t, |dst| Inst::Binary { dst, op: BinOp::Copysign, lhs: x, rhs: y }))
            }
            "to_bits" => {
                let src = self.float_arg(name, &args[0], None)?;
                let t = jihoo_ir::types::bitcast(self.ty(src)).unwrap();
                Ok(self.emit_to(t, |dst| Inst::Bitcast { dst, src }))
            }
            "from_bits" => {
                // `u32` bits make an `f32`, `u64` bits an `f64`.
                let hint = match expected {
                    Some(Type::Float(FloatTy::F32)) => Type::Int(IntTy::U32),
                    _ => Type::Int(IntTy::U64),
                };
                let src = self.expr(&args[0], Some(&hint))?;
                let t = match self.ty(src) {
                    t @ Type::Int(IntTy::U32 | IntTy::U64) => jihoo_ir::types::bitcast(t).unwrap(),
                    t => {
                        let msg = format!("`from_bits` takes the bits of an f32 (u32) or of an f64 (u64), not {t}");
                        return Err(Error::new(args[0].pos, msg));
                    }
                };
                Ok(self.emit_to(t, |dst| Inst::Bitcast { dst, src }))
            }
            _ => {
                let op = match name {
                    "sqrt" => UnOp::Sqrt,
                    "floor" => UnOp::Floor,
                    "ceil" => UnOp::Ceil,
                    "trunc" => UnOp::Trunc,
                    "round" => UnOp::Round,
                    "abs" => UnOp::Abs,
                    _ => unreachable!("not a float builtin: {name}"),
                };
                let src = self.float_arg(name, &args[0], expected)?;
                let t = self.ty(src).clone();
                Ok(self.emit_to(t, |dst| Inst::Unary { dst, op, src }))
            }
        }
    }
}
