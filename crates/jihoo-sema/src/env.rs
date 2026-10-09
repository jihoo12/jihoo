//! Module-level queries, computed lazily and memoized.
//!
//! Struct fields, function signatures, constants and lowered functions are only
//! computed when something asks for them, so items can refer to each other in any
//! order, and compile-time evaluation can ask for any function it needs. A query
//! that (directly or indirectly) asks for itself is a cycle and becomes an error.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use jihoo_ir as ir;
use jihoo_ir::{layout, Profile, Type};
use jihoo_syntax::ast::*;
use jihoo_syntax::loader::Module;
use jihoo_syntax::{Error, Pos};

use crate::comptime::ConstValue;
use crate::generic::{is_type_param, Binding, Bindings};
use crate::{is_builtin, FnCx};

#[derive(Debug, Clone)]
pub(crate) struct Sig {
    pub params: Vec<Type>,
    pub ret: Type,
}

pub(crate) type Fields = Rc<Vec<(String, Type)>>;
/// The variants of an enum: names and payload types.
pub(crate) type Variants = Rc<Vec<(String, Vec<Type>)>>;

/// An instance of a generic function.
struct Instance {
    name: String,
    fn_name: String,
    bindings: Rc<Bindings>,
    sig: Result<Rc<Sig>, Error>,
}

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

/// What the checker needs to know about a module.
struct ModInfo {
    /// Prefix of the module's items; empty for the root module.
    name: String,
    imports: HashMap<String, usize>,
    /// The scope of code at the top level of the module.
    root: Rc<Bindings>,
}

pub(crate) struct Env<'p> {
    pub profile: Profile,
    modules: Vec<ModInfo>,
    /// Declarations by key: the module-qualified name, such as `alloc.push`
    /// (root-module items keep their plain names). Values are (module, decl).
    struct_decls: HashMap<String, (usize, &'p StructDecl)>,
    fn_decls: HashMap<String, (usize, &'p FnDecl)>,
    const_decls: HashMap<String, (usize, &'p ConstDecl)>,
    structs: Memo<Fields>,
    enums: Memo<Variants>,
    sigs: Memo<Rc<Sig>>,
    consts: Memo<Rc<(Type, ConstValue)>>,
    funcs: Memo<Rc<ir::Function>>,
    /// Numbers the helper functions made for `comptime` expressions.
    pub comptime_ids: Cell<u32>,
    /// The next number `unique` hands out, across every compile-time run.
    pub uniques: Cell<u64>,
    /// Generic instances by key, their keys by name, and the ones not compiled yet.
    instances: RefCell<HashMap<String, Instance>>,
    /// Instances of generic structs: names by key, and (name, declaration name,
    /// bindings) in creation order. Names rather than `&StructDecl` keep `Env`
    /// covariant in `'p`.
    struct_keys: RefCell<HashMap<String, String>>,
    struct_instances: RefCell<Vec<(String, String, Rc<Bindings>)>>,
    instance_keys: RefCell<HashMap<String, String>>,
    pending: RefCell<VecDeque<String>>,
}

impl<'p> Env<'p> {
    /// Indexes every module's items, reporting duplicate and reserved names.
    pub fn new(profile: Profile, mods: &'p [Module]) -> (Self, Vec<Error>) {
        let mut errors = Vec::new();
        let modules = mods
            .iter()
            .enumerate()
            .map(|(i, m)| ModInfo {
                name: m.name.clone(),
                imports: m.imports.clone(),
                root: Rc::new(Bindings::in_module(i)),
            })
            .collect();
        let mut env = Env {
            profile,
            modules,
            struct_decls: HashMap::new(),
            fn_decls: HashMap::new(),
            const_decls: HashMap::new(),
            structs: Memo::new(),
            enums: Memo::new(),
            sigs: Memo::new(),
            consts: Memo::new(),
            funcs: Memo::new(),
            comptime_ids: Cell::new(0),
            uniques: Cell::new(0),
            instances: RefCell::new(HashMap::new()),
            struct_keys: RefCell::new(HashMap::new()),
            struct_instances: RefCell::new(Vec::new()),
            instance_keys: RefCell::new(HashMap::new()),
            pending: RefCell::new(VecDeque::new()),
        };
        for (m, module) in mods.iter().enumerate() {
            let prog = &module.program;
            for s in &prog.structs {
                if Type::from_name(&s.name).is_some() {
                    errors.push(Error::new(s.pos, format!("`{}` is a builtin type name", s.name)));
                } else if env.struct_decls.insert(env.item_key(m, &s.name), (m, s)).is_some() {
                    errors.push(Error::new(s.pos, format!("type `{}` is defined twice", s.name)));
                }
            }
            for f in &prog.funcs {
                if is_builtin(&f.name) {
                    errors.push(Error::new(f.pos, format!("`{}` is a builtin and cannot be redefined", f.name)));
                } else if env.fn_decls.insert(env.item_key(m, &f.name), (m, f)).is_some() {
                    errors.push(Error::new(f.pos, format!("function `{}` is defined twice", f.name)));
                }
            }
            for c in &prog.consts {
                if env.const_decls.insert(env.item_key(m, &c.name), (m, c)).is_some() {
                    errors.push(Error::new(c.pos, format!("constant `{}` is defined twice", c.name)));
                }
            }
        }
        (env, errors)
    }

    // ---- modules ----

    fn item_key(&self, module: usize, item: &str) -> String {
        let prefix = &self.modules[module].name;
        if prefix.is_empty() {
            item.to_string()
        } else {
            format!("{prefix}.{item}")
        }
    }

    /// The key of `name` as written in `module`: `item` is that module's own
    /// item, `alias.item` an item of a module it imports. `None` for an unknown
    /// alias.
    pub fn key(&self, module: usize, name: &str) -> Option<String> {
        match name.split_once('.') {
            Some((alias, item)) => {
                let target = *self.modules[module].imports.get(alias)?;
                Some(self.item_key(target, item))
            }
            None => Some(self.item_key(module, name)),
        }
    }

    /// True if `module` imports a module under the name `alias`.
    pub fn is_alias(&self, module: usize, alias: &str) -> bool {
        self.modules[module].imports.contains_key(alias)
    }

    /// The scope at the top level of `module`.
    pub fn root(&self, module: usize) -> &Rc<Bindings> {
        &self.modules[module].root
    }

    /// The module that function `key` is declared in.
    pub fn fn_module(&self, key: &str) -> usize {
        self.fn_decls[key].0
    }

    /// Checks that code in `from` may use item `key`: its own module's items, or
    /// `pub` items of other modules.
    pub fn check_visible(&self, pos: Pos, from: usize, key: &str) -> Result<(), Error> {
        let item = self
            .fn_decls
            .get(key)
            .map(|&(m, d)| (m, d.is_pub))
            .or_else(|| self.struct_decls.get(key).map(|&(m, d)| (m, d.is_pub)))
            .or_else(|| self.const_decls.get(key).map(|&(m, d)| (m, d.is_pub)));
        match item {
            Some((m, false)) if m != from => {
                let module = &self.modules[m].name;
                Err(Error::new(pos, format!("`{key}` is private to module `{module}` (mark it `pub` to use it here)")))
            }
            _ => Ok(()),
        }
    }

    /// `name` as written in scope `b`, resolved to a key, or an error that says
    /// which module alias is unknown.
    pub fn key_or_err(&self, pos: Pos, b: &Bindings, name: &str) -> Result<String, Error> {
        self.key(b.module, name).ok_or_else(|| {
            let alias = name.split('.').next().unwrap_or(name);
            Error::new(pos, format!("unknown module `{alias}` (is it imported?)"))
        })
    }

    // ---- types ----

    /// Resolves a type expression. `b` gives the comptime parameters in scope.
    pub fn resolve(&self, t: &TypeExpr, b: &Rc<Bindings>) -> Result<Type, Error> {
        self.resolve_in(t, b, false)
    }

    /// Like `resolve`; `in_macro` also allows the types that only exist while
    /// compiling: `expr`, and `str` in freestanding programs.
    pub fn resolve_in(&self, t: &TypeExpr, b: &Rc<Bindings>, in_macro: bool) -> Result<Type, Error> {
        match &t.kind {
            TypeExprKind::Ptr(inner) => {
                if self.profile != Profile::Freestanding {
                    return Err(Error::new(t.pos, "pointer types are only available in freestanding mode"));
                }
                Ok(Type::ptr(self.resolve_in(inner, b, in_macro)?))
            }
            TypeExprKind::Array(elem, n) => {
                Ok(Type::array(self.resolve_in(elem, b, in_macro)?, self.array_len(n, b)?))
            }
            TypeExprKind::Fn(params, ret) => {
                let params = params.iter().map(|p| self.resolve_in(p, b, in_macro)).collect::<Result<_, _>>()?;
                let ret = match ret {
                    Some(r) => self.resolve_in(r, b, in_macro)?,
                    None => Type::Unit,
                };
                Ok(Type::Fn(params, Box::new(ret)))
            }
            TypeExprKind::Generic(name, args) => {
                let key = self.key_or_err(t.pos, b, name)?;
                self.check_visible(t.pos, b.module, &key)?;
                let Some((dm, decl)) = self.struct_decls.get(&key).copied() else {
                    return Err(Error::new(t.pos, format!("unknown type `{name}`")));
                };
                let kind = decl.kind();
                if decl.params.is_empty() {
                    return Err(Error::new(t.pos, format!("{kind} `{name}` takes no arguments")));
                }
                if decl.params.len() != args.len() {
                    let msg = format!("{kind} `{name}` takes {} arguments, {} given", decl.params.len(), args.len());
                    return Err(Error::new(t.pos, msg));
                }
                let bindings = self.bind(name, dm, decl.params.iter().zip(args), b)?;
                Ok(named(decl, self.struct_instance(&key, bindings)))
            }
            TypeExprKind::Named(name) => {
                match b.get(name) {
                    Some(Binding::Type(t)) => return Ok(t.clone()),
                    Some(Binding::Value(..)) => {
                        return Err(Error::new(t.pos, format!("`{name}` is a value, not a type")))
                    }
                    None => {}
                }
                if name == "type" {
                    return Err(Error::new(t.pos, "`type` can only be the type of a `comptime` parameter"));
                }
                if let Some(code) = match name.as_str() {
                    "expr" => Some(Type::Expr),
                    "stmts" => Some(Type::Stmts),
                    "items" => Some(Type::Items),
                    _ => None,
                } {
                    return if in_macro {
                        Ok(code)
                    } else {
                        Err(Error::new(t.pos, format!("`{name}` (a piece of code) is only available in macros")))
                    };
                }
                if let Some(ty) = Type::from_name(name) {
                    if ty == Type::Str && self.profile == Profile::Freestanding && !in_macro {
                        return Err(Error::new(
                            t.pos,
                            "type `str` is garbage collected and is not available in freestanding mode (use `*u8`)",
                        ));
                    }
                    Ok(ty)
                } else if let Some(&(_, decl)) = self.key(b.module, name).and_then(|k| self.struct_decls.get(&k)) {
                    if !decl.params.is_empty() {
                        let kind = decl.kind();
                        return Err(Error::new(t.pos, format!("{kind} `{name}` is generic; write `{name}(...)`")));
                    }
                    let key = self.key(b.module, name).unwrap();
                    self.check_visible(t.pos, b.module, &key)?;
                    Ok(named(decl, key))
                } else if name.contains('.') && self.key(b.module, name).is_none() {
                    Err(self.key_or_err(t.pos, b, name).unwrap_err())
                } else if name == "ptr" {
                    Err(Error::new(t.pos, "unknown type `ptr`; byte pointers are written `*u8`"))
                } else {
                    Err(Error::new(t.pos, format!("unknown type `{name}`")))
                }
            }
        }
    }

    /// An array length: any integer expression, evaluated at compile time.
    pub fn array_len(&self, e: &Expr, b: &Rc<Bindings>) -> Result<u64, Error> {
        if let ExprKind::Int(n) = e.kind {
            return Ok(n as u64); // the lexer only produces non-negative literals
        }
        let (ty, v) = self.comptime(e, Some(&Type::I64), b)?;
        match (ty, v) {
            (Type::Int(_), ConstValue::Int(n)) if n >= 0 => Ok(n as u64),
            // Negative, or a u64 above i64::MAX (stored as a negative bit pattern).
            (Type::Int(_), ConstValue::Int(_)) => {
                Err(Error::new(e.pos, "array length must be between 0 and 2^63 - 1"))
            }
            (ty, _) => Err(Error::new(e.pos, format!("array length must be an integer, found {ty}"))),
        }
    }

    /// A struct's declaration and the bindings of its parameters (empty unless
    /// `name` is an instance of a generic struct).
    fn struct_decl(&self, pos: Pos, name: &str) -> Result<(&'p StructDecl, Rc<Bindings>), Error> {
        if let Some(&(m, d)) = self.struct_decls.get(name) {
            return Ok((d, self.root(m).clone()));
        }
        let instances = self.struct_instances.borrow();
        let found = instances.iter().find(|(n, _, _)| n == name);
        found
            .map(|(_, d, b)| (self.struct_decls[d.as_str()].1, b.clone()))
            .ok_or_else(|| Error::new(pos, format!("unknown struct `{name}`")))
    }

    /// The name of the instance of generic struct `decl_key` for `bindings`,
    /// which is how it reads in messages: `Pair(i64)`.
    fn struct_instance(&self, decl_key: &str, bindings: Bindings) -> String {
        let key = format!("{decl_key}({})", bindings.key());
        if let Some(name) = self.struct_keys.borrow().get(&key) {
            return name.clone();
        }
        let mut name = format!("{decl_key}({})", bindings.args());
        if self.struct_instances.borrow().iter().any(|(n, _, _)| *n == name) {
            name = format!("{name}#{}", self.struct_instances.borrow().len());
        }
        self.struct_keys.borrow_mut().insert(key, name.clone());
        self.struct_instances.borrow_mut().push((name.clone(), decl_key.to_string(), Rc::new(bindings)));
        name
    }

    /// Every generic struct and enum instance so far, in creation order, as types.
    pub fn struct_instances(&self) -> Vec<Type> {
        let instances = self.struct_instances.borrow();
        instances.iter().map(|(n, d, _)| named(self.struct_decls[d.as_str()].1, n.clone())).collect()
    }

    /// The key of the generic declaration that type `name` is an instance of.
    pub fn instance_decl(&self, name: &str) -> Option<String> {
        self.struct_instances.borrow().iter().find(|(n, _, _)| n == name).map(|(_, d, _)| d.clone())
    }

    /// The instance of generic struct or enum `decl_key` with type arguments `args`.
    pub fn instance_of(&self, decl_key: &str, args: Vec<Type>) -> Type {
        let decl = self.struct_decls[decl_key].1;
        let mut b = Bindings::in_module(self.struct_decls[decl_key].0);
        for (p, t) in decl.params.iter().zip(args) {
            b.push(&p.name, Binding::Type(t));
        }
        named(decl, self.struct_instance(decl_key, b))
    }

    /// The declaration of the struct or enum `key` (a module-qualified name).
    pub fn type_decl(&self, key: &str) -> Option<(usize, &'p StructDecl)> {
        self.struct_decls.get(key).copied()
    }

    pub fn struct_fields(&self, pos: Pos, name: &str) -> Result<Fields, Error> {
        let (decl, bindings) = self.struct_decl(pos, name)?;
        if !decl.params.is_empty() && bindings.is_empty() {
            return Err(Error::new(pos, format!("struct `{name}` is generic; write `{name}(...)`")));
        }
        if decl.is_enum() {
            return Err(Error::new(pos, format!("`{name}` is an enum; it has variants, not fields")));
        }
        self.structs.get(
            name,
            || Error::new(decl.pos, format!("struct `{name}` depends on itself")),
            || {
                let mut fields: Vec<(String, Type)> = Vec::new();
                for f in &decl.fields {
                    if fields.iter().any(|(n, _)| *n == f.name) {
                        return Err(Error::new(f.pos, format!("field `{}` is declared twice", f.name)));
                    }
                    let ty = self.resolve(&f.ty, &bindings).map_err(|mut e| {
                        if !bindings.is_empty() {
                            e.msg = format!("{} (in `{name}`)", e.msg);
                        }
                        e
                    })?;
                    fields.push((f.name.clone(), ty));
                }
                Ok(Rc::new(fields))
            },
        )
    }

    /// The variants of enum `name` (an instance name for generic enums).
    pub fn enum_variants(&self, pos: Pos, name: &str) -> Result<Variants, Error> {
        let (decl, bindings) = self.struct_decl(pos, name)?;
        if !decl.params.is_empty() && bindings.is_empty() {
            return Err(Error::new(pos, format!("enum `{name}` is generic; write `{name}(...)`")));
        }
        let Some(decls) = &decl.variants else {
            return Err(Error::new(pos, format!("`{name}` is a struct, not an enum")));
        };
        self.enums.get(
            name,
            || Error::new(decl.pos, format!("enum `{name}` depends on itself")),
            || {
                let mut variants: Vec<(String, Vec<Type>)> = Vec::new();
                for v in decls {
                    if variants.iter().any(|(n, _)| *n == v.name) {
                        return Err(Error::new(v.pos, format!("variant `{}` is declared twice", v.name)));
                    }
                    let tys = v.fields.iter().map(|t| self.resolve(t, &bindings)).collect::<Result<_, _>>();
                    let tys = tys.map_err(|mut e| {
                        if !bindings.is_empty() {
                            e.msg = format!("{} (in `{name}`)", e.msg);
                        }
                        e
                    })?;
                    variants.push((v.name.clone(), tys));
                }
                if variants.len() > u32::MAX as usize {
                    return Err(Error::new(decl.pos, format!("enum `{name}` has too many variants")));
                }
                Ok(Rc::new(variants))
            },
        )
    }

    /// The fields of a struct or the payload types of every variant of an enum.
    fn members(&self, pos: Pos, t: &Type) -> Result<Vec<Vec<Type>>, Error> {
        Ok(match t {
            Type::Struct(name) => vec![self.struct_fields(pos, name)?.iter().map(|(_, t)| t.clone()).collect()],
            Type::Enum(name) => self.enum_variants(pos, name)?.iter().map(|(_, ts)| ts.clone()).collect(),
            _ => vec![],
        })
    }

    /// A struct or enum may not contain itself by value; use a pointer instead.
    pub fn check_acyclic(&self, t: &Type) -> Result<(), Error> {
        self.acyclic(t, t, &mut Vec::new())
    }

    fn acyclic(&self, root: &Type, t: &Type, stack: &mut Vec<String>) -> Result<(), Error> {
        let (Type::Struct(root_name) | Type::Enum(root_name)) = root else { return Ok(()) };
        let decl = self.struct_decl(Pos::new(1, 1), root_name)?.0;
        let name = t.to_string();
        if stack.contains(&name) {
            let path = stack.join(" -> ");
            let msg = format!("{} `{root}` contains itself ({path} -> {name}); use a pointer", decl.kind());
            return Err(Error::new(decl.pos, msg));
        }
        stack.push(name);
        for inner in self.members(decl.pos, t)?.iter().flatten() {
            if let Some(inner) = named_by_value(inner) {
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
        let fields = self.struct_fields(Pos::default(), s).expect("struct was resolved before");
        fields[index as usize].1.clone()
    }

    /// The payload types of variant `index` of enum type `t`, which was already resolved.
    pub fn payload_types(&self, t: &Type, index: u32) -> Vec<Type> {
        let Type::Enum(e) = t else { unreachable!("not an enum: {t}") };
        let variants = self.enum_variants(Pos::default(), e).expect("enum was resolved before");
        variants[index as usize].1.clone()
    }

    pub fn layout(&self, pos: Pos, t: &Type) -> Result<layout::Layout, Error> {
        if let Some(s) = named_by_value(t) {
            self.check_acyclic(s)?;
        }
        layout::of(t, &|t| self.members(pos, t).ok())
            .ok_or_else(|| Error::new(pos, format!("type {t} has no fixed memory layout (it contains a GC reference)")))
    }

    // ---- functions and constants ----

    pub fn has_function(&self, module: usize, name: &str) -> bool {
        self.key(module, name).is_some_and(|k| self.fn_decls.contains_key(&k))
    }

    /// The declaration of function `key` if it is generic.
    pub fn generic(&self, key: &str) -> Option<&'p FnDecl> {
        self.fn_decls.get(key).map(|&(_, d)| d).filter(|d| !d.is_macro && d.params.iter().any(|p| p.comptime))
    }

    /// The declaration of `key` if it is a macro.
    pub fn macro_decl(&self, key: &str) -> Option<&'p FnDecl> {
        self.fn_decls.get(key).map(|&(_, d)| d).filter(|d| d.is_macro)
    }

    /// The signature of function `name`, or `None` if there is no such function.
    /// Generic functions have no signature of their own, only their instances.
    pub fn signature(&self, name: &str) -> Option<Result<Rc<Sig>, Error>> {
        let (m, decl) = *self.fn_decls.get(name)?;
        let scope = self.root(m);
        if self.generic(name).is_some() {
            return Some(Err(Error::new(decl.pos, format!("`{name}` has comptime parameters, so it cannot be used here"))));
        }
        Some(self.sigs.get(
            name,
            || Error::new(decl.pos, format!("the signature of `{name}` depends on itself")),
            || {
                for p in &decl.params {
                    if is_type_param(p) {
                        return Err(Error::new(p.ty.pos, "`type` can only be the type of a `comptime` parameter"));
                    }
                }
                let is_macro = decl.is_macro;
                let params: Vec<Type> = decl
                    .params
                    .iter()
                    .map(|p| self.resolve_in(&p.ty, scope, is_macro))
                    .collect::<Result<_, _>>()?;
                let ret = match &decl.ret {
                    Some(t) => self.resolve_in(t, scope, is_macro)?,
                    None => Type::Unit,
                };
                if is_macro {
                    if !ret.is_code() {
                        let msg = format!("macro `{name}` must return `expr`, `stmts` or `items`, not {ret}");
                        return Err(Error::new(decl.pos, msg));
                    }
                    for (p, t) in decl.params.iter().zip(&params) {
                        if p.comptime {
                            return Err(Error::new(p.pos, "macro parameters are already compile-time; drop `comptime`"));
                        }
                        if !matches!(t, Type::Expr | Type::Int(_) | Type::Bool | Type::Str) {
                            return Err(Error::new(
                                p.ty.pos,
                                format!("macro parameters must be `expr`, integers, bools or `str`, not {t}"),
                            ));
                        }
                    }
                }
                Ok(Rc::new(Sig { params, ret }))
            },
        ))
    }

    pub(crate) fn find_instance(&self, key: &str) -> Option<(String, Result<Rc<Sig>, Error>)> {
        self.instances.borrow().get(key).map(|i| (i.name.clone(), i.sig.clone()))
    }

    pub(crate) fn instance_count(&self) -> u32 {
        self.instances.borrow().len() as u32
    }

    pub(crate) fn add_instance(
        &self,
        key: String,
        name: String,
        fn_name: String,
        bindings: Rc<Bindings>,
        sig: Result<Rc<Sig>, Error>,
    ) {
        if sig.is_ok() {
            self.pending.borrow_mut().push_back(name.clone());
        }
        self.instance_keys.borrow_mut().insert(name.clone(), key.clone());
        self.instances.borrow_mut().insert(key, Instance { name, fn_name, bindings, sig });
    }

    /// An instance that was asked for but not compiled yet.
    pub fn next_pending(&self) -> Option<String> {
        self.pending.borrow_mut().pop_front()
    }

    /// True while `name` is being lowered (so it cannot run at compile time yet).
    pub fn function_in_progress(&self, name: &str) -> bool {
        self.funcs.in_progress(name)
    }

    /// The JIR of function `name` (a plain function or an instance), which must exist.
    pub fn function(&self, name: &str) -> Result<Rc<ir::Function>, Error> {
        let instance = self.instance_keys.borrow().get(name).map(|key| {
            let inst = &self.instances.borrow()[key];
            (inst.fn_name.clone(), inst.bindings.clone(), inst.sig.clone())
        });
        let (decl, bindings, sig) = match instance {
            Some((fn_name, b, sig)) => (self.fn_decls[fn_name.as_str()].1, b, Some(sig)),
            None => {
                let (m, decl) = self.fn_decls[name];
                (decl, self.root(m).clone(), None)
            }
        };
        self.funcs.get(
            name,
            || Error::new(decl.pos, format!("`{name}` is needed at compile time while it is being compiled")),
            || {
                let sig = match sig {
                    Some(sig) => sig?,
                    None => self.signature(name).unwrap()?,
                };
                let cx = FnCx::new(self, sig, bindings.clone());
                cx.lower_fn(decl, name).map(Rc::new).map_err(|mut e| {
                    if !bindings.describe().is_empty() {
                        e.msg = format!("{} (in `{}` with {})", e.msg, decl.name, bindings.describe());
                    }
                    e
                })
            },
        )
    }

    /// The type and value of constant `name`, or `None` if there is no such constant.
    pub fn constant(&self, name: &str) -> Option<Result<Rc<(Type, ConstValue)>, Error>> {
        let (cm, decl) = *self.const_decls.get(name)?;
        let scope = self.root(cm);
        Some(self.consts.get(
            name,
            || Error::new(decl.pos, format!("constant `{name}` depends on itself")),
            || {
                let want = decl.ty.as_ref().map(|t| self.resolve(t, scope)).transpose()?;
                let (ty, v) = self.comptime(&decl.value, want.as_ref(), scope)?;
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
            let pos = Pos::new(1, 1);
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

/// The struct or enum `t` holds by value, looking through arrays (pointers break
/// cycles).
fn named_by_value(t: &Type) -> Option<&Type> {
    match t {
        Type::Struct(_) | Type::Enum(_) => Some(t),
        Type::Array(elem, _) => named_by_value(elem),
        _ => None,
    }
}

/// The type that declaration `decl` declares under the name `name`.
fn named(decl: &StructDecl, name: String) -> Type {
    if decl.is_enum() {
        Type::Enum(name)
    } else {
        Type::Struct(name)
    }
}
