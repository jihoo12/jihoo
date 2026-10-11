//! The atomic builtins, for memory that threads share in native and
//! freestanding programs: `atomic_load`, `atomic_store`, the read-modify-writes
//! (`atomic_swap`, `atomic_add`, `atomic_sub`, `atomic_and`, `atomic_or`,
//! `atomic_xor`), `atomic_cas`, and `fence`. All are sequentially consistent.
//!
//! Like the float builtins they are predeclared: a module may define a
//! function of the same name, which then hides the builtin in that module.

use jihoo_ir::{AtomicOp, Inst, Reg, Type};
use jihoo_syntax::ast::Expr;
use jihoo_syntax::{Error, Pos};

use crate::FnCx;

/// The names of the atomic builtins.
pub(crate) fn is_atomic_builtin(name: &str) -> bool {
    matches!(name, "atomic_load" | "atomic_store" | "atomic_cas" | "fence") || rmw_op(name).is_some()
}

fn rmw_op(name: &str) -> Option<AtomicOp> {
    AtomicOp::ALL.into_iter().find(|op| op.mnemonic() == name)
}

impl FnCx<'_> {
    pub(crate) fn atomic_builtin(&mut self, pos: Pos, name: &str, args: &[Expr]) -> Result<Reg, Error> {
        if !self.profile().is_compiled() {
            return Err(Error::new(pos, format!("`{name}` is only available in native and freestanding mode")));
        }
        let arity = match name {
            "fence" => 0,
            "atomic_load" => 1,
            "atomic_cas" => 3,
            _ => 2,
        };
        if args.len() != arity {
            let what = match arity {
                0 => "no arguments".to_string(),
                1 => "exactly 1 argument".to_string(),
                n => format!("{n} arguments"),
            };
            return Err(Error::new(pos, format!("`{name}` takes {what}")));
        }
        if name == "fence" {
            self.emit(Inst::Fence);
            return Ok(self.unit());
        }
        // Loads, stores, swaps and compare-and-swaps also work on pointers;
        // the arithmetic only on integers.
        let op = rmw_op(name);
        let pointers = !matches!(op, Some(o) if o != AtomicOp::Swap);
        let ptr = self.expr(&args[0], None)?;
        let t = match self.ty(ptr) {
            Type::Ptr(t) if t.is_atomic(pointers) => (**t).clone(),
            Type::Ptr(t) => {
                let what = if pointers { "integers and pointers" } else { "integers" };
                return Err(Error::new(args[0].pos, format!("`{name}` works on {what}, not {t}")));
            }
            t => return Err(Error::new(args[0].pos, format!("`{name}` needs a pointer, found {t}"))),
        };
        // The other arguments are values of the type pointed to.
        let mut values = Vec::with_capacity(arity - 1);
        for a in &args[1..] {
            let r = self.expr(a, Some(&t))?;
            self.expect(a.pos, r, &t, &format!("an argument of `{name}`"))?;
            values.push(r);
        }
        Ok(match (name, op, &values[..]) {
            ("atomic_load", ..) => self.emit_to(t, |dst| Inst::AtomicLoad { dst, ptr }),
            ("atomic_store", _, &[value]) => {
                self.emit(Inst::AtomicStore { ptr, value });
                self.unit()
            }
            ("atomic_cas", _, &[expected, new]) => {
                self.emit_to(Type::Bool, |dst| Inst::AtomicCas { dst, ptr, expected, new })
            }
            (_, Some(op), &[value]) => self.emit_to(t, |dst| Inst::AtomicRmw { dst, op, ptr, value }),
            _ => unreachable!("not an atomic builtin: {name}"),
        })
    }
}
