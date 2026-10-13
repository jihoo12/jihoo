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
        // A last parameter `rest: [expr]` takes the remaining arguments.
        let rest = sig.params.last().is_some_and(|t| *t == Type::List(Box::new(Type::Expr)));
        let fixed = sig.params.len() - usize::from(rest);
        if (!rest && args.len() != fixed) || args.len() < fixed {
            let at_least = if rest { "at least " } else { "" };
            return Err(Error::new(pos, format!("macro `{name}` takes {at_least}{fixed} arguments, {} given", args.len())));
        }

        let mut values = Vec::new();
        for ((p, a), want) in decl.params.iter().zip(&args[..fixed]).zip(&sig.params) {
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
        if rest {
            values.push(ConstValue::Agg(args[fixed..].iter().map(|a| ConstValue::Str(a.text.clone())).collect()));
        }

        let expansion = Expansion::new(self.env.fn_module(&key), self.bindings.module);
        let env = self.env;
        let mut type_of = |text: &str| self.type_here(pos, text, &expansion);
        let ConstValue::Str(code) = env.run_typed(pos, vec![], &key, &values, &sig.ret, &mut type_of)? else {
            unreachable!("macros return code")
        };
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
                HoleKind::Type => (matches!(t, Type::Str | Type::Expr), "a `str` (a type, as `type_of` gives)"),
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
                HoleKind::Type => jihoo_ir::HoleKind::Type,
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

    /// `type_of(e)`: the type code `e` would have where the macro was called,
    /// as the text of a type, which a type hole (`let x: $t`) takes. The VM
    /// asks for types it was not given, and the macro runs again with them
    /// (`expand_code`).
    pub(crate) fn type_of(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        if !self.in_macro {
            return Err(Error::new(pos, "`type_of` can only be used inside a macro"));
        }
        let [arg] = args else {
            return Err(Error::new(pos, "`type_of` takes exactly 1 argument"));
        };
        let r = self.expr(arg, None)?;
        if *self.ty(r) != Type::Expr {
            return Err(Error::new(arg.pos, format!("`type_of` needs an `expr`, not {}", self.ty(r))));
        }
        Ok(self.emit_to(Type::Str, |dst| Inst::TypeOf { dst, code: r }))
    }

    /// `type_shape(t)`: the parts of the struct or enum type that code `t`
    /// names where the macro was called, as text: `struct a b` (the field
    /// names), `enum V/2 E/0` (the variants and how many values each holds),
    /// or `other`. It asks the compiler as `type_of` does, with the code
    /// marked `#shape `; `lib/meta.jh` reads the text (`meta.shape`).
    pub(crate) fn type_shape(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        if !self.in_macro {
            return Err(Error::new(pos, "`type_shape` can only be used inside a macro"));
        }
        let [arg] = args else {
            return Err(Error::new(pos, "`type_shape` takes exactly 1 argument"));
        };
        let r = self.expr(arg, None)?;
        if *self.ty(r) != Type::Expr {
            return Err(Error::new(arg.pos, format!("`type_shape` needs an `expr`, not {}", self.ty(r))));
        }
        let text = self.emit_to(Type::Str, |dst| Inst::CodeText { dst, src: r });
        let mark = self.emit_to(Type::Str, |dst| Inst::Str { dst, value: SHAPE.to_string() });
        let marked = self.emit_to(Type::Str, |dst| Inst::Binary { dst, op: jihoo_ir::BinOp::Add, lhs: mark, rhs: text });
        let code = self.emit_to(Type::Expr, |dst| Inst::CodeText { dst, src: marked });
        Ok(self.emit_to(Type::Str, |dst| Inst::TypeOf { dst, code }))
    }

    /// `to_items(s)`: text as items, which an item macro may return.
    pub(crate) fn text_to_items(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        if !self.in_macro {
            return Err(Error::new(pos, "`to_items` can only be used inside a macro"));
        }
        let [arg] = args else {
            return Err(Error::new(pos, "`to_items` takes exactly 1 argument"));
        };
        let r = self.expr(arg, Some(&Type::Str))?;
        self.expect(arg.pos, r, &Type::Str, "the argument of `to_items`")?;
        Ok(self.emit_to(Type::Items, |dst| Inst::CodeText { dst, src: r }))
    }

    /// The parts of the type that `text` names here, for `type_shape`.
    fn shape_here(&mut self, pos: Pos, text: &str, expansion: &Expansion) -> Result<String, Error> {
        let code = jihoo_syntax::number_marks(text, expansion.id);
        let mut e = jihoo_syntax::parse_expr(&code)
            .map_err(|err| Error::new(pos, plain_names(&format!("`type_shape` got code that does not parse: {} in `{code}`", err.msg))))?;
        expansion.resolve(self.env, Code::Expr(&mut e), &|n| self.is_local_name(n));
        set_pos(&mut e, pos);
        let t = crate::generic::expr_to_type(&e).and_then(|t| self.resolve(&t)).map_err(|err| {
            Error::new(err.pos, plain_names(&format!("{} (in `type_shape`, which needs a type)", err.msg)))
        })?;
        let shape = match &t {
            Type::Struct(_) if self.env.tuple_parts(&t).is_some() => "other".to_string(),
            Type::Struct(n) => {
                let fields = self.env.struct_fields(pos, n)?;
                std::iter::once("struct".to_string()).chain(fields.iter().map(|(f, _)| f.clone())).collect::<Vec<_>>().join(" ")
            }
            Type::Enum(n) => {
                let variants = self.env.enum_variants(pos, n)?;
                let parts = variants.iter().map(|(v, ts)| format!("{v}/{}", ts.len()));
                std::iter::once("enum".to_string()).chain(parts).collect::<Vec<_>>().join(" ")
            }
            _ => "other".to_string(),
        };
        Ok(shape)
    }

    /// The type `text`, code from expansion `expansion`, has here, where the
    /// macro was called: the code is lowered as it would be, and then taken
    /// out again, so only its type is left.
    fn type_here(&mut self, pos: Pos, text: &str, expansion: &Expansion) -> Result<String, Error> {
        if let Some(rest) = text.strip_prefix(SHAPE) {
            return self.shape_here(pos, rest, expansion);
        }
        let code = jihoo_syntax::number_marks(text, expansion.id);
        let mut e = jihoo_syntax::parse_expr(&code)
            .map_err(|err| Error::new(pos, plain_names(&format!("`type_of` got code that does not parse: {} in `{code}`", err.msg))))?;
        expansion.resolve(self.env, Code::Expr(&mut e), &|n| self.is_local_name(n));
        set_pos(&mut e, pos);
        let saved = self.save();
        let r = self.expr(&e, None).map(|r| self.ty(r).clone());
        self.restore(saved);
        let t = r.map_err(|err| {
            let msg = format!("{} (in `type_of`, which sees the names where the macro is called, not the ones its code declares)", err.msg);
            Error::new(err.pos, plain_names(&msg))
        })?;
        Ok(self.env.spell_type(&t))
    }

    /// `code_text(e)`: the text of code with its hygiene marks, for code that
    /// reads code (`lib/meta.jh`); `to_code(s)` turns such text back into code.
    pub(crate) fn code_text(&mut self, pos: Pos, name: &str, args: &[Expr]) -> Result<Reg, Error> {
        if !self.in_macro {
            return Err(Error::new(pos, format!("`{name}` can only be used inside a macro")));
        }
        let [arg] = args else {
            return Err(Error::new(pos, format!("`{name}` takes exactly 1 argument")));
        };
        let to_code = name == "to_code";
        let r = self.expr(arg, to_code.then_some(&Type::Str))?;
        if to_code {
            self.expect(arg.pos, r, &Type::Str, "the argument of `to_code`")?;
            return Ok(self.emit_to(Type::Expr, |dst| Inst::CodeText { dst, src: r }));
        }
        if !self.ty(r).is_code() {
            return Err(Error::new(arg.pos, format!("`code_text` needs code, not {}", self.ty(r))));
        }
        Ok(self.emit_to(Type::Str, |dst| Inst::CodeText { dst, src: r }))
    }
}

/// What marks a question of `type_shape` among those of `type_of`.
const SHAPE: &str = "#shape ";

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
