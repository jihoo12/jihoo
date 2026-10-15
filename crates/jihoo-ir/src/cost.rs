//! Where a native program copies memory it owns: the `clone`s the ownership
//! pass inserted (`crate::ownership`), each with the source position of the
//! code around it (`loc`, which sema writes when asked) and why the value is
//! copied. `jihoo build --cost` prints them.
//!
//! A `clone` of a `ref`, `cell` or `chan` only counts one more holder, so it is
//! not listed; every other one copies lists, strings or a closure's captured
//! values.

use std::collections::{HashMap, HashSet};

use crate::{Function, Inst, Module, Reg, Terminator, Type};

/// One copy the program makes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Copy {
    /// `(file, line, col)` of the code that makes it, if known.
    pub loc: Option<(u32, u32, u32)>,
    /// The JIR function it is in.
    pub func: String,
    /// The type of the value copied, as JIR writes it.
    pub ty: String,
    /// Why, in words.
    pub why: String,
    /// Whether it copies a parameter on entry, because the caller keeps it.
    pub on_entry: bool,
}

/// The copies of a module after the ownership pass, in source order, each
/// once. Only functions the program can run count: a function whose callers
/// all call its consuming version (`f.own`) never makes the copies the
/// version they avoid makes.
pub fn copies(m: &Module) -> Vec<Copy> {
    let used = reachable(m);
    let mut out: Vec<Copy> = m.funcs.iter().filter(|f| used.contains(f.name.as_str())).flat_map(in_function).collect();
    out.sort();
    out.dedup_by(|a, b| a.loc == b.loc && a.ty == b.ty && a.why == b.why);
    out
}

/// The functions the program can run: the entry points and exported ones,
/// and what they call or name, in turn.
fn reachable(m: &Module) -> HashSet<&str> {
    let by_name: HashMap<&str, &Function> = m.funcs.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut todo: Vec<&str> =
        m.funcs.iter().filter(|f| f.name == "main" || f.name == "_start" || f.export.is_some()).map(|f| f.name.as_str()).collect();
    let mut seen: HashSet<&str> = todo.iter().copied().collect();
    while let Some(name) = todo.pop() {
        let Some(f) = by_name.get(name) else { continue };
        for b in &f.blocks {
            let named = b.insts.iter().filter_map(|inst| match inst {
                Inst::Call { func, .. } | Inst::FuncRef { func, .. } | Inst::Closure { func, .. } => Some(func.as_str()),
                _ => None,
            });
            let tail = match &b.term {
                Terminator::TailCall { func, .. } => Some(func.as_str()),
                _ => None,
            };
            for g in named.chain(tail) {
                if seen.insert(g) {
                    todo.push(g);
                }
            }
        }
    }
    seen
}

/// Whether a `clone` of a value of type `t` copies memory, rather than counting
/// one more holder of a shared box.
fn copies_memory(t: &Type) -> bool {
    !matches!(t, Type::Ref(_) | Type::Cell(_) | Type::Chan(_))
}

/// The copies function `f` makes itself, in its order.
pub fn in_function(f: &Function) -> Vec<Copy> {
    let mut out = Vec::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        for (i, inst) in b.insts.iter().enumerate() {
            let Inst::Clone { dst, src } = *inst else { continue };
            let ty = f.reg_type(dst);
            if !copies_memory(ty) {
                continue;
            }
            let here = loc_at(f, bi, i);
            let why = reason(f, &b.insts, i, dst, src, &b.term);
            let on_entry = dst == src && src.0 < f.params.len() as u32;
            out.push(Copy { loc: here, func: f.name.clone(), ty: ty.jir(), why, on_entry });
        }
    }
    out
}

/// The source position of instruction `i` of block `block`: the one written
/// before it in its block, or else the next one, or else the function's.
pub fn loc_at(f: &Function, block: usize, i: usize) -> Option<(u32, u32, u32)> {
    let insts = &f.blocks[block].insts;
    let i = i.min(insts.len());
    insts[..i]
        .iter()
        .rev()
        .find_map(loc)
        .or_else(|| insts[i..].iter().find_map(loc))
        .or_else(|| f.blocks.iter().flat_map(|b| &b.insts).find_map(loc))
}

fn loc(inst: &Inst) -> Option<(u32, u32, u32)> {
    match *inst {
        Inst::Loc { file, line, col } => Some((file, line, col)),
        _ => None,
    }
}

/// Why `clone` number `i` of `insts`, `dst = clone src`, is there.
fn reason(f: &Function, insts: &[Inst], i: usize, dst: Reg, src: Reg, term: &Terminator) -> String {
    let params = f.params.len() as u32;
    if dst == src {
        if src.0 < params {
            return format!(
                "parameter {} is copied on entry, since the function changes or keeps it; \
                 a caller that hands it over at its last use calls a version that does not copy it",
                src.0 + 1
            );
        }
        return "a value is copied in place".into();
    }
    // A part read just before: the read gave a view, which could not stay one.
    if let Some(prev) = i.checked_sub(1).map(|j| &insts[j]) {
        let read = match prev {
            Inst::CellGet { dst: d, .. } if *d == src => Some("a value read out of a cell is copied, since the cell can change"),
            Inst::Field { dst: d, .. }
            | Inst::Elem { dst: d, .. }
            | Inst::GetPath { dst: d, .. }
            | Inst::Payload { dst: d, .. }
            | Inst::Deref { dst: d, .. }
                if *d == src =>
            {
                Some(
                    "a part read out of a value is copied, since the part or the value changes \
                     while the other is still used",
                )
            }
            _ => None,
        };
        if let Some(why) = read {
            return why.into();
        }
    }
    // A part read out of a value, kept as a view of it until here.
    let part = f.blocks.iter().flat_map(|b| &b.insts).any(|inst| {
        matches!(inst, Inst::Field { dst: d, .. } | Inst::Elem { dst: d, .. } | Inst::GetPath { dst: d, .. }
            | Inst::Payload { dst: d, .. } | Inst::Deref { dst: d, .. } if *d == src)
    });
    let what = match () {
        _ if src.0 < params => format!("parameter {}", src.0 + 1),
        _ if part => "a part read out of a value".to_string(),
        _ => "a value".to_string(),
    };
    // What takes the copy: the next instruction that reads it, or the terminator.
    let user = insts[i + 1..].iter().find(|inst| {
        let mut inst = (*inst).clone();
        inst.regs_mut().iter().any(|r| **r == dst)
    });
    let to = match user {
        Some(Inst::Copy { .. }) => "assigned",
        Some(Inst::Struct { .. } | Inst::Array { .. } | Inst::Variant { .. }) => "put into a new value",
        Some(Inst::SetField { .. } | Inst::SetElem { .. } | Inst::SetPath { .. }) => "stored into a part of a value",
        Some(Inst::Push { .. }) => "pushed onto a list",
        Some(Inst::Store { .. }) => "stored through a pointer",
        Some(Inst::Send { .. }) => "sent on a channel",
        Some(Inst::NewCell { .. } | Inst::CellSet { .. }) => "put into a cell",
        Some(Inst::Ref { .. }) => "put into a `ref`",
        Some(Inst::Closure { .. }) => "captured by a closure",
        Some(Inst::Spawn { .. }) => "handed to a thread",
        Some(_) => "used",
        None => match term {
            Terminator::Ret(_) => "returned",
            Terminator::TailCall { .. } => "passed on in a tail call",
            _ => "used",
        },
    };
    // A parameter the function owns, as its consuming version does all it
    // copies on entry, or after copying it on entry, is copied because the
    // function uses it again, not for the caller.
    let owned = src.0 < params
        && (f.name.ends_with(".own")
            || f.blocks.iter().flat_map(|b| &b.insts).any(|inst| matches!(*inst, Inst::Clone { dst: d, src: s } if d == src && s == src)));
    let after = match () {
        _ if src.0 < params && !owned => "the caller still owns it",
        _ if part => "the value it was read from still holds it",
        _ => "it is still used afterwards",
    };
    format!("{what} is {to} and copied, since {after}")
}
