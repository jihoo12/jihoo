//! Tuples: `(a, b)`, of type `(A, B)`.
//!
//! A tuple type is a struct whose name is how the type is written
//! (`(i64, str)`), so JIR, the VM and LLVM see nothing new. Its fields are
//! named `_0`, `_1`, ... (JIR field names are words), and `t.0` reads the
//! first by its index. Two tuple types with the same parts are the same type,
//! wherever they are written; their parts are public.

use std::rc::Rc;

use jihoo_ir::{Inst, Reg, Type};
use jihoo_syntax::ast::Expr;
use jihoo_syntax::Error;

use crate::env::{Env, Fields};
use crate::FnCx;

impl Env<'_> {
    /// The tuple type of `parts`, of which there are at least two.
    pub fn tuple_type(&self, parts: Vec<Type>) -> Type {
        let shown: Vec<String> = parts.iter().map(Type::to_string).collect();
        let name = format!("({})", shown.join(", "));
        if self.tuple_named(&name).is_none() {
            let fields = parts.into_iter().enumerate().map(|(i, t)| (format!("_{i}"), t)).collect();
            self.tuples.borrow_mut().push((name.clone(), Rc::new(fields)));
        }
        Type::Struct(name)
    }

    /// The parts of tuple type `name`, if it is one.
    pub fn tuple_named(&self, name: &str) -> Option<Fields> {
        self.tuples.borrow().iter().find(|(n, _)| n == name).map(|(_, f)| f.clone())
    }

    /// The parts of `t`, if it is a tuple type.
    pub fn tuple_parts(&self, t: &Type) -> Option<Fields> {
        match t {
            Type::Struct(n) => self.tuple_named(n),
            _ => None,
        }
    }

    /// Every tuple type so far, in creation order.
    pub fn tuple_types(&self) -> Vec<Type> {
        self.tuples.borrow().iter().map(|(n, _)| Type::Struct(n.clone())).collect()
    }
}

impl FnCx<'_> {
    /// `(a, b, ...)`. Each part takes its expected type from `expected`, so
    /// `let p: (u8, i64) = (1, 2)` works.
    pub(crate) fn tuple(&mut self, items: &[Expr], expected: Option<&Type>) -> Result<Reg, Error> {
        let hints = expected.and_then(|t| self.env.tuple_parts(t)).filter(|f| f.len() == items.len());
        let mut regs = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let hint = hints.as_ref().map(|f| f[i].1.clone());
            let r = self.expr(item, hint.as_ref())?;
            if *self.ty(r) == Type::Unit {
                return Err(Error::new(item.pos, "a part of a tuple must have a value; this expression has none"));
            }
            regs.push(r);
        }
        let ty = self.env.tuple_type(regs.iter().map(|r| self.ty(*r).clone()).collect());
        let Type::Struct(name) = ty.clone() else { unreachable!() };
        Ok(self.emit_to(ty, |dst| Inst::Struct { dst, name, fields: regs }))
    }
}
