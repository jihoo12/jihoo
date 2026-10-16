//! What code can do besides compute its result, for rewrite rules
//! (`rules.rs`).
//!
//! A rule moves code: the functions it is given may be called in another
//! order or another number of times than the pattern calls them, and the
//! parts of the pattern that are not its parameters run after all of the
//! parameters' parts instead of between them. That is only unobservable for
//! code that is *inert*: it has no effect anyone can see, and it always
//! returns. (Or for any code, when the rule is shown to call it as the
//! pattern does: `rules.rs`, `keeps_order`.) Code that prints but cannot trap, or that can trap but does
//! nothing else, can tell the order apart either way: a trap ends the
//! program at a different point of what it printed.
//!
//! The summary is a conservative reading of JIR: whatever the compiler cannot
//! see (a function value, a C function, a syscall, assembly, a function still
//! being compiled) counts as doing anything, and any loop or recursion as
//! possibly never returning.

use std::collections::HashMap;
use std::fmt;

use jihoo_ir::{BinOp, Function, Inst, Reg, Terminator, Type};

/// What code may do, as a set of the kinds below; empty is inert.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Effects(u8);

impl Effects {
    pub const NONE: Effects = Effects(0);
    /// Writes to standard output.
    pub const PRINTS: Effects = Effects(1);
    /// Reads standard input, or the program's arguments.
    pub const READS_INPUT: Effects = Effects(2);
    /// Reads or writes state that other code shares: cells, memory behind
    /// pointers, atomics, channels, tasks, files.
    pub const SHARED_STATE: Effects = Effects(4);
    /// Runs code the compiler cannot see, which may do anything above.
    pub const UNKNOWN: Effects = Effects(8);
    /// May trap, or may never return.
    pub const MAY_FAIL: Effects = Effects(16);
    /// Code the compiler cannot see: it may do anything.
    const ANYTHING: Effects = Effects(8 | 16);

    /// No effect and always returns: free to run in any order, any number
    /// of times.
    pub fn inert(self) -> bool {
        self == Effects::NONE
    }

    pub fn has(self, e: Effects) -> bool {
        self.0 & e.0 != 0
    }
}

impl std::ops::BitOr for Effects {
    type Output = Effects;
    fn bitor(self, o: Effects) -> Effects {
        Effects(self.0 | o.0)
    }
}

impl std::ops::BitOrAssign for Effects {
    fn bitor_assign(&mut self, o: Effects) {
        self.0 |= o.0;
    }
}

impl fmt::Display for Effects {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let names = [
            (Effects::PRINTS, "prints"),
            (Effects::READS_INPUT, "reads input"),
            (Effects::SHARED_STATE, "uses shared state"),
            (Effects::UNKNOWN, "runs unknown code"),
            (Effects::MAY_FAIL, "may trap or not return"),
        ];
        let set: Vec<&str> = names.iter().filter(|(e, _)| self.has(*e)).map(|(_, n)| *n).collect();
        if set.is_empty() {
            write!(f, "inert")
        } else {
            write!(f, "{}", set.join(", "))
        }
    }
}

impl crate::env::Env<'_> {
    /// What calling function `name` (a JIR name) may do, through every call it
    /// makes.
    pub(crate) fn effects_of(&self, name: &str) -> Effects {
        self.effects_in(name).0
    }

    /// The effects of `name`, and whether that answer is final: one that
    /// met a function still being compiled or summarized assumed the worst
    /// of it, and is worked out again next time instead of remembered.
    fn effects_in(&self, name: &str) -> (Effects, bool) {
        if let Some(&e) = self.effects.borrow().get(name) {
            return (e, true);
        }
        if !self.has_ir_function(name) {
            // A C function, or a name the program does not define.
            return (Effects::ANYTHING, true);
        }
        if self.function_in_progress(name) || self.effects_open.borrow().contains(name) {
            return (Effects::ANYTHING, false);
        }
        self.effects_open.borrow_mut().insert(name.to_string());
        let r = match self.function(name) {
            Ok(f) => {
                let mut fin = true;
                (self.body(&f, &mut fin), fin)
            }
            Err(_) => (Effects::ANYTHING, true),
        };
        self.effects_open.borrow_mut().remove(name);
        if r.1 {
            self.effects.borrow_mut().insert(name.to_string(), r.0);
        }
        r
    }

    /// What terminator `term` of block `block` may do. A jump back to the
    /// same block or an earlier one may close a loop: every cycle has one.
    pub(crate) fn term_effects(&self, block: usize, term: &Terminator, fin: &mut bool) -> Effects {
        let mut e = Effects::NONE;
        if term.successors().iter().any(|s| s.0 as usize <= block) {
            e |= Effects::MAY_FAIL;
        }
        if let Terminator::TailCall { func, .. } = term {
            e |= self.call_effects(func, fin);
        }
        e
    }

    fn call_effects(&self, func: &str, fin: &mut bool) -> Effects {
        let (e, f) = self.effects_in(func);
        *fin &= f;
        e
    }

    /// What `insts` may do, given the types of their registers.
    pub(crate) fn insts_effects<'i>(
        &self,
        insts: impl Iterator<Item = &'i Inst>,
        ty: &dyn Fn(Reg) -> Type,
        fin: &mut bool,
    ) -> Effects {
        let insts: Vec<&Inst> = insts.collect();
        // Registers every write of which is the same constant.
        let mut consts: HashMap<Reg, Option<i64>> = HashMap::new();
        for inst in &insts {
            match inst {
                Inst::Const { dst, value } => {
                    let c = consts.entry(*dst).or_insert(Some(*value));
                    if *c != Some(*value) {
                        *c = None;
                    }
                }
                other => {
                    if let Some(d) = other.dst() {
                        consts.insert(d, None);
                    }
                }
            }
        }
        let mut e = Effects::NONE;
        for inst in insts {
            e |= match inst {
                Inst::Const { .. }
                | Inst::FConst { .. }
                | Inst::Unit { .. }
                | Inst::Str { .. }
                | Inst::Copy { .. }
                | Inst::Clone { .. }
                | Inst::Drop { .. }
                | Inst::Unary { .. }
                | Inst::Cast { .. }
                | Inst::Bitcast { .. }
                | Inst::FuncRef { .. }
                | Inst::Closure { .. }
                | Inst::Struct { .. }
                | Inst::Field { .. }
                | Inst::Take { .. }
                | Inst::SetField { .. }
                | Inst::Addr { .. }
                | Inst::FieldPtr { .. }
                | Inst::Variant { .. }
                | Inst::Tag { .. }
                | Inst::Payload { .. }
                | Inst::Ref { .. }
                | Inst::Deref { .. }
                | Inst::NewChan { .. }
                | Inst::NewCell { .. }
                | Inst::Array { .. }
                | Inst::Splat { .. }
                | Inst::Loc { .. }
                | Inst::ToStr { .. }
                | Inst::ParseFloat { .. }
                | Inst::Len { .. } => Effects::NONE,
                // Integer division traps on 0 (and MIN / -1), so not by any
                // other constant.
                Inst::Binary { op: BinOp::Div | BinOp::Rem, lhs, rhs, .. } if !matches!(ty(*lhs), Type::Float(_)) => {
                    match consts.get(rhs) {
                        Some(Some(c)) if *c != 0 && *c != -1 => Effects::NONE,
                        _ => Effects::MAY_FAIL,
                    }
                }
                Inst::Binary { .. } => Effects::NONE,
                // Bounds-checked.
                Inst::GetPath { path, .. } | Inst::SetPath { path, .. } => {
                    if path.iter().any(|s| matches!(s, jihoo_ir::PathStep::Elem(_))) {
                        Effects::MAY_FAIL
                    } else {
                        Effects::NONE
                    }
                }
                Inst::Elem { .. }
                | Inst::SetElem { .. }
                | Inst::ElemPtr { .. }
                | Inst::StrByte { .. }
                | Inst::Slice { .. }
                | Inst::Repeat { .. }
                | Inst::FromBytes { .. }
                | Inst::Trap => Effects::MAY_FAIL,
                // On a cell, what everything holding it sees changes.
                Inst::Push { root, .. } => match ty(*root) {
                    Type::Cell(_) => Effects::SHARED_STATE,
                    _ => Effects::NONE,
                },
                Inst::Pop { root, .. } => match ty(*root) {
                    Type::Cell(_) => Effects::SHARED_STATE | Effects::MAY_FAIL,
                    _ => Effects::MAY_FAIL,
                },
                Inst::CellGet { .. } | Inst::CellSet { .. } => Effects::SHARED_STATE,
                Inst::Load { .. }
                | Inst::Store { .. }
                | Inst::AtomicLoad { .. }
                | Inst::AtomicStore { .. }
                | Inst::AtomicRmw { .. }
                | Inst::AtomicCas { .. }
                | Inst::Fence
                | Inst::Send { .. }
                | Inst::Recv { .. }
                | Inst::Spawn { .. }
                | Inst::Select { .. } => Effects::SHARED_STATE | Effects::MAY_FAIL,
                Inst::Print { .. } => Effects::PRINTS,
                Inst::ReadLine { .. } | Inst::Args { .. } => Effects::READS_INPUT,
                Inst::ReadFile { .. } | Inst::WriteFile { .. } => Effects::SHARED_STATE,
                Inst::Call { func, .. } => self.call_effects(func, fin),
                Inst::CallIndirect { .. }
                | Inst::Syscall { .. }
                | Inst::Asm { .. }
                | Inst::Unique { .. }
                | Inst::Stringify { .. }
                | Inst::CodeText { .. }
                | Inst::TypeOf { .. }
                | Inst::Quote { .. } => Effects::ANYTHING,
            };
        }
        e
    }

    /// What `f`, a function not registered with the environment (a rule's
    /// pattern), may do.
    pub(crate) fn body_effects(&self, f: &Function) -> Effects {
        self.body(f, &mut true)
    }

    fn body(&self, f: &Function, fin: &mut bool) -> Effects {
        let regs = |r: Reg| f.reg_type(r).clone();
        let mut e = self.insts_effects(f.blocks.iter().flat_map(|b| &b.insts), &regs, fin);
        for (i, b) in f.blocks.iter().enumerate() {
            e |= self.term_effects(i, &b.term, fin);
        }
        e
    }
}
