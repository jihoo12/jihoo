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
//!   its root is not changed while the view is still used, except in place at
//!   a part disjoint from the view's (liveness counts a use of a view as a use
//!   of its root, so the root is not freed or moved away before the view's
//!   last use either). Otherwise a `clone` right after the read makes it a
//!   value of its own. So `len(g.nodes)` copies nothing, and neither does
//!   `let t = s.text` while `s.at` changes.
//!   A `copy` of a value that is still used afterwards, or of a parameter,
//!   is a view on the same terms, so the copy of a list that a `for` loop
//!   walks costs nothing unless the body changes the list.
//! - A value that is only *taken apart* (see [`takes`]): its owning parts read
//!   out once each, in a block where the value dies, gives the parts away:
//!   they move out, and only the parts left are freed. Each arm of a `match`
//!   takes apart what it matches. So taking `(it, x)` out of `Some((it, x))`,
//!   as a `for` loop over an iterator does each round, copies nothing.
//! - A part read out of a value that is used again (a field, an element, or
//!   a path of them) is *taken* (`take`, see [`field_takes`]) when the read
//!   would otherwise be copied and the part is not read again before it, or
//!   a part holding it, is written, or the value dies: the part moves out
//!   and the value keeps it empty. So `m.inner = f(m.inner)` and
//!   `rows[i] = f(rows[i])` copy nothing.
//!
//!   A parameter taken apart is one only when the function
//!   owns it: one that would copy a part it reads clones the parameter on
//!   entry instead, and so has a consuming version (below) that copies
//!   nothing.
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
//! any of them are values of the caller's own at their last use calls
//! `f.own`: those values move, and the caller clones the others, as `f`
//! would have. So `xs = f(xs)` changes `xs` in place, copying nothing, and
//! `m = maps.set(m, k, v)` copies at most `k`.
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

use crate::{Block, BlockId, Function, Inst, Module, Ownership, PathStep, Reg, Terminator, Type};

/// Inserts `clone` and `drop` into every function of a native module.
pub fn insert(m: &mut Module) {
    let mut owning = Owning::new(m);
    for f in &mut m.funcs {
        split_params(f, &mut owning);
    }
    // The parameters each function clones on entry, by position. Taking
    // fields does not change them: it takes a parameter's only when the
    // function clones the parameter anyway.
    let mut cloning: HashMap<String, Vec<usize>> = HashMap::new();
    for f in &m.funcs {
        let c = cloned_params(f, &mut owning);
        if !c.is_empty() {
            cloning.insert(f.name.clone(), c);
        }
    }
    // A parameter handed at its last use to a callee that clones it is one
    // the function had better clone itself: the copy is made once either way,
    // and its consuming version then hands the caller's value on, so a chain
    // of functions that only pass a list along copies nothing.
    loop {
        let mut changed = false;
        for f in &m.funcs {
            let more = handed_on(f, &mut owning, &cloning);
            if !more.is_empty() {
                let c = cloning.entry(f.name.clone()).or_default();
                c.extend(more);
                c.sort_unstable();
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for f in &mut m.funcs {
        field_takes(f, &mut owning, &cloning);
    }
    // The parameters of each function that own memory, which its version for
    // tail calls takes over.
    let mut owning_params: HashMap<String, Vec<usize>> = HashMap::new();
    for f in &m.funcs {
        let ps: Vec<usize> = (0..f.params.len()).filter(|&i| owning.owns(&f.params[i])).collect();
        if !ps.is_empty() {
            owning_params.insert(f.name.clone(), ps);
        }
    }
    let originals: HashMap<String, Function> = m
        .funcs
        .iter()
        .filter(|f| cloning.contains_key(&f.name) || owning_params.contains_key(&f.name))
        .map(|f| (f.name.clone(), f.clone()))
        .collect();
    let mut wanted = BTreeSet::new();
    let mut tails = BTreeSet::new();
    let mut cx = Cx { owning: &mut owning, cloning: &cloning, owning_params: &owning_params, wanted: &mut wanted, tails: &mut tails };
    for f in &mut m.funcs {
        function(f, &mut cx, &[]);
    }
    // The consuming versions that calls asked for, those for tail calls, and
    // those they ask for in turn.
    let mut made = HashSet::new();
    loop {
        let (name, given, version) = if let Some(name) = wanted.pop_first() {
            let given = cloning[&name].clone();
            (name.clone(), given, consuming(&name))
        } else if let Some(name) = tails.pop_first() {
            let given = owning_params[&name].clone();
            (name.clone(), given, tail_version(&name))
        } else {
            break;
        };
        if !made.insert(version.clone()) {
            continue;
        }
        let mut f = originals[&name].clone();
        f.name = version;
        f.export = None;
        let mut cx = Cx { owning: &mut owning, cloning: &cloning, owning_params: &owning_params, wanted: &mut wanted, tails: &mut tails };
        function(&mut f, &mut cx, &given);
        m.funcs.push(f);
    }
}

/// The version of function `f` that tail calls call: it takes over every
/// parameter that owns memory, since the caller's frame is gone.
fn tail_version(f: &str) -> String {
    format!("{f}.tail")
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
    /// The parameters of each function that own memory.
    owning_params: &'a HashMap<String, Vec<usize>>,
    /// Functions whose consuming version a call uses.
    wanted: &'a mut BTreeSet<String>,
    /// Functions whose version for tail calls a tail call uses.
    tails: &'a mut BTreeSet<String>,
}

/// The parameters (by position) that `f` must have values of its own of: it
/// reads them and also changes, consumes or replaces them, or takes them
/// apart (see [`takes`]) and would have to copy a part it reads.
fn cloned_params(f: &Function, owning: &mut Owning) -> Vec<usize> {
    let tracked: HashSet<Reg> = (0..f.regs.len() as u32).map(Reg).filter(|&r| owning.owns(f.reg_type(r))).collect();
    if tracked.is_empty() {
        return vec![];
    }
    let live_at_entry = liveness(f, &tracked, &HashMap::new())[0].clone();
    // A copy that stays a view of the parameter does not consume it.
    let roots = views(f, &tracked, &HashSet::new());
    let all: HashSet<Reg> = (0..f.params.len() as u32).map(Reg).collect();
    let taken = takes(f, &tracked, owning, &all);
    (0..f.params.len())
        .filter(|&i| {
            let p = Reg(i as u32);
            // A part read out of it that cannot stay a view, or that is consumed.
            let copies_part = taken.moves.iter().any(|(_, d, s)| {
                *s == p && (!roots.contains_key(d) || f.blocks.iter().flat_map(|b| &b.insts).any(|inst| consumes(inst, *d, &roots)))
            });
            tracked.contains(&p)
                && live_at_entry.contains(&p)
                && (copies_part || f.blocks.iter().any(|b| {
                    b.insts.iter().any(|inst| {
                        consumes(inst, p, &roots)
                            || defined(inst).contains(&p)
                            || changes(inst, p)
                    }) || term_uses(&b.term).contains(&p)
                }))
        })
        .collect()
}

/// The parameters of `f` that it does not clone yet (by `cloning`) but hands,
/// or a part of which it hands, at their last use to a callee that clones
/// them, or to a tail call, whose callee takes its parameters over.
fn handed_on(f: &Function, owning: &mut Owning, cloning: &HashMap<String, Vec<usize>>) -> Vec<usize> {
    let tracked: HashSet<Reg> = (0..f.regs.len() as u32).map(Reg).filter(|&r| owning.owns(f.reg_type(r))).collect();
    let already = cloning.get(&f.name).map_or(&[][..], Vec::as_slice);
    let candidates: Vec<usize> = (0..f.params.len()).filter(|&i| !already.contains(&i) && tracked.contains(&Reg(i as u32))).collect();
    if candidates.is_empty() {
        return vec![];
    }
    let roots = views(f, &tracked, &HashSet::new());
    let live_in = liveness(f, &tracked, &roots);
    if candidates.iter().all(|&i| !live_in[0].contains(&Reg(i as u32))) {
        return vec![];
    }
    let mut out = Vec::new();
    for (b, block) in f.blocks.iter().enumerate() {
        // Live registers after each instruction, from the end of the block.
        let mut live = live_out(f, b, &live_in);
        let handed = |args: &[Reg], takes: &[usize], live: &HashSet<Reg>, out: &mut Vec<usize>| {
            for &i in &candidates {
                let p = Reg(i as u32);
                // The parameter, or a part of it read as a view.
                let at: Vec<usize> = args.iter().enumerate().filter(|(_, a)| **a == p || roots.get(a) == Some(&p)).map(|(j, _)| j).collect();
                let [j] = at[..] else { continue };
                // What is handed on is not used again: the parameter, or the
                // view (whose part a consuming version takes out of it, while
                // the rest of the parameter may still be used).
                let dead_after = if args[j] == p { !live.contains(&p) } else { !live.contains(&args[j]) };
                if takes.contains(&j) && dead_after && live_in[0].contains(&p) && !out.contains(&i) {
                    out.push(i);
                }
            }
        };
        if let Terminator::TailCall { func, args } = &block.term {
            let takes: Vec<usize> = (0..args.len()).collect();
            if f.name != *func {
                handed(args, &takes, &live, &mut out);
            }
        }
        step_term(&block.term, &mut live, &tracked, &roots);
        for inst in block.insts.iter().rev() {
            if let Inst::Call { func, args, .. } = inst {
                if let Some(takes) = cloning.get(func) {
                    handed(args, takes, &live, &mut out);
                }
            }
            step_back(inst, &mut live, &tracked, &roots);
        }
    }
    out
}

/// Whether `inst` consumes `r`; a copy that stays a view (a key of `roots`)
/// does not.
fn consumes(inst: &Inst, r: Reg, roots: &HashMap<Reg, Reg>) -> bool {
    let alias = matches!(inst, Inst::Copy { dst, .. } if roots.contains_key(dst));
    let mut i = inst.clone();
    !alias && consumed(&mut i).into_iter().any(|c| *c == r)
}

/// Whether values of a type own memory: lists, strings and function values
/// (a closure owns its captured values), and structs, arrays and enums that
/// hold one by value.
pub struct Owning {
    members: HashMap<String, Vec<Type>>,
    memo: HashMap<String, bool>,
    /// The fields of each struct, and the values of each variant of each enum.
    pub(crate) fields: HashMap<String, Vec<Type>>,
    pub(crate) variants: HashMap<String, Vec<Vec<Type>>>,
}

impl Owning {
    pub fn new(m: &Module) -> Owning {
        let mut members = HashMap::new();
        let mut fields = HashMap::new();
        let mut variants = HashMap::new();
        for s in &m.structs {
            let ts: Vec<Type> = s.fields.iter().map(|(_, t)| t.clone()).collect();
            members.insert(s.name.clone(), ts.clone());
            fields.insert(s.name.clone(), ts);
        }
        for e in &m.enums {
            members.insert(e.name.clone(), e.variants.iter().flat_map(|(_, ts)| ts.clone()).collect());
            variants.insert(e.name.clone(), e.variants.iter().map(|(_, ts)| ts.clone()).collect());
        }
        Owning { members, memo: HashMap::new(), fields, variants }
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

pub(crate) fn path_regs(path: &[PathStep]) -> impl Iterator<Item = Reg> + '_ {
    path.iter().filter_map(|s| match s {
        PathStep::Elem(r) => Some(*r),
        PathStep::Field(_) => None,
    })
}

/// The operands an instruction consumes (takes ownership of), as places that
/// can be replaced by a clone.
pub(crate) fn consumed(inst: &mut Inst) -> Vec<&mut Reg> {
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
pub(crate) fn borrowed(inst: &Inst) -> Vec<Reg> {
    use Inst::*;
    match inst {
        Const { .. } | FConst { .. } | Unit { .. } | Str { .. } | FuncRef { .. } | ReadLine { .. } | Args { .. } | Trap | Fence | Loc { .. } => {
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
        | ReadFile { path: src, .. }
        | ParseFloat { src, .. }
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
        WriteFile { path, data, .. } => vec![*path, *data],
        Store { ptr, .. } | Send { chan: ptr, .. } => vec![*ptr],
        GetPath { src, path, .. } | CellGet { cell: src, path, .. } | Take { src, path, .. } => {
            [*src].into_iter().chain(path_regs(path)).collect()
        }
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
pub(crate) fn defined(inst: &Inst) -> Vec<Reg> {
    use Inst::*;
    match inst {
        Store { .. } | AtomicStore { .. } | Send { .. } | Spawn { .. } | Print { .. } | Trap | Fence | Drop { .. } | Loc { .. } => {
            vec![]
        }
        CellSet { .. } => vec![],
        Copy { dst, src } if dst == src => vec![],
        Push { root, .. } => vec![*root],
        Pop { dst, root, .. } | Take { dst, src: root, .. } => vec![*dst, *root],
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
pub(crate) fn changes(inst: &Inst, r: Reg) -> bool {
    use Inst::*;
    match inst {
        Push { root, .. } | Pop { root, .. } | Take { src: root, .. } => *root == r,
        SetField { dst, src, .. } | SetElem { dst, src, .. } | SetPath { dst, src, .. } => *dst == r && *src == r,
        _ => false,
    }
}

fn term_uses(t: &Terminator) -> Vec<Reg> {
    match t {
        Terminator::Branch { cond, .. } => vec![*cond],
        Terminator::Ret(r) => vec![*r],
        Terminator::TailCall { args, .. } => args.clone(),
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
///
/// Changing the root in place does not end a view of a part the change
/// cannot touch: `let t = s.text` stays a view while `s.at += 1`, since
/// fields are disjoint whatever elements the paths pass through. So each
/// view also knows its path from the root, where it has one (not through a
/// payload or a ref).
fn views(f: &Function, tracked: &HashSet<Reg>, moves: &HashSet<(usize, Reg, Reg)>) -> HashMap<Reg, Reg> {
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
    // The path of the part each part read reads, if it is one of fields and
    // elements.
    let mut step: HashMap<Reg, Option<Vec<PathStep>>> = HashMap::new();
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
                if !moves.contains(&(b, dst, src)) {
                    source.insert(dst, (src, true));
                    step.insert(dst, part_path(inst).map(|(_, _, p)| p));
                }
            } else if let Inst::Copy { dst, src } = inst {
                if dst != src {
                    source.insert(*dst, (*src, src.0 < params || live.contains(src)));
                    step.insert(*dst, Some(vec![]));
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
        let mut paths: HashMap<Reg, Option<Vec<PathStep>>> = HashMap::new();
        for &d in &is_view {
            let mut r = source[&d].0;
            // Outside in, as the reads go from the root.
            let mut path = step[&d].clone();
            while is_view.contains(&r) {
                path = match (&step[&r], path) {
                    (Some(outer), Some(inner)) => Some(outer.iter().chain(&inner).copied().collect()),
                    _ => None,
                };
                r = source[&r].0;
            }
            roots.insert(d, r);
            paths.insert(d, path);
        }
        // A view whose root is written while the view is live must be a copy,
        // unless the root is changed in place at a part disjoint from it.
        let live_in = liveness(f, tracked, &roots);
        let mut conflicts = HashSet::new();
        for b in 0..f.blocks.len() {
            let mut live = live_out(f, b, &live_in);
            step_term(&f.blocks[b].term, &mut live, tracked, &roots);
            for inst in f.blocks[b].insts.iter().rev() {
                for w in defined(inst) {
                    let changed = changed_part(inst, w);
                    for (v, root) in &roots {
                        let apart = match (&changed, &paths[v]) {
                            (Some(c), Some(p)) => overlap(p, c) == Overlap::Disjoint,
                            _ => false,
                        };
                        if *root == w && live.contains(v) && !defined(inst).contains(v) && !apart {
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

/// The part of `r` that `inst` changes in place, keeping the rest: a field,
/// an element or a path of them that it writes, takes, or pushes onto or pops
/// from.
pub(crate) fn changed_part(inst: &Inst, r: Reg) -> Option<Vec<PathStep>> {
    match inst {
        Inst::SetField { dst, src, index, value } if *dst == r && *src == r && *value != r => {
            Some(vec![PathStep::Field(*index)])
        }
        Inst::SetElem { dst, src, index, value } if *dst == r && *src == r && *value != r => {
            Some(vec![PathStep::Elem(*index)])
        }
        Inst::SetPath { dst, src, path, value } if *dst == r && *src == r && *value != r => Some(path.clone()),
        Inst::Push { root, path, value } if *root == r && *value != r => Some(path.clone()),
        Inst::Pop { root, path, dst } if *root == r && *dst != r => Some(path.clone()),
        Inst::Take { src, path, dst } if *src == r && *dst != r => Some(path.clone()),
        _ => None,
    }
}

/// Turns reads of a part of a value (a field, an element, or a path of them)
/// that would be copied into `take`s where that cannot be told apart: the
/// value is the function's own (not a view, and a parameter only if the
/// function copies it anyway), no view of it is still used, and on every way
/// on from the read the part, or a part holding it, is written whole, or the
/// value is written or dies, before anything reads the part or the whole
/// value. The value keeps the part empty meanwhile, which writing it or
/// dropping the value frees nothing of.
fn field_takes(f: &mut Function, owning: &mut Owning, cloning: &HashMap<String, Vec<usize>>) {
    let tracked: HashSet<Reg> = (0..f.regs.len() as u32).map(Reg).filter(|&r| owning.owns(f.reg_type(r))).collect();
    if tracked.is_empty() {
        return;
    }
    let params = f.params.len() as u32;
    let roots = views(f, &tracked, &HashSet::new());
    // Including the parameters it hands on to callees that clone them.
    let copied_params = cloning.get(&f.name).cloned().unwrap_or_default();
    let live_in = liveness(f, &tracked, &roots);
    // How many instructions write each register, to follow `let x = s.f`.
    let mut writes: HashMap<Reg, usize> = HashMap::new();
    for inst in f.blocks.iter().flat_map(|b| &b.insts) {
        for d in defined(inst) {
            *writes.entry(d).or_default() += 1;
        }
    }
    // The part of which value each register views, when it is a part read
    // (or only ever a copy of one).
    let mut parts_of: HashMap<Reg, (Reg, Vec<PathStep>)> = HashMap::new();
    for inst in f.blocks.iter().flat_map(|b| &b.insts) {
        if let Some((d, s, q)) = part_path(inst) {
            if writes.get(&d) == Some(&1) {
                parts_of.insert(d, (s, q));
            }
        }
    }
    for _ in 0..4 {
        for inst in f.blocks.iter().flat_map(|b| &b.insts) {
            if let Inst::Copy { dst: d, src: s } = *inst {
                if let Some(p) = parts_of.get(&s).cloned().filter(|_| writes.get(&d) == Some(&1)) {
                    parts_of.entry(d).or_insert(p);
                }
            }
        }
    }
    let mut found = Vec::new();
    for (b, block) in f.blocks.iter().enumerate() {
        let mut live = live_out(f, b, &live_in);
        step_term(&block.term, &mut live, &tracked, &roots);
        let mut after = Vec::with_capacity(block.insts.len());
        for inst in block.insts.iter().rev() {
            after.push(live.clone());
            step_back(inst, &mut live, &tracked, &roots);
        }
        after.reverse();
        for (i, inst) in block.insts.iter().enumerate() {
            let Some((dst, src, path)) = part_path(inst) else { continue };
            if !tracked.contains(&dst) || dst == src || roots.contains_key(&src) || path_regs(&path).any(|x| x == src) {
                continue;
            }
            if src.0 < params && !copied_params.contains(&(src.0 as usize)) {
                continue;
            }
            // Worth it only where the part would be copied: it cannot stay a
            // view, or it is consumed, or handed to a function that copies it.
            // The part read, and the registers that only ever hold a copy of it.
            let mut part = HashSet::from([dst]);
            for _ in 0..4 {
                for inst in f.blocks.iter().flat_map(|b| &b.insts) {
                    if let Inst::Copy { dst: d, src: s } = *inst {
                        if part.contains(&s) && writes.get(&d) == Some(&1) && roots.get(&d) == Some(&src) {
                            part.insert(d);
                        }
                    }
                }
            }
            let copied = !roots.contains_key(&dst)
                || f.blocks.iter().flat_map(|b| &b.insts).any(|inst| {
                    part.iter().any(|&r| consumes(inst, r, &roots))
                        || matches!(inst, Inst::Call { func, args, .. }
                            if cloning.get(func.as_str()).is_some_and(|ps| ps.iter().any(|&p| part.contains(&args[p]))))
                });
            // Another view of the value, of a part that may overlap this one.
            let viewed = after[i].iter().any(|v| {
                !part.contains(v)
                    && roots.get(v) == Some(&src)
                    && !parts_of.get(v).is_some_and(|(s, q)| *s == src && overlap(&path, q) == Overlap::Disjoint)
            });
            if copied && !viewed && part_unread(f, b, i, src, &path) {
                found.push((b, i, dst, src, path));
            }
        }
    }
    for (b, i, dst, src, path) in found {
        f.blocks[b].insts[i] = Inst::Take { dst, src, path };
    }
}

/// `inst` as a read of a part that a `take` could move out: its result, what
/// it reads from, and the part's path.
pub(crate) fn part_path(inst: &Inst) -> Option<(Reg, Reg, Vec<PathStep>)> {
    match inst {
        Inst::Field { dst, src, index } => Some((*dst, *src, vec![PathStep::Field(*index)])),
        Inst::Elem { dst, src, index } => Some((*dst, *src, vec![PathStep::Elem(*index)])),
        Inst::GetPath { dst, src, path } => Some((*dst, *src, path.clone())),
        _ => None,
    }
}

/// How the part of a value at path `q` stands to the part at `p`.
#[derive(PartialEq)]
pub(crate) enum Overlap {
    /// No part of one is part of the other.
    Disjoint,
    Same,
    /// `q` is a part of `p`.
    Inside,
    /// `p` is a part of `q`.
    Holds,
    /// Elements at different index registers, which may be the same.
    Unknown,
}

pub(crate) fn overlap(p: &[PathStep], q: &[PathStep]) -> Overlap {
    for (a, b) in p.iter().zip(q) {
        match (a, b) {
            (PathStep::Field(x), PathStep::Field(y)) if x != y => return Overlap::Disjoint,
            (PathStep::Field(_), PathStep::Field(_)) => {}
            (PathStep::Elem(x), PathStep::Elem(y)) if x == y => {}
            _ => return Overlap::Unknown,
        }
    }
    match q.len().cmp(&p.len()) {
        std::cmp::Ordering::Equal => Overlap::Same,
        std::cmp::Ordering::Greater => Overlap::Inside,
        std::cmp::Ordering::Less => Overlap::Holds,
    }
}

/// Whether, on every way on from instruction `at` of block `block`, the part
/// of `r` at path `p` (or a part holding it) is written whole, or `r` is
/// written or dies, before the part or the whole of `r` is read. A part
/// written at an unknown element, or inside the empty part, counts as read;
/// so does writing a register the path indexes with, after which the same
/// register may mean another element.
fn part_unread(f: &Function, block: usize, at: usize, r: Reg, p: &[PathStep]) -> bool {
    enum Step {
        Next,
        Fine,
        Read,
    }
    let indices: Vec<Reg> = path_regs(p).collect();
    let read = |q: &[PathStep]| if overlap(p, q) == Overlap::Disjoint { Step::Next } else { Step::Read };
    let write = |q: &[PathStep]| match overlap(p, q) {
        Overlap::Disjoint => Step::Next,
        Overlap::Same | Overlap::Holds => Step::Fine,
        Overlap::Inside | Overlap::Unknown => Step::Read,
    };
    let step = |inst: &Inst| -> Step {
        if defined(inst).iter().any(|d| indices.contains(d)) {
            return Step::Read;
        }
        let mentions = |path: &[PathStep]| path_regs(path).any(|x| x == r);
        match inst {
            Inst::Field { src, index, .. } if *src == r => read(&[PathStep::Field(*index)]),
            Inst::Elem { src, index, .. } if *src == r && *index != r => read(&[PathStep::Elem(*index)]),
            Inst::GetPath { src, path, .. } | Inst::Take { src, path, .. } if *src == r && !mentions(path) => read(path),
            Inst::SetField { dst, src, index, value } if *dst == r && *src == r && *value != r => {
                write(&[PathStep::Field(*index)])
            }
            Inst::SetElem { dst, src, index, value } if *dst == r && *src == r && *value != r && *index != r => {
                write(&[PathStep::Elem(*index)])
            }
            Inst::SetPath { dst, src, path, value } if *dst == r && *src == r && *value != r && !mentions(path) => {
                write(path)
            }
            Inst::Push { root, path, value } if *root == r && *value != r && !mentions(path) => read(path),
            Inst::Pop { root, path, dst } if *root == r && *dst != r && !mentions(path) => read(path),
            Inst::Drop { src } if *src == r => Step::Fine,
            inst if uses(inst).contains(&r) => Step::Read,
            inst if defined(inst).contains(&r) => Step::Fine,
            _ => Step::Next,
        }
    };
    let mut seen = HashSet::new();
    let mut todo = vec![(block, at + 1)];
    while let Some((b, from)) = todo.pop() {
        let mut fine = false;
        for inst in &f.blocks[b].insts[from..] {
            match step(inst) {
                Step::Next => {}
                Step::Fine => {
                    fine = true;
                    break;
                }
                Step::Read => return false,
            }
        }
        if fine {
            continue;
        }
        let term = &f.blocks[b].term;
        if term_uses(term).contains(&r) {
            return false;
        }
        for s in term.successors() {
            if seen.insert(s.0 as usize) {
                todo.push((s.0 as usize, 0));
            }
        }
    }
    true
}

/// A part of a value: field `index` of a struct, or value `index` of
/// variant `variant` of an enum.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Part {
    Field(u32),
    Payload { variant: u32, index: u32 },
}

impl Part {
    /// The instruction that reads this part of `src` into `dst`.
    fn read(self, dst: Reg, src: Reg) -> Inst {
        match self {
            Part::Field(index) => Inst::Field { dst, src, index },
            Part::Payload { variant, index } => Inst::Payload { dst, src, variant, index },
        }
    }
}

/// `inst` as a read of a part: what it reads into, from what, and which part.
fn part_read(inst: &Inst) -> Option<(Reg, Reg, Part)> {
    match *inst {
        Inst::Field { dst, src, index } => Some((dst, src, Part::Field(index))),
        Inst::Payload { dst, src, variant, index } => Some((dst, src, Part::Payload { variant, index })),
        _ => None,
    }
}

/// The values a function takes apart, and how.
struct Takes {
    /// The part reads that move their part out of the value, as (block, dst,
    /// src).
    moves: HashSet<(usize, Reg, Reg)>,
    /// At the last use of each value taken apart, as (block, dst, src): the
    /// owning parts still in it, which are freed instead of the value.
    rest: HashMap<(usize, Reg, Reg), Vec<(Part, Type)>>,
}

/// The values of `f` that are only taken apart: values of the function's own
/// (parameters only among `owned_params`, and nothing read out of or copied
/// from another value, unless taken apart from it in turn) that nothing uses
/// but `tag` and part reads. In each block that reads parts of one, they are
/// parts of one variant, each owning part is read once, and the value is dead
/// after the last of them; so the arms of a `match` each take apart what they
/// match. Their owning parts can then move out: nothing can read the value
/// again, so nothing can tell.
fn takes(f: &Function, tracked: &HashSet<Reg>, owning: &mut Owning, owned_params: &HashSet<Reg>) -> Takes {
    let params = f.params.len() as u32;
    let base = liveness(f, tracked, &HashMap::new());
    // Live registers after each instruction.
    let mut live_after: Vec<Vec<HashSet<Reg>>> = Vec::with_capacity(f.blocks.len());
    for (b, block) in f.blocks.iter().enumerate() {
        let mut live = live_out(f, b, &base);
        step_term(&block.term, &mut live, tracked, &HashMap::new());
        let mut after = Vec::with_capacity(block.insts.len());
        for inst in block.insts.iter().rev() {
            after.push(live.clone());
            step_back(inst, &mut live, tracked, &HashMap::new());
        }
        after.reverse();
        live_after.push(after);
    }
    let mut used: HashMap<Reg, Vec<(usize, usize)>> = HashMap::new();
    let mut defs: HashMap<Reg, Vec<(usize, usize)>> = HashMap::new();
    let mut in_term = HashSet::new();
    for (b, block) in f.blocks.iter().enumerate() {
        for (i, inst) in block.insts.iter().enumerate() {
            for r in uses(inst).into_iter().filter(|r| tracked.contains(r)) {
                used.entry(r).or_default().push((b, i));
            }
            for d in defined(inst).into_iter().filter(|d| tracked.contains(d)) {
                defs.entry(d).or_default().push((b, i));
            }
        }
        in_term.extend(term_uses(&block.term));
    }
    let inst = |(b, i): (usize, usize)| &f.blocks[b].insts[i];

    // The values whose uses allow it, with the blocks they are taken apart in.
    let mut candidates: HashMap<Reg, Vec<usize>> = HashMap::new();
    'values: for (&r, uses) in &used {
        if (r.0 < params && !owned_params.contains(&r)) || in_term.contains(&r) {
            continue;
        }
        // Per block: the variant read, the owning parts read, the reads.
        #[derive(Default)]
        struct InBlock {
            variant: Option<u32>,
            owning_parts: HashSet<Part>,
            reads: HashSet<Reg>,
        }
        let mut blocks: HashMap<usize, InBlock> = HashMap::new();
        for &at in uses {
            let read = match (inst(at), part_read(inst(at))) {
                (Inst::Tag { .. }, _) => continue,
                (_, Some((dst, src, part))) if src == r && dst != r => (dst, part),
                _ => continue 'values,
            };
            let here = blocks.entry(at.0).or_default();
            if !here.reads.insert(read.0) {
                continue 'values;
            }
            if let Part::Payload { variant: v, .. } = read.1 {
                if here.variant.is_some_and(|w| w != v) {
                    continue 'values;
                }
                here.variant = Some(v);
            }
            if tracked.contains(&read.0) && !here.owning_parts.insert(read.1) {
                continue 'values;
            }
        }
        let mut taken_in = Vec::new();
        for (&b, here) in &blocks {
            if here.owning_parts.is_empty() {
                continue;
            }
            // Dead after its last use in the block, and not written in between.
            let positions: Vec<usize> = uses.iter().filter(|(ub, _)| *ub == b).map(|(_, i)| *i).collect();
            let (first, last) = (*positions.iter().min().unwrap(), *positions.iter().max().unwrap());
            let written = defs.get(&r).is_some_and(|ds| ds.iter().any(|&(db, i)| db == b && i > first && i <= last));
            if live_after[b][last].contains(&r) || written {
                continue 'values;
            }
            taken_in.push(b);
        }
        if !taken_in.is_empty() {
            taken_in.sort_unstable();
            candidates.insert(r, taken_in);
        }
    }
    // Each of them must be a value of the function's own: made fresh, or taken
    // out of another value taken apart.
    loop {
        let fresh = |r: &Reg| {
            let ds = defs.get(r).map_or(&[][..], |ds| &ds[..]);
            (r.0 < params || !ds.is_empty())
                && ds.iter().all(|&at| match inst(at) {
                    Inst::Copy { .. } | Inst::Load { .. } | Inst::Deref { .. } | Inst::Elem { .. } | Inst::GetPath { .. } => false,
                    i => match part_read(i) {
                        Some((_, src, _)) => candidates.get(&src).is_some_and(|bs| bs.contains(&at.0)),
                        None => true,
                    },
                })
        };
        let gone: Vec<Reg> = candidates.keys().filter(|r| !fresh(r)).copied().collect();
        if gone.is_empty() {
            break;
        }
        for r in gone {
            candidates.remove(&r);
        }
    }

    let mut taken = Takes { moves: HashSet::new(), rest: HashMap::new() };
    for (&r, &b) in candidates.iter().flat_map(|(r, bs)| bs.iter().map(move |b| (r, b))) {
        let mut parts = HashSet::new();
        let mut variant = None;
        let mut last = None;
        for &(ub, i) in &used[&r] {
            if ub != b {
                continue;
            }
            let at = inst((ub, i));
            let dst = defined(at)[0];
            if last.is_none_or(|(j, _)| i > j) {
                last = Some((i, dst));
            }
            if let Some((dst, _, part)) = part_read(at) {
                if let Part::Payload { variant: v, .. } = part {
                    variant = Some(v);
                }
                if tracked.contains(&dst) {
                    taken.moves.insert((b, dst, r));
                    parts.insert(part);
                }
            }
        }
        let all: Vec<(Part, Type)> = match (f.reg_type(r), variant) {
            (Type::Enum(name), Some(v)) => owning.variants[name][v as usize]
                .iter()
                .enumerate()
                .map(|(k, t)| (Part::Payload { variant: v, index: k as u32 }, t.clone()))
                .collect(),
            (Type::Struct(name), None) => {
                owning.fields[name].iter().enumerate().map(|(k, t)| (Part::Field(k as u32), t.clone())).collect()
            }
            _ => unreachable!("parts are read out of a struct or an enum"),
        };
        let rest = all.into_iter().filter(|(p, t)| !parts.contains(p) && owning.owns(t)).collect();
        let (_, dst) = last.unwrap();
        taken.rest.insert((b, dst, r), rest);
    }
    taken
}

/// Inserts clones and drops into `f`. The parameters at `given` are handed
/// over by the caller (in a consuming version); the others are borrowed.
fn function(f: &mut Function, cx: &mut Cx<'_>, given: &[usize]) {
    // What is decided here, for `check_ownership` (`owncheck.rs`).
    f.ownership = Some(Ownership { own: given.iter().map(|&i| i as u32).collect(), ..Default::default() });
    let owning = &mut *cx.owning;
    let mut tracked: HashSet<Reg> = (0..f.regs.len() as u32).map(Reg).filter(|&r| owning.owns(f.reg_type(r))).collect();
    if tracked.is_empty() {
        return;
    }
    // What `insert` worked out, including the parameters handed on to a
    // callee that clones them; a consuming or tail version is `given` them.
    let cloned = cx.cloning.get(&f.name).cloned().unwrap_or_default();
    entry_without_predecessors(f);
    // Parameters handed over, or copied on entry, are the function's own.
    let owned_params: HashSet<Reg> =
        (0..f.params.len()).filter(|i| given.contains(i) || cloned.contains(i)).map(|i| Reg(i as u32)).collect();
    let taken = takes(f, &tracked, owning, &owned_params);

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
    // A parameter handed over that is never read dies at once (added last,
    // so that the drop is not taken for a use that needs one after it).
    let unread: Vec<Inst> = given
        .iter()
        .map(|&i| Reg(i as u32))
        .filter(|p| tracked.contains(p) && !live_at_entry.contains(p))
        .map(|src| Inst::Drop { src })
        .collect();

    // Parts read out of values are views where that cannot be told apart from
    // a copy; the others become copies right after the read.
    //
    // The read goes to a view of its own, which the copy is made from, so the
    // value read from stays alive until the copy is made.
    let mut roots = views(f, &tracked, &taken.moves);
    let mut new_regs = Vec::new();
    let mut next = f.regs.len() as u32;
    for (k, b) in f.blocks.iter_mut().enumerate() {
        let mut insts = Vec::with_capacity(b.insts.len());
        for mut inst in std::mem::take(&mut b.insts) {
            // What a cell holds can change through any other holder of the
            // cell, so a part read out of it is always a copy.
            let read_from_cell = match &inst {
                Inst::CellGet { dst, cell, .. } => Some((*dst, *cell)),
                _ => None,
            };
            let copy = projection(&inst)
                .filter(|&(d, s)| tracked.contains(&d) && !roots.contains_key(&d) && !taken.moves.contains(&(k, d, s)))
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
    let mut decided = Ownership { own: given.iter().map(|&i| i as u32).collect(), ..Default::default() };
    decided.views = roots.keys().copied().collect();
    decided.views.sort_by_key(|r| r.0);
    decided.moves = taken.moves.iter().map(|&(_, d, _)| d).collect();

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
            // The values it reads, through views too, before clones replace any.
            let read: HashSet<Reg> = uses(&inst).iter().flat_map(|u| [*u].into_iter().chain(roots.get(u).copied())).collect();
            let mut moved = HashSet::new();
            let mut clones = Vec::new();
            // A call to a function that clones parameters on entry, with
            // values of our own at their last use for any of them, hands those
            // over to its consuming version instead. The others it copies
            // itself, as the function would have: so `m = maps.set(m, k, v)`
            // copies the key it cannot give, not the map.
            if let Inst::Call { func, args, .. } = &mut inst {
                if let Some(params) = cx.cloning.get(func.as_str()) {
                    let gives = |a: Reg| {
                        owned.contains(&a)
                            && !live_after.contains(&a)
                            && args.iter().filter(|&&x| x == a).count() == 1
                            && !roots.values().any(|r| *r == a && live_after.iter().any(|v| roots.get(v) == Some(&a)))
                    };
                    let given: Vec<bool> = params.iter().map(|&i| gives(args[i])).collect();
                    if given.iter().any(|&g| g) {
                        cx.wanted.insert(func.clone());
                        *func = consuming(func);
                        for (&i, &g) in params.iter().zip(&given) {
                            if g {
                                moved.insert(args[i]);
                            } else {
                                let t = Reg(f.regs.len() as u32);
                                f.regs.push(f.regs[args[i].0 as usize].clone());
                                clones.push(Inst::Clone { dst: t, src: args[i] });
                                args[i] = t;
                            }
                        }
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
            let dst = defined(&inst).first().copied();
            insts.extend(clones);
            // A register written with a value made from its old one (`v =
            // v.items[k]`, read through a view, or `xs = g(xs)` borrowing it)
            // is live after, but its old value dies here: it moves to a
            // register of its own first, which is freed after. (Not `%p =
            // clone %p`, which makes a borrowed value or a view one's own.)
            let mut replaced = None;
            let keeps_old = matches!(inst, Inst::Clone { .. });
            if let Some(d) = dst.filter(|&d| owned.contains(&d) && !moved.contains(&d) && !changes(&inst, d) && !keeps_old) {
                let only_dst = defined(&inst) == [d] && inst.dst() == Some(d);
                if read.contains(&d) && only_dst {
                    let t = Reg(f.regs.len() as u32);
                    f.regs.push(f.regs[d.0 as usize].clone());
                    insts.push(Inst::Copy { dst: t, src: d });
                    for r in inst.regs_mut().into_iter().skip(1) {
                        if *r == d {
                            *r = t;
                        }
                    }
                    replaced = Some(t);
                }
            }
            insts.push(inst);
            if let Some(t) = replaced {
                insts.push(Inst::Drop { src: t });
            }
            for src in dying {
                // A value taken apart frees only the parts left in it.
                match dst.and_then(|d| taken.rest.get(&(b, d, src))) {
                    Some(parts) => {
                        for (part, ty) in parts {
                            let t = Reg(f.regs.len() as u32);
                            f.regs.push(ty.clone());
                            decided.moves.push(t);
                            insts.push(part.read(t, src));
                            insts.push(Inst::Drop { src: t });
                        }
                    }
                    None => insts.push(Inst::Drop { src }),
                }
            }
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
        // A tail call gives its arguments away, as `ret` does its value, to
        // the callee's version that takes them over.
        if let Terminator::TailCall { func, args } = &mut term {
            let mut dying = Vec::new();
            let mut moved = HashSet::new();
            for a in args.iter_mut() {
                if !tracked.contains(a) {
                    continue;
                }
                if let Some(&root) = roots.get(a) {
                    if owned.contains(&root) && !dying.contains(&root) {
                        dying.push(root);
                    }
                }
                if borrowed_params.contains(a) || !moved.insert(*a) {
                    let t = Reg(f.regs.len() as u32);
                    f.regs.push(f.regs[a.0 as usize].clone());
                    insts.push(Inst::Clone { dst: t, src: *a });
                    *a = t;
                }
            }
            dying.retain(|x| !moved.contains(x));
            dying.sort_by_key(|x| x.0);
            insts.extend(dying.into_iter().map(|src| Inst::Drop { src }));
            if cx.owning_params.contains_key(func.as_str()) {
                cx.tails.insert(func.clone());
                *func = tail_version(func);
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
    f.blocks[0].insts.splice(0..0, unread);
    f.ownership = Some(decided);
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
