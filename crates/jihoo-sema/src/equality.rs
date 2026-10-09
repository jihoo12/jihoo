//! `==` and `!=` on structs, enums, arrays and refs: structural equality.
//!
//! Two values are equal when they have the same shape and equal parts: the same
//! fields, the same variant with equal payloads, equal elements. A `ref` is
//! compared by what it refers to (refs are immutable, so which object it is
//! cannot be observed anyway). Pointers compare as addresses, as before.
//! Functions and channels cannot be compared, and neither can a value that
//! holds one.
//!
//! Each such type gets a helper function `fn.eq.N(a: T, b: T) -> bool` (`fn` is
//! a keyword, so the name is never a user's), made the first time the type is
//! compared. Helpers call each other, and themselves for recursive types such
//! as `enum List { Cons(i64, ref List), Nil }`. Since they are ordinary
//! functions, nothing changes in JIR or the backends.

use std::rc::Rc;

use jihoo_ir::{BinOp, BlockId, Inst, IntTy, Reg, Terminator, Type, UnOp};
use jihoo_syntax::{Error, Pos};

use crate::env::{Env, Sig};
use crate::generic::Bindings;
use crate::FnCx;

/// Whether `==` compares values of type `t` with a helper function.
pub(crate) fn is_structural(t: &Type) -> bool {
    matches!(t, Type::Struct(_) | Type::Enum(_) | Type::Array(..) | Type::Ref(_))
}

impl Env<'_> {
    /// Why values of type `t` cannot be compared, if they cannot.
    fn incomparable(&self, t: &Type) -> Option<String> {
        let parts: Vec<Type> = match t {
            Type::Fn(..) => return Some(format!("function values ({t}) cannot be compared")),
            Type::Chan(_) => return Some(format!("channels ({t}) cannot be compared")),
            Type::Cell(_) => return Some(format!("cells ({t}) cannot be compared; compare what they hold, `*a == *b`")),
            Type::Expr | Type::Stmts | Type::Items => return Some(format!("{t} values cannot be compared")),
            Type::Array(elem, _) | Type::Ref(elem) => vec![(**elem).clone()],
            Type::Struct(s) => self.struct_fields(Pos::default(), s).ok()?.iter().map(|(_, t)| t.clone()).collect(),
            Type::Enum(e) => self.enum_variants(Pos::default(), e).ok()?.iter().flat_map(|(_, ts)| ts.clone()).collect(),
            _ => return None,
        };
        // A type that holds itself (through a ref) is checked once.
        let mut seen = self.eq_checking.borrow_mut();
        if seen.contains(&t.jir()) {
            return None;
        }
        seen.push(t.jir());
        drop(seen);
        let why = parts.iter().find_map(|p| self.incomparable(p));
        self.eq_checking.borrow_mut().pop();
        why
    }

    /// The name of the helper that compares two values of type `t`, made on
    /// first use.
    fn eq_helper(&self, t: &Type) -> Result<String, Error> {
        if let Some(name) = self.eq_helpers.borrow().get(&t.jir()) {
            return Ok(name.clone());
        }
        let name = format!("fn.eq.{}", self.eq_helpers.borrow().len());
        // Registered before the body is made, so a recursive type's helper can
        // call itself.
        self.eq_helpers.borrow_mut().insert(t.jir(), name.clone());
        let sig = Sig { params: vec![t.clone(), t.clone()], ret: Type::Bool };
        let mut cx = FnCx::new(self, Rc::new(sig), Rc::new(Bindings::in_module(0)));
        let (a, b) = (cx.new_reg(t.clone()), cx.new_reg(t.clone()));
        cx.eq_body(t, a, b)?;
        let f = cx.finish(&name, Type::Bool, Pos::default())?;
        self.add_lambda(f, t.available_in(self.profile));
        Ok(name)
    }
}

impl FnCx<'_> {
    /// `lhs == rhs` (or `!=` with `negate`) for values of a type that
    /// [`is_structural`].
    pub(crate) fn structural_eq(&mut self, pos: Pos, lhs: Reg, rhs: Reg, negate: bool) -> Result<Reg, Error> {
        let t = self.ty(lhs).clone();
        let sym = if negate { "!=" } else { "==" };
        if let Some(why) = self.env.incomparable(&t) {
            return Err(Error::new(pos, format!("cannot apply `{sym}` to {t}: {why}")));
        }
        let eq = self.equal(&t, lhs, rhs)?;
        Ok(if negate { self.emit_to(Type::Bool, |dst| Inst::Unary { dst, op: UnOp::Not, src: eq }) } else { eq })
    }

    /// Whether `a` and `b`, of comparable type `t`, are equal.
    fn equal(&mut self, t: &Type, a: Reg, b: Reg) -> Result<Reg, Error> {
        Ok(match t {
            Type::Unit => self.konst(Type::Bool, 1),
            _ if is_structural(t) => {
                let func = self.env.eq_helper(t)?;
                self.emit_to(Type::Bool, |dst| Inst::Call { dst, func, args: vec![a, b] })
            }
            _ => self.emit_to(Type::Bool, |dst| Inst::Binary { dst, op: BinOp::Eq, lhs: a, rhs: b }),
        })
    }

    /// The body of an equality helper for `a` and `b` of type `t`: returns
    /// false at the first difference, true at the end.
    fn eq_body(&mut self, t: &Type, a: Reg, b: Reg) -> Result<(), Error> {
        let unequal = self.new_block();
        match t {
            Type::Struct(s) => {
                let fields = self.env.struct_fields(Pos::default(), s)?;
                for (i, (_, ft)) in fields.iter().enumerate() {
                    let index = i as u32;
                    let fa = self.emit_to(ft.clone(), |dst| Inst::Field { dst, src: a, index });
                    let fb = self.emit_to(ft.clone(), |dst| Inst::Field { dst, src: b, index });
                    self.require(ft, fa, fb, unequal)?;
                }
            }
            Type::Ref(inner) => {
                let da = self.emit_to((**inner).clone(), |dst| Inst::Deref { dst, src: a });
                let db = self.emit_to((**inner).clone(), |dst| Inst::Deref { dst, src: b });
                self.require(inner, da, db, unequal)?;
            }
            Type::Array(elem, n) => {
                // for i in 0..n: a[i] == b[i]
                let i = self.konst(Type::I64, 0);
                let len = self.konst(Type::I64, *n as i64);
                let (head, body, done) = (self.new_block(), self.new_block(), self.new_block());
                self.terminate(Terminator::Jump(head));
                self.switch_to(head);
                let more = self.emit_to(Type::Bool, |dst| Inst::Binary { dst, op: BinOp::Lt, lhs: i, rhs: len });
                self.terminate(Terminator::Branch { cond: more, then: body, els: done });
                self.switch_to(body);
                let ea = self.emit_to((**elem).clone(), |dst| Inst::Elem { dst, src: a, index: i });
                let eb = self.emit_to((**elem).clone(), |dst| Inst::Elem { dst, src: b, index: i });
                self.require(elem, ea, eb, unequal)?;
                let one = self.konst(Type::I64, 1);
                self.emit(Inst::Binary { dst: i, op: BinOp::Add, lhs: i, rhs: one });
                self.terminate(Terminator::Jump(head));
                self.switch_to(done);
            }
            Type::Enum(e) => {
                // The same variant, then equal payloads.
                let u32 = Type::Int(IntTy::U32);
                let ta = self.emit_to(u32.clone(), |dst| Inst::Tag { dst, src: a });
                let tb = self.emit_to(u32.clone(), |dst| Inst::Tag { dst, src: b });
                let same = self.emit_to(Type::Bool, |dst| Inst::Binary { dst, op: BinOp::Eq, lhs: ta, rhs: tb });
                self.go_on_if(same, unequal);
                let variants = self.env.enum_variants(Pos::default(), e)?;
                let done = self.new_block();
                for (v, (_, payload)) in variants.iter().enumerate().filter(|(_, (_, p))| !p.is_empty()) {
                    let variant = v as u32;
                    let k = self.konst(u32.clone(), v as i64);
                    let this = self.emit_to(Type::Bool, |dst| Inst::Binary { dst, op: BinOp::Eq, lhs: ta, rhs: k });
                    let (cmp, next) = (self.new_block(), self.new_block());
                    self.terminate(Terminator::Branch { cond: this, then: cmp, els: next });
                    self.switch_to(cmp);
                    for (j, pt) in payload.iter().enumerate() {
                        let index = j as u32;
                        let pa = self.emit_to(pt.clone(), |dst| Inst::Payload { dst, src: a, variant, index });
                        let pb = self.emit_to(pt.clone(), |dst| Inst::Payload { dst, src: b, variant, index });
                        self.require(pt, pa, pb, unequal)?;
                    }
                    self.terminate(Terminator::Jump(done));
                    self.switch_to(next);
                }
                self.terminate(Terminator::Jump(done));
                self.switch_to(done);
            }
            _ => unreachable!("{t} is not compared structurally"),
        }
        let yes = self.konst(Type::Bool, 1);
        self.terminate(Terminator::Ret(yes));
        self.switch_to(unequal);
        let no = self.konst(Type::Bool, 0);
        self.terminate(Terminator::Ret(no));
        Ok(())
    }

    /// Goes on if parts `a` and `b` of type `t` are equal, else to `unequal`.
    fn require(&mut self, t: &Type, a: Reg, b: Reg, unequal: BlockId) -> Result<(), Error> {
        let eq = self.equal(t, a, b)?;
        self.go_on_if(eq, unequal);
        Ok(())
    }

    fn go_on_if(&mut self, cond: Reg, otherwise: BlockId) {
        let ok = self.new_block();
        self.terminate(Terminator::Branch { cond, then: ok, els: otherwise });
        self.switch_to(ok);
    }
}
