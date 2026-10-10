//! Macro hygiene.
//!
//! The names a quote template writes are *marked* when the macro is parsed
//! (`tmp` becomes `tmp#`, see `Parser::mark`), and each expansion numbers its
//! marks (`tmp#7`, `jihoo_syntax::number_marks`). Names that holes insert, the
//! caller's code, are never marked. Once the produced code is parsed, this
//! module decides what each marked name of that expansion means:
//!
//! - A local the expansion declares (`let tmp`, a parameter, a pattern
//!   binding) keeps its marked name, so the caller's code cannot see it and it
//!   cannot capture the caller's `tmp`. A use of it keeps the name too.
//! - Any other marked name refers to something where the macro is defined: it
//!   becomes `#M:name`, which the compiler resolves as if written in module M
//!   (`Env::key`, `Env::viewer`), so a `pub macro` can use its module's private
//!   functions, types and imports.
//! - A name the macro's module does not have is looked up at the call site,
//!   but only among items (`#C:name` for the caller's module C), never the
//!   caller's locals; builtins stay builtins.
//!
//! Macro arguments that produced code passes on to another macro are text, so
//! their marked names are renamed the same way, before that macro sees them.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};

use jihoo_syntax::ast::{names_in_expr, names_in_program, names_in_stmt, Expr, NameRole, Program, Stmt};

use crate::env::Env;

/// Numbers expansions across the whole compilation, item macro rounds and
/// all, so that the locals of two expansions never share a name.
static EXPANSIONS: AtomicU32 = AtomicU32::new(0);

/// One run of a macro: the code it produced, where the macro is defined, and
/// where it was called.
pub(crate) struct Expansion {
    pub id: u32,
    /// The module the macro is declared in.
    pub def: usize,
    /// The module of the code that called it.
    pub caller: usize,
}

/// Code an expansion produced, parsed.
pub(crate) enum Code<'a> {
    Expr(&'a mut Expr),
    Stmts(&'a mut [Stmt]),
    Items(&'a mut Program),
}

impl Code<'_> {
    fn names(&mut self, f: &mut dyn FnMut(&mut String, NameRole)) {
        match self {
            Code::Expr(e) => names_in_expr(e, f),
            Code::Stmts(stmts) => stmts.iter_mut().for_each(|s| names_in_stmt(s, f)),
            Code::Items(p) => names_in_program(p, f),
        }
    }
}

impl Expansion {
    pub(crate) fn new(def: usize, caller: usize) -> Expansion {
        Expansion { id: EXPANSIONS.fetch_add(1, Ordering::Relaxed), def, caller }
    }

    /// Gives the marked names of `code` their meaning (see the module comment).
    /// `is_local` says whether a name is a local of the caller where the code
    /// goes: one an outer expansion declared, used by code passed on to this one.
    pub(crate) fn resolve(&self, env: &Env, mut code: Code, is_local: &dyn Fn(&str) -> bool) {
        let mut declared = HashSet::new();
        code.names(&mut |n, role| {
            if role == NameRole::Binds && n.contains('#') {
                declared.insert(n.clone());
            }
        });
        let rename = |name: &str| self.rename(env, name, &declared, is_local);
        code.names(&mut |n, role| match role {
            NameRole::Binds => {}
            NameRole::Uses => {
                if let Some(new) = rename(n) {
                    *n = new;
                }
            }
            NameRole::Code => *n = jihoo_syntax::mark_names(n, &mut |name| rename(name)),
        });
    }

    /// What `name` becomes, if it is a marked name of this expansion that is
    /// not a local.
    fn rename(&self, env: &Env, name: &str, declared: &HashSet<String>, is_local: &dyn Fn(&str) -> bool) -> Option<String> {
        let (head, rest) = name.split_at(name.find('.').unwrap_or(name.len()));
        let (base, mark) = head.split_once('#')?;
        if base.is_empty() || mark.parse::<u32>().ok()? != self.id || declared.contains(head) || is_local(head) {
            return None;
        }
        Some(if env.defines(self.def, base) {
            format!("#{}:{base}{rest}", self.def)
        } else if crate::is_builtin(base) {
            format!("{base}{rest}")
        } else {
            format!("#{}:{base}{rest}", self.caller)
        })
    }
}

/// `#3:helper` as module 3 and `helper`: a name as written in module 3.
pub(crate) fn qualified(name: &str) -> Option<(usize, &str)> {
    let (m, rest) = name.strip_prefix('#')?.split_once(':')?;
    Some((m.parse().ok()?, rest))
}
