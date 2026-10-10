//! Macros: functions that run at compile time and return code.
//!
//! ```text
//! macro power(x: expr, n: i64) -> expr {
//!     let e = quote(1)
//!     let i = 0
//!     while i < n {
//!         e = quote($e * $x)
//!         i = i + 1
//!     }
//!     return e
//! }
//!
//! power!(y, 3)    // expands to ((((1) * (y)) * (y)) * (y))
//! ```
//!
//! A macro is compiled like any function and run on the VM. Its `expr`
//! parameters receive the *source text* of the arguments; its other parameters
//! (integers, bools, `str`) receive their values, computed at compile time.
//! `quote(...)` builds code from a template whose holes (`$x`, `$(e)`) insert an
//! `expr` in parentheses, or a value as a literal. The returned code is parsed and
//! compiled in place of the call, in the caller's scope, with the names the
//! template wrote kept apart from the caller's (see `hygiene.rs`).
//!
//! Code values are kept as text while the macro runs. Every quote template was
//! checked to be an expression when the macro was parsed, and holes are inserted
//! with parentheses, so the result always parses.

use jihoo_ir::{plain_names, Inst, Reg, Type};
use jihoo_syntax::ast::{set_pos, set_stmt_pos, CodeKind, Expr, HoleKind, MacroArg};
use jihoo_syntax::{Error, Pos};

use crate::comptime::ConstValue;
use crate::hygiene::{Code, Expansion};
use crate::FnCx;

/// Bound on nested expansions, to stop macros that expand to themselves.
const MAX_DEPTH: u32 = 64;

impl FnCx<'_> {
    /// Runs macro `name` on `args`; returns the kind of code (`expr`, `stmts` or
    /// `items`), its text with the hygiene marks numbered, and the expansion,
    /// which resolves those marks once the code is parsed.
    pub(crate) fn expand_code(
        &mut self,
        pos: Pos,
        name: &str,
        args: &[MacroArg],
    ) -> Result<(Type, String, Expansion), Error> {
        let key = self.env.key_or_err(pos, &self.bindings, name)?;
        self.env.check_visible(pos, self.env.viewer(self.bindings.module, name), &key)?;
        let Some(decl) = self.env.macro_decl(&key) else {
            return Err(Error::new(pos, format!("`{name}` is not a macro")));
        };
        if self.macro_depth >= MAX_DEPTH {
            return Err(Error::new(pos, format!("macro expansion is too deep (over {MAX_DEPTH}); does `{name}!` expand to itself?")));
        }
        let sig = self.env.signature(&key).unwrap()?;
        if sig.params.len() != args.len() {
            return Err(Error::new(
                pos,
                format!("macro `{name}` takes {} arguments, {} given", sig.params.len(), args.len()),
            ));
        }

        let mut values = Vec::new();
        for ((p, a), want) in decl.params.iter().zip(args).zip(&sig.params) {
            if *want == Type::Expr {
                values.push(ConstValue::Str(a.text.clone()));
            } else {
                let (ty, v) = self.env.comptime_in(&a.expr, Some(want), &self.bindings, true)?;
                if ty != *want {
                    let msg = format!("argument `{}` of macro `{name}` must be {want}, found {ty}", p.name);
                    return Err(Error::new(a.expr.pos, msg));
                }
                values.push(v);
            }
        }

        let ConstValue::Str(code) = self.env.run(pos, vec![], &key, &values, &sig.ret)? else {
            unreachable!("macros return code")
        };
        let expansion = Expansion::new(self.env.fn_module(&key), self.bindings.module);
        let code = jihoo_syntax::number_marks(&code, expansion.id);
        Ok((sig.ret.clone(), code, expansion))
    }

    /// True if `name` is a local or a comptime parameter where code is being
    /// compiled.
    fn is_local_name(&self, name: &str) -> bool {
        self.local(name).is_some() || self.bindings.get(name).is_some()
    }

    /// The kind of code macro `name` returns, if it is a macro.
    pub(crate) fn macro_kind(&self, name: &str) -> Option<Type> {
        let key = self.env.key(self.bindings.module, name)?;
        self.env.macro_decl(&key)?;
        Some(self.env.signature(&key)?.ok()?.ret.clone())
    }

    /// Expands a macro used as an expression.
    pub(crate) fn expand(&mut self, pos: Pos, name: &str, args: &[MacroArg]) -> Result<Expr, Error> {
        let (kind, code, expansion) = self.expand_code(pos, name, args)?;
        if kind != Type::Expr {
            let where_ = if kind == Type::Stmts { "on a line of its own" } else { "at the top level" };
            return Err(Error::new(pos, format!("`{name}!` produces {kind}, so use it {where_}")));
        }
        let mut e = jihoo_syntax::parse_expr(&code).map_err(|err| bad_code(pos, name, &err, &code))?;
        expansion.resolve(self.env, Code::Expr(&mut e), &|n| self.is_local_name(n));
        set_pos(&mut e, pos);
        Ok(e)
    }

    /// Expands a statement macro in place: its statements join the current block.
    pub(crate) fn macro_stmts(&mut self, pos: Pos, name: &str, args: &[MacroArg]) -> Result<(), Error> {
        let (_, code, expansion) = self.expand_code(pos, name, args)?;
        let mut stmts = jihoo_syntax::parse_stmts(&code).map_err(|err| bad_code(pos, name, &err, &code))?;
        expansion.resolve(self.env, Code::Stmts(&mut stmts), &|n| self.is_local_name(n));
        self.macro_depth += 1;
        let r = stmts.iter_mut().try_for_each(|s| {
            set_stmt_pos(s, pos);
            self.stmt(s)
        });
        self.macro_depth -= 1;
        r.map_err(|e| in_macro(e, name))
    }

    pub(crate) fn macro_call(
        &mut self,
        pos: Pos,
        name: &str,
        args: &[MacroArg],
        expected: Option<&Type>,
    ) -> Result<Reg, Error> {
        let expanded = self.expand(pos, name, args)?;
        self.macro_depth += 1;
        let r = self.expr(&expanded, expected);
        self.macro_depth -= 1;
        r.map_err(|e| in_macro(e, name))
    }

    pub(crate) fn quote(
        &mut self,
        pos: Pos,
        kind: CodeKind,
        pieces: &[String],
        holes: &[(HoleKind, Expr)],
    ) -> Result<Reg, Error> {
        if !self.in_macro {
            return Err(Error::new(pos, "`quote` can only be used inside a macro"));
        }
        let mut regs = Vec::new();
        let mut kinds = Vec::new();
        for (hk, h) in holes {
            let r = self.expr(h, None)?;
            let t = self.ty(r).clone();
            let (ok, want) = match hk {
                HoleKind::Expr => (matches!(t, Type::Expr | Type::Int(_) | Type::Float(_) | Type::Bool | Type::Str), "`expr`, a number, a bool or a `str`"),
                HoleKind::Ident => (matches!(t, Type::Str | Type::Expr), "a `str` (a name)"),
                HoleKind::Stmts => (matches!(t, Type::Stmts | Type::Expr), "`stmts` or `expr`"),
                HoleKind::Items => (t == Type::Items, "`items`"),
            };
            if !ok {
                return Err(Error::new(h.pos, format!("this hole needs {want}, not {t}")));
            }
            regs.push(r);
            kinds.push(match hk {
                HoleKind::Expr => jihoo_ir::HoleKind::Expr,
                HoleKind::Ident => jihoo_ir::HoleKind::Ident,
                HoleKind::Stmts => jihoo_ir::HoleKind::Stmts,
                HoleKind::Items => jihoo_ir::HoleKind::Items,
            });
        }
        let ty = match kind {
            CodeKind::Expr => Type::Expr,
            CodeKind::Stmts => Type::Stmts,
            CodeKind::Items => Type::Items,
        };
        let pieces = pieces.to_vec();
        Ok(self.emit_to(ty, |dst| Inst::Quote { dst, pieces, holes: regs, kinds }))
    }

    /// `stringify(e)`: the source text of a piece of code, as a `str`.
    pub(crate) fn stringify(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        if !self.in_macro {
            return Err(Error::new(pos, "`stringify` can only be used inside a macro"));
        }
        let [arg] = args else {
            return Err(Error::new(pos, "`stringify` takes exactly 1 argument"));
        };
        let r = self.expr(arg, None)?;
        if !self.ty(r).is_code() {
            return Err(Error::new(arg.pos, format!("`stringify` needs code, not {}", self.ty(r))));
        }
        // Code is text on the VM; this drops the hygiene marks of its names.
        Ok(self.emit_to(Type::Str, |dst| Inst::Stringify { dst, code: r }))
    }
}

fn bad_code(pos: Pos, name: &str, err: &Error, code: &str) -> Error {
    let msg = format!("`{name}!` produced code that does not parse: {} in `{code}`", err.msg);
    Error::new(pos, plain_names(&msg))
}

/// Says which macro made the code an error is in.
pub(crate) fn in_macro(mut e: Error, name: &str) -> Error {
    if !e.msg.contains("produced by `") {
        e.msg = format!("{} (in code produced by `{}!`)", plain_names(&e.msg), plain_names(name));
    }
    e
}
