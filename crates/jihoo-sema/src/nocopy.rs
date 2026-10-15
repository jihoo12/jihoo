//! `nocopy`: a function that promises, natively, to copy nothing.
//!
//! ```jihoo
//! fn grow(xs: [i64], x: i64) -> [i64] nocopy {
//!     push(xs, x)
//!     return xs
//! }
//! ```
//!
//! Native programs have no GC: the ownership pass (`jihoo_ir::ownership`)
//! moves values at their last use and copies them where the original is still
//! used, which `jihoo build --cost` lists. `nocopy` turns that list into a
//! rule: after the pass, the function and every function it can call, through
//! the calls the pass chose, must hold no `clone` that copies memory (a list, a
//! string, a closure's captured values). Otherwise the copy is a compile error
//! that says where it is, why it is there, and through which calls the
//! function reaches it.
//!
//! The function is taken as called with arguments handed over: when it has a
//! consuming version (`f.own`), that is the one checked. A caller that keeps
//! using an argument pays for its copy, and its own `--cost` shows it.
//!
//! Calls of function values are followed too. When no parameter of the
//! function can hold a function value, every function value it meets was made
//! by it or by what it calls, so the functions those name are the ones such
//! calls can reach: they are checked as if called (an iterator's closure, a
//! `go` statement's function). Otherwise a call of a function value whose
//! function is not known in the same function is an error: the compiler
//! cannot see what the caller passed.

use std::collections::{HashMap, HashSet, VecDeque};

use jihoo_ir::{cost, Function, Inst, Module, Reg, Terminator, Type};
use jihoo_syntax::{Error, Pos};

use crate::env::Env;

/// A source position, `(file, line, col)`, if known.
type Loc = Option<(u32, u32, u32)>;

/// How each function was reached: its caller, and where in the root the chain
/// of calls leading to it starts.
type Reached = HashMap<String, (String, Loc)>;

/// Why a function reached from a `nocopy` one breaks the promise.
enum Breach {
    Copy(cost::Copy),
    /// A call of a function value, at that position.
    Indirect(Loc),
}

impl Env<'_> {
    /// Checks every `nocopy` function of `m`, after the ownership pass.
    pub(crate) fn check_nocopy(&self, m: &Module) -> Vec<Error> {
        let by_name: HashMap<&str, &Function> = m.funcs.iter().map(|f| (f.name.as_str(), f)).collect();
        let cx = Cx { m, by_name };
        let mut errors = Vec::new();
        for f in m.funcs.iter().filter(|f| !f.name.ends_with(".own")) {
            let Some((decl, _)) = self.decl_of(&f.name) else { continue };
            let Some(at) = decl.nocopy else { continue };
            let own = format!("{}.own", f.name);
            let root = if cx.by_name.contains_key(own.as_str()) { own } else { f.name.clone() };
            if let Some(e) = self.nocopy_breach(&cx, &root, &decl.name, at) {
                errors.push(e);
            }
        }
        errors
    }

    /// The first copy that `root`, the function `name` declared `nocopy` at
    /// `at`, or a function it calls makes, nearest calls first.
    fn nocopy_breach(&self, cx: &Cx, root: &str, name: &str, at: Pos) -> Option<Error> {
        // Whether every function value it meets is made inside.
        let closed = cx.by_name.get(root).is_some_and(|f| !f.params.iter().any(|t| cx.may_hold_fn(t, &mut HashSet::new())));
        let mut from: Reached = HashMap::new();
        let mut seen: HashSet<String> = HashSet::from([root.to_string()]);
        let mut queue = VecDeque::from([root.to_string()]);
        while let Some(g) = queue.pop_front() {
            let Some(f) = cx.by_name.get(g.as_str()) else { continue };
            // Without a consuming version, the root copies the parameters a
            // caller keeps on entry: the caller's copy, not the function's.
            let ours = |c: &cost::Copy| !(g == root && c.on_entry);
            let breach = cost::in_function(f).into_iter().find(ours).map(Breach::Copy);
            let (callees, indirect) = calls(f, closed);
            let breach = breach.or(indirect.map(Breach::Indirect));
            if let Some(b) = breach {
                return Some(self.nocopy_error(root, name, at, &g, &from, b));
            }
            for (callee, loc) in callees {
                if seen.insert(callee.clone()) {
                    let start = if g == root { loc } else { from[&g].1 };
                    from.insert(callee.clone(), (g.clone(), start));
                    queue.push_back(callee);
                }
            }
        }
        None
    }

    fn nocopy_error(
        &self,
        root: &str,
        name: &str,
        at: Pos,
        g: &str,
        from: &Reached,
        breach: Breach,
    ) -> Error {
        let pos_of = |loc: Loc| loc.map_or(at, |(file, line, col)| Pos { file: file as u16, line, col });
        // The name it was written with: without the ownership pass's
        // versions (`f.own`, `f.tail`) and an instance's number.
        let user = |jir: &str| {
            let mut base = jir;
            while let Some(b) = base.strip_suffix(".own").or_else(|| base.strip_suffix(".tail")) {
                base = b;
            }
            self.decl_of(base).map_or(base.to_string(), |(d, _)| d.name.clone())
        };
        // The calls from `root` to `g`.
        let mut chain = vec![g.to_string()];
        while let Some((caller, _)) = from.get(chain.last().unwrap()) {
            chain.push(caller.clone());
        }
        chain.reverse();
        let (what, loc) = match &breach {
            Breach::Copy(c) => (format!("copies {}", c.ty), c.loc),
            Breach::Indirect(loc) => ("calls a function value, whose copies the compiler cannot see".to_string(), *loc),
        };
        let why = match &breach {
            Breach::Copy(c) => format!(": {}", c.why),
            Breach::Indirect(_) => "; call a function by its name, or take it as a `comptime` parameter".to_string(),
        };
        let line = loc.map_or(String::new(), |(_, line, _)| format!(", line {line}"));
        let msg = match chain.len() {
            1 => format!("`{name}` is `nocopy`, but it {what} here{why}"),
            2 => format!("`{name}` is `nocopy`, but this call of `{}` {what} (in `{}`{line}){why}", user(g), user(g)),
            _ => {
                let through: Vec<String> = chain[1..].iter().map(|f| format!("`{}`", user(f))).collect();
                format!(
                    "`{name}` is `nocopy`, but this call of `{}` {what} (in `{}`{line}, through {}){why}",
                    user(&chain[1]),
                    user(g),
                    through.join(" -> ")
                )
            }
        };
        let pos = if g == root { pos_of(loc) } else { pos_of(from[&chain[1]].1) };
        Error::new(pos, jihoo_ir::plain_names(&msg))
    }
}

struct Cx<'m> {
    m: &'m Module,
    by_name: HashMap<&'m str, &'m Function>,
}

impl Cx<'_> {
    /// Whether a value of type `t` can hold a function value. `seen` breaks
    /// cycles through recursive types.
    fn may_hold_fn(&self, t: &Type, seen: &mut HashSet<String>) -> bool {
        match t {
            // A pointer may point at anything.
            Type::Fn(..) | Type::Ptr(_) => true,
            Type::List(e) | Type::Array(e, _) | Type::Ref(e) | Type::Cell(e) | Type::Chan(e) => self.may_hold_fn(e, seen),
            Type::Struct(n) | Type::Enum(n) if !seen.insert(n.clone()) => false,
            Type::Struct(n) => {
                let Some(s) = self.m.structs.iter().find(|s| s.name == *n) else { return true };
                s.fields.iter().any(|(_, t)| self.may_hold_fn(t, seen))
            }
            Type::Enum(n) => {
                let Some(e) = self.m.enums.iter().find(|e| e.name == *n) else { return true };
                e.variants.iter().flat_map(|(_, ts)| ts).any(|t| self.may_hold_fn(t, seen))
            }
            _ => false,
        }
    }
}

/// The functions `f` calls by name, each with where it first does, and where
/// it first calls a function value whose function it does not know, if it
/// does. When `closed`, the functions it makes values of count as called.
fn calls(f: &Function, closed: bool) -> (Vec<(String, Loc)>, Option<Loc>) {
    // Registers that only ever hold one known function: `go g(x)` and
    // `let h = g` followed by `h(x)` call `g`.
    enum Write {
        Func(String),
        CopyOf(Reg),
        Other,
    }
    let mut writes: HashMap<Reg, Vec<Write>> = HashMap::new();
    for p in 0..f.params.len() as u32 {
        writes.entry(Reg(p)).or_default().push(Write::Other);
    }
    for inst in f.blocks.iter().flat_map(|b| &b.insts) {
        let Some(dst) = inst.dst() else { continue };
        let w = match inst {
            Inst::FuncRef { func, .. } | Inst::Closure { func, .. } => Write::Func(func.clone()),
            Inst::Copy { src, .. } => Write::CopyOf(*src),
            _ => Write::Other,
        };
        writes.entry(dst).or_default().push(w);
    }
    fn known(writes: &HashMap<Reg, Vec<Write>>, r: Reg, depth: u32) -> Option<String> {
        let mut found: Option<String> = None;
        for w in writes.get(&r)? {
            let func = match w {
                Write::Func(func) => func.clone(),
                Write::CopyOf(src) if depth < 16 => known(writes, *src, depth + 1)?,
                _ => return None,
            };
            if found.as_ref().is_some_and(|f| *f != func) {
                return None;
            }
            found = Some(func);
        }
        found
    }
    let mut out: Vec<(String, Loc)> = Vec::new();
    let mut indirect = None;
    let mut add = |name: &str, loc| {
        if !out.iter().any(|(n, _)| n == name) {
            out.push((name.to_string(), loc));
        }
    };
    for (bi, b) in f.blocks.iter().enumerate() {
        for (i, inst) in b.insts.iter().enumerate() {
            match inst {
                Inst::Call { func, .. } => add(func, cost::loc_at(f, bi, i)),
                Inst::FuncRef { func, .. } | Inst::Closure { func, .. } if closed => add(func, cost::loc_at(f, bi, i)),
                Inst::CallIndirect { .. } | Inst::Spawn { .. } if closed => {}
                Inst::CallIndirect { callee, .. } | Inst::Spawn { callee, .. } => match known(&writes, *callee, 0) {
                    Some(func) => add(&func, cost::loc_at(f, bi, i)),
                    None => {
                        indirect.get_or_insert(cost::loc_at(f, bi, i));
                    }
                },
                _ => {}
            }
        }
        if let Terminator::TailCall { func, .. } = &b.term {
            add(func, cost::loc_at(f, bi, b.insts.len()));
        }
    }
    (out, indirect)
}
