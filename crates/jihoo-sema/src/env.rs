//! Module-level queries, computed lazily and memoized.
//!
//! Struct fields, function signatures, constants and lowered functions are only
//! computed when something asks for them, so items can refer to each other in any
//! order, and compile-time evaluation can ask for any function it needs. A query
//! that (directly or indirectly) asks for itself is a cycle and becomes an error.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use jihoo_ir as ir;
use jihoo_ir::{layout, Profile, Type};
use jihoo_syntax::ast::*;
use jihoo_syntax::{Error, Pos};

use crate::comptime::ConstValue;
use crate::{is_builtin, FnCx};

#[derive(Debug, Clone)]
pub(crate) struct Sig {
    pub params: Vec<Type>,
    pub ret: Type,
}

pub(crate) type Fields = Rc<Vec<(String, Type)>>;

/// Memoized results; `None` marks a query that is still being computed.
struct Memo<T> {
    map: RefCell<HashMap<String, Option<Result<T, Error>>>>,
}

impl<T: Clone> Memo<T> {
    fn new() -> Self {
        Memo { map: RefCell::new(HashMap::new()) }
    }

    fn in_progress(&self, key: &str) -> bool {
        matches!(self.map.borrow().get(key), Some(None))
    }

    fn get(
        &self,
        key: &str,
        cycle: impl FnOnce() -> Error,
        compute: impl FnOnce() -> Result<T, Error>,
    ) -> Result<T, Error> {
        if let Some(state) = self.map.borrow().get(key) {
            return match state {
                Some(r) => r.clone(),
                None => Err(cycle()),
            };
        }
        // No borrow is held while computing: `compute` may run other queries.
        self.map.borrow_mut().insert(key.to_string(), None);
        let r = compute();
        self.map.borrow_mut().insert(key.to_string(), Some(r.clone()));
        r
    }
}

pub(crate) struct Env<'p> {
    pub profile: Profile,
    struct_decls: HashMap<&'p str, &'p StructDecl>,
    fn_decls: HashMap<&'p str, &'p FnDecl>,
    const_decls: HashMap<&'p str, &'p ConstDecl>,
    structs: Memo<Fields>,
    sigs: Memo<Rc<Sig>>,
    consts: Memo<Rc<(Type, ConstValue)>>,
    funcs: Memo<Rc<ir::Function>>,
    /// Numbers the helper functions made for `comptime` expressions.
    pub comptime_ids: Cell<u32>,
}

impl<'p> Env<'p> {
    /// Indexes the program's items, reporting duplicate and reserved names.
    pub fn new(profile: Profile, prog: &'p Program) -> (Self, Vec<Error>) {
        let mut errors = Vec::new();
        let mut env = Env {
            profile,
            struct_decls: HashMap::new(),
            fn_decls: HashMap::new(),
            const_decls: HashMap::new(),
            structs: Memo::new(),
            sigs: Memo::new(),
            consts: Memo::new(),
            funcs: Memo::new(),
            comptime_ids: Cell::new(0),
        };
        for s in &prog.structs {
            if Type::from_name(&s.name).is_some() {
                errors.push(Error::new(s.pos, format!("`{}` is a builtin type name", s.name)));
            } else if env.struct_decls.insert(&s.name, s).is_some() {
                errors.push(Error::new(s.pos, format!("struct `{}` is defined twice", s.name)));
            }
        }
        for f in &prog.funcs {
            if is_builtin(&f.name) {
                errors.push(Error::new(f.pos, format!("`{}` is a builtin and cannot be redefined", f.name)));
            } else if env.fn_decls.insert(&f.name, f).is_some() {
                errors.push(Error::new(f.pos, format!("function `{}` is defined twice", f.name)));
            }
        }
        for c in &prog.consts {
            if env.const_decls.insert(&c.name, c).is_some() {
                errors.push(Error::new(c.pos, format!("constant `{}` is defined twice", c.name)));
            }
        }
        (env, errors)
    }

    // ---- types ----

    pub fn resolve(&self, t: &TypeExpr) -> Result<Type, Error> {
        match &t.kind {
            TypeExprKind::Ptr(inner) => {
                if self.profile != Profile::Freestanding {
                    return Err(Error::new(t.pos, "pointer types are only available in freestanding mode"));
                }
                Ok(Type::ptr(self.resolve(inner)?))
            }
            TypeExprKind::Array(elem, n) => Ok(Type::array(self.resolve(elem)?, self.array_len(n)?)),
            TypeExprKind::Named(name) => {
                if let Some(ty) = Type::from_name(name) {
                    if ty == Type::Str && self.profile == Profile::Freestanding {
                        return Err(Error::new(
                            t.pos,
                            "type `str` is garbage collected and is not available in freestanding mode (use `*u8`)",
                        ));
                    }
                    Ok(ty)
                } else if self.struct_decls.contains_key(name.as_str()) {
                    Ok(Type::Struct(name.clone()))
                } else if name == "ptr" {
                    Err(Error::new(t.pos, "unknown type `ptr`; byte pointers are written `*u8`"))
                } else {
                    Err(Error::new(t.pos, format!("unknown type `{name}`")))
                }
            }
        }
    }

    /// An array length: any integer expression, evaluated at compile time.
    pub fn array_len(&self, e: &Expr) -> Result<u64, Error> {
        if let ExprKind::Int(n) = e.kind {
            return Ok(n as u64); // the lexer only produces non-negative literals
        }
        let (ty, v) = self.comptime(e, Some(&Type::I64))?;
        match (ty, v) {
            (Type::Int(_), ConstValue::Int(n)) if n >= 0 => Ok(n as u64),
            // Negative, or a u64 above i64::MAX (stored as a negative bit pattern).
            (Type::Int(_), ConstValue::Int(_)) => {
                Err(Error::new(e.pos, "array length must be between 0 and 2^63 - 1"))
            }
            (ty, _) => Err(Error::new(e.pos, format!("array length must be an integer, found {ty}"))),
        }
    }

    pub fn struct_fields(&self, pos: Pos, name: &str) -> Result<Fields, Error> {
        let decl = *self
            .struct_decls
            .get(name)
            .ok_or_else(|| Error::new(pos, format!("unknown struct `{name}`")))?;
        self.structs.get(
            name,
            || Error::new(decl.pos, format!("struct `{name}` depends on itself")),
            || {
                let mut fields: Vec<(String, Type)> = Vec::new();
                for f in &decl.fields {
                    if fields.iter().any(|(n, _)| *n == f.name) {
                        return Err(Error::new(f.pos, format!("field `{}` is declared twice", f.name)));
                    }
                    fields.push((f.name.clone(), self.resolve(&f.ty)?));
                }
                Ok(Rc::new(fields))
            },
        )
    }

    /// A struct may not contain itself by value; use a pointer instead.
    pub fn check_acyclic(&self, name: &str) -> Result<(), Error> {
        self.acyclic(name, name, &mut Vec::new())
    }

    fn acyclic(&self, root: &str, name: &str, stack: &mut Vec<String>) -> Result<(), Error> {
        let pos = self.struct_decls[root].pos;
        if stack.iter().any(|s| s == name) {
            let path = stack.join(" -> ");
            return Err(Error::new(pos, format!("struct `{root}` contains itself ({path} -> {name}); use a pointer")));
        }
        stack.push(name.to_string());
        for (_, t) in self.struct_fields(pos, name)?.iter() {
            if let Some(inner) = struct_by_value(t) {
                self.acyclic(root, inner, stack)?;
            }
        }
        stack.pop();
        Ok(())
    }

    /// Index and type of field `name` of struct type `t`.
    pub fn field(&self, pos: Pos, t: &Type, name: &str) -> Result<(u32, Type), Error> {
        let Type::Struct(s) = t else {
            return Err(Error::new(pos, format!("type {t} has no fields")));
        };
        self.struct_fields(pos, s)?
            .iter()
            .enumerate()
            .find(|(_, (n, _))| n == name)
            .map(|(i, (_, t))| (i as u32, t.clone()))
            .ok_or_else(|| Error::new(pos, format!("struct `{s}` has no field `{name}`")))
    }

    /// Type of field `index` of `t`, which was already resolved.
    pub fn field_type(&self, t: &Type, index: u32) -> Type {
        let Type::Struct(s) = t else { unreachable!("not a struct: {t}") };
        let fields = self.struct_fields(Pos { line: 0, col: 0 }, s).expect("struct was resolved before");
        fields[index as usize].1.clone()
    }

    pub fn layout(&self, pos: Pos, t: &Type) -> Result<layout::Layout, Error> {
        if let Some(s) = struct_by_value(t) {
            self.check_acyclic(s)?;
        }
        let fields = |name: &str| {
            let fields = self.struct_fields(pos, name).ok()?;
            Some(fields.iter().map(|(_, t)| t.clone()).collect())
        };
        layout::of(t, &fields)
            .ok_or_else(|| Error::new(pos, format!("type {t} has no fixed memory layout (it contains a GC reference)")))
    }

    // ---- functions and constants ----

    pub fn has_function(&self, name: &str) -> bool {
        self.fn_decls.contains_key(name)
    }

    /// The signature of function `name`, or `None` if there is no such function.
    pub fn signature(&self, name: &str) -> Option<Result<Rc<Sig>, Error>> {
        let decl = *self.fn_decls.get(name)?;
        Some(self.sigs.get(
            name,
            || Error::new(decl.pos, format!("the signature of `{name}` depends on itself")),
            || {
                let params = decl.params.iter().map(|p| self.resolve(&p.ty)).collect::<Result<_, _>>()?;
                let ret = match &decl.ret {
                    Some(t) => self.resolve(t)?,
                    None => Type::Unit,
                };
                Ok(Rc::new(Sig { params, ret }))
            },
        ))
    }

    /// True while `name` is being lowered (so it cannot run at compile time yet).
    pub fn function_in_progress(&self, name: &str) -> bool {
        self.funcs.in_progress(name)
    }

    /// The JIR of function `name`, which must exist.
    pub fn function(&self, name: &str) -> Result<Rc<ir::Function>, Error> {
        let decl = self.fn_decls[name];
        self.funcs.get(
            name,
            || Error::new(decl.pos, format!("`{name}` is needed at compile time while it is being compiled")),
            || {
                let sig = self.signature(name).unwrap()?;
                Ok(Rc::new(FnCx::new(self, sig).lower_fn(decl)?))
            },
        )
    }

    /// The type and value of constant `name`, or `None` if there is no such constant.
    pub fn constant(&self, name: &str) -> Option<Result<Rc<(Type, ConstValue)>, Error>> {
        let decl = *self.const_decls.get(name)?;
        Some(self.consts.get(
            name,
            || Error::new(decl.pos, format!("constant `{name}` depends on itself")),
            || {
                let want = decl.ty.as_ref().map(|t| self.resolve(t)).transpose()?;
                let (ty, v) = self.comptime(&decl.value, want.as_ref())?;
                if let Some(want) = want {
                    if want != ty {
                        let msg = format!("the value of `{name}` must be {want}, found {ty}");
                        return Err(Error::new(decl.value.pos, msg));
                    }
                }
                if ty == Type::Unit {
                    return Err(Error::new(decl.pos, format!("`{name}` would have type unit; this expression has no value")));
                }
                Ok(Rc::new((ty, v)))
            },
        ))
    }

    pub fn check_entry(&self, prog: &Program) -> Result<(), Error> {
        let entry = self.profile.entry();
        let Some(decl) = prog.funcs.iter().find(|f| f.name == entry) else {
            let pos = Pos { line: 1, col: 1 };
            return Err(Error::new(pos, format!("{} program needs `fn {entry}()`", self.profile.as_str())));
        };
        let sig = self.signature(entry).unwrap()?;
        if !sig.params.is_empty() {
            return Err(Error::new(decl.pos, format!("`{entry}` must not take parameters")));
        }
        if !matches!(sig.ret, Type::Unit | Type::I64) {
            return Err(Error::new(decl.pos, format!("`{entry}` must return nothing or i64, not {}", sig.ret)));
        }
        Ok(())
    }
}

/// The struct `t` holds by value, looking through arrays (pointers break cycles).
fn struct_by_value(t: &Type) -> Option<&str> {
    match t {
        Type::Struct(s) => Some(s),
        Type::Array(elem, _) => struct_by_value(elem),
        _ => None,
    }
}
