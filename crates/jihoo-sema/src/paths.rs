//! Paths: references that are data.
//!
//! `path(Graph).nodes[_].edges[_]` is the type of a way to get from a `Graph`
//! value to one of its parts. A path value holds only the indices (one `i64`
//! for each `[_]`), so it can be stored, returned and sent anywhere: it points
//! into nothing, and so cannot dangle. Following it needs the root, `g[p]`,
//! which is an ordinary place: `g[p]` is `g.nodes[i].edges[j]`, read, written
//! and bounds-checked like that, with no other cost.
//!
//! A path type is a struct of its indices, named for its shape
//! (`path(Graph).nodes[_]`), so JIR, the VM and LLVM see nothing new. A path
//! stays inside its root's value: it steps through struct fields and array
//! and list elements, never through a pointer, `ref` or `cell`.

use std::rc::Rc;

use jihoo_ir::{Inst, Reg, Type};
use jihoo_syntax::ast::{Expr, PathShape};
use jihoo_syntax::{Error, Pos};

use crate::env::{Env, Fields};
use crate::place::{Place, Step};
use crate::FnCx;

pub(crate) struct PathDef {
    pub root: Type,
    pub shape: Vec<PathShape>,
    /// Where the type was first written: field names are looked up, and their
    /// privacy checked, from this module.
    pub module: usize,
    pub pos: Pos,
}

impl PathDef {
    fn holes(&self) -> usize {
        self.shape.iter().filter(|s| **s == PathShape::Elem).count()
    }

    pub fn fields(&self) -> Fields {
        Rc::new((0..self.holes()).map(|k| (format!("i{k}"), Type::I64)).collect())
    }
}

/// A step of a resolved path, with the type it leads to.
pub(crate) enum Resolved {
    Field(u32),
    /// An element, at the path value's index number `k`.
    Elem(u32),
}

fn spelling(root: &Type, shape: &[PathShape]) -> String {
    let mut s = format!("path({root})");
    for step in shape {
        match step {
            PathShape::Field(n) => {
                s.push('.');
                s.push_str(n);
            }
            PathShape::Elem => s.push_str("[_]"),
        }
    }
    s
}

impl Env<'_> {
    /// The path type from `root` along `shape`, written in module `module`.
    pub fn path_type(&self, pos: Pos, root: Type, shape: Vec<PathShape>, module: usize) -> Result<Type, Error> {
        if !matches!(&root, Type::Struct(n) if self.path_named(n).is_none()) {
            return Err(Error::new(pos, format!("a path starts at a struct, not {root}")));
        }
        let name = spelling(&root, &shape);
        if self.path_named(&name).is_none() {
            let def = PathDef { root, shape, module, pos };
            self.paths.borrow_mut().push((name.clone(), Rc::new(def)));
        }
        Ok(Type::Struct(name))
    }

    pub fn path_named(&self, name: &str) -> Option<Rc<PathDef>> {
        self.paths.borrow().iter().find(|(n, _)| n == name).map(|(_, d)| d.clone())
    }

    pub fn path_def(&self, t: &Type) -> Option<Rc<PathDef>> {
        match t {
            Type::Struct(n) => self.path_named(n),
            _ => None,
        }
    }

    /// Every path type so far, in creation order.
    pub fn path_types(&self) -> Vec<Type> {
        self.paths.borrow().iter().map(|(n, _)| Type::Struct(n.clone())).collect()
    }

    /// The steps of path type `def` and the type of each part they lead to.
    pub fn path_steps(&self, pos: Pos, def: &PathDef) -> Result<Vec<(Resolved, Type)>, Error> {
        let mut cur = def.root.clone();
        let mut steps = Vec::new();
        let mut hole = 0;
        for step in &def.shape {
            let (r, next) = match (step, &cur) {
                (PathShape::Field(n), Type::Struct(s)) if self.path_named(s).is_none() => {
                    let (index, ty) = self.field(pos, &cur, n, def.module)?;
                    (Resolved::Field(index), ty)
                }
                (PathShape::Elem, Type::Array(elem, _) | Type::List(elem)) => {
                    hole += 1;
                    (Resolved::Elem(hole - 1), (**elem).clone())
                }
                (PathShape::Field(n), t) => {
                    let msg = format!("a path cannot take `.{n}` of {t} (paths go through struct fields and elements only)");
                    return Err(Error::new(pos, msg));
                }
                (PathShape::Elem, t) => {
                    let msg = format!("a path cannot take an element of {t} (paths go through struct fields and elements only)");
                    return Err(Error::new(pos, msg));
                }
            };
            steps.push((r, next.clone()));
            cur = next;
        }
        Ok(steps)
    }
}

impl FnCx<'_> {
    /// `path(T)`: the empty path from a `T` to itself.
    pub(crate) fn path_root(&mut self, pos: Pos, t: &jihoo_syntax::ast::TypeExpr) -> Result<Reg, Error> {
        let root = self.resolve(t)?;
        let ty = self.env.path_type(pos, root, vec![], self.bindings.module)?;
        let Type::Struct(name) = ty.clone() else { unreachable!() };
        Ok(self.emit_to(ty, |dst| Inst::Struct { dst, name, fields: vec![] }))
    }

    /// `p.name` or `p[i]` on a path `p`: the path one step further. `index`
    /// is the new element's index, for `[i]`.
    pub(crate) fn extend_path(&mut self, pos: Pos, base: Place, step: PathShape, index: Option<Reg>) -> Result<Place, Error> {
        let def = self.env.path_def(base.ty()).expect("a path");
        let mut shape = def.shape.clone();
        shape.push(step);
        let ty = self.env.path_type(pos, def.root.clone(), shape, def.module)?;
        let new = self.env.path_def(&ty).unwrap();
        self.env.path_steps(pos, &new)?;
        let old = self.read(base);
        let mut fields: Vec<Reg> =
            (0..def.holes() as u32).map(|k| self.emit_to(Type::I64, |dst| Inst::Field { dst, src: old, index: k })).collect();
        fields.extend(index);
        let Type::Struct(name) = ty.clone() else { unreachable!() };
        let root = self.emit_to(ty.clone(), |dst| Inst::Struct { dst, name, fields });
        Ok(Place::Reg { root, path: vec![], ty, readonly: Some("a temporary value") })
    }

    /// Whether `r` holds a path.
    pub(crate) fn is_path(&self, r: Reg) -> bool {
        self.env.path_def(self.ty(r)).is_some()
    }

    /// `root[p]`: the part of the value at `root` that path `p` leads to.
    pub(crate) fn follow_path(&mut self, index: &Expr, root: Place, p: Reg) -> Result<Place, Error> {
        let Some(def) = self.env.path_def(self.ty(p)) else {
            let msg = format!("cannot index into {} with {} (a struct is indexed with a path)", root.ty(), self.ty(p));
            return Err(Error::new(index.pos, msg));
        };
        if def.root != *root.ty() {
            let msg = format!("{} leads from `{}`, not from `{}`", self.ty(p), def.root, root.ty());
            return Err(Error::new(index.pos, msg));
        }
        let mut place = root;
        for (step, ty) in self.env.path_steps(index.pos, &def)? {
            place = match step {
                Resolved::Field(i) => self.step(place, Step::Field(i), ty),
                Resolved::Elem(k) => {
                    let i = self.emit_to(Type::I64, |dst| Inst::Field { dst, src: p, index: k });
                    self.step(place, Step::Elem(i), ty)
                }
            };
        }
        Ok(place)
    }
}
