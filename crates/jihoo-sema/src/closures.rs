//! Anonymous functions and closures.
//!
//! `fn(x: i64) -> i64 { return x + n }` is lifted into a function of its own,
//! named `fn.N` (`fn` is a keyword, so no user name can clash with it). Every
//! local visible where it is written becomes a leading parameter of that
//! function; after lowering the body, the ones it never reads are dropped and
//! the registers renumbered. What remains are the captures:
//!
//! - none: the value is a plain `funcref`, in both profiles;
//! - some: `closure @fn.N(%captured...)` builds a function value that passes
//!   the captured values before its own arguments. Hosted only, since the
//!   captured values live on the GC heap.
//!
//! Values are captured when the closure is made (by value, like every other
//! copy in jihoo), and captured variables cannot be assigned inside it.

use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use jihoo_ir as ir;
use jihoo_ir::{Inst, Profile, Reg, Type};
use jihoo_syntax::ast::Lambda;
use jihoo_syntax::{Error, Pos};

use crate::env::Sig;
use crate::FnCx;

/// Something a lambda may capture.
enum Capture {
    /// A local variable.
    Local(String),
    /// A captured value of a closure this instance received as a comptime
    /// argument (`f` in `fn each(comptime f: fn(i64), ...)`).
    Closure(String),
}

impl FnCx<'_> {
    pub(crate) fn lambda(&mut self, pos: Pos, l: &Lambda, expected: Option<&Type>) -> Result<Reg, Error> {
        let lifted = self.lift(l, expected)?;
        if lifted.captured.is_empty() {
            return Ok(self.emit_to(lifted.ty, |dst| Inst::FuncRef { dst, func: lifted.func }));
        }
        let what = format!("this function captures {}", lifted.names.join(", "));
        self.closure_value(pos, &what, lifted.func, lifted.captured, lifted.ty)
    }

    /// A closure value: hosted only, since the captured values go to the GC
    /// heap. `what` describes it in errors.
    pub(crate) fn closure_value(
        &mut self,
        pos: Pos,
        what: &str,
        func: String,
        captures: Vec<Reg>,
        ty: Type,
    ) -> Result<Reg, Error> {
        if self.profile() != Profile::Hosted && !self.in_macro {
            let msg = format!(
                "{what}, so it is a closure; storing or passing a closure as a value needs the GC, which only \
                 hosted mode has (in freestanding code, pass it to a `comptime` parameter, or pass the values as arguments)"
            );
            return Err(Error::new(pos, msg));
        }
        Ok(self.emit_to(ty, |dst| Inst::Closure { dst, func, captures }))
    }

    /// Lifts the lambda into a function of its own.
    pub(crate) fn lift(&mut self, l: &Lambda, expected: Option<&Type>) -> Result<Lifted, Error> {
        // Types left out come from the expected function type.
        let want = match expected {
            Some(Type::Fn(ps, r)) if ps.len() == l.params.len() => Some((ps, r)),
            _ => None,
        };
        let mut params = Vec::with_capacity(l.params.len());
        for (i, (p, name, ty)) in l.params.iter().enumerate() {
            params.push(match (ty, want) {
                (Some(t), _) => self.resolve(t)?,
                (None, Some((ps, _))) => ps[i].clone(),
                (None, None) => {
                    let msg = format!("cannot infer the type of parameter `{name}`; write `{name}: T`");
                    return Err(Error::new(*p, msg));
                }
            });
        }
        let ret = match (&l.ret, want) {
            (Some(t), _) => self.resolve(t)?,
            (None, Some((_, r))) => (**r).clone(),
            (None, None) => Type::Unit,
        };

        // Every visible local (the innermost one of each name), ordered by name,
        // then the captured values of closures this instance received.
        let mut locals: BTreeMap<String, Reg> = BTreeMap::new();
        for scope in &self.scopes {
            locals.extend(scope.iter().map(|(n, r)| (n.clone(), *r)));
        }
        let mut outer: Vec<(Capture, Reg)> = locals.into_iter().map(|(n, r)| (Capture::Local(n), r)).collect();
        let mut closures: Vec<(&String, &Vec<Reg>)> = self.closure_regs.iter().collect();
        closures.sort_by(|a, b| a.0.cmp(b.0));
        for (n, regs) in closures {
            outer.extend(regs.iter().map(|r| (Capture::Closure(n.clone()), *r)));
        }

        let id = self.env.lambda_ids.get();
        self.env.lambda_ids.set(id + 1);
        let name = format!("fn.{id}");
        let all_params = outer.iter().map(|(_, r)| self.ty(*r).clone()).chain(params.iter().cloned()).collect();
        let mut cx = FnCx::new(self.env, Rc::new(Sig { params: all_params, ret: ret.clone() }), self.bindings.clone());
        cx.in_comptime = self.in_comptime;
        cx.in_macro = self.in_macro;
        cx.macro_depth = self.macro_depth;
        for (capture, r) in &outer {
            let c = cx.new_reg(self.ty(*r).clone());
            match capture {
                Capture::Local(n) => {
                    cx.scopes[0].insert(n.clone(), c);
                    cx.captured.insert(c);
                }
                Capture::Closure(n) => cx.closure_regs.entry(n.clone()).or_default().push(c),
            }
        }
        let mut seen = HashSet::new();
        for ((p, n, _), t) in l.params.iter().zip(&params) {
            if !seen.insert(n) {
                return Err(Error::new(*p, format!("duplicate parameter `{n}`")));
            }
            let r = cx.new_reg(t.clone());
            cx.scopes[0].insert(n.clone(), r);
        }
        cx.block(&l.body)?;
        let mut f = cx.finish(&name, ret.clone(), l.body.end)?;

        let used = drop_unused_params(&mut f, outer.len());
        let kept: Vec<&(Capture, Reg)> = outer.iter().zip(&used).filter(|(_, u)| **u).map(|(c, _)| c).collect();
        let mut names: Vec<String> = Vec::new();
        for (c, _) in &kept {
            let (Capture::Local(n) | Capture::Closure(n)) = c;
            let n = format!("`{n}`");
            if !names.contains(&n) {
                names.push(n);
            }
        }
        let captured = kept.iter().map(|(_, r)| *r).collect();
        // Functions made inside macros only exist while compiling.
        self.env.add_lambda(f, !self.in_macro);
        Ok(Lifted { func: name, captured, names, ty: Type::Fn(params, Box::new(ret)) })
    }
}

/// A lambda lifted into a function.
pub(crate) struct Lifted {
    pub func: String,
    /// The registers it captures, passed before its own arguments.
    pub captured: Vec<Reg>,
    /// What it captures, for messages: "`k`", "`f`".
    pub names: Vec<String>,
    pub ty: Type,
}

/// Drops those of the first `k` parameters of `f` that its code never uses, and
/// renumbers the registers. Returns which of the `k` are kept.
fn drop_unused_params(f: &mut ir::Function, k: usize) -> Vec<bool> {
    let mut used = vec![false; k];
    for b in &mut f.blocks {
        let regs = b.insts.iter_mut().flat_map(|i| i.regs_mut()).chain(b.term.regs_mut());
        for r in regs {
            if (r.0 as usize) < k {
                used[r.0 as usize] = true;
            }
        }
    }
    let keep = |i: usize| i >= k || used[i];
    let mut new_index = Vec::with_capacity(f.regs.len());
    let mut next = 0u32;
    for i in 0..f.regs.len() {
        new_index.push(next);
        if keep(i) {
            next += 1;
        }
    }
    for b in &mut f.blocks {
        let regs = b.insts.iter_mut().flat_map(|i| i.regs_mut()).chain(b.term.regs_mut());
        for r in regs {
            r.0 = new_index[r.0 as usize];
        }
    }
    let mut i = 0;
    f.regs.retain(|_| {
        i += 1;
        keep(i - 1)
    });
    let mut i = 0;
    f.params.retain(|_| {
        i += 1;
        keep(i - 1)
    });
    used
}
