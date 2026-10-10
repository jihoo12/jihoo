//! Generic functions: `comptime` parameters.
//!
//! A function with `comptime` parameters is not compiled on its own. Each call
//! evaluates the comptime arguments (types, or values computed on the VM) and
//! asks for the *instance* of the function for exactly those arguments; instances
//! are compiled once and named `name.N` in JIR. Inside an instance, the comptime
//! parameters are *bindings*: `T` resolves to a type, `N` to a constant.
//!
//! As with C++ templates and Zig, the body of a generic function is checked only
//! when it is instantiated.

use std::rc::Rc;

use jihoo_ir::{Inst, Reg, Type};
use jihoo_syntax::ast::*;
use jihoo_syntax::{Error, Pos};

use crate::comptime::ConstValue;
use crate::env::Env;
use crate::FnCx;

#[derive(Debug, Clone)]
pub(crate) enum Binding {
    Type(Type),
    Value(Type, ConstValue),
    /// A closure passed to a `comptime` function parameter: the lifted function
    /// `func` and the types of its captured values. The instance receives the
    /// captured values as hidden arguments after its own, and calls `func`
    /// directly.
    Closure { ty: Type, func: String, captures: Vec<Type> },
}

/// What [`Env::bind`] asks before evaluating a comptime argument of type `want`
/// on the VM: a binding made some other way (a closure), if any.
pub(crate) type BindHook<'h> = dyn FnMut(&Expr, &Type) -> Result<Option<Binding>, Error> + 'h;

/// The scope that names are resolved in: the module the code is written in, and
/// the comptime parameters of the instance being compiled, in declaration order.
#[derive(Debug, Clone, Default)]
pub(crate) struct Bindings {
    pub module: usize,
    items: Vec<(String, Binding)>,
}

impl Bindings {
    pub fn in_module(module: usize) -> Self {
        Bindings { module, items: Vec::new() }
    }

    pub fn get(&self, name: &str) -> Option<&Binding> {
        self.items.iter().rev().find(|(n, _)| n == name).map(|(_, b)| b)
    }

    pub(crate) fn push(&mut self, name: &str, b: Binding) {
        self.items.push((name.to_string(), b));
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The closures bound, in order, with the types of their captured values:
    /// the hidden parameters of the instance.
    pub fn closures(&self) -> impl Iterator<Item = (&str, &[Type])> {
        self.items.iter().filter_map(|(n, b)| match b {
            Binding::Closure { captures, .. } => Some((n.as_str(), captures.as_slice())),
            _ => None,
        })
    }

    /// The bound values as they would be written as arguments: `i64, 4`.
    pub fn args(&self) -> String {
        let parts: Vec<String> = self
            .items
            .iter()
            .map(|(_, b)| match b {
                Binding::Type(t) => t.to_string(),
                Binding::Value(_, ConstValue::Int(v)) => v.to_string(),
                Binding::Value(Type::Float(t), ConstValue::Float(x)) => jihoo_vm::float_text(*t, f64::from_bits(*x)),
                Binding::Value(_, ConstValue::Bool(v)) => v.to_string(),
                Binding::Value(_, ConstValue::Func(f)) | Binding::Closure { func: f, .. } => f.clone(),
                Binding::Value(_, v) => format!("{v:?}"),
            })
            .collect();
        parts.join(", ")
    }

    /// Identifies the instance: equal keys mean equal bindings.
    pub fn key(&self) -> String {
        let parts: Vec<String> = self
            .items
            .iter()
            .map(|(_, b)| match b {
                Binding::Type(t) => t.jir(),
                Binding::Value(t, v) => format!("{}:{v:?}", t.jir()),
                Binding::Closure { ty, func, .. } => format!("{}:closure {func}", ty.jir()),
            })
            .collect();
        parts.join(",")
    }

    /// For error messages: `T = u8, N = 4`.
    pub fn describe(&self) -> String {
        let parts: Vec<String> = self
            .items
            .iter()
            .map(|(n, b)| match b {
                Binding::Type(t) => format!("{n} = {t}"),
                Binding::Value(_, ConstValue::Int(v)) => format!("{n} = {v}"),
                Binding::Value(Type::Float(t), ConstValue::Float(x)) => format!("{n} = {}", jihoo_vm::float_text(*t, f64::from_bits(*x))),
                Binding::Value(_, ConstValue::Func(f)) | Binding::Closure { func: f, .. } => format!("{n} = {f}"),
                Binding::Value(_, v) => format!("{n} = {v:?}"),
            })
            .collect();
        parts.join(", ")
    }
}

pub(crate) fn is_type_param(p: &Param) -> bool {
    matches!(&p.ty.kind, TypeExprKind::Named(n) if n == "type")
}

/// Reads an argument written in expression syntax as a type: `u8`, `*u8` (a
/// dereference), `ref u8` and `[u8; 4]` (an array repeat) all parse as expressions.
fn expr_to_type(e: &Expr) -> Result<TypeExpr, Error> {
    let kind = match &e.kind {
        ExprKind::Var(n) => TypeExprKind::Named(n.clone()),
        ExprKind::Deref(inner) => TypeExprKind::Ptr(Box::new(expr_to_type(inner)?)),
        ExprKind::NewRef(inner) => TypeExprKind::Ref(Box::new(expr_to_type(inner)?)),
        ExprKind::ArrayRepeat(elem, n) => TypeExprKind::Array(Box::new(expr_to_type(elem)?), n.clone()),
        // `Pair(u8)` parses as a call.
        ExprKind::Call(name, args) => TypeExprKind::Generic(name.clone(), args.clone()),
        ExprKind::Type(t) => return Ok(t.clone()),
        _ => return Err(Error::new(e.pos, "expected a type")),
    };
    Ok(TypeExpr { pos: e.pos, kind })
}

impl FnCx<'_> {
    /// Calls generic function `key` (its module-qualified name).
    pub(crate) fn call_generic(&mut self, pos: Pos, key: &str, decl: &FnDecl, args: &[Expr]) -> Result<Reg, Error> {
        let name = &decl.name;
        if decl.params.len() != args.len() {
            return Err(Error::new(
                pos,
                format!("`{name}` takes {} arguments, {} given", decl.params.len(), args.len()),
            ));
        }

        // Comptime arguments first: they determine the instance. A closure is
        // lowered here, where its captured locals are; its captured values
        // become hidden arguments.
        let comptime = decl.params.iter().zip(args).filter(|(p, _)| p.comptime);
        let env = self.env;
        let outer = self.bindings.clone();
        let mut hidden = Vec::new();
        let b = env.bind(name, env.fn_module(key), comptime, &outer, &mut |a, want| {
            self.closure_arg(a, want, &mut hidden)
        })?;
        let (instance, sig) = self.env.instance(pos, key, decl, b)?;

        let mut regs = Vec::new();
        let runtime = decl.params.iter().zip(args).filter(|(p, _)| !p.comptime);
        for ((p, a), want) in runtime.zip(&sig.params) {
            let r = self.expr(a, Some(want))?;
            self.expect(a.pos, r, want, &format!("argument `{}` of `{name}`", p.name))?;
            regs.push(r);
        }
        regs.extend(hidden);
        Ok(self.emit_to(sig.ret.clone(), |dst| Inst::Call { dst, func: instance, args: regs }))
    }

    /// A comptime argument of function type `want` that is a closure: a lambda
    /// written here, or a closure this instance received itself. Its captured
    /// values are added to `hidden`. A lambda that captures nothing is an
    /// ordinary function value.
    fn closure_arg(&mut self, a: &Expr, want: &Type, hidden: &mut Vec<Reg>) -> Result<Option<Binding>, Error> {
        if !matches!(want, Type::Fn(..)) {
            return Ok(None);
        }
        match &a.kind {
            ExprKind::Lambda(l) => {
                let lifted = self.lift(l, Some(want))?;
                if lifted.captured.is_empty() {
                    return Ok(Some(Binding::Value(lifted.ty, ConstValue::Func(lifted.func))));
                }
                let captures = lifted.captured.iter().map(|r| self.ty(*r).clone()).collect();
                hidden.extend(lifted.captured);
                Ok(Some(Binding::Closure { ty: lifted.ty, func: lifted.func, captures }))
            }
            ExprKind::Var(n) if self.local(n).is_none() => match self.bindings.get(n) {
                Some(b @ Binding::Closure { .. }) => {
                    let b = b.clone();
                    hidden.extend(self.closure_regs[n.as_str()].iter().copied());
                    Ok(Some(b))
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }
}

impl<'p> Env<'p> {
    /// Evaluates comptime arguments into bindings for the matching parameters of
    /// `owner` (a function or struct). `outer` gives the bindings of the code the
    /// arguments are written in.
    pub fn bind<'a>(
        &self,
        owner: &str,
        owner_module: usize,
        params_args: impl Iterator<Item = (&'a Param, &'a Expr)>,
        outer: &Rc<Bindings>,
        hook: &mut BindHook<'_>,
    ) -> Result<Bindings, Error> {
        // Parameter types are written in the owner's module; arguments in `outer`.
        let mut b = Bindings::in_module(owner_module);
        for (p, a) in params_args {
            if is_type_param(p) {
                let t = self.resolve(&expr_to_type(a)?, outer)?;
                b.push(&p.name, Binding::Type(t));
            } else {
                // The parameter's type may use earlier parameters: `comptime x: T`.
                let want = self.resolve(&p.ty, &Rc::new(b.clone()))?;
                let binding = match hook(a, &want)? {
                    Some(binding) => binding,
                    None => {
                        let (ty, v) = self.comptime(a, Some(&want), outer)?;
                        Binding::Value(ty, v)
                    }
                };
                let (Binding::Value(ty, _) | Binding::Closure { ty, .. }) = &binding else { unreachable!() };
                if *ty != want {
                    let msg = format!("comptime argument `{}` of `{owner}` must be {want}, found {ty}", p.name);
                    return Err(Error::new(a.pos, msg));
                }
                b.push(&p.name, binding);
            }
        }
        Ok(b)
    }

    /// Signature-only part of instantiating `decl`: the body is compiled later, so
    /// an instance can call itself recursively.
    pub fn instance(
        &self,
        pos: Pos,
        fn_key: &str,
        decl: &FnDecl,
        b: Bindings,
    ) -> Result<(String, Rc<crate::env::Sig>), Error> {
        let key = format!("{fn_key}({})", b.key());
        if let Some((name, sig)) = self.find_instance(&key) {
            return sig.map(|s| (name, s));
        }
        let n = self.instance_count();
        if n >= MAX_INSTANCES {
            return Err(Error::new(
                pos,
                format!("too many instances of generic functions (over {MAX_INSTANCES}); does `{}` instantiate itself forever?", decl.name),
            ));
        }
        let name = format!("{fn_key}.{n}");
        let b = Rc::new(b);
        let sig = (|| {
            let mut params: Vec<Type> = decl
                .params
                .iter()
                .filter(|p| !p.comptime)
                .map(|p| self.resolve(&p.ty, &b))
                .collect::<Result<_, _>>()?;
            // The captured values of closures come last.
            params.extend(b.closures().flat_map(|(_, captures)| captures.iter().cloned()));
            let ret = match &decl.ret {
                Some(t) => self.resolve(t, &b)?,
                None => Type::Unit,
            };
            Ok(Rc::new(crate::env::Sig { params, ret }))
        })();
        self.add_instance(key, name.clone(), fn_key.to_string(), b, sig.clone());
        sig.map(|s| (name, s))
    }
}

/// Bound on the number of instances, to stop runaway instantiation.
pub(crate) const MAX_INSTANCES: u32 = 1000;
