//! The ownership pass: who frees a native list, and when.
//!
//! jihoo values are values: assigning or passing one copies it, and no two
//! variables ever share a list. On the VM the GC makes that cheap (copies share
//! until one is changed). Native code has no GC, so this pass decides, from
//! liveness alone, what each copy of a value that owns memory (a list, or an
//! aggregate holding one; see [`owns`]) does:
//!
//! - A *consuming* use (`copy`, building a struct or array from it, storing it
//!   into a part of another value, `push`, `ret`, `spawn`) **moves** the value
//!   when the register is not used again, and otherwise gets a `clone` (a deep
//!   copy) inserted before it.
//! - A *borrowing* use (reading a part, `len`, a call argument) leaves the value
//!   where it is.
//! - Reading an owning part out of a value (`field`, `elem`, `getpath`,
//!   `payload`) gives a *view*: the backend copies the header only, so the view
//!   shares the elements with the value it was read from, its *root*. A view is
//!   kept as one when nothing can tell: the view itself is never changed, and
//!   its root is not changed while the view is still used (liveness counts a
//!   use of a view as a use of its root, so the root is not freed or moved
//!   away before the view's last use either). Otherwise a `clone` right after
//!   the read makes it a value of its own. So `len(g.nodes)` copies nothing.
//!   A `copy` of a value that is still used afterwards, or of a parameter,
//!   is a view on the same terms, so the copy of a list that a `for` loop
//!   walks costs nothing unless the body changes the list.
//! - Where a register's value dies without being moved (after its last use, on
//!   a branch that does not use it again, or right after it is made if it is
//!   never used) a `drop` frees it.
//!
//! Parameters are borrowed: the caller keeps its argument and frees it. A
//! function that changes, consumes or reassigns a parameter it also reads
//! first clones it on entry, as the VM copies a list the first time a
//! function changes it. A value assigned to a parameter that never meets the
//! caller's (as in `let n = len(xs); xs = [n]`) is first moved to a register
//! of its own, so replacing a parameter alone copies nothing
//! (`split_params`). A thread started with `spawn` owns its arguments.
//!
//! Such a function `f` also gets a *consuming* version, `f.own`, that takes
//! those parameters over instead of cloning them. A call whose arguments for
//! them are values of the caller's own at their last use calls `f.own`, and
//! the values move: so `xs = f(xs)` changes `xs` in place, copying nothing.
//!
//! No borrow checker is needed because nothing can refer to a value's memory:
//! references into values are paths (indices), and pointers to owning values
//! are refused by the frontend. Liveness is computed on registers, which
//! already have value semantics, so a move is never visible: it only happens
//! where nothing could tell the difference.
//!
//! `ref` and `cell` are the values whose copies do share: natively a counted
//! box, where `clone` counts one more holder and `drop` one fewer. What a ref
//! holds never changes, so `deref` is a view like a part read. What a cell
//! holds can change through any holder, so `cellget` is always copied.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::{Block, BlockId, Function, Inst, Module, PathStep, Reg, Terminator, Type};

/// Inserts `clone` and `drop` into every function of a native module.
pub fn insert(m: &mut Module) {
    let mut owning = Owning::new(m);
    for f in &mut m.funcs {
        split_params(f, &mut owning);
    }
    // The parameters each function clones on entry, by position.
    let mut cloning: HashMap<String, Vec<usize>> = HashMap::new();
    for f in &m.funcs {
        let c = cloned_params(f, &mut owning);
        if !c.is_empty() {
            cloning.insert(f.name.clone(), c);
        }
    }
    let originals: HashMap<String, Function> =
        m.funcs.iter().filter(|f| cloning.contains_key(&f.name)).map(|f| (f.name.clone(), f.clone())).collect();
    let mut wanted = BTreeSet::new();
    let mut cx = Cx { owning: &mut owning, cloning: &cloning, wanted: &mut wanted };
    for f in &mut m.funcs {
        function(f, &mut cx, &[]);
    }
    // The consuming versions that calls asked for, and those they ask for.
    let mut made = HashSet::new();
    while let Some(name) = wanted.pop_first() {
        if !made.insert(name.clone()) {
            continue;
        }
        let mut f = originals[&name].clone();
        f.name = consuming(&name);
        f.export = None;
        let mut cx = Cx { owning: &mut owning, cloning: &cloning, wanted: &mut wanted };
        function(&mut f, &mut cx, &cloning[&name]);
        m.funcs.push(f);
    }
}

/// Gives a parameter's later values registers of their own where they never
/// meet its first one. `xs = [n]` after reading `xs` then writes a new
/// register: the parameter stays borrowed, the new value is the function's
/// own, and neither is copied. Without this, the register would hold the
/// caller's value and then one of the function's own, and the function would
/// have to copy the caller's on entry so that it could free whatever the
/// register holds.
///
/// The definitions of the parameter (on entry, and each instruction that
/// writes it) that reach a common use must share a register, and so must an
/// instruction's definition and those of the value it reads, when it reads and
/// writes the register (`push`, `xs = xs + ys`); each group of them that does
/// not include the one on entry gets a new register.
fn split_params(f: &mut Function, owning: &mut Owning) {
    for i in 0..f.params.len() {
        let p = Reg(i as u32);
        if owning.owns(f.reg_type(p)) {
            split(f, p);
        }
    }
}

fn split(f: &mut Function, p: Reg) {
    // Definition 0 is the one on entry; the others are instructions, by place.
    let mut defs: Vec<(usize, usize)> = vec![(usize::MAX, usize::MAX)];
    let mut def_at: HashMap<(usize, usize), usize> = HashMap::new();
    for (b, block) in f.blocks.iter().enumerate() {
        for (k, inst) in block.insts.iter().enumerate() {
            if defined(inst).contains(&p) {
                def_at.insert((b, k), defs.len());
                defs.push((b, k));
            }
        }
    }
    if defs.len() == 1 {
        return;
    }
    let reads = |inst: &Inst| {
        let mut copy = inst.clone();
        let written = usize::from(defined(inst).contains(&p));
        uses(inst).contains(&p) || changes(inst, p) || copy.regs_mut().into_iter().filter(|r| **r == p).count() > written
    };

    // Which definitions reach the start of each block.
    let n = f.blocks.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, block) in f.blocks.iter().enumerate() {
        for s in block.term.successors() {
            preds[s.0 as usize].push(b);
        }
    }
    let out_of = |b: usize, reach: &BTreeSet<usize>| -> BTreeSet<usize> {
        let last = f.blocks[b].insts.iter().enumerate().rev().find_map(|(k, _)| def_at.get(&(b, k)));
        match last {
            Some(&d) => BTreeSet::from([d]),
            None => reach.clone(),
        }
    };
    let mut reach_in: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    reach_in[0].insert(0);
    loop {
        let mut changed = false;
        for b in 0..n {
            let mut inn: BTreeSet<usize> = if b == 0 { BTreeSet::from([0]) } else { BTreeSet::new() };
            for &q in &preds[b] {
                inn.extend(out_of(q, &reach_in[q]));
            }
            if inn != reach_in[b] {
                reach_in[b] = inn;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Join the definitions that must share a register.
    let mut parent: Vec<usize> = (0..defs.len()).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut x = x;
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let join = |parent: &mut Vec<usize>, set: &BTreeSet<usize>, also: Option<usize>| {
        let mut all = set.iter().copied().chain(also);
        if let Some(first) = all.next() {
            let r = find(parent, first);
            for d in all {
                let other = find(parent, d);
                parent[other] = r;
            }
        }
    };
    for (b, inn) in reach_in.iter().enumerate() {
        let mut reach = inn.clone();
        for (k, inst) in f.blocks[b].insts.iter().enumerate() {
            let def = def_at.get(&(b, k)).copied();
            if reads(inst) {
                join(&mut parent, &reach, def);
            }
            if let Some(d) = def {
                reach = BTreeSet::from([d]);
            }
        }
        if term_uses(&f.blocks[b].term).contains(&p) {
            join(&mut parent, &reach, None);
        }
    }

    // A register for each group, the parameter's own for the group on entry.
    let entry = find(&mut parent, 0);
    let mut reg_of: HashMap<usize, Reg> = HashMap::from([(entry, p)]);
    for d in 1..defs.len() {
        let g = find(&mut parent, d);
        if let std::collections::hash_map::Entry::Vacant(e) = reg_of.entry(g) {
            e.insert(Reg(f.regs.len() as u32));
            f.regs.push(f.regs[p.0 as usize].clone());
        }
    }
    if reg_of.len() == 1 {
        return;
    }
    for (b, inn) in reach_in.iter().enumerate() {
        let mut reach = inn.clone();
        for k in 0..f.blocks[b].insts.len() {
            let def = def_at.get(&(b, k)).copied();
            // Reading and writing in one instruction joined both, so any of
            // them names the register.
            let group = def.or_else(|| reach.iter().next().copied());
            if let Some(g) = group {
                let r = reg_of[&find(&mut parent, g)];
                for x in f.blocks[b].insts[k].regs_mut() {
                    if *x == p {
                        *x = r;
                    }
                }
            }
            if let Some(d) = def {
                reach = BTreeSet::from([d]);
            }
        }
        if let Some(&g) = reach.iter().next() {
            let r = reg_of[&find(&mut parent, g)];
            for x in f.blocks[b].term.regs_mut() {
                if *x == p {
                    *x = r;
                }
            }
        }
    }
}

/// The name of the version of `f` that takes its cloned parameters over.
fn consuming(f: &str) -> String {
    format!("{f}.own")
}

struct Cx<'a> {
    owning: &'a mut Owning,
    /// The parameters each function clones on entry.
    cloning: &'a HashMap<String, Vec<usize>>,
    /// Functions whose consuming version a call uses.
    wanted: &'a mut BTreeSet<String>,
}

/// The parameters (by position) that `f` must have values of its own of: it
/// reads them and also changes, consumes or replaces them.
fn cloned_params(f: &Function, owning: &mut Owning) -> Vec<usize> {
    let tracked: HashSet<Reg> = (0..f.regs.len() as u32).map(Reg).filter(|&r| owning.owns(f.reg_type(r))).collect();
    if tracked.is_empty() {
        return vec![];
    }
    let live_at_entry = liveness(f, &tracked, &HashMap::new())[0].clone();
    // A copy that stays a view of the parameter does not consume it.
    let roots = views(f, &tracked);
    (0..f.params.len())
        .filter(|&i| {
            let p = Reg(i as u32);
            tracked.contains(&p)
                && live_at_entry.contains(&p)
                && f.blocks.iter().any(|b| {
                    b.insts.iter().any(|inst| {
                        let alias = matches!(inst, Inst::Copy { dst, .. } if roots.contains_key(dst));
                        let mut i = inst.clone();
                        (!alias && consumed(&mut i).into_iter().any(|r| *r == p))
                            || defined(inst).contains(&p)
                            || changes(inst, p)
                    }) || matches!(b.term, Terminator::Ret(r) if r == p)
                })
        })
        .collect()
}

/// Whether values of a type own memory: lists, strings and function values
/// (a closure owns its captured values), and structs, arrays and enums that
/// hold one by value.
pub struct Owning {
    members: HashMap<String, Vec<Type>>,
    memo: HashMap<String, bool>,
}

impl Owning {
    pub fn new(m: &Module) -> Owning {
        let mut members = HashMap::new();
        for s in &m.structs {
            members.insert(s.name.clone(), s.fields.iter().map(|(_, t)| t.clone()).collect());
        }
        for e in &m.enums {
            members.insert(e.name.clone(), e.variants.iter().flat_map(|(_, ts)| ts.clone()).collect());
        }
        Owning { members, memo: HashMap::new() }
    }

    pub fn owns(&mut self, t: &Type) -> bool {
        match t {
            // A function value may be a closure, which owns what it captured.
            Type::List(_) | Type::Str | Type::Fn(..) | Type::Ref(_) | Type::Cell(_) | Type::Chan(_) => true,
            Type::Array(elem, _) => self.owns(elem),
            Type::Struct(name) | Type::Enum(name) => {
                if let Some(&r) = self.memo.get(name) {
                    return r;
                }
                // A type holds itself only through a list, which owns anyway.
                self.memo.insert(name.clone(), false);
                let members = self.members.get(name).cloned().unwrap_or_default();
                let r = members.iter().any(|m| self.owns(m));
                self.memo.insert(name.clone(), r);
                r
            }
            _ => false,
        }
    }
}

fn path_regs(path: &[PathStep]) -> impl Iterator<Item = Reg> + '_ {
    path.iter().filter_map(|s| match s {
        PathStep::Elem(r) => Some(*r),
        PathStep::Field(_) => None,
    })
}

/// The operands an instruction consumes (takes ownership of), as places that
/// can be replaced by a clone.
fn consumed(inst: &mut Inst) -> Vec<&mut Reg> {
    use Inst::*;
    match inst {
        Copy { dst, src } if dst == src => vec![],
        Copy { src, .. } => vec![src],
        Struct { fields, .. } | Variant { fields, .. } | Array { items: fields, .. } => fields.iter_mut().collect(),
        SetField { dst, src, value, .. } | SetElem { dst, src, value, .. } | SetPath { dst, src, value, .. } => {
            // A copy of `src` with a part replaced: in place when it is the same register.
            if dst == src {
                vec![value]
            } else {
                vec![src, value]
            }
        }
        Store { value, .. } | Push { value, .. } | Send { value, .. } | NewCell { value, .. } | CellSet { value, .. } => {
            vec![value]
        }
        Ref { src, .. } => vec![src],
        // A thread owns the function value it runs and its arguments.
        Spawn { callee, args } => std::iter::once(callee).chain(args.iter_mut()).collect(),
        Closure { captures: args, .. } => args.iter_mut().collect(),
        _ => vec![],
    }
}

/// The operands an instruction reads without taking them.
fn borrowed(inst: &Inst) -> Vec<Reg> {
    use Inst::*;
    match inst {
        Const { .. } | FConst { .. } | Unit { .. } | Str { .. } | FuncRef { .. } | ReadLine { .. } | Trap | Fence => {
            vec![]
        }
        Copy { .. } | Struct { .. } | Variant { .. } | Array { .. } | Ref { .. } | Closure { .. } | Spawn { .. } => vec![],
        Clone { src, .. }
        | Drop { src }
        | Unary { src, .. }
        | Cast { src, .. }
        | Bitcast { src, .. }
        | Field { src, .. }
        | Tag { src, .. }
        | Payload { src, .. }
        | Deref { src, .. }
        | NewChan { cap: src, .. }
        | Recv { chan: src, .. }
        | ToStr { src, .. }
        | Len { src, .. }
        | FromBytes { src, .. }
        | Addr { src, .. }
        | Unique { prefix: src, .. }
        | Stringify { code: src, .. }
        | CodeText { src, .. }
        | TypeOf { code: src, .. }
        | Load { ptr: src, .. }
        | AtomicLoad { ptr: src, .. }
        | FieldPtr { ptr: src, .. }
        | Splat { value: src, .. }
        | Print { src } => vec![*src],
        Binary { lhs, rhs, .. } => vec![*lhs, *rhs],
        Elem { src, index, .. } | StrByte { src, index, .. } | ElemPtr { ptr: src, index, .. } => vec![*src, *index],
        Repeat { value, count, .. } => vec![*value, *count],
        SetField { dst, src, .. } => [*src].into_iter().filter(|s| s == dst).collect(),
        SetElem { dst, src, index, .. } => [*src].into_iter().filter(|s| s == dst).chain([*index]).collect(),
        SetPath { dst, src, path, .. } => [*src].into_iter().filter(|s| s == dst).chain(path_regs(path)).collect(),
        AtomicRmw { ptr, value, .. } | AtomicStore { ptr, value } => vec![*ptr, *value],
        AtomicCas { ptr, expected, new, .. } => vec![*ptr, *expected, *new],
        Slice { src, lo, hi, .. } => vec![*src, *lo, *hi],
        Store { ptr, .. } | Send { chan: ptr, .. } => vec![*ptr],
        GetPath { src, path, .. } | CellGet { cell: src, path, .. } => [*src].into_iter().chain(path_regs(path)).collect(),
        CellSet { cell, path, .. } => [*cell].into_iter().chain(path_regs(path)).collect(),
        Push { root, path, .. } | Pop { root, path, .. } => [*root].into_iter().chain(path_regs(path)).collect(),
        NewCell { .. } => vec![],
        Select { cases, .. } => cases
            .iter()
            .flat_map(|c| match c {
                crate::SelectCase::Recv { chan, .. } => vec![*chan],
                crate::SelectCase::Send { chan, value } => vec![*chan, *value],
            })
            .collect(),
        Call { args, .. } | Syscall { args, .. } | Asm { args, .. } | Quote { holes: args, .. } => args.clone(),
        CallIndirect { callee, args, .. } => [*callee].into_iter().chain(args.iter().copied()).collect(),
    }
}

/// The registers an instruction writes. An instruction that changes a value in
/// place (`push`, `setpath %r, %r`) both borrows and writes it.
fn defined(inst: &Inst) -> Vec<Reg> {
    use Inst::*;
    match inst {
        Store { .. } | AtomicStore { .. } | Send { .. } | Spawn { .. } | Print { .. } | Trap | Fence | Drop { .. } => {
            vec![]
        }
        CellSet { .. } => vec![],
        Copy { dst, src } if dst == src => vec![],
        Push { root, .. } => vec![*root],
        Pop { dst, root, .. } => vec![*dst, *root],
        Select { dst, cases, .. } => [*dst]
            .into_iter()
            .chain(cases.iter().filter_map(|c| match c {
                crate::SelectCase::Recv { dst, .. } => Some(*dst),
                crate::SelectCase::Send { .. } => None,
            }))
            .collect(),
        _ => {
            let mut i = inst.clone();
            vec![*i.regs_mut().remove(0)]
        }
    }
}

/// Whether the instruction changes `r` in place, keeping what it owns.
fn changes(inst: &Inst, r: Reg) -> bool {
    use Inst::*;
    match inst {
        Push { root, .. } | Pop { root, .. } => *root == r,
        SetField { dst, src, .. } | SetElem { dst, src, .. } | SetPath { dst, src, .. } => *dst == r && *src == r,
        _ => false,
    }
}

fn term_uses(t: &Terminator) -> Vec<Reg> {
    match t {
        Terminator::Branch { cond, .. } => vec![*cond],
        Terminator::Ret(r) => vec![*r],
        Terminator::Jump(_) | Terminator::Unreachable => vec![],
    }
}

/// Registers live at the start of each block, given the owning registers. A
/// use of a view (a key of `roots`) is also a use of its root.
fn liveness(f: &Function, tracked: &HashSet<Reg>, roots: &HashMap<Reg, Reg>) -> Vec<HashSet<Reg>> {
    let n = f.blocks.len();
    let mut live_in: Vec<HashSet<Reg>> = vec![HashSet::new(); n];
    loop {
        let mut changed = false;
        for b in (0..n).rev() {
            let mut live = live_out(f, b, &live_in);
            step_term(&f.blocks[b].term, &mut live, tracked, roots);
            for inst in f.blocks[b].insts.iter().rev() {
                step_back(inst, &mut live, tracked, roots);
            }
            if live != live_in[b] {
                live_in[b] = live;
                changed = true;
            }
        }
        if !changed {
            return live_in;
        }
    }
}

fn live_out(f: &Function, b: usize, live_in: &[HashSet<Reg>]) -> HashSet<Reg> {
    let mut out = HashSet::new();
    for s in f.blocks[b].term.successors() {
        out.extend(live_in[s.0 as usize].iter().copied());
    }
    out
}

fn step_term(t: &Terminator, live: &mut HashSet<Reg>, tracked: &HashSet<Reg>, roots: &HashMap<Reg, Reg>) {
    for r in term_uses(t).into_iter().filter(|r| tracked.contains(r)) {
        live.insert(r);
        live.extend(roots.get(&r));
    }
}

/// Live registers before `inst`, from those after it.
fn step_back(inst: &Inst, live: &mut HashSet<Reg>, tracked: &HashSet<Reg>, roots: &HashMap<Reg, Reg>) {
    for d in defined(inst) {
        live.remove(&d);
    }
    for r in uses(inst).into_iter().filter(|r| tracked.contains(r)) {
        live.insert(r);
        live.extend(roots.get(&r));
    }
}

fn uses(inst: &Inst) -> Vec<Reg> {
    let mut i = inst.clone();
    consumed(&mut i).into_iter().map(|r| *r).chain(borrowed(inst)).collect()
}

/// The register an instruction reads an owning part out of, giving a view.
fn projection(inst: &Inst) -> Option<(Reg, Reg)> {
    match inst {
        Inst::Field { dst, src, .. }
        | Inst::Elem { dst, src, .. }
        | Inst::GetPath { dst, src, .. }
        | Inst::Payload { dst, src, .. }
        // What a `ref` refers to never changes, and lives while the ref does.
        | Inst::Deref { dst, src } => Some((*dst, *src)),
        _ => None,
    }
}

/// Which owning registers can stay views, and the root of each. `borrowed`
/// registers (parameters) are never written, so they are roots that stay.
fn views(f: &Function, tracked: &HashSet<Reg>) -> HashMap<Reg, Reg> {
    // Only a register written once, by a part read or a copy, and never
    // changed in place, can be a view. A part read starts one; so does a copy
    // of a parameter, or of a value still used after the copy (such as the
    // list a `for` loop walks), where a move is not possible anyway. Any other
    // copy is a view only if its source is.
    let params = f.params.len() as u32;
    let base = liveness(f, tracked, &HashMap::new());
    let mut writes: HashMap<Reg, usize> = HashMap::new();
    let mut changed = HashSet::new();
    let mut source: HashMap<Reg, (Reg, bool)> = HashMap::new();
    for (b, block) in f.blocks.iter().enumerate() {
        let mut live = live_out(f, b, &base);
        step_term(&block.term, &mut live, tracked, &HashMap::new());
        for inst in block.insts.iter().rev() {
            for d in defined(inst) {
                *writes.entry(d).or_default() += 1;
                if changes(inst, d) {
                    changed.insert(d);
                }
            }
            if let Some((dst, src)) = projection(inst) {
                source.insert(dst, (src, true));
            } else if let Inst::Copy { dst, src } = inst {
                if dst != src {
                    source.insert(*dst, (*src, src.0 < params || live.contains(src)));
                }
            }
            step_back(inst, &mut live, tracked, &HashMap::new());
        }
    }
    source.retain(|d, _| tracked.contains(d) && writes.get(d) == Some(&1) && !changed.contains(d) && d.0 >= params);
    let mut excluded: HashSet<Reg> = HashSet::new();
    loop {
        // A register that starts a view is one unless excluded; a copy is one
        // if its source is.
        let mut is_view: HashSet<Reg> =
            source.iter().filter(|(d, (_, starts))| *starts && !excluded.contains(d)).map(|(d, _)| *d).collect();
        loop {
            let more: Vec<Reg> = source
                .iter()
                .filter(|(d, (s, starts))| {
                    !*starts && !excluded.contains(d) && !is_view.contains(d) && is_view.contains(s)
                })
                .map(|(d, _)| *d)
                .collect();
            if more.is_empty() {
                break;
            }
            is_view.extend(more);
        }
        let mut roots = HashMap::new();
        for &d in &is_view {
            let mut r = source[&d].0;
            while is_view.contains(&r) {
                r = source[&r].0;
            }
            roots.insert(d, r);
        }
        // A view whose root is written while the view is live must be a copy.
        let live_in = liveness(f, tracked, &roots);
        let mut conflicts = HashSet::new();
        for b in 0..f.blocks.len() {
            let mut live = live_out(f, b, &live_in);
            step_term(&f.blocks[b].term, &mut live, tracked, &roots);
            for inst in f.blocks[b].insts.iter().rev() {
                for w in defined(inst) {
                    for (v, root) in &roots {
                        if *root == w && live.contains(v) && !defined(inst).contains(v) {
                            conflicts.insert(*v);
                        }
                    }
                }
                step_back(inst, &mut live, tracked, &roots);
            }
        }
        if conflicts.is_empty() {
            return roots;
        }
        excluded.extend(conflicts);
    }
}

/// Inserts clones and drops into `f`. The parameters at `given` are handed
/// over by the caller (in a consuming version); the others are borrowed.
fn function(f: &mut Function, cx: &mut Cx<'_>, given: &[usize]) {
    let owning = &mut *cx.owning;
    let mut tracked: HashSet<Reg> = (0..f.regs.len() as u32).map(Reg).filter(|&r| owning.owns(f.reg_type(r))).collect();
    if tracked.is_empty() {
        return;
    }
    let cloned = cloned_params(f, owning);
    entry_without_predecessors(f);

    // Parameters are borrowed. One whose value the function changes, consumes
    // or replaces while it still reads it gets a copy of its own on entry,
    // unless the caller handed it over. One never read is never freed here:
    // only later values, which the function owns.
    let live_at_entry = liveness(f, &tracked, &HashMap::new())[0].clone();
    let mut borrowed_params = HashSet::new();
    let mut entry_clones = Vec::new();
    for i in 0..f.params.len() {
        let p = Reg(i as u32);
        if !tracked.contains(&p) || !live_at_entry.contains(&p) || given.contains(&i) {
            continue;
        }
        if cloned.contains(&i) {
            entry_clones.push(Inst::Clone { dst: p, src: p });
        } else {
            borrowed_params.insert(p);
        }
    }
    f.blocks[0].insts.splice(0..0, entry_clones);

    // Parts read out of values are views where that cannot be told apart from
    // a copy; the others become copies right after the read.
    //
    // The read goes to a view of its own, which the copy is made from, so the
    // value read from stays alive until the copy is made.
    let mut roots = views(f, &tracked);
    let mut new_regs = Vec::new();
    let mut next = f.regs.len() as u32;
    for b in &mut f.blocks {
        let mut insts = Vec::with_capacity(b.insts.len());
        for mut inst in std::mem::take(&mut b.insts) {
            // What a cell holds can change through any other holder of the
            // cell, so a part read out of it is always a copy.
            let read_from_cell = match &inst {
                Inst::CellGet { dst, cell, .. } => Some((*dst, *cell)),
                _ => None,
            };
            let copy = projection(&inst)
                .filter(|(d, _)| tracked.contains(d) && !roots.contains_key(d))
                .or(read_from_cell.filter(|(d, _)| tracked.contains(d)));
            match copy {
                Some((d, src)) => {
                    let view = Reg(next);
                    next += 1;
                    new_regs.push(f.regs[d.0 as usize].clone());
                    *inst.regs_mut()[0] = view;
                    roots.insert(view, roots.get(&src).copied().unwrap_or(src));
                    tracked.insert(view);
                    insts.push(inst);
                    insts.push(Inst::Clone { dst: d, src: view });
                }
                None => insts.push(inst),
            }
        }
        b.insts = insts;
    }
    f.regs.extend(new_regs);
    // Neither borrowed parameters nor views are ever freed: their memory is
    // someone else's. Consuming one copies it. A value read through a pointer
    // (a function value; nothing else that owns memory can be) is borrowed
    // from that memory too.
    let loaded = f.blocks.iter().flat_map(|b| &b.insts).filter_map(|i| match i {
        Inst::Load { dst, .. } if tracked.contains(dst) => Some(*dst),
        _ => None,
    });
    let borrowed_params: HashSet<Reg> =
        borrowed_params.into_iter().chain(roots.keys().copied()).chain(loaded.collect::<Vec<_>>()).collect();

    let owned: HashSet<Reg> = tracked.difference(&borrowed_params).copied().collect();
    let live_in = liveness(f, &tracked, &roots);

    // Within blocks: clones before consuming uses that are not last uses, drops
    // after last uses that do not move.
    for b in 0..f.blocks.len() {
        let mut live = live_out(f, b, &live_in);
        let block = std::mem::replace(&mut f.blocks[b], Block { insts: vec![], term: Terminator::Unreachable });
        // `ret` moves its value.
        step_term(&block.term, &mut live, &tracked, &roots);
        let mut after: Vec<HashSet<Reg>> = Vec::with_capacity(block.insts.len());
        for inst in block.insts.iter().rev() {
            after.push(live.clone());
            step_back(inst, &mut live, &tracked, &roots);
        }
        after.reverse();

        let mut insts = Vec::with_capacity(block.insts.len());
        // What `inst` reads, the roots of the views among them (read through
        // them), and what it writes.
        let touched = |inst: &Inst| -> HashSet<Reg> {
            let u = uses(inst);
            let through = u.iter().filter_map(|r| roots.get(r)).copied().collect::<Vec<_>>();
            u.into_iter().chain(through).chain(defined(inst)).collect()
        };
        for (mut inst, live_after) in block.insts.into_iter().zip(after) {
            let borrows = borrowed(&inst);
            let before = touched(&inst);
            let mut moved = HashSet::new();
            let mut clones = Vec::new();
            // A call to a function that clones parameters on entry, with
            // values of our own at their last use for all of them, hands them
            // over to its consuming version instead.
            if let Inst::Call { func, args, .. } = &mut inst {
                if let Some(params) = cx.cloning.get(func.as_str()) {
                    let gives = params.iter().all(|&i| {
                        let a = args[i];
                        owned.contains(&a)
                            && !live_after.contains(&a)
                            && args.iter().filter(|&&x| x == a).count() == 1
                            && !roots.values().any(|r| *r == a && live_after.iter().any(|v| roots.get(v) == Some(&a)))
                    });
                    if gives {
                        cx.wanted.insert(func.clone());
                        *func = consuming(func);
                        moved.extend(params.iter().map(|&i| args[i]));
                    }
                }
            }
            // A view copied into a view is the same view.
            let alias = matches!(&inst, Inst::Copy { dst, .. } if roots.contains_key(dst));
            if !alias {
                let slots = consumed(&mut inst);
                let regs: Vec<Reg> = slots.iter().map(|r| **r).collect();
                for (k, slot) in slots.into_iter().enumerate() {
                    let r = *slot;
                    if !owned.contains(&r) && !borrowed_params.contains(&r) {
                        continue;
                    }
                    let used_again = live_after.contains(&r)
                        || regs[k + 1..].contains(&r)
                        || borrows.contains(&r)
                        || borrowed_params.contains(&r);
                    if used_again {
                        let t = Reg(f.regs.len() as u32);
                        f.regs.push(f.regs[r.0 as usize].clone());
                        clones.push(Inst::Clone { dst: t, src: r });
                        *slot = t;
                    } else {
                        moved.insert(r);
                    }
                }
            }
            let mut dying: Vec<Reg> =
                before.into_iter().filter(|r| owned.contains(r) && !live_after.contains(r) && !moved.contains(r)).collect();
            dying.sort_by_key(|r| r.0);
            insts.extend(clones);
            insts.push(inst);
            insts.extend(dying.into_iter().map(|src| Inst::Drop { src }));
        }
        // `ret` gives its value away: a value of its own moves, a borrowed one
        // or a view is copied. Everything else still owned dies here.
        let mut term = block.term;
        if let Terminator::Ret(r) = &mut term {
            if tracked.contains(r) {
                let mut dying: Vec<Reg> = roots.get(r).copied().into_iter().filter(|x| owned.contains(x)).collect();
                if borrowed_params.contains(r) {
                    let t = Reg(f.regs.len() as u32);
                    f.regs.push(f.regs[r.0 as usize].clone());
                    insts.push(Inst::Clone { dst: t, src: *r });
                    *r = t;
                }
                dying.sort_by_key(|x| x.0);
                insts.extend(dying.into_iter().map(|src| Inst::Drop { src }));
            }
        }
        f.blocks[b] = Block { insts, term };
    }

    // On edges: a value live at the end of a block but not at the start of a
    // successor dies on the way there.
    let preds = predecessor_counts(f);
    for b in 0..f.blocks.len() {
        let out = live_out(f, b, &live_in);
        let succs = f.blocks[b].term.successors();
        for (k, s) in succs.iter().enumerate() {
            let mut dying: Vec<Reg> =
                out.iter().filter(|r| owned.contains(r) && !live_in[s.0 as usize].contains(r)).copied().collect();
            if dying.is_empty() {
                continue;
            }
            dying.sort_by_key(|r| r.0);
            let drops = dying.into_iter().map(|src| Inst::Drop { src });
            if preds[s.0 as usize] == 1 {
                f.blocks[s.0 as usize].insts.splice(0..0, drops);
            } else {
                // A block of its own on this edge.
                let id = BlockId(f.blocks.len() as u32);
                f.blocks.push(Block { insts: drops.collect(), term: Terminator::Jump(*s) });
                *f.blocks[b].term.successors_mut()[k] = id;
            }
        }
    }
}

/// How many edges lead to each block.
fn predecessor_counts(f: &Function) -> Vec<usize> {
    let mut n = vec![0; f.blocks.len()];
    for b in &f.blocks {
        for s in b.term.successors() {
            n[s.0 as usize] += 1;
        }
    }
    n
}

/// Makes sure nothing jumps back to the entry block, so code put at its start
/// runs once.
fn entry_without_predecessors(f: &mut Function) {
    if predecessor_counts(f)[0] == 0 {
        return;
    }
    for b in &mut f.blocks {
        for s in b.term.successors_mut() {
            s.0 += 1;
        }
    }
    f.blocks.insert(0, Block { insts: vec![], term: Terminator::Jump(BlockId(1)) });
}
