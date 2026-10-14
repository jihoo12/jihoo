//! Inferring the type arguments of a call to a generic function.
//!
//! A call that leaves out the `comptime T: type` arguments, giving exactly one
//! argument per runtime parameter, has them inferred: each argument with a type
//! of its own is lowered and its type matched against its parameter's type
//! (`[T]` against `[i64]` gives `T = i64`). Arguments that take their type from
//! where they are used (literals, anonymous functions, `[]`) wait until the
//! parameters their type mentions are known, and are then lowered with that as
//! their expected type; a literal nothing else decides takes its default type,
//! as in `let`. Value parameters (`comptime N: i64`) are never inferred.

use std::collections::HashMap;
use std::rc::Rc;

use jihoo_ir::{Inst, Reg, Type};
use jihoo_syntax::ast::*;
use jihoo_syntax::{Error, Pos};

use crate::generic::{expr_to_type, Binding, Bindings};
use crate::env::Env;
use crate::FnCx;

/// The type parameters of one call, or of an impl, and what is known of them
/// so far.
pub(crate) struct Found<'d> {
    /// The callee's module, where its parameter types are written.
    pub module: usize,
    /// The callee's name, for messages.
    pub callee: &'d str,
    /// The type parameters in declaration order.
    pub params: Vec<&'d str>,
    pub types: HashMap<&'d str, (Type, Pos)>,
    /// Whether a type must match exactly (an impl's), rather than give what
    /// it can (a call's).
    pub strict: bool,
}

impl<'d> Found<'d> {
    fn is_param(&self, n: &str) -> bool {
        self.params.contains(&n)
    }

    /// The first type parameter that `t` mentions and that is not known yet.
    pub fn unknown_in(&self, t: &TypeExpr) -> Option<&'d str> {
        match &t.kind {
            TypeExprKind::Named(n) => {
                // `I.Item`, a type of `I`'s, is known once `I` is.
                let n = n.split_once('.').map_or(n.as_str(), |(p, _)| p);
                self.params.iter().copied().find(|p| *p == n && !self.types.contains_key(p))
            }
            TypeExprKind::Ptr(x)
            | TypeExprKind::List(x)
            | TypeExprKind::Array(x, _)
            | TypeExprKind::Ref(x)
            | TypeExprKind::Chan(x)
            | TypeExprKind::Cell(x)
            | TypeExprKind::Path(x, _) => self.unknown_in(x),
            TypeExprKind::Generic(_, args) => {
                let args: Vec<TypeExpr> = args.iter().filter_map(|a| expr_to_type(a).ok()).collect();
                args.iter().find_map(|t| self.unknown_in(t))
            }
            TypeExprKind::Fn(ps, r) => ps.iter().chain(r.as_deref()).find_map(|t| self.unknown_in(t)),
            TypeExprKind::Tuple(ps) => ps.iter().find_map(|t| self.unknown_in(t)),
        }
    }

    /// The bindings of the parameters known so far, in declaration order.
    pub fn bindings(&self) -> Bindings {
        let mut b = Bindings::in_module(self.module);
        for p in &self.params {
            if let Some((t, _)) = self.types.get(p) {
                b.push(p, Binding::Type(t.clone()));
            }
        }
        b
    }
}

/// Whether an argument takes its type from its parameter, so that it should be
/// lowered after the arguments that decide that type.
fn needs_expected(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::ArrayLit(_) | ExprKind::ArrayRepeat(..) => true,
        ExprKind::Unary(UnOp::Neg, x) => needs_expected(x),
        ExprKind::Tuple(parts) => parts.iter().any(needs_expected),
        ExprKind::Lambda(l) => l.ret.is_none() || l.params.iter().any(|(_, _, t)| t.is_none()),
        _ => false,
    }
}

impl FnCx<'_> {
    /// Calls generic function `key`, inferring its type arguments from `args`,
    /// one per runtime parameter.
    pub(crate) fn call_inferred(&mut self, pos: Pos, key: &str, decl: &FnDecl, args: &[Expr]) -> Result<Reg, Error> {
        let name = &decl.name;
        let module = self.env.fn_module(key);
        if let Some(p) = decl.params.iter().find(|p| p.comptime && !self.env.is_type_param(module, p)) {
            let msg = format!("`{name}` takes {} arguments, {} given; `{}` is not a type, so it cannot be inferred", decl.params.len(), args.len(), p.name);
            return Err(Error::new(pos, msg));
        }
        // `max(i64, 1)`: the types written out, and an argument missing.
        if let Some(a) = args.first() {
            let named = match &a.kind {
                ExprKind::Var(n) => self.local(n).is_none(),
                ExprKind::Type(_) => true,
                _ => false,
            };
            if named && expr_to_type(a).is_ok_and(|t| self.resolve(&t).is_ok()) {
                let msg = format!("`{name}` takes {} arguments, {} given", decl.params.len(), args.len());
                return Err(Error::new(pos, msg));
            }
        }
        let type_params = decl.params.iter().filter(|p| p.comptime).map(|p| p.name.as_str()).collect();
        let params: Vec<&Param> = decl.params.iter().filter(|p| !p.comptime).collect();
        let how = format!("write the type arguments of `{name}`");
        let (bindings, regs) = self.infer_args(pos, module, name, type_params, &params, args, &how)?;
        let (instance, sig) = self.env.instance(pos, key, decl, bindings)?;
        for ((p, a), (r, want)) in params.iter().zip(args).zip(regs.iter().zip(&sig.params)) {
            self.expect(a.pos, *r, want, &format!("argument `{}` of `{name}`", p.name))?;
        }
        Ok(self.emit_to(sig.ret.clone(), |dst| Inst::Call { dst, func: instance, args: regs }))
    }

    /// Lowers `args`, one for each of `params`, inferring the types
    /// `type_params` that the parameter types (written in `module`) mention.
    /// Gives their bindings and the arguments, not yet checked against the
    /// parameter types; `how` says how a caller could give them instead.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn infer_args<'d>(
        &mut self,
        pos: Pos,
        module: usize,
        name: &'d str,
        type_params: Vec<&'d str>,
        params: &[&Param],
        args: &[Expr],
        how: &str,
    ) -> Result<(Bindings, Vec<Reg>), Error> {
        let mut found = Found { module, callee: name, params: type_params, types: HashMap::new(), strict: false };

        // Arguments with a type of their own, or whose parameter type is known,
        // in order; then the others, which wait for the types they need.
        let mut regs: Vec<Option<Reg>> = vec![None; args.len()];
        for (i, (p, a)) in params.iter().zip(args).enumerate() {
            let want = self.expected_arg(&found, &p.ty, a)?;
            if want.is_none() && needs_expected(a) {
                continue;
            }
            regs[i] = Some(self.inferring_arg(&mut found, p, a, want)?);
        }
        for (i, (p, a)) in params.iter().zip(args).enumerate() {
            if regs[i].is_some() {
                continue;
            }
            let want = self.expected_arg(&found, &p.ty, a)?;
            if want.is_none() {
                if let (ExprKind::Lambda(_), Some(t)) = (&a.kind, found.unknown_in(&p.ty)) {
                    let msg = format!(
                        "cannot infer `{t}` of `{name}`: annotate this function's types, or {how}"
                    );
                    return Err(Error::new(a.pos, msg));
                }
            }
            regs[i] = Some(self.inferring_arg(&mut found, p, a, want)?);
        }
        if let Some(t) = found.params.iter().find(|t| !found.types.contains_key(*t)) {
            let msg = format!("cannot infer `{t}` of `{name}` from its arguments; {how}");
            return Err(Error::new(pos, msg));
        }
        Ok((found.bindings(), regs.into_iter().map(Option::unwrap).collect()))
    }

    /// Lowers argument `a` of parameter `p`, expecting `want`, and matches its
    /// type against the parameter's.
    fn inferring_arg(&mut self, found: &mut Found, p: &Param, a: &Expr, want: Option<Type>) -> Result<Reg, Error> {
        let r = self.expr(a, want.as_ref())?;
        let got = self.ty(r).clone();
        self.env.unify(found, &p.ty, &got, a.pos)?;
        Ok(r)
    }

    /// The type argument `a` can expect for parameter type `t`, if what is
    /// known decides it. For an anonymous function, a function type whose
    /// unknown parts the function itself annotates.
    fn expected_arg(&mut self, found: &Found, t: &TypeExpr, a: &Expr) -> Result<Option<Type>, Error> {
        let known = Rc::new(found.bindings());
        if found.unknown_in(t).is_none() {
            return self.env.resolve(t, &known).map(Some);
        }
        let (ExprKind::Lambda(l), TypeExprKind::Fn(ps, r)) = (&a.kind, &t.kind) else { return Ok(None) };
        if ps.len() != l.params.len() {
            return Ok(None);
        }
        let mut params = Vec::with_capacity(ps.len());
        for ((_, _, own), p) in l.params.iter().zip(ps) {
            params.push(match own {
                Some(own) => self.resolve(own)?,
                None if found.unknown_in(p).is_none() => self.env.resolve(p, &known)?,
                None => return Ok(None),
            });
        }
        let ret = match (&l.ret, r) {
            (Some(own), _) => self.resolve(own)?,
            (None, None) => Type::Unit,
            (None, Some(r)) if found.unknown_in(r).is_none() => self.env.resolve(r, &known)?,
            (None, Some(_)) => return Ok(None),
        };
        Ok(Some(Type::Fn(params, Box::new(ret))))
    }
}

impl Env<'_> {
    /// Matches type `t`, written in `found.module`, against `got`, learning
    /// the type parameters of `found`. Says whether they match: where the
    /// shapes differ, a strict match fails, and any other learns nothing (the
    /// argument's type is checked against the instance's signature later).
    pub(crate) fn unify(&self, found: &mut Found, t: &TypeExpr, got: &Type, pos: Pos) -> Result<bool, Error> {
        let ok = match (&t.kind, got) {
            (TypeExprKind::Named(n), _) if found.is_param(n) => {
                let n = found.params.iter().copied().find(|p| p == n).unwrap();
                match found.types.get(n) {
                    Some((before, _)) if before != got => {
                        if found.strict {
                            return Ok(false);
                        }
                        let msg = format!("`{n}` of `{}` is {got} here, but {before} from an earlier argument", found.callee);
                        return Err(Error::new(pos, msg));
                    }
                    Some(_) => {}
                    None => {
                        found.types.insert(n, (got.clone(), pos));
                    }
                }
                true
            }
            (TypeExprKind::Ptr(x), Type::Ptr(y))
            | (TypeExprKind::List(x), Type::List(y))
            | (TypeExprKind::Ref(x), Type::Ref(y))
            | (TypeExprKind::Chan(x), Type::Chan(y))
            | (TypeExprKind::Cell(x), Type::Cell(y)) => self.unify(found, x, y, pos)?,
            (TypeExprKind::Array(x, n), Type::Array(y, len)) => {
                let ok = self.unify(found, x, y, pos)?;
                ok && (!found.strict || self.array_len(n, &Rc::new(found.bindings())).is_ok_and(|n| n == *len))
            }
            (TypeExprKind::Fn(ps, r), Type::Fn(qs, s)) if ps.len() == qs.len() => {
                let mut ok = true;
                for (p, q) in ps.iter().zip(qs) {
                    ok &= self.unify(found, p, q, pos)?;
                }
                match r {
                    Some(r) => ok &= self.unify(found, r, s, pos)?,
                    None => ok &= **s == Type::Unit,
                }
                ok
            }
            (TypeExprKind::Tuple(ps), _) => match self.tuple_parts(got) {
                Some(parts) if parts.len() == ps.len() => {
                    let mut ok = true;
                    for (p, (_, q)) in ps.iter().zip(parts.iter()) {
                        ok &= self.unify(found, p, q, pos)?;
                    }
                    ok
                }
                _ => false,
            },
            (TypeExprKind::Generic(g, args), Type::Struct(n) | Type::Enum(n)) => {
                let Some((decl_key, bound)) = self.instance_bindings(n) else { return Ok(!found.strict) };
                if self.key(found.module, g).as_deref() != Some(decl_key.as_str()) {
                    return Ok(!found.strict);
                }
                let (_, decl) = self.type_decl(&decl_key).unwrap();
                let mut ok = true;
                for (p, a) in decl.params.iter().zip(args) {
                    match (bound.get(&p.name), expr_to_type(a)) {
                        (Some(Binding::Type(q)), Ok(a)) => ok &= self.unify(found, &a, &q.clone(), pos)?,
                        _ => ok = false,
                    }
                }
                ok
            }
            // A type without parameters in it, such as `i64` in `Pair(T, i64)`.
            (TypeExprKind::Named(_), _) if found.strict => self.resolve(t, &Rc::new(found.bindings())).is_ok_and(|t| t == *got),
            _ => false,
        };
        Ok(ok || !found.strict)
    }
}
