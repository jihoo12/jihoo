//! Places: expressions that denote a storage location and can be read, written,
//! or have their address taken.
//!
//! - A *register place* is a variable plus a path of struct fields (`a.b.c`).
//!   Writing it rebuilds the struct with `setfield`, innermost field first.
//! - A *memory place* is a pointer (`*p`, `p[i]`, `p.x` where `p: *Struct`).
//!   Writing it is a `store`; field access uses `fieldptr`.

use jihoo_ir::{BinOp, Inst, Reg, Type};
use jihoo_syntax::ast::{Expr, ExprKind};
use jihoo_syntax::{Error, Pos};

use crate::FnCx;

pub(crate) enum Place {
    Reg {
        root: Reg,
        /// Field indices from `root` down to this place.
        path: Vec<u32>,
        ty: Type,
        /// False for temporaries such as `f().x`.
        assignable: bool,
    },
    Mem {
        /// Pointer to the place.
        ptr: Reg,
        ty: Type,
    },
}

impl Place {
    pub(crate) fn ty(&self) -> &Type {
        match self {
            Place::Reg { ty, .. } | Place::Mem { ty, .. } => ty,
        }
    }
}

impl FnCx<'_> {
    pub(crate) fn place(&mut self, e: &Expr) -> Result<Place, Error> {
        match &e.kind {
            ExprKind::Var(name) => {
                let root = self.lookup(e.pos, name)?;
                Ok(Place::Reg { root, path: vec![], ty: self.ty(root).clone(), assignable: true })
            }
            ExprKind::Field(base, name) => {
                let base = self.place(base)?;
                match base.ty().clone() {
                    t @ Type::Struct(_) => {
                        let (index, fty) = self.env.field(e.pos, &t, name)?;
                        Ok(match base {
                            Place::Reg { root, mut path, assignable, .. } => {
                                path.push(index);
                                Place::Reg { root, path, ty: fty, assignable }
                            }
                            Place::Mem { ptr, .. } => {
                                let ptr = self.emit_to(Type::ptr(fty.clone()), |dst| Inst::FieldPtr { dst, ptr, index });
                                Place::Mem { ptr, ty: fty }
                            }
                        })
                    }
                    // `p.x` with `p: *Struct` reads through the pointer, like Go.
                    Type::Ptr(inner) if matches!(*inner, Type::Struct(_)) => {
                        let (index, fty) = self.env.field(e.pos, &inner, name)?;
                        let base_ptr = self.read(base);
                        let ptr = self.emit_to(Type::ptr(fty.clone()), |dst| Inst::FieldPtr { dst, ptr: base_ptr, index });
                        Ok(Place::Mem { ptr, ty: fty })
                    }
                    t => Err(Error::new(e.pos, format!("type {t} has no fields"))),
                }
            }
            ExprKind::Index(base, index) => {
                let base_pos = base.pos;
                let b = self.expr(base, None)?;
                let Some(elem) = self.ty(b).pointee().cloned() else {
                    return Err(Error::new(base_pos, format!("cannot index into {}", self.ty(b))));
                };
                let i = self.expr(index, Some(&Type::I64))?;
                self.expect(index.pos, i, &Type::I64, "an index")?;
                let ptr_ty = self.ty(b).clone();
                let ptr = self.emit_to(ptr_ty, |dst| Inst::Binary { dst, op: BinOp::Add, lhs: b, rhs: i });
                Ok(Place::Mem { ptr, ty: elem })
            }
            ExprKind::Deref(inner) => {
                let p = self.expr(inner, None)?;
                let Some(ty) = self.ty(p).pointee().cloned() else {
                    return Err(Error::new(e.pos, format!("cannot dereference {}", self.ty(p))));
                };
                Ok(Place::Mem { ptr: p, ty })
            }
            _ => {
                let root = self.expr(e, None)?;
                Ok(Place::Reg { root, path: vec![], ty: self.ty(root).clone(), assignable: false })
            }
        }
    }

    pub(crate) fn read(&mut self, place: Place) -> Reg {
        match place {
            Place::Reg { root, path, .. } => {
                let mut cur = root;
                for index in path {
                    let ty = self.env.field_type(self.ty(cur), index);
                    cur = self.emit_to(ty, |dst| Inst::Field { dst, src: cur, index });
                }
                cur
            }
            Place::Mem { ptr, ty } => self.emit_to(ty, |dst| Inst::Load { dst, ptr }),
        }
    }

    /// Writes `value` (already checked to have the place's type) to `place`.
    pub(crate) fn write(&mut self, pos: Pos, place: Place, value: Reg) -> Result<(), Error> {
        match place {
            Place::Reg { assignable: false, .. } => Err(Error::new(pos, "cannot assign to this expression")),
            Place::Reg { root, path, .. } => {
                // The struct values along the path: root, root.a, root.a.b, ...
                let mut along = vec![root];
                for &index in &path[..path.len().saturating_sub(1)] {
                    let cur = *along.last().unwrap();
                    let ty = self.env.field_type(self.ty(cur), index);
                    along.push(self.emit_to(ty, |dst| Inst::Field { dst, src: cur, index }));
                }
                // Rebuild from the innermost struct outwards; the last step writes `root`.
                let mut new = value;
                for (k, &index) in path.iter().enumerate().rev() {
                    let src = along[k];
                    let dst = if k == 0 { root } else { self.new_reg(self.ty(src).clone()) };
                    self.emit(Inst::SetField { dst, src, index, value: new });
                    new = dst;
                }
                if path.is_empty() {
                    self.emit(Inst::Copy { dst: root, src: value });
                }
                Ok(())
            }
            Place::Mem { ptr, .. } => {
                self.emit(Inst::Store { ptr, value });
                Ok(())
            }
        }
    }

    pub(crate) fn addr_of(&mut self, pos: Pos, place: Place) -> Result<Reg, Error> {
        match place {
            Place::Mem { ptr, .. } => Ok(ptr),
            Place::Reg { assignable: false, .. } => {
                Err(Error::new(pos, "cannot take the address of a temporary value"))
            }
            Place::Reg { root, path, .. } => {
                let root_ty = self.ty(root).clone();
                let mut ptr = self.emit_to(Type::ptr(root_ty.clone()), |dst| Inst::Addr { dst, src: root });
                let mut ty = root_ty;
                for index in path {
                    ty = self.env.field_type(&ty, index);
                    let base = ptr;
                    ptr = self.emit_to(Type::ptr(ty.clone()), |dst| Inst::FieldPtr { dst, ptr: base, index });
                }
                Ok(ptr)
            }
        }
    }
}
