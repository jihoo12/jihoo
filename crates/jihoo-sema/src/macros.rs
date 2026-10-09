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
//! compiled in place of the call, in the caller's scope: macros are not hygienic.
//!
//! Code values are kept as text while the macro runs. Every quote template was
//! checked to be an expression when the macro was parsed, and holes are inserted
//! with parentheses, so the result always parses.

use jihoo_ir::{Inst, Reg, Type};
use jihoo_syntax::ast::{set_pos, Expr, MacroArg};
use jihoo_syntax::{Error, Pos};

use crate::comptime::ConstValue;
use crate::FnCx;

/// Bound on nested expansions, to stop macros that expand to themselves.
const MAX_DEPTH: u32 = 64;

impl FnCx<'_> {
    /// Runs macro `name` on `args` and parses the code it returns.
    pub(crate) fn expand(&mut self, pos: Pos, name: &str, args: &[MacroArg]) -> Result<Expr, Error> {
        let Some(decl) = self.env.macro_decl(name) else {
            return Err(Error::new(pos, format!("`{name}` is not a macro")));
        };
        if self.macro_depth >= MAX_DEPTH {
            return Err(Error::new(pos, format!("macro expansion is too deep (over {MAX_DEPTH}); does `{name}!` expand to itself?")));
        }
        let sig = self.env.signature(name).unwrap()?;
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

        let ConstValue::Str(code) = self.env.run(pos, vec![], name, &values, &Type::Expr)? else {
            unreachable!("macros return `expr`")
        };
        let mut e = jihoo_syntax::parse_expr(&code).map_err(|err| {
            Error::new(pos, format!("`{name}!` produced code that does not parse: {} in `{code}`", err.msg))
        })?;
        set_pos(&mut e, pos);
        Ok(e)
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
        r.map_err(|mut e| {
            if !e.msg.contains("produced by `") {
                e.msg = format!("{} (in code produced by `{name}!`)", e.msg);
            }
            e
        })
    }

    pub(crate) fn quote(&mut self, pos: Pos, pieces: &[String], holes: &[Expr]) -> Result<Reg, Error> {
        if !self.in_macro {
            return Err(Error::new(pos, "`quote` can only be used inside a macro"));
        }
        let mut regs = Vec::new();
        for h in holes {
            let r = self.expr(h, None)?;
            if !matches!(self.ty(r), Type::Expr | Type::Int(_) | Type::Bool | Type::Str) {
                return Err(Error::new(
                    h.pos,
                    format!("only `expr`, integers, bools and `str` can be inserted into code, not {}", self.ty(r)),
                ));
            }
            regs.push(r);
        }
        let pieces = pieces.to_vec();
        Ok(self.emit_to(Type::Expr, |dst| Inst::Quote { dst, pieces, holes: regs }))
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
        self.expect(arg.pos, r, &Type::Expr, "the argument of `stringify`")?;
        // Both are text on the VM, so this is just a change of type.
        Ok(self.emit_to(Type::Str, |dst| Inst::Copy { dst, src: r }))
    }
}
