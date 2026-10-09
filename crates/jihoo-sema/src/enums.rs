//! Enums: building variants, and `match`.
//!
//! A variant is written after the enum that declares it: `Shape.Circle(2)`,
//! `Shape.Empty`, `geo.Shape.Empty`, `Option(i64).Some(1)`. For a generic enum
//! the type arguments can be left out (`Option.Some(1)`): they come from the
//! expected type, or else from the payload values.
//!
//! `match` is a statement. It reads the tag once and tests the arms in order;
//! an arm that matches a variant binds its payload values to new locals. Every
//! variant (or every bool) must be covered, or there must be a `_` arm, so the
//! fall-through block after the last test is unreachable.

use std::collections::HashSet;

use jihoo_ir::{BinOp, Inst, IntTy, Reg, Terminator, Type};
use jihoo_syntax::ast::*;
use jihoo_syntax::{Error, Pos};

use crate::generic::{is_type_param, Binding};
use crate::FnCx;

/// An enum named in an expression.
pub(crate) enum EnumRef {
    /// A complete type: `Shape`, `Option(i64)`, or a type parameter bound to an enum.
    Known(Type),
    /// A generic enum written without arguments (`Option`), by key.
    Generic(String),
}

impl FnCx<'_> {
    /// The enum that `e` names, if it names one: `Shape`, `geo.Shape`,
    /// `Option(i64)`, or `T` bound to an enum type.
    pub(crate) fn enum_path(&self, e: &Expr) -> Result<Option<EnumRef>, Error> {
        let name = match &e.kind {
            ExprKind::Var(n) if self.local(n).is_none() => match self.bindings.get(n) {
                Some(Binding::Type(t @ Type::Enum(_))) => return Ok(Some(EnumRef::Known(t.clone()))),
                Some(_) => return Ok(None),
                None => n.clone(),
            },
            ExprKind::Field(base, n) => match &base.kind {
                ExprKind::Var(a) if self.local(a).is_none() && self.env.is_alias(self.bindings.module, a) => {
                    format!("{a}.{n}")
                }
                _ => return Ok(None),
            },
            // `Option(i64)` parses as a call.
            ExprKind::Call(n, args) if self.local(n.split('.').next().unwrap_or(n)).is_none() => {
                let Some(key) = self.env.key(self.bindings.module, n) else { return Ok(None) };
                if !self.env.type_decl(&key).is_some_and(|(_, d)| d.is_enum()) {
                    return Ok(None);
                }
                let t = TypeExpr { pos: e.pos, kind: TypeExprKind::Generic(n.clone(), args.clone()) };
                return Ok(Some(EnumRef::Known(self.resolve(&t)?)));
            }
            _ => return Ok(None),
        };
        let Some(key) = self.env.key(self.bindings.module, &name) else { return Ok(None) };
        let Some((_, decl)) = self.env.type_decl(&key) else { return Ok(None) };
        if !decl.is_enum() {
            return Ok(None);
        }
        self.env.check_visible(e.pos, self.bindings.module, &key)?;
        Ok(Some(if decl.params.is_empty() { EnumRef::Known(Type::Enum(key)) } else { EnumRef::Generic(key) }))
    }

    /// Builds variant `variant` of `en`. `args` are the payload values, `None`
    /// when written without parentheses.
    pub(crate) fn construct(
        &mut self,
        pos: Pos,
        en: EnumRef,
        variant: &str,
        args: Option<&[Expr]>,
        expected: Option<&Type>,
    ) -> Result<Reg, Error> {
        let (ty, given) = match en {
            EnumRef::Known(t) => (t, None),
            EnumRef::Generic(key) => match expected {
                Some(t @ Type::Enum(n)) if self.env.instance_decl(n).as_deref() == Some(key.as_str()) => {
                    (t.clone(), None)
                }
                _ => {
                    // Evaluate the payload first: its types decide the instance.
                    let regs = args
                        .unwrap_or_default()
                        .iter()
                        .map(|a| self.expr(a, None))
                        .collect::<Result<Vec<_>, _>>()?;
                    (self.infer_enum(pos, &key, variant, &regs)?, Some(regs))
                }
            },
        };
        let Type::Enum(name) = &ty else { unreachable!() };
        let variants = self.env.enum_variants(pos, name)?;
        let Some(index) = variants.iter().position(|(n, _)| n == variant) else {
            return Err(Error::new(pos, format!("enum `{name}` has no variant `{variant}`")));
        };
        let payload = &variants[index].1;
        let what = format!("`{name}.{variant}`");
        let fields = match (args, given) {
            (None, _) if !payload.is_empty() => {
                let msg = format!("{what} holds {} values; write `{name}.{variant}(...)`", payload.len());
                return Err(Error::new(pos, msg));
            }
            (Some(_), _) if payload.is_empty() => {
                return Err(Error::new(pos, format!("{what} holds no values; drop the parentheses")));
            }
            (None, _) => vec![],
            (Some(args), None) => self.call_args(pos, &what, payload, args)?,
            (Some(args), Some(regs)) => {
                if payload.len() != args.len() {
                    let msg = format!("{what} takes {} arguments, {} given", payload.len(), args.len());
                    return Err(Error::new(pos, msg));
                }
                for (i, ((a, r), want)) in args.iter().zip(&regs).zip(payload).enumerate() {
                    self.expect(a.pos, *r, want, &format!("argument {} of {what}", i + 1))?;
                }
                regs
            }
        };
        Ok(self.emit_to(ty.clone(), |dst| Inst::Variant { dst, index: index as u32, fields }))
    }

    /// The instance of generic enum `key` that variant `variant` with payload
    /// `regs` belongs to: each type parameter is the type of a payload value
    /// declared as exactly that parameter.
    fn infer_enum(&self, pos: Pos, key: &str, variant: &str, regs: &[Reg]) -> Result<Type, Error> {
        let (_, decl) = self.env.type_decl(key).unwrap();
        let name = &decl.name;
        let Some(v) = decl.variants.iter().flatten().find(|v| v.name == variant) else {
            return Err(Error::new(pos, format!("enum `{name}` has no variant `{variant}`")));
        };
        let mut args = Vec::new();
        for p in &decl.params {
            let found = v.fields.iter().zip(regs).find(|(t, _)| matches!(&t.kind, TypeExprKind::Named(n) if *n == p.name));
            match found {
                Some((_, r)) if is_type_param(p) => args.push(self.ty(*r).clone()),
                _ => {
                    let msg = format!(
                        "cannot infer `{}` of `{name}` here; write `{name}(...).{variant}` or give the value a type",
                        p.name
                    );
                    return Err(Error::new(pos, msg));
                }
            }
        }
        Ok(self.env.instance_of(key, args))
    }

    /// `match value { pattern => body ... }`.
    pub(crate) fn match_stmt(&mut self, pos: Pos, value: &Expr, arms: &[MatchArm]) -> Result<(), Error> {
        let s = self.expr(value, None)?;
        let ty = self.ty(s).clone();
        let variants = match &ty {
            Type::Enum(name) => Some(self.env.enum_variants(pos, name)?),
            Type::Int(_) | Type::Bool => None,
            t => {
                return Err(Error::new(value.pos, format!("cannot `match` on {t}; `match` works on enums, integers and bools")))
            }
        };
        // What the arms compare against: the tag of an enum, else the value itself.
        let subject = if variants.is_some() { self.emit_to(Type::Int(IntTy::U32), |dst| Inst::Tag { dst, src: s }) } else { s };

        let end = self.new_block();
        let mut seen: HashSet<i128> = HashSet::new();
        let mut wild = false;
        for arm in arms {
            if wild {
                return Err(Error::new(arm.pos, "unreachable arm: the `_` arm above already matches everything"));
            }
            let body = self.new_block();
            let mut binds: Vec<(Pos, String, Reg)> = Vec::new();
            let test = match (&arm.pattern, &variants) {
                (Pattern::Wild, _) => {
                    wild = true;
                    None
                }
                (Pattern::Variant(name, payload), Some(variants)) => {
                    let Some(index) = variants.iter().position(|(n, _)| n == name) else {
                        let names: Vec<&str> = variants.iter().map(|(n, _)| n.as_str()).collect();
                        let msg = format!("enum `{ty}` has no variant `{name}` (it has {})", names.join(", "));
                        return Err(Error::new(arm.pos, msg));
                    };
                    let want = &variants[index].1;
                    match payload {
                        None if !want.is_empty() => {
                            let ignore = vec!["_"; want.len()].join(", ");
                            let msg = format!("`{name}` holds {} values; write `{name}({ignore})` to ignore them", want.len());
                            return Err(Error::new(arm.pos, msg));
                        }
                        Some(_) if want.is_empty() => {
                            return Err(Error::new(arm.pos, format!("`{name}` holds no values; drop the parentheses")));
                        }
                        Some(names) if names.len() != want.len() => {
                            let msg = format!("`{name}` holds {} values, {} given", want.len(), names.len());
                            return Err(Error::new(arm.pos, msg));
                        }
                        _ => {}
                    }
                    for (i, (p, n)) in payload.iter().flatten().enumerate() {
                        if let Some(n) = n {
                            if binds.iter().any(|(_, b, _)| b == n) {
                                return Err(Error::new(*p, format!("`{n}` is bound twice in this pattern")));
                            }
                            let t = want[i].clone();
                            binds.push((*p, n.clone(), self.new_reg(t)));
                        }
                    }
                    if !seen.insert(index as i128) {
                        return Err(Error::new(arm.pos, format!("unreachable arm: `{name}` is already matched above")));
                    }
                    Some(self.konst(Type::Int(IntTy::U32), index as i64))
                }
                (Pattern::Variant(name, _), None) => {
                    return Err(Error::new(arm.pos, format!("`{name}` is a variant pattern, but the value is {ty}, not an enum")));
                }
                (Pattern::Int(n), None) if matches!(ty, Type::Int(_)) => {
                    let t = ty.as_int().unwrap();
                    if *n < t.min() || *n > t.max() {
                        return Err(Error::new(arm.pos, format!("{n} does not fit in {ty}")));
                    }
                    if !seen.insert(*n) {
                        return Err(Error::new(arm.pos, format!("unreachable arm: {n} is already matched above")));
                    }
                    Some(self.konst(ty.clone(), *n as i64))
                }
                (Pattern::Bool(b), None) if ty == Type::Bool => {
                    if !seen.insert(*b as i128) {
                        return Err(Error::new(arm.pos, format!("unreachable arm: `{b}` is already matched above")));
                    }
                    Some(self.konst(Type::Bool, *b as i64))
                }
                (Pattern::Int(_) | Pattern::Bool(_), _) => {
                    return Err(Error::new(arm.pos, format!("this pattern does not match values of type {ty}")));
                }
            };
            match test {
                Some(k) => {
                    let cond = self.emit_to(Type::Bool, |dst| Inst::Binary { dst, op: BinOp::Eq, lhs: subject, rhs: k });
                    let next = self.new_block();
                    self.terminate(Terminator::Branch { cond, then: body, els: next });
                    self.arm(body, end, s, &arm.pattern, &variants, binds, &arm.body)?;
                    self.switch_to(next);
                }
                None => {
                    self.terminate(Terminator::Jump(body));
                    self.arm(body, end, s, &arm.pattern, &variants, binds, &arm.body)?;
                }
            }
        }

        if !wild {
            // Not matched by any arm: only possible if some value is not covered.
            let missing: Vec<String> = match (&ty, &variants) {
                (_, Some(vs)) => {
                    vs.iter().enumerate().filter(|(i, _)| !seen.contains(&(*i as i128))).map(|(_, (n, _))| format!("`{n}`")).collect()
                }
                (Type::Bool, _) => [false, true].iter().filter(|b| !seen.contains(&(**b as i128))).map(|b| format!("`{b}`")).collect(),
                _ => vec!["every other integer".into()],
            };
            if !missing.is_empty() {
                let msg = format!("`match` does not cover {}; add arms for them, or a `_ => ...` arm", missing.join(", "));
                return Err(Error::new(pos, msg));
            }
            self.terminate(Terminator::Unreachable);
        }
        self.switch_to(end);
        Ok(())
    }

    /// Lowers the body of one arm in block `at`, with its bindings in scope.
    #[allow(clippy::too_many_arguments)]
    fn arm(
        &mut self,
        at: jihoo_ir::BlockId,
        end: jihoo_ir::BlockId,
        s: Reg,
        pattern: &Pattern,
        variants: &Option<crate::env::Variants>,
        binds: Vec<(Pos, String, Reg)>,
        body: &Block,
    ) -> Result<(), Error> {
        self.switch_to(at);
        self.scopes.push(Default::default());
        if let (Pattern::Variant(name, Some(names)), Some(vs)) = (pattern, variants) {
            let variant = vs.iter().position(|(n, _)| n == name).unwrap() as u32;
            let mut binds = binds.into_iter();
            for (index, (_, n)) in names.iter().enumerate() {
                if n.is_some() {
                    let (_, n, dst) = binds.next().unwrap();
                    self.emit(Inst::Payload { dst, src: s, variant, index: index as u32 });
                    self.scopes.last_mut().unwrap().insert(n, dst);
                }
            }
        }
        let r = self.block(body);
        self.scopes.pop();
        r?;
        self.terminate(Terminator::Jump(end));
        Ok(())
    }
}
