//! Places: expressions that denote a storage location and can be read, written,
//! or have their address taken.
//!
//! - A *register place* is a variable plus a path of struct fields and array
//!   elements (`a.b[i].c`). Writing it rebuilds the aggregates along the path with
//!   `setfield`/`setelem`, innermost first.
//! - A *memory place* is a pointer (`*p`, `p[i]`, `p.x` where `p: *Struct`).
//!   Writing it is a `store`; fields and elements use `fieldptr`/`elemptr`.
//! - What a `ref` refers to (`*r`, `r.x`, `r[i]`) is a register place holding
//!   the value read through the ref. It cannot be written: refs are immutable.
//! - What a `cell` holds (`*c`, `c.x`, `c[i]`, `c.a[i]`) is a *cell place*:
//!   one `cellget` reads it and one `cellset` writes it, so everything holding
//!   the cell sees the change.
//! - `root[p]` with a path `p` is the part of `root` the path leads to
//!   (`paths.rs`), and `p.x`, `p[i]` are longer paths.

use jihoo_ir::{BinOp, Inst, PathStep, Reg, Type};
use jihoo_syntax::ast::{Expr, ExprKind, PathShape};
use jihoo_syntax::{Error, Pos};

use crate::FnCx;

fn path_step(s: Step) -> PathStep {
    match s {
        Step::Field(i) => PathStep::Field(i),
        Step::Elem(r) => PathStep::Elem(r),
    }
}

const BEHIND_REF: &str = "a value behind a `ref` (refs are immutable; build a new value and a new ref instead)";

const IN_STR: &str = "a byte of a str (strings are immutable; build a new one instead)";

const OWNED_IN_MEMORY: &str = "a list, or a value holding one, cannot be read or written through a pointer \
     (it owns its memory, which only its variable may free; use a path to refer to a part of it)";

const CAPTURED: &str =
    "a captured variable (a closure keeps its own copy, taken when it was made; pass the value as an argument or return it instead)";

#[derive(Debug, Clone, Copy)]
pub(crate) enum Step {
    Field(u32),
    /// An array element; the register holds the `i64` index.
    Elem(Reg),
}

#[derive(Clone)]
pub(crate) enum Place {
    Reg {
        root: Reg,
        path: Vec<Step>,
        ty: Type,
        /// What the place is, if it cannot be written: "a temporary value" for
        /// `f().x`, for example. `None` for variables.
        readonly: Option<&'static str>,
    },
    Mem {
        /// Pointer to the place.
        ptr: Reg,
        ty: Type,
    },
    /// What `cell` holds, or the part of it at `path`.
    Cell {
        cell: Reg,
        path: Vec<Step>,
        ty: Type,
    },
}

impl Place {
    pub(crate) fn ty(&self) -> &Type {
        match self {
            Place::Reg { ty, .. } | Place::Mem { ty, .. } | Place::Cell { ty, .. } => ty,
        }
    }
}

impl FnCx<'_> {
    /// Type of `t` after taking one step into it.
    fn step_type(&self, t: &Type, step: Step) -> Type {
        match (step, t) {
            (Step::Field(i), _) => self.env.field_type(t, i),
            (Step::Elem(_), Type::Array(elem, _) | Type::List(elem)) => (**elem).clone(),
            _ => unreachable!("cannot step into {t}"),
        }
    }

    /// Extends `base` (whose type is a struct or array) by `step`.
    pub(crate) fn step(&mut self, base: Place, step: Step, ty: Type) -> Place {
        match base {
            Place::Reg { root, mut path, readonly, .. } => {
                path.push(step);
                Place::Reg { root, path, ty, readonly }
            }
            Place::Mem { ptr, .. } => self.step_ptr(ptr, step, ty),
            Place::Cell { cell, mut path, .. } => {
                path.push(step);
                Place::Cell { cell, path, ty }
            }
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
        let place = self.place_of(e)?;
        if let Place::Mem { ty, .. } = &place {
            if self.profile() == jihoo_ir::Profile::Native && self.env.owns(ty) {
                return Err(Error::new(e.pos, format!("{OWNED_IN_MEMORY}, found {ty}")));
            }
        }
        Ok(place)
    }

    /// Field `name` of `base`, which may also be a path, or a pointer, cell or
    /// ref to a struct.
    pub(crate) fn field_place(&mut self, pos: Pos, base: Place, name: &str) -> Result<Place, Error> {
        match base.ty().clone() {
            t if self.env.path_def(&t).is_some() => self.extend_path(pos, base, PathShape::Field(name.to_string()), None),
            t @ Type::Struct(_) => {
                let (index, fty) = self.env.field(pos, &t, name, self.bindings.module)?;
                Ok(self.step(base, Step::Field(index), fty))
            }
            // `p.x` with `p: *Struct` reads through the pointer, like Go.
            Type::Ptr(inner) if matches!(*inner, Type::Struct(_)) => {
                let (index, fty) = self.env.field(pos, &inner, name, self.bindings.module)?;
                let ptr = self.read(base);
                Ok(self.step_ptr(ptr, Step::Field(index), fty))
            }
            // `c.x` with `c: cell Struct` is a part of what the cell holds.
            Type::Cell(inner) if matches!(*inner, Type::Struct(_)) => {
                let (index, fty) = self.env.field(pos, &inner, name, self.bindings.module)?;
                let cell = self.read(base);
                Ok(Place::Cell { cell, path: vec![Step::Field(index)], ty: fty })
            }
            // `r.x` with `r: ref Struct` reads through the ref.
            Type::Ref(inner) if matches!(*inner, Type::Struct(_)) => {
                let (index, fty) = self.env.field(pos, &inner, name, self.bindings.module)?;
                let target = self.deref(base);
                Ok(self.step(target, Step::Field(index), fty))
            }
            t => Err(Error::new(pos, format!("type {t} has no fields"))),
        }
    }

    fn place_of(&mut self, e: &Expr) -> Result<Place, Error> {
        match &e.kind {
            ExprKind::Var(name) if self.local(name).is_some() => {
                let root = self.lookup(e.pos, name)?;
                let readonly = self.captured.contains(&root).then_some(CAPTURED);
                Ok(Place::Reg { root, path: vec![], ty: self.ty(root).clone(), readonly })
            }
            // `alias.CONST`: a constant of an imported module.
            ExprKind::Field(base, item)
                if matches!(&base.kind, ExprKind::Var(a) if self.local(a).is_none() && self.env.is_alias(self.bindings.module, a)) =>
            {
                let ExprKind::Var(alias) = &base.kind else { unreachable!() };
                let root = self.var(e.pos, &format!("{alias}.{}", crate::hygiene::unqualified(item)))?;
                Ok(Place::Reg { root, path: vec![], ty: self.ty(root).clone(), readonly: Some("a constant") })
            }
            ExprKind::Field(base, name) => {
                let base = self.place(base)?;
                self.field_place(e.pos, base, name)
            }
            ExprKind::Index(base_expr, index) => {
                let base = self.place(base_expr)?;
                match base.ty().clone() {
                    t if self.env.path_def(&t).is_some() => {
                        let i = self.index(index)?;
                        self.extend_path(e.pos, base, PathShape::Elem, Some(i))
                    }
                    // `g[p]` with a path `p` into a struct.
                    Type::Struct(_) => {
                        let p = self.expr(index, None)?;
                        self.follow_path(index, base, p)
                    }
                    Type::Cell(inner) if matches!(*inner, Type::Struct(_)) => {
                        let cell = self.read(base);
                        let p = self.expr(index, None)?;
                        self.follow_path(index, Place::Cell { cell, path: vec![], ty: *inner }, p)
                    }
                    Type::Ref(inner) if matches!(*inner, Type::Struct(_)) => {
                        let target = self.deref(base);
                        let p = self.expr(index, None)?;
                        self.follow_path(index, target, p)
                    }
                    // `gp[p]` with `gp: *Struct` follows the path in memory; `gp[i]`
                    // is still `*(gp + i)`.
                    Type::Ptr(inner) if matches!(*inner, Type::Struct(_)) => {
                        let ptr = self.read(base);
                        let i = self.expr(index, Some(&Type::I64))?;
                        if self.is_path(i) {
                            return self.follow_path(index, Place::Mem { ptr, ty: *inner }, i);
                        }
                        self.expect(index.pos, i, &Type::I64, "an index")?;
                        let ty = Type::Ptr(inner.clone());
                        let ptr = self.emit_to(ty, |dst| Inst::Binary { dst, op: BinOp::Add, lhs: ptr, rhs: i });
                        Ok(Place::Mem { ptr, ty: *inner })
                    }
                    Type::Array(elem, _) | Type::List(elem) => {
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
                    // `c[i]` with `c: cell [T; N]` or `cell [T]` is a part of what the cell holds.
                    Type::Cell(inner) if matches!(*inner, Type::Array(..) | Type::List(_)) => {
                        let (Type::Array(elem, _) | Type::List(elem)) = *inner else { unreachable!() };
                        let cell = self.read(base);
                        let i = self.index(index)?;
                        Ok(Place::Cell { cell, path: vec![Step::Elem(i)], ty: *elem })
                    }
                    // `r[i]` with `r: ref [T; N]` reads through the ref.
                    Type::Ref(inner) if matches!(*inner, Type::Array(..) | Type::List(_)) => {
                        let (Type::Array(elem, _) | Type::List(elem)) = *inner else { unreachable!() };
                        let target = self.deref(base);
                        let i = self.index(index)?;
                        Ok(self.step(target, Step::Elem(i), *elem))
                    }
                    // `p[i]` with `p: *T` is `*(p + i)`: no bounds check.
                    p @ Type::Ptr(_) => {
                        let elem = p.pointee().unwrap().clone();
                        let b = self.read(base);
                        let i = self.index(index)?;
                        let ptr = self.emit_to(p, |dst| Inst::Binary { dst, op: BinOp::Add, lhs: b, rhs: i });
                        Ok(Place::Mem { ptr, ty: elem })
                    }
                    // `s[i]` with `s: str` is a byte, which cannot be assigned.
                    Type::Str => {
                        let src = self.read(base);
                        let index = self.index(index)?;
                        let root = self.emit_to(Type::Int(jihoo_ir::IntTy::U8), |dst| Inst::StrByte { dst, src, index });
                        Ok(Place::Reg { root, path: vec![], ty: self.ty(root).clone(), readonly: Some(IN_STR) })
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
                if let Type::Cell(ty) = self.ty(p).clone() {
                    return Ok(Place::Cell { cell: p, path: vec![], ty: *ty });
                }
                if let Type::Ref(ty) = self.ty(p).clone() {
                    let root = self.emit_to(*ty.clone(), |dst| Inst::Deref { dst, src: p });
                    return Ok(Place::Reg { root, path: vec![], ty: *ty, readonly: Some(BEHIND_REF) });
                }
                let Some(ty) = self.ty(p).pointee().cloned() else {
                    return Err(Error::new(e.pos, format!("cannot dereference {}", self.ty(p))));
                };
                Ok(Place::Mem { ptr: p, ty })
            }
            _ => {
                let root = self.expr(e, None)?;
                Ok(Place::Reg { root, path: vec![], ty: self.ty(root).clone(), readonly: Some("a temporary value") })
            }
        }
    }

    /// The place a `ref` place refers to.
    pub(crate) fn deref(&mut self, r: Place) -> Place {
        let src = self.read(r);
        let Type::Ref(ty) = self.ty(src).clone() else { unreachable!("not a ref") };
        let root = self.emit_to(*ty.clone(), |dst| Inst::Deref { dst, src });
        Place::Reg { root, path: vec![], ty: *ty, readonly: Some(BEHIND_REF) }
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
            // A nested part is read in one go, so the parts on the way never sit
            // in registers (which would keep the VM from updating them in place).
            Place::Reg { root, path, ty, .. } if path.len() >= 2 => {
                let path = path.into_iter().map(path_step).collect();
                self.emit_to(ty, |dst| Inst::GetPath { dst, src: root, path })
            }
            Place::Reg { root, path, .. } => path.into_iter().fold(root, |cur, step| self.read_step(cur, step)),
            Place::Mem { ptr, ty } => self.emit_to(ty, |dst| Inst::Load { dst, ptr }),
            Place::Cell { cell, path, ty } => {
                let path = path.into_iter().map(path_step).collect();
                self.emit_to(ty, |dst| Inst::CellGet { dst, cell, path })
            }
        }
    }

    /// Writes `value` (already checked to have the place's type) to `place`.
    pub(crate) fn write(&mut self, pos: Pos, place: Place, value: Reg) -> Result<(), Error> {
        match place {
            Place::Reg { readonly: Some(what), .. } => Err(Error::new(pos, format!("cannot assign to {what}"))),
            Place::Reg { root, path, .. } if path.len() >= 2 => {
                let path = path.into_iter().map(path_step).collect();
                self.emit(Inst::SetPath { dst: root, src: root, path, value });
                Ok(())
            }
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
            Place::Cell { cell, path, .. } => {
                let path = path.into_iter().map(path_step).collect();
                self.emit(Inst::CellSet { cell, path, value });
                Ok(())
            }
        }
    }

    /// The list a `push` or `pop` changes, as the register and path to it.
    fn list_place(&mut self, what: &str, e: &Expr) -> Result<(Reg, Vec<PathStep>, Type), Error> {
        let place = self.place(e)?;
        let Type::List(elem) = place.ty().clone() else {
            return Err(Error::new(e.pos, format!("`{what}` needs a list, found {}", place.ty())));
        };
        match place {
            Place::Reg { readonly: Some(r), .. } => Err(Error::new(e.pos, format!("cannot `{what}` on {r}"))),
            Place::Reg { root, path, .. } => Ok((root, path.into_iter().map(path_step).collect(), *elem)),
            Place::Cell { cell, path, .. } => Ok((cell, path.into_iter().map(path_step).collect(), *elem)),
            Place::Mem { .. } => unreachable!("lists are never in memory"),
        }
    }

    /// `push(list, value)`: appends `value` to the list, a variable or a part
    /// of one (`s.items`), or what a cell holds.
    pub(crate) fn list_push(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        let [list, value] = args else {
            return Err(Error::new(pos, "`push` takes 2 arguments: a list and a value"));
        };
        let (root, path, elem) = self.list_place("push", list)?;
        let value_reg = self.expr(value, Some(&elem))?;
        self.expect(value.pos, value_reg, &elem, "the value pushed")?;
        self.emit(Inst::Push { root, path, value: value_reg });
        Ok(self.unit())
    }

    /// `pop(list)`: removes the last element of the list and gives it.
    pub(crate) fn list_pop(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        let [list] = args else {
            return Err(Error::new(pos, "`pop` takes 1 argument: a list"));
        };
        let (root, path, elem) = self.list_place("pop", list)?;
        Ok(self.emit_to(elem, |dst| Inst::Pop { dst, root, path }))
    }

    pub(crate) fn addr_of(&mut self, pos: Pos, place: Place) -> Result<Reg, Error> {
        if self.env.owns(place.ty()) {
            let msg = format!(
                "cannot take the address of {}: it owns its memory, which only its variable may free; use a path",
                place.ty()
            );
            return Err(Error::new(pos, msg));
        }
        match place {
            Place::Mem { ptr, .. } => Ok(ptr),
            Place::Cell { .. } => Err(Error::new(pos, "cannot take the address of what a cell holds")),
            Place::Reg { readonly: Some(what), .. } => Err(Error::new(pos, format!("cannot take the address of {what}"))),
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
