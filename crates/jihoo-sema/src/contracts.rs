//! Contracts: `requires` and `ensures` clauses on functions.
//!
//! ```jihoo
//! fn isqrt(n: i64) -> i64
//!     requires n >= 0
//!     ensures result * result <= n
//! { ... }
//! ```
//!
//! Every clause is checked when the program runs, in every profile: the
//! `requires` clauses at the start of the body, the `ensures` clauses in one
//! exit block that every `return` goes to, with `result` the value returned
//! and the parameters as they were when the function was called. A clause that
//! is false calls a function of its own, `f.requires.0` or `f.ensures.1`, that
//! traps, so the VM's error (and a debugger's backtrace) names the clause.
//!
//! The compiler also tries to break every contract before the program runs
//! (`refute.rs`).
//!
//! A function with `ensures` makes no tail calls: the check runs after the
//! call returns. Its calls of itself are ordinary calls too, so that each one's
//! result is checked against its own parameters.

use std::collections::{HashMap, HashSet};

use jihoo_ir::{BlockId, Inst, Reg, Terminator, Type};
use jihoo_syntax::ast::{names_in_expr, Contract, ContractKind, FnDecl, NameRole};
use jihoo_syntax::Error;

use crate::FnCx;

/// Where the `return`s of a function with `ensures` go.
pub(crate) struct Exit {
    block: BlockId,
    /// The value returned, assigned by every `return`.
    result: Reg,
    /// `result`, and copies of the parameters the clauses read, made at the
    /// start.
    scope: HashMap<String, Reg>,
    used: bool,
}

/// The JIR name of the function that clause `i` of `func` calls when it is
/// false.
pub(crate) fn breach_name(func: &str, kind: ContractKind, i: usize) -> String {
    format!("{func}.{}.{i}", kind.word())
}

/// `fn name(params) { trap() }`.
fn breach_fn(name: String, params: Vec<Type>) -> jihoo_ir::Function {
    jihoo_ir::Function {
        name,
        export: None,
        regs: params.clone(),
        params,
        ret: Type::Unit,
        blocks: vec![jihoo_ir::Block { insts: vec![Inst::Trap], term: Terminator::Unreachable }],
    }
}

/// The names `e` refers to.
pub(crate) fn names_used(e: &jihoo_syntax::ast::Expr) -> HashSet<String> {
    let mut e = e.clone();
    let mut out = HashSet::new();
    names_in_expr(&mut e, &mut |n, role| {
        if role == NameRole::Uses {
            out.insert(n.clone());
        }
    });
    out
}

impl FnCx<'_> {
    /// Before the body of `f`, lowered as `name`: checks the contract's
    /// clauses can be written here, emits the `requires` checks and gets ready
    /// for the `ensures` ones.
    pub(crate) fn contracts_start(&mut self, f: &FnDecl, name: &str) -> Result<(), Error> {
        if f.contracts.is_empty() {
            return Ok(());
        }
        let ensures: Vec<&Contract> = f.contracts.iter().filter(|c| c.kind == ContractKind::Ensures).collect();
        if let Some(c) = ensures.first() {
            if self.sig.ret == Type::Unit {
                let msg = format!("`ensures` is about the value `{}` returns, and it returns nothing", f.name);
                return Err(Error::new(c.pos, msg));
            }
            if let Some(p) = f.params.iter().find(|p| p.name == "result") {
                return Err(Error::new(p.pos, "a function with `ensures` cannot have a parameter named `result`; `result` is the value it returns"));
            }
        }
        // An `ensures` breach gets the value returned, which the compiler
        // shows when it finds one (`refute.rs`), as can a debugger.
        for (i, c) in f.contracts.iter().enumerate() {
            let params = if c.kind == ContractKind::Ensures { vec![self.sig.ret.clone()] } else { vec![] };
            self.env.add_lambda(breach_fn(breach_name(name, c.kind, i), params), true);
        }

        // The parameters as they were called, for `ensures`.
        let mut scope = HashMap::new();
        let read: HashSet<String> = ensures.iter().flat_map(|c| names_used(&c.cond)).collect();
        for p in f.params.iter().filter(|p| !p.comptime && read.contains(&p.name)) {
            let src = self.scopes[0][&p.name];
            let copy = self.emit_to(self.ty(src).clone(), |dst| Inst::Copy { dst, src });
            scope.insert(p.name.clone(), copy);
        }

        for (i, c) in f.contracts.iter().enumerate().filter(|(_, c)| c.kind == ContractKind::Requires) {
            self.check_clause(c, breach_name(name, c.kind, i), vec![])?;
        }

        if !ensures.is_empty() {
            let result = self.new_reg(self.sig.ret.clone());
            scope.insert("result".to_string(), result);
            let block = self.new_block();
            self.exit = Some(Exit { block, result, scope, used: false });
        }
        Ok(())
    }

    /// `return r` in a function with `ensures`: goes to the exit block.
    /// Returns false for other functions.
    pub(crate) fn return_through_exit(&mut self, r: Reg) -> bool {
        let Some(exit) = &mut self.exit else { return false };
        exit.used = true;
        let (block, result) = (exit.block, exit.result);
        self.emit(Inst::Copy { dst: result, src: r });
        self.terminate(Terminator::Jump(block));
        true
    }

    /// After the body: the exit block, which checks the `ensures` clauses and
    /// returns.
    pub(crate) fn contracts_end(&mut self, f: &FnDecl, name: &str) -> Result<(), Error> {
        let Some(exit) = self.exit.take() else { return Ok(()) };
        if !exit.used {
            return Ok(());
        }
        let here = self.cur;
        self.switch_to(exit.block);
        // Only `result` and the parameters: the body's locals are gone.
        let saved = std::mem::replace(&mut self.scopes, vec![exit.scope]);
        let mut r = Ok(());
        for (i, c) in f.contracts.iter().enumerate().filter(|(_, c)| c.kind == ContractKind::Ensures) {
            r = self.check_clause(c, breach_name(name, c.kind, i), vec![exit.result]);
            if r.is_err() {
                break;
            }
        }
        self.scopes = saved;
        r?;
        self.terminate(Terminator::Ret(exit.result));
        self.switch_to(here);
        Ok(())
    }

    /// Calls `breach` with `args` unless the clause holds.
    fn check_clause(&mut self, c: &Contract, breach: String, args: Vec<Reg>) -> Result<(), Error> {
        let v = self.expr(&c.cond, Some(&Type::Bool))?;
        self.expect(c.cond.pos, v, &Type::Bool, &format!("a `{}` clause", c.kind.word()))?;
        let (ok, bad) = (self.new_block(), self.new_block());
        self.terminate(Terminator::Branch { cond: v, then: ok, els: bad });
        self.switch_to(bad);
        let dst = self.new_reg(Type::Unit);
        self.emit(Inst::Call { dst, func: breach, args });
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
        Ok(())
    }
}
