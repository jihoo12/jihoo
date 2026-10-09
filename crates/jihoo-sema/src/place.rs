//! Places: expressions that denote a storage location and can be read, written,
//! or have their address taken.
//!
//! - A *register place* is a variable plus a path of struct fields and array
//!   elements (`a.b[i].c`). Writing it rebuilds the aggregates along the path with
//!   `setfield`/`setelem`, innermost first.
//! - A *memory place* is a pointer (`*p`, `p[i]`, `p.x` where `p: *Struct`).
//!   Writing it is a `store`; fields and elements use `fieldptr`/`elemptr`.

use jihoo_ir::{BinOp, Inst, Reg, Type};
use jihoo_syntax::ast::{Expr, ExprKind};
use jihoo_syntax::{Error, Pos};

use crate::FnCx;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Step {
    Field(u32),
    /// An array element; the register holds the `i64` index.
    Elem(Reg),
}

pub(crate) enum Place {
    Reg {
        root: Reg,
        path: Vec<Step>,
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
    /// Type of `t` after taking one step into it.
    fn step_type(&self, t: &Type, step: Step) -> Type {
        match (step, t) {
            (Step::Field(i), _) => self.env.field_type(t, i),
            (Step::Elem(_), Type::Array(elem, _)) => (**elem).clone(),
            _ => unreachable!("cannot step into {t}"),
        }
    }

    /// Extends `base` (whose type is a struct or array) by `step`.
    fn step(&mut self, base: Place, step: Step, ty: Type) -> Place {
        match base {
            Place::Reg { root, mut path, assignable, .. } => {
                path.push(step);
                Place::Reg { root, path, ty, assignable }
            }
            Place::Mem { ptr, .. } => self.step_ptr(ptr, step, ty),
        }
    }

    /// The place `step` leads to inside the aggregate `ptr` points to.
    fn step_ptr(&mut self, ptr: Reg, step: Step, ty: Type) -> Place {
        let ptr_ty = Type::ptr(ty.clone());
        let ptr = match step {
            Step::Field(index) => self.emit_to(ptr_ty, |dst| Inst::FieldPtr { dst, ptr, index }),
            Step::Elem(index) => self.emit_to(ptr_ty, |dst| Inst::ElemPtr { dst, ptr, index }),
        };
        Place::Mem { ptr, ty }
    }

    pub(crate) fn place(&mut self, e: &Expr) -> Result<Place, Error> {
        match &e.kind {
            ExprKind::Var(name) if self.local(name).is_some() => {
                let root = self.lookup(e.pos, name)?;
                Ok(Place::Reg { root, path: vec![], ty: self.ty(root).clone(), assignable: true })
            }
            ExprKind::Field(base, name) => {
                let base = self.place(base)?;
                match base.ty().clone() {
                    t @ Type::Struct(_) => {
                        let (index, fty) = self.env.field(e.pos, &t, name)?;
                        Ok(self.step(base, Step::Field(index), fty))
                    }
                    // `p.x` with `p: *Struct` reads through the pointer, like Go.
                    Type::Ptr(inner) if matches!(*inner, Type::Struct(_)) => {
                        let (index, fty) = self.env.field(e.pos, &inner, name)?;
                        let ptr = self.read(base);
                        Ok(self.step_ptr(ptr, Step::Field(index), fty))
                    }
                    t => Err(Error::new(e.pos, format!("type {t} has no fields"))),
                }
            }
            ExprKind::Index(base_expr, index) => {
                let base = self.place(base_expr)?;
                match base.ty().clone() {
                    Type::Array(elem, _) => {
                        let i = self.index(index)?;
                        Ok(self.step(base, Step::Elem(i), *elem))
                    }
                    // `p[i]` with `p: *[T; N]` indexes the array, like Go.
                    Type::Ptr(inner) if matches!(*inner, Type::Array(..)) => {
                        let Type::Array(elem, _) = *inner else { unreachable!() };
                        let ptr = self.read(base);
                        let i = self.index(index)?;
                        Ok(self.step_ptr(ptr, Step::Elem(i), *elem))
                    }
                    // `p[i]` with `p: *T` is `*(p + i)`: no bounds check.
                    p @ Type::Ptr(_) => {
                        let elem = p.pointee().unwrap().clone();
                        let b = self.read(base);
                        let i = self.index(index)?;
                        let ptr = self.emit_to(p, |dst| Inst::Binary { dst, op: BinOp::Add, lhs: b, rhs: i });
                        Ok(Place::Mem { ptr, ty: elem })
                    }
                    t => Err(Error::new(base_expr.pos, format!("cannot index into {t}"))),
                }
            }
            // A macro may expand to a place: `field!(p) = 1`.
            ExprKind::MacroCall(name, args) => {
                let expanded = self.expand(e.pos, name, args)?;
                self.macro_depth += 1;
                let r = self.place(&expanded);
                self.macro_depth -= 1;
                r
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

    fn index(&mut self, index: &Expr) -> Result<Reg, Error> {
        let i = self.expr(index, Some(&Type::I64))?;
        self.expect(index.pos, i, &Type::I64, "an index")?;
        Ok(i)
    }

    /// Reads one step out of the aggregate value in `src`.
    fn read_step(&mut self, src: Reg, step: Step) -> Reg {
        let ty = self.step_type(self.ty(src), step);
        match step {
            Step::Field(index) => self.emit_to(ty, |dst| Inst::Field { dst, src, index }),
            Step::Elem(index) => self.emit_to(ty, |dst| Inst::Elem { dst, src, index }),
        }
    }

    pub(crate) fn read(&mut self, place: Place) -> Reg {
        match place {
            Place::Reg { root, path, .. } => path.into_iter().fold(root, |cur, step| self.read_step(cur, step)),
            Place::Mem { ptr, ty } => self.emit_to(ty, |dst| Inst::Load { dst, ptr }),
        }
    }

    /// Writes `value` (already checked to have the place's type) to `place`.
    pub(crate) fn write(&mut self, pos: Pos, place: Place, value: Reg) -> Result<(), Error> {
        match place {
            Place::Reg { assignable: false, .. } => Err(Error::new(pos, "cannot assign to this expression")),
            Place::Reg { root, path, .. } => {
                // The aggregates along the path: root, root.a, root.a[i], ...
                let mut along = vec![root];
                for &step in &path[..path.len().saturating_sub(1)] {
                    let next = self.read_step(*along.last().unwrap(), step);
                    along.push(next);
                }
                // Rebuild from the innermost aggregate outwards; the last step writes `root`.
                let mut new = value;
                for (k, &step) in path.iter().enumerate().rev() {
                    let src = along[k];
                    let dst = if k == 0 { root } else { self.new_reg(self.ty(src).clone()) };
                    self.emit(match step {
                        Step::Field(index) => Inst::SetField { dst, src, index, value: new },
                        Step::Elem(index) => Inst::SetElem { dst, src, index, value: new },
                    });
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
                for step in path {
                    ty = self.step_type(&ty, step);
                    let Place::Mem { ptr: p, .. } = self.step_ptr(ptr, step, ty.clone()) else { unreachable!() };
                    ptr = p;
                }
                Ok(ptr)
            }
        }
    }
}
