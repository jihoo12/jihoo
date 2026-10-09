//! Enums: building variants. (`match` is in `patterns.rs`.)
//!
//! A variant is written after the enum that declares it: `Shape.Circle(2)`,
//! `Shape.Empty`, `geo.Shape.Empty`, `Option(i64).Some(1)`. For a generic enum
//! the type arguments can be left out (`Option.Some(1)`): they come from the
//! expected type, or else from the payload values.

use jihoo_ir::{Inst, Reg, Type};
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
                    // Evaluate the payload in order. Once the values so far decide
                    // the instance, the rest get its payload types as hints, so
                    // `List.Cons(1, ref List.Nil)` works.
                    let args = args.unwrap_or_default();
                    let mut regs = Vec::with_capacity(args.len());
                    let mut found: Result<Type, String> = Err(String::new());
                    for (i, a) in args.iter().enumerate() {
                        let hint = match &found {
                            Ok(t) => self.payload_hint(t, variant, i)?,
                            Err(_) => None,
                        };
                        regs.push(self.expr(a, hint.as_ref())?);
                        if found.is_err() {
                            found = self.infer_enum(pos, &key, variant, &regs)?;
                        }
                    }
                    if args.is_empty() {
                        found = self.infer_enum(pos, &key, variant, &regs)?;
                    }
                    let ty = found.map_err(|param| {
                        let name = &self.env.type_decl(&key).unwrap().1.name;
                        let msg = format!(
                            "cannot infer `{param}` of `{name}` here; write `{name}(...).{variant}` or give the value a type"
                        );
                        Error::new(pos, msg)
                    })?;
                    (ty, Some(regs))
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

    /// The instance of generic enum `key` that variant `variant` with the first
    /// payload values `regs` belongs to: each type parameter is the type of a
    /// payload value declared as exactly that parameter. `Err` names a
    /// parameter that these values do not decide.
    fn infer_enum(&self, pos: Pos, key: &str, variant: &str, regs: &[Reg]) -> Result<Result<Type, String>, Error> {
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
                _ => return Ok(Err(p.name.clone())),
            }
        }
        Ok(Ok(self.env.instance_of(key, args)))
    }

    /// The type of payload value `i` of variant `variant` of enum type `t`, if
    /// there is one.
    fn payload_hint(&self, t: &Type, variant: &str, i: usize) -> Result<Option<Type>, Error> {
        let Type::Enum(name) = t else { return Ok(None) };
        let variants = self.env.enum_variants(Pos::default(), name)?;
        Ok(variants.iter().find(|(n, _)| n == variant).and_then(|(_, ts)| ts.get(i).cloned()))
    }
}
