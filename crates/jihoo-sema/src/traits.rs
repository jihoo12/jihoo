//! Traits: `trait`, `impl` and bounds (`comptime T: Ord`).
//!
//! An impl is a set of ordinary functions. Each function of an impl, and each
//! default body of a trait, is compiled like a generic function whose one
//! comptime parameter is `Self`: per type, as an instance named after its key
//! (`impl.N.name`, `trait.Name.name`; both start with a keyword, so no user
//! function has such a name). A call through a trait is a direct call of that
//! instance, so nothing is new in JIR.
//!
//! There is at most one impl of a trait for a type, and it is in the module of
//! the trait or of the type, so finding it looks at those two modules only.
//! Bounds are checked where a generic function or struct is instantiated; the
//! bodies are still checked per instance.

use std::collections::HashSet;
use std::rc::Rc;

use jihoo_ir::{Inst, Reg, Type};
use jihoo_syntax::ast::*;
use jihoo_syntax::{Error, Pos};

use crate::env::{Env, ImplHead};
use crate::generic::{is_type_param, Binding, Bindings};
use crate::hygiene::qualified;
use crate::FnCx;

/// Whether trait function `f` takes `Self` first, and so can be called as a
/// method: `x.f(...)`.
fn takes_self(f: &FnDecl) -> bool {
    f.params.first().is_some_and(|p| matches!(&p.ty.kind, TypeExprKind::Named(n) if n == "Self"))
}

fn named(t: &TypeExpr) -> Option<&str> {
    match &t.kind {
        TypeExprKind::Named(n) => Some(n),
        _ => None,
    }
}

impl<'p> Env<'p> {
    /// The key of trait `name`, as written in `module`, if it names one.
    pub fn trait_key(&self, module: usize, name: &str) -> Option<String> {
        self.key(module, name).filter(|k| self.trait_decls.contains_key(k))
    }

    /// The name of trait `key`, for messages.
    pub fn trait_name(&self, key: &str) -> &'p str {
        &self.trait_decls[key].1.name
    }

    /// Function `name` of trait `key`.
    pub fn trait_fn(&self, key: &str, name: &str) -> Option<&'p TraitFn> {
        self.trait_decls[key].1.funcs.iter().find(|f| f.decl.name == name)
    }

    /// The traits a comptime parameter `p` of an item in `module` asks of its
    /// type: none for `comptime T: type`. `None` if `p` takes a value.
    pub fn param_bounds(&self, module: usize, p: &Param) -> Result<Option<Vec<String>>, Error> {
        if !p.comptime {
            return Ok(None);
        }
        if is_type_param(p) {
            if let Some(b) = p.bounds.first() {
                let msg = format!("a type with traits is written with the traits alone: `comptime {}: Trait + Trait`", p.name);
                return Err(Error::new(b.pos, msg));
            }
            return Ok(Some(vec![]));
        }
        let Some(first) = named(&p.ty).filter(|n| self.trait_key(module, n).is_some()) else {
            if let Some(b) = p.bounds.first() {
                return Err(Error::new(b.pos, "only traits can be joined with `+`"));
            }
            return Ok(None);
        };
        let mut keys = Vec::new();
        for (pos, n) in std::iter::once((p.ty.pos, Some(first))).chain(p.bounds.iter().map(|b| (b.pos, named(b)))) {
            let Some(key) = n.and_then(|n| self.trait_key(module, n)) else {
                return Err(Error::new(pos, "only traits can be joined with `+`"));
            };
            self.check_visible(pos, self.viewer(module, n.unwrap()), &key)?;
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        Ok(Some(keys))
    }

    /// Whether comptime parameter `p` of an item in `module` takes a type.
    pub fn is_type_param(&self, module: usize, p: &Param) -> bool {
        matches!(self.param_bounds(module, p), Ok(Some(_)))
    }

    /// Checks that the types `b` binds to `params` (of `owner`, declared in
    /// `module`) implement the traits the parameters ask for.
    pub fn check_bounds(&self, pos: Pos, owner: &str, module: usize, params: &[Param], b: &Bindings) -> Result<(), Error> {
        for p in params {
            let Some(bounds) = self.param_bounds(module, p)? else { continue };
            let Some(Binding::Type(t)) = b.get(&p.name) else { continue };
            for key in bounds {
                if self.find_impl(&key, t).is_none() {
                    let msg = format!("{t} does not implement `{}`, which `{}` of `{owner}` needs", self.trait_name(&key), p.name);
                    return Err(Error::new(pos, msg));
                }
            }
        }
        Ok(())
    }

    /// The module that declares type `t`, if it is a struct or enum.
    fn home_module(&self, t: &Type) -> Option<usize> {
        let (Type::Struct(n) | Type::Enum(n)) = t else { return None };
        let decl = self.instance_bindings(n).map_or_else(|| n.clone(), |(d, _)| d);
        self.type_decl(&decl).map(|(m, _)| m)
    }

    /// The trait and the type of impl `i`.
    fn impl_head(&self, i: usize) -> Result<ImplHead, Error> {
        if let Some(r) = self.impl_heads.borrow().get(&i) {
            return r.clone();
        }
        let (m, decl) = self.impls[i];
        let r = (|| {
            let Some(key) = self.trait_key(m, &decl.trait_name) else {
                return Err(Error::new(decl.pos, format!("unknown trait `{}`", decl.trait_name)));
            };
            self.check_visible(decl.pos, self.viewer(m, &decl.trait_name), &key)?;
            let ty = self.resolve(&decl.ty, self.root(m))?;
            Ok((key, ty))
        })();
        self.impl_heads.borrow_mut().insert(i, r.clone());
        r
    }

    /// The impl of trait `key` for type `t`, by number, if there is one.
    pub fn find_impl(&self, key: &str, t: &Type) -> Option<usize> {
        let trait_module = self.trait_decls[key].0;
        let home = self.home_module(t);
        (0..self.impls.len()).find(|&i| {
            let m = self.impls[i].0;
            (m == trait_module || Some(m) == home)
                && matches!(self.impl_head(i), Ok((k, ty)) if k == key && ty == *t)
        })
    }

    /// Function `name` of impl `i` of trait `key`: the impl's own, or else the
    /// trait's default. Its key, module and declaration.
    fn impl_fn(&self, key: &str, i: usize, name: &str) -> Option<(String, usize, &'p FnDecl)> {
        let (m, decl) = self.impls[i];
        let fn_key = if decl.funcs.iter().any(|f| f.name == name) {
            self.item_key(m, &format!("impl.{i}.{name}"))
        } else {
            let (tm, t) = self.trait_decls[key];
            self.item_key(tm, &format!("trait.{}.{name}", t.name))
        };
        let (m, decl) = self.fn_decl(&fn_key)?;
        Some((fn_key, m, decl))
    }

    /// The traits that code in `module` can use: its own, the prelude's, and
    /// the public ones of the modules it imports, in a fixed order.
    fn visible_traits(&self, module: usize) -> Vec<String> {
        let mut modules: HashSet<usize> = self.imports_of(module).collect();
        modules.extend(self.prelude_module());
        let mut keys: Vec<String> = self
            .trait_decls
            .iter()
            .filter(|(_, &(m, t))| m == module || (modules.contains(&m) && t.is_pub))
            .map(|(k, _)| k.clone())
            .collect();
        keys.sort();
        keys
    }

    /// Whether values of type `t` have a field `name`, directly or through a
    /// pointer, cell or ref.
    fn has_field(&self, t: &Type, name: &str) -> bool {
        let t = match t {
            Type::Ptr(inner) | Type::Cell(inner) | Type::Ref(inner) => inner,
            t => t,
        };
        let Type::Struct(s) = t else { return false };
        self.struct_fields(Pos::default(), s).is_ok_and(|fields| fields.iter().any(|(n, _)| n == name))
    }

    /// The signature of function `decl` of `module` with `Self` = `t`.
    fn self_sig(&self, module: usize, decl: &FnDecl, t: &Type) -> Result<(Vec<Type>, Type), Error> {
        let mut b = Bindings::in_module(module);
        b.push("Self", Binding::Type(t.clone()));
        let b = Rc::new(b);
        let params = decl.params.iter().map(|p| self.resolve(&p.ty, &b)).collect::<Result<_, _>>()?;
        let ret = match &decl.ret {
            Some(r) => self.resolve(r, &b)?,
            None => Type::Unit,
        };
        Ok((params, ret))
    }

    /// Checks every trait and impl, used or not: what each impl gives against
    /// its trait, where it is, and that no type has two impls of a trait. The
    /// prelude's impls are left out: some are for types that a profile does
    /// not have (`str` in freestanding programs), and the compiler's own tests
    /// check them.
    pub fn check_traits(&self) -> Vec<Error> {
        let mut errors = Vec::new();
        let mut keys: Vec<&String> = self.trait_decls.keys().collect();
        keys.sort();
        for key in keys {
            let t = self.trait_decls[key].1;
            for f in &t.funcs {
                if let Some(p) = f.decl.params.iter().find(|p| p.comptime) {
                    errors.push(Error::new(p.pos, "trait functions cannot have `comptime` parameters"));
                }
            }
        }
        for (m, p) in self.comptime_params() {
            if let Err(e) = self.param_bounds(m, p) {
                errors.push(e);
            }
        }
        let mut seen: Vec<(String, Type)> = Vec::new();
        for (i, &(m, decl)) in self.impls.iter().enumerate() {
            if Some(m) == self.prelude_module() {
                continue;
            }
            let (key, ty) = match self.impl_head(i) {
                Ok(head) => head,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            let (tm, t) = self.trait_decls[&key];
            if m != tm && self.home_module(&ty) != Some(m) {
                let msg = match self.home_module(&ty) {
                    Some(_) => format!("an impl of `{}` for {ty} must be in the module of `{}` or of {ty}", t.name, t.name),
                    None => format!("an impl of `{}` for {ty} must be in the module of `{}`", t.name, t.name),
                };
                errors.push(Error::new(decl.pos, msg));
                continue;
            }
            if seen.contains(&(key.clone(), ty.clone())) {
                errors.push(Error::new(decl.pos, format!("`{}` is implemented for {ty} twice", t.name)));
                continue;
            }
            seen.push((key.clone(), ty.clone()));
            for f in &decl.funcs {
                let Some(tf) = self.trait_fn(&key, &f.name) else {
                    errors.push(Error::new(f.pos, format!("`{}` has no function `{}`", t.name, f.name)));
                    continue;
                };
                if let Some(p) = f.params.iter().find(|p| p.comptime) {
                    errors.push(Error::new(p.pos, "functions of an impl cannot have `comptime` parameters"));
                    continue;
                }
                let r = self.self_sig(tm, &tf.decl, &ty).and_then(|want| Ok((want, self.self_sig(m, f, &ty)?)));
                match r {
                    Ok((want, got)) if want != got => {
                        let shown = |(ps, r): &(Vec<Type>, Type)| {
                            let ps: Vec<String> = ps.iter().map(Type::to_string).collect();
                            format!("fn({}) -> {r}", ps.join(", "))
                        };
                        let msg = format!(
                            "`{}` of `{}` for {ty} must be {}, not {}",
                            f.name,
                            t.name,
                            shown(&want),
                            shown(&got)
                        );
                        errors.push(Error::new(f.pos, msg));
                    }
                    Ok(_) => {}
                    Err(e) => errors.push(e),
                }
            }
            for tf in t.funcs.iter().filter(|tf| !tf.has_body) {
                if !decl.funcs.iter().any(|f| f.name == tf.decl.name) {
                    let msg = format!("this impl of `{}` for {ty} lacks `{}`, which has no default", t.name, tf.decl.name);
                    errors.push(Error::new(decl.pos, msg));
                }
            }
        }
        errors
    }
}

impl FnCx<'_> {
    /// `x.name(args)`, if a trait the code can see has a function `name` that
    /// takes `Self` first: the function for the type of `x`, unless `x` has a
    /// field `name`, which comes first. `None` if there is no such trait, and
    /// the call is a call of a field.
    pub(crate) fn method_call(&mut self, pos: Pos, base: &Expr, written: &str, args: &[Expr]) -> Result<Option<Reg>, Error> {
        // In a macro's code, `#3:name`: seen from module 3, where the macro is.
        let (module, name) = qualified(written).unwrap_or((self.bindings.module, written));
        let traits: Vec<String> = self
            .env
            .visible_traits(module)
            .into_iter()
            .filter(|k| self.env.trait_fn(k, name).is_some_and(|f| takes_self(&f.decl)))
            .collect();
        if traits.is_empty() {
            return Ok(None);
        }
        let place = self.place(base)?;
        if self.env.has_field(place.ty(), name) {
            let field = self.field_place(pos, place, written)?;
            let f = self.read(field);
            return self.call_value(pos, f, &format!("`{name}`"), args).map(Some);
        }
        let ty = place.ty().clone();
        let key = self.pick_trait(pos, &traits, &ty, name)?;
        let recv = self.read(place);
        self.call_trait_fn(pos, &key, name, ty, Some(recv), args).map(Some)
    }

    /// `path.name(args)`, where `path` (not a local) names a trait or a type:
    /// `Ord.compare(a, b)`, `T.zero()`, `shapes.Shape.area(s)`. `None` if it
    /// names neither.
    pub(crate) fn path_call(&mut self, pos: Pos, path: &str, name: &str, args: &[Expr]) -> Result<Option<Reg>, Error> {
        let module = self.bindings.module;
        if let Some(key) = self.env.trait_key(module, path) {
            self.env.check_visible(pos, self.env.viewer(module, path), &key)?;
            return self.trait_call(pos, &key, name, args).map(Some);
        }
        if self.local(path).is_some() || self.env.is_alias(module, path) {
            return Ok(None);
        }
        // In a macro's code, `#3:Point`: a type as module 3, where the macro is, sees it.
        let resolved = match qualified(path) {
            Some((m, bare)) => {
                let named = TypeExpr { pos, kind: TypeExprKind::Named(bare.to_string()) };
                self.env.resolve(&named, self.env.root(m))
            }
            None => self.resolve(&TypeExpr { pos, kind: TypeExprKind::Named(path.to_string()) }),
        };
        match resolved {
            Ok(ty) => self.type_call(pos, ty, name, args).map(Some),
            Err(_) => Ok(None),
        }
    }

    /// `T.name(args)` or `Point.name(args)`: function `name` of the trait the
    /// code can see that has one, for type `ty`.
    fn type_call(&mut self, pos: Pos, ty: Type, name: &str, args: &[Expr]) -> Result<Reg, Error> {
        let traits: Vec<String> = self
            .env
            .visible_traits(self.bindings.module)
            .into_iter()
            .filter(|k| self.env.trait_fn(k, name).is_some())
            .collect();
        if traits.is_empty() {
            return Err(Error::new(pos, format!("no trait has a function `{name}` for {ty}")));
        }
        let key = self.pick_trait(pos, &traits, &ty, name)?;
        self.call_trait_fn(pos, &key, name, ty, None, args)
    }

    /// `Ord.name(args)`: function `name` of trait `key`, for the type of
    /// `Self` that the arguments give.
    pub(crate) fn trait_call(&mut self, pos: Pos, key: &str, name: &str, args: &[Expr]) -> Result<Reg, Error> {
        let trait_name = self.env.trait_name(key);
        let Some(tf) = self.env.trait_fn(key, name) else {
            return Err(Error::new(pos, format!("`{trait_name}` has no function `{name}`")));
        };
        let decl = &tf.decl;
        let callee = format!("{trait_name}.{name}");
        if decl.params.len() != args.len() {
            let msg = format!("`{callee}` takes {} arguments, {} given", decl.params.len(), args.len());
            return Err(Error::new(pos, msg));
        }
        let module = self.env.trait_decls[key].0;
        let params: Vec<&Param> = decl.params.iter().collect();
        let how = format!("write the type, as in `Type.{name}(...)`");
        let (b, regs) = self.infer_args(pos, module, &callee, vec!["Self"], &params, args, &how)?;
        let Some(Binding::Type(ty)) = b.get("Self").cloned() else { unreachable!("Self is inferred") };
        let (func, sig) = self.trait_instance(pos, key, name, ty)?;
        for ((p, a), (r, want)) in params.iter().zip(args).zip(regs.iter().zip(&sig.params)) {
            self.expect(a.pos, *r, want, &format!("argument `{}` of `{callee}`", p.name))?;
        }
        Ok(self.emit_to(sig.ret.clone(), |dst| Inst::Call { dst, func, args: regs }))
    }

    /// The one trait among `traits` (all with a function `name`) that type `ty`
    /// implements.
    fn pick_trait(&self, pos: Pos, traits: &[String], ty: &Type, name: &str) -> Result<String, Error> {
        let mut found = traits.iter().filter(|k| self.env.find_impl(k, ty).is_some());
        match (found.next(), found.next()) {
            (Some(k), None) => Ok(k.clone()),
            (Some(a), Some(b)) => {
                let (a, b) = (self.env.trait_name(a), self.env.trait_name(b));
                let msg = format!("`{name}` of {ty} is in both `{a}` and `{b}`; write `{a}.{name}(...)` or `{b}.{name}(...)`");
                Err(Error::new(pos, msg))
            }
            (None, _) => {
                let names: Vec<String> = traits.iter().map(|k| format!("`{}`", self.env.trait_name(k))).collect();
                let msg = format!("{ty} does not implement {}, which has `{name}`", names.join(" or "));
                Err(Error::new(pos, msg))
            }
        }
    }

    /// The instance of function `name` of trait `key` for `Self` = `ty`.
    fn trait_instance(&mut self, pos: Pos, key: &str, name: &str, ty: Type) -> Result<(String, Rc<crate::env::Sig>), Error> {
        let trait_name = self.env.trait_name(key);
        let Some(i) = self.env.find_impl(key, &ty) else {
            return Err(Error::new(pos, format!("{ty} does not implement `{trait_name}`")));
        };
        let Some((fn_key, module, decl)) = self.env.impl_fn(key, i, name) else {
            return Err(Error::new(pos, format!("the impl of `{trait_name}` for {ty} lacks `{name}`")));
        };
        let mut b = Bindings::in_module(module);
        b.push("Self", Binding::Type(ty));
        self.env.instance(pos, &fn_key, decl, b)
    }

    /// Calls function `name` of trait `key` for `Self` = `ty`, with `recv`, a
    /// value already lowered, before `args`.
    fn call_trait_fn(&mut self, pos: Pos, key: &str, name: &str, ty: Type, recv: Option<Reg>, args: &[Expr]) -> Result<Reg, Error> {
        let callee = format!("{}.{name}", self.env.trait_name(key));
        let (func, sig) = self.trait_instance(pos, key, name, ty)?;
        let given = args.len() + usize::from(recv.is_some());
        if given != sig.params.len() {
            return Err(Error::new(pos, format!("`{callee}` takes {} arguments, {given} given", sig.params.len())));
        }
        let mut regs: Vec<Reg> = recv.into_iter().collect();
        for (a, want) in args.iter().zip(&sig.params[regs.len()..]) {
            let r = self.expr(a, Some(want))?;
            self.expect(a.pos, r, want, &format!("an argument of `{callee}`"))?;
            regs.push(r);
        }
        Ok(self.emit_to(sig.ret.clone(), |dst| Inst::Call { dst, func, args: regs }))
    }
}
