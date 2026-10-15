//! Checks what the ownership pass (`ownership.rs`) decided, independently
//! of how it decided it: translation validation for native memory.
//!
//! The pass records, per function, which parameters it takes over, which
//! registers are views and which part reads move their part out
//! ([`Ownership`]). From that and the meaning of each instruction (what it
//! consumes, borrows, defines or changes in place), this module follows
//! every register that owns memory along every path and requires:
//!
//! - a value is freed (`drop`) or moved exactly once: no double free, no use
//!   after either, and none left at a `ret` or a tail call (no leak);
//! - only values of the function's own are freed, moved or changed in
//!   place, never a borrowed parameter or a view;
//! - a view is used only while the value it shares memory with is alive and
//!   unchanged, except in place at a part disjoint from the view's;
//! - a value is in the same state on every way into a block.
//!
//! A value taken apart (its owning parts moved out one by one) needs no
//! `drop`, but all of its owning parts must have been moved.
//!
//! The pass can then grow cleverer without its mistakes becoming leaks or
//! use after free in programs: they are compile errors instead.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::ownership::{borrowed, changed_part, changes, consumed, defined, overlap, Overlap, Owning};
use crate::{Function, Inst, Module, PathStep, Reg, Terminator, Type};

/// One owning part of a struct or enum value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Field(u32),
    Payload(u32, u32),
}

/// A part read: its result, what it reads from, which part (for fields and
/// payloads) and the path to it (where there is one).
type PartRead = (Reg, Reg, Option<Part>, Option<Vec<PathStep>>);

#[derive(Clone, Debug, PartialEq)]
enum St {
    /// Never written: natively all zeros, so freeing it frees nothing.
    Unwritten,
    /// Moved away or freed.
    Gone,
    /// The function's own, less the parts moved out of it.
    Owned(BTreeSet<Part>),
    /// The caller's (a parameter not handed over), or memory a pointer
    /// reaches: read only.
    Borrowed,
    /// Shares the memory of `root` at the path (unknown through a payload,
    /// a ref or a cell).
    View(Reg, Option<Vec<PathStep>>),
    /// A view whose root changed, or that differs between the ways in.
    Stale,
}

/// Checks every function of a native module that the ownership pass has
/// annotated.
pub fn check_ownership(m: &Module) -> Result<(), String> {
    let mut owning = Owning::new(m);
    let own: HashMap<&str, Vec<u32>> =
        m.funcs.iter().filter_map(|f| f.ownership.as_ref().map(|o| (f.name.as_str(), o.own.clone()))).collect();
    for f in &m.funcs {
        if f.ownership.is_some() {
            Checker::new(f, &mut owning, &own).run().map_err(|e| format!("@{}: {e}", f.name))?;
        }
    }
    Ok(())
}

struct Checker<'a> {
    f: &'a Function,
    owning: &'a mut Owning,
    own: &'a HashMap<&'a str, Vec<u32>>,
    tracked: Vec<bool>,
    views: HashSet<Reg>,
    moves: HashSet<Reg>,
}

impl<'a> Checker<'a> {
    fn new(f: &'a Function, owning: &'a mut Owning, own: &'a HashMap<&'a str, Vec<u32>>) -> Self {
        let tracked = f.regs.iter().map(|t| owning.owns(t)).collect();
        let o = f.ownership.as_ref().unwrap();
        Checker {
            f,
            owning,
            own,
            tracked,
            views: o.views.iter().copied().collect(),
            moves: o.moves.iter().copied().collect(),
        }
    }

    fn tracked(&self, r: Reg) -> bool {
        self.tracked.get(r.0 as usize).copied().unwrap_or(false)
    }

    fn run(&mut self) -> Result<(), String> {
        let f = self.f;
        let given = &f.ownership.as_ref().unwrap().own;
        let mut entry = vec![St::Unwritten; f.regs.len()];
        for (p, s) in entry.iter_mut().enumerate().take(f.params.len()) {
            *s = if given.contains(&(p as u32)) { St::Owned(BTreeSet::new()) } else { St::Borrowed };
        }
        let mut ins: Vec<Option<Vec<St>>> = vec![None; f.blocks.len()];
        ins[0] = Some(entry);
        let mut todo = vec![0usize];
        while let Some(b) = todo.pop() {
            let mut st = ins[b].clone().unwrap();
            for (i, inst) in f.blocks[b].insts.iter().enumerate() {
                self.inst(&mut st, inst).map_err(|e| format!("bb{b}, instruction {i} (`{inst}`): {e}"))?;
            }
            let term = &f.blocks[b].term;
            self.term(&mut st, term).map_err(|e| format!("bb{b}, `{term}`: {e}"))?;
            for s in term.successors() {
                let s = s.0 as usize;
                let merged = match &ins[s] {
                    None => st.clone(),
                    Some(old) => self.join(old, &st).map_err(|e| format!("on the way into bb{s}: {e}"))?,
                };
                if ins[s].as_ref() != Some(&merged) {
                    ins[s] = Some(merged);
                    todo.push(s);
                }
            }
        }
        Ok(())
    }

    fn join(&mut self, a: &[St], b: &[St]) -> Result<Vec<St>, String> {
        let mut out = Vec::with_capacity(a.len());
        for (r, (x, y)) in a.iter().zip(b).enumerate() {
            // A value emptied of what it owns is as good as gone.
            let (x, y) = (&self.gone_if_emptied(Reg(r as u32), x), &self.gone_if_emptied(Reg(r as u32), y));
            let s = if x == y || !self.tracked(Reg(r as u32)) {
                x.clone()
            } else {
                match (x, y) {
                    (St::Gone | St::Unwritten, St::Gone | St::Unwritten) => St::Gone,
                    (St::Owned(_), _) | (_, St::Owned(_)) => {
                        return Err(format!("%{r} is the function's own on one way in and not on another ({x:?}, {y:?})"))
                    }
                    _ => St::Stale,
                }
            };
            out.push(s);
        }
        Ok(out)
    }

    /// The owning parts of a value of type `t` in variant `variant`.
    fn owning_parts(&mut self, t: &Type, variant: Option<u32>) -> Vec<Part> {
        match (t, variant) {
            (Type::Struct(name), _) => {
                let fields = self.owning.fields.get(name).cloned().unwrap_or_default();
                (0..fields.len()).filter(|&k| self.owning.owns(&fields[k])).map(|k| Part::Field(k as u32)).collect()
            }
            (Type::Enum(name), Some(v)) => {
                let payload = self.owning.variants.get(name).and_then(|vs| vs.get(v as usize)).cloned().unwrap_or_default();
                (0..payload.len()).filter(|&k| self.owning.owns(&payload[k])).map(|k| Part::Payload(v, k as u32)).collect()
            }
            _ => vec![],
        }
    }

    fn gone_if_emptied(&mut self, r: Reg, s: &St) -> St {
        match s {
            St::Owned(moved) if self.emptied(r, moved) => St::Gone,
            other => other.clone(),
        }
    }

    /// Whether `r`, owned with `moved` taken out, has nothing left to free.
    fn emptied(&mut self, r: Reg, moved: &BTreeSet<Part>) -> bool {
        if moved.is_empty() {
            return false;
        }
        let variant = moved.iter().find_map(|p| match p {
            Part::Payload(v, _) => Some(*v),
            Part::Field(_) => None,
        });
        let t = self.f.reg_type(r).clone();
        self.owning_parts(&t, variant).iter().all(|p| moved.contains(p))
    }

    /// Fails if `r` still owns something that nothing will free.
    fn not_leaked(&mut self, st: &[St], r: Reg) -> Result<(), String> {
        match &st[r.0 as usize] {
            St::Owned(moved) if !self.emptied(r, moved) => Err(format!("%{} is still the function's own here: a leak", r.0)),
            _ => Ok(()),
        }
    }

    /// Reads `r`, or its `part`.
    fn read(&self, st: &[St], r: Reg, part: Option<Part>) -> Result<(), String> {
        match &st[r.0 as usize] {
            St::Owned(moved) => match part {
                Some(p) if moved.contains(&p) => Err(format!("%{} is read after its part {p:?} moved out", r.0)),
                None if !moved.is_empty() => Err(format!("%{} is read after parts of it moved out", r.0)),
                _ => Ok(()),
            },
            St::Borrowed | St::View(..) => Ok(()),
            St::Unwritten => Err(format!("%{} is read before it is written", r.0)),
            St::Gone => Err(format!("%{} is used after it was moved or freed", r.0)),
            St::Stale => Err(format!("view %{} is used after the value it shares memory with changed", r.0)),
        }
    }

    /// Moves `r` away, which must be the function's own and whole.
    fn take(&self, st: &mut [St], r: Reg) -> Result<(), String> {
        match &st[r.0 as usize] {
            St::Owned(moved) if moved.is_empty() => {
                st[r.0 as usize] = St::Gone;
                Self::invalidate(st, r, None);
                Ok(())
            }
            St::Owned(_) => Err(format!("%{} is moved after parts of it moved out", r.0)),
            St::Borrowed => Err(format!("%{} is borrowed, but this moves it", r.0)),
            St::View(..) => Err(format!("%{} is a view, but this moves it", r.0)),
            other => {
                self.read(st, r, None)?;
                Err(format!("%{} cannot be moved ({other:?})", r.0))
            }
        }
    }

    /// Ends the views of what the cell `cell` holds; copies of the handle
    /// itself stay.
    fn invalidate_contents(st: &mut [St], cell: Reg) {
        for s in st.iter_mut() {
            if let St::View(r, p) = s {
                if *r == cell && p.as_deref() != Some(&[]) {
                    *s = St::Stale;
                }
            }
        }
    }

    /// Ends the views of `root` that a change at `at` (anywhere if `None`)
    /// can reach.
    fn invalidate(st: &mut [St], root: Reg, at: Option<&[PathStep]>) {
        for s in st.iter_mut() {
            if let St::View(r, p) = s {
                if *r == root {
                    let apart = matches!((p.as_deref(), at), (Some(p), Some(w)) if overlap(p, w) == Overlap::Disjoint);
                    if !apart {
                        *s = St::Stale;
                    }
                }
            }
        }
    }

    /// Writes `d` anew with `to`.
    fn define(&mut self, st: &mut [St], d: Reg, to: St) -> Result<(), String> {
        if !self.tracked(d) {
            return Ok(());
        }
        self.not_leaked(st, d)?;
        Self::invalidate(st, d, None);
        st[d.0 as usize] = to;
        Ok(())
    }

    /// A view of `src` at `step` below it.
    fn view_of(&self, st: &[St], src: Reg, step: Option<Vec<PathStep>>) -> Result<St, String> {
        Ok(match &st[src.0 as usize] {
            St::Owned(_) | St::Borrowed => St::View(src, step),
            St::View(root, p) => St::View(
                *root,
                match (p, step) {
                    (Some(p), Some(s)) => Some(p.iter().chain(&s).copied().collect()),
                    _ => None,
                },
            ),
            _ => {
                self.read(st, src, None)?;
                return Err(format!("a view of %{}, which is not readable", src.0));
            }
        })
    }

    fn inst(&mut self, st: &mut [St], inst: &Inst) -> Result<(), String> {
        let owned = || St::Owned(BTreeSet::new());
        match inst {
            Inst::Copy { dst, src } if dst == src => return Ok(()),
            Inst::Copy { dst, src } if self.tracked(*dst) => {
                if self.views.contains(dst) {
                    self.read(st, *src, None)?;
                    let v = self.view_of(st, *src, Some(vec![]))?;
                    return self.define(st, *dst, v);
                }
                self.take(st, *src)?;
                return self.define(st, *dst, owned());
            }
            Inst::Clone { dst, src } => {
                self.read(st, *src, None)?;
                if dst == src {
                    return match &st[src.0 as usize] {
                        St::Borrowed | St::View(..) => {
                            st[dst.0 as usize] = owned();
                            Ok(())
                        }
                        _ => Err(format!("`clone %{0}, %{0}` of a value that is already the function's own", src.0)),
                    };
                }
                return self.define(st, *dst, owned());
            }
            Inst::Drop { src } => {
                return match &st[src.0 as usize] {
                    St::Unwritten => Ok(()),
                    St::Owned(moved) if moved.is_empty() => {
                        st[src.0 as usize] = St::Gone;
                        Self::invalidate(st, *src, None);
                        Ok(())
                    }
                    St::Owned(_) => Err(format!("%{} is freed after parts of it moved out: they are freed twice", src.0)),
                    St::Gone => Err(format!("%{} is freed after it was moved or freed", src.0)),
                    St::Borrowed => Err(format!("%{} is borrowed, but this frees it", src.0)),
                    St::View(..) | St::Stale => Err(format!("%{} is a view, but this frees it", src.0)),
                };
            }
            Inst::Load { dst, .. } => return self.define(st, *dst, St::Borrowed),
            _ => {}
        }

        // A read of a part: a view, a part moved out, or a value that does
        // not own memory.
        let part_read: Option<PartRead> = match inst {
            Inst::Field { dst, src, index } => {
                Some((*dst, *src, Some(Part::Field(*index)), Some(vec![PathStep::Field(*index)])))
            }
            Inst::Payload { dst, src, variant, index } => Some((*dst, *src, Some(Part::Payload(*variant, *index)), None)),
            Inst::Elem { dst, src, index } => Some((*dst, *src, None, Some(vec![PathStep::Elem(*index)]))),
            Inst::GetPath { dst, src, path } => Some((*dst, *src, None, Some(path.clone()))),
            Inst::Deref { dst, src } => Some((*dst, *src, None, None)),
            Inst::CellGet { dst, cell, .. } => Some((*dst, *cell, None, None)),
            _ => None,
        };
        if let Some((dst, src, part, path)) = part_read {
            for r in borrowed(inst) {
                if self.tracked(r) {
                    self.read(st, r, if r == src { part } else { None })?;
                }
            }
            if !self.tracked(dst) {
                return Ok(());
            }
            if self.moves.contains(&dst) {
                let Some(p) = part else {
                    return Err(format!("%{} moves a part out of %{}, which only fields and payloads can", dst.0, src.0));
                };
                match &mut st[src.0 as usize] {
                    St::Owned(moved) => {
                        moved.insert(p);
                    }
                    other => return Err(format!("a part moves out of %{}, which is not the function's own ({other:?})", src.0)),
                }
                Self::invalidate(st, src, None);
                // Emptied of what it owns, it needs no `drop`, and its other
                // parts can still be read.
                return self.define(st, dst, owned());
            }
            if self.views.contains(&dst) {
                let v = self.view_of(st, src, path)?;
                return self.define(st, dst, v);
            }
            return Err(format!("%{} reads a part of %{} that owns memory, but is neither a view nor a move", dst.0, src.0));
        }

        // Everything else: what it consumes, borrows, changes and defines.
        let mut moved_regs: Vec<Reg> = consumed(&mut inst.clone()).into_iter().map(|r| *r).collect();
        let mut read_regs = borrowed(inst);
        if let Inst::Call { func, args, .. } = inst {
            if let Some(own) = self.own.get(func.as_str()) {
                for &i in own {
                    let a = args[i as usize];
                    if self.tracked(a) {
                        moved_regs.push(a);
                        if let Some(k) = read_regs.iter().position(|r| *r == a) {
                            read_regs.remove(k);
                        }
                    }
                }
            }
        }
        // A cell is a handle: changing what it holds changes no register.
        let is_cell = |r: Reg| matches!(self.f.reg_type(r), Type::Cell(_));
        let in_place: Vec<Reg> =
            defined(inst).into_iter().filter(|&d| changes(inst, d) && self.tracked(d)).collect();
        for &r in &read_regs {
            if self.tracked(r) {
                self.read(st, r, None)?;
            }
        }
        for &r in &moved_regs {
            if self.tracked(r) {
                self.take(st, r)?;
            }
        }
        for &r in &in_place {
            if is_cell(r) {
                Self::invalidate_contents(st, r);
                continue;
            }
            match &st[r.0 as usize] {
                St::Owned(_) => {}
                St::Borrowed => return Err(format!("%{} is borrowed, but this changes it in place", r.0)),
                St::View(..) | St::Stale => return Err(format!("%{} is a view, but this changes it in place", r.0)),
                _ => self.read(st, r, None)?,
            }
            let at = changed_part(inst, r);
            Self::invalidate(st, r, at.as_deref());
        }
        if let Inst::CellSet { cell, .. } = inst {
            Self::invalidate_contents(st, *cell);
        }
        for d in defined(inst) {
            if in_place.contains(&d) {
                continue;
            }
            self.define(st, d, St::Owned(BTreeSet::new()))?;
        }
        Ok(())
    }

    fn term(&mut self, st: &mut [St], t: &Terminator) -> Result<(), String> {
        let exits = match t {
            Terminator::Ret(r) => {
                if self.tracked(*r) {
                    self.take(st, *r)?;
                }
                true
            }
            Terminator::TailCall { func, args } => {
                let own = self.own.get(func.as_str()).cloned().unwrap_or_default();
                for (i, &a) in args.iter().enumerate() {
                    if !self.tracked(a) {
                        continue;
                    }
                    if own.contains(&(i as u32)) {
                        self.take(st, a)?;
                    } else {
                        self.read(st, a, None)?;
                    }
                }
                true
            }
            Terminator::Branch { .. } | Terminator::Jump(_) | Terminator::Unreachable => false,
        };
        if exits {
            for r in 0..st.len() {
                if self.tracked(Reg(r as u32)) {
                    self.not_leaked(st, Reg(r as u32))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::check_ownership;

    /// Checks `body`, the inside of `fn @f([i64]) -> i64`, with what the pass
    /// decided in `decided`.
    fn check(decided: &str, regs: &str, body: &str) -> Result<(), String> {
        let text = format!(
            "jir {}\nprofile native\n\nfn @f([i64]) -> i64 {{\n  regs [i64] {regs}\n{decided}\n{body}\n}}\n",
            crate::JIR_VERSION
        );
        check_ownership(&crate::parse(&text).unwrap())
    }

    #[test]
    fn a_list_made_and_freed_once_is_fine() {
        check("  own", "[i64] i64", "bb0:\n  %1 = array()\n  %2 = len %1\n  drop %1\n  ret %2").unwrap();
        // A borrowed parameter is read, and freed by the caller.
        check("  own", "i64", "bb0:\n  %1 = len %0\n  ret %1").unwrap();
        // One handed over is the function's to free.
        check("  own 0", "i64", "bb0:\n  %1 = len %0\n  drop %0\n  ret %1").unwrap();
    }

    #[test]
    fn mistakes_are_errors() {
        let err = |d: &str, regs: &str, body: &str| check(d, regs, body).unwrap_err();
        assert!(err("  own", "[i64] i64", "bb0:\n  %1 = array()\n  drop %1\n  drop %1\n  %2 = const 0\n  ret %2")
            .contains("freed after it was moved or freed"));
        assert!(err("  own", "[i64] [i64] i64", "bb0:\n  %1 = array()\n  %2 = copy %1\n  %3 = len %1\n  drop %2\n  ret %3")
            .contains("used after it was moved"));
        assert!(err("  own", "[i64] i64", "bb0:\n  %1 = array()\n  %2 = len %1\n  ret %2").contains("a leak"));
        assert!(err("  own", "i64", "bb0:\n  drop %0\n  %1 = const 0\n  ret %1").contains("borrowed, but this frees it"));
        assert!(err("  own", "i64 i64", "bb0:\n  %1 = const 1\n  push %0, (), %1\n  %2 = const 0\n  ret %2")
            .contains("borrowed, but this changes it in place"));
        // A view used after the list it shares memory with changed.
        assert!(err(
            "  own 0\n  views %1",
            "[i64] i64 i64",
            "bb0:\n  %1 = copy %0\n  %2 = const 1\n  push %0, (), %2\n  %3 = len %1\n  drop %0\n  ret %3"
        )
        .contains("view %1 is used after"));
        // Freed on one way into a block and not on the other: a leak on one
        // way, whichever the checker meets first.
        let e = err(
            "  own",
            "[i64] bool i64",
            "bb0:\n  %1 = array()\n  %2 = const 1\n  br %2, bb1, bb2\nbb1:\n  drop %1\n  jmp bb2\nbb2:\n  %3 = const 0\n  ret %3",
        );
        assert!(e.contains("on one way in and not on another") || e.contains("a leak"), "{e}");
        // Owned on both ways in, but freed on one: the join is the error.
        let e = err(
            "  own",
            "[i64] bool i64",
            "bb0:\n  %1 = array()\n  %2 = const 1\n  br %2, bb1, bb2\nbb1:\n  drop %1\n  jmp bb3\nbb2:\n  jmp bb3\nbb3:\n  %3 = len %1\n  drop %1\n  ret %3",
        );
        assert!(e.contains("on one way in and not on another") || e.contains("used after"), "{e}");
    }
}
