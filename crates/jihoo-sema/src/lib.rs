//! Semantic analysis: type checks the AST and lowers it to JIR in one pass.
//!
//! Types: function signatures are written out, local variable types are inferred
//! from their initializers. Integer literals take their type from context
//! (`let x: u8 = 1`, `p[i] == 0`), defaulting to `i64`. The operator typing rules
//! are shared with the IR verifier (`jihoo_ir::types`), so well-typed programs
//! always produce valid IR.
//!
//! Module-level items are analyzed lazily (see `env.rs`), which is what lets
//! `comptime` code call any function (see `comptime.rs`).
//!
//! Errors are collected per item: one error stops the current function, but the
//! remaining items are still checked.

mod asm;
mod atomics;
mod closures;
mod comptime;
mod enums;
mod equality;
mod env;
mod floats;
mod generic;
mod hygiene;
mod infer;
mod loops;
mod macros;
mod paths;
mod patterns;
mod place;
mod tail_calls;
mod tasks;
mod traits;
mod tuples;

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use jihoo_ir as ir;
use jihoo_ir::types;
use jihoo_ir::{BlockId, FloatTy, Inst, IntTy, Profile, Reg, Terminator, Type};
use jihoo_syntax::ast::*;
use jihoo_syntax::loader::Module;
use jihoo_syntax::{Error, Pos};

use comptime::ConstValue;
use env::{Env, Sig};
use generic::{Binding, Bindings};

/// Checks and lowers a single-file program.
pub fn analyze(prog: &Program) -> Result<ir::Module, Vec<Error>> {
    let root = Module { name: String::new(), file: 0, program: prog.clone(), imports: HashMap::new() };
    analyze_modules(&[root, jihoo_syntax::loader::prelude(1)])
}

/// What to put into the JIR beyond the program itself.
#[derive(Clone, Copy, Default)]
pub struct Options {
    /// Source positions (`loc`), whose files are numbered as `Pos::file`;
    /// the module's `files` are left for the caller to name.
    pub locs: bool,
}

/// Checks and lowers a program made of modules (see `jihoo_syntax::loader`);
/// `mods[0]` is the root, whose attributes choose the profile.
pub fn analyze_modules(mods: &[Module]) -> Result<ir::Module, Vec<Error>> {
    analyze_modules_with(mods, Options::default())
}

/// `analyze_modules`, with `opts`.
pub fn analyze_modules_with(mods: &[Module], opts: Options) -> Result<ir::Module, Vec<Error>> {
    let mut errors = Vec::new();

    // Without an attribute, a program is native.
    let mut profile: Option<Profile> = None;
    for (pos, attr) in &mods[0].program.attrs {
        let chosen = match attr.as_str() {
            "hosted" => Profile::Hosted,
            "native" => Profile::Native,
            "freestanding" => Profile::Freestanding,
            "comptime" => {
                errors.push(Error::new(*pos, "`#![comptime]` is for libraries that macros use; a program runs"));
                continue;
            }
            other => {
                errors.push(Error::new(*pos, format!("unknown attribute `#![{other}]`")));
                continue;
            }
        };
        if let Some(p) = profile {
            let msg = format!("the program is already {}; it cannot also be {}", p.as_str(), chosen.as_str());
            errors.push(Error::new(*pos, msg));
        }
        profile = Some(chosen);
    }
    let profile = profile.unwrap_or(Profile::Native);
    // A library may say what it needs; it cannot choose the mode. A
    // freestanding library (no GC, no libc) also works in native programs; a
    // native one needs libc; a hosted one needs the VM.
    for m in &mods[1..] {
        for (pos, attr) in &m.program.attrs {
            let ok = match attr.as_str() {
                // Only used while compiling, in any program.
                "comptime" => true,
                "hosted" => profile == Profile::Hosted,
                "freestanding" => profile.is_compiled(),
                "native" => profile == Profile::Native,
                other => {
                    errors.push(Error::new(*pos, format!("unknown attribute `#![{other}]`")));
                    continue;
                }
            };
            if !ok {
                let msg = format!("module `{}` is {attr}-only, but the program is {}", m.name, profile.as_str());
                errors.push(Error::new(*pos, msg));
            }
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    let expanded = expand_item_macros(profile, mods)?;
    let mods = &expanded[..];

    let (mut env, name_errors) = Env::new(profile, mods);
    env.locs = opts.locs;
    let env = env;
    errors.extend(name_errors);
    if !errors.is_empty() {
        return Err(errors);
    }
    errors.extend(env.check_traits());

    // Ask for every item; the lazy queries compute what each one needs.
    // Generic structs and enums only exist as instances, collected at the end.
    let mut structs = Vec::new();
    let mut enums = Vec::new();
    let mut define = |t: Type, pos: Pos, errors: &mut Vec<Error>| {
        // Types that hold code, such as `meta.Node`, only exist while compiling.
        let r = env.check_acyclic(&t).and_then(|_| match t {
            _ if env.holds_code(&t) => Ok(()),
            Type::Enum(name) => {
                let variants = env.enum_variants(pos, &name)?;
                enums.push(ir::EnumDef { name, variants: (*variants).clone() });
                Ok(())
            }
            Type::Struct(name) => {
                let fields = env.struct_fields(pos, &name)?;
                structs.push(ir::StructDef { name, fields: (*fields).clone() });
                Ok(())
            }
            _ => unreachable!(),
        });
        if let Err(e) = r {
            errors.push(e);
        }
    };
    for (m, module) in mods.iter().enumerate() {
        for s in module.program.structs.iter().filter(|s| s.params.is_empty()) {
            let key = env.key(m, &s.name).unwrap();
            // The prelude's types are only defined when used, at the end.
            if env.in_prelude(&key) {
                continue;
            }
            let t = if s.is_enum() { Type::Enum(key) } else { Type::Struct(key) };
            define(t, s.pos, &mut errors);
        }
        for c in &module.program.consts {
            if let Some(Err(e)) = env.constant(&env.key(m, &c.name).unwrap()) {
                errors.push(e);
            }
        }
    }
    // Generic functions are compiled per instance; macros and functions on code
    // only run while compiling.
    let mut plain = Vec::new();
    let mut macros = Vec::new();
    let mut externs: Vec<(Pos, ir::ExternFn)> = Vec::new();
    for (m, module) in mods.iter().enumerate() {
        for f in &module.program.funcs {
            let key = env.key(m, &f.name).unwrap();
            if f.is_extern {
                match env.signature(&key) {
                    Some(Ok(sig)) => {
                        let (params, ret) = (sig.params.clone(), sig.ret.clone());
                        let e = ir::ExternFn { name: f.name.clone(), params, ret, variadic: f.variadic };
                        // Several modules may declare the same C function, the same way.
                        match externs.iter().find(|(_, x)| x.name == e.name) {
                            Some((_, x)) if *x == e => {}
                            Some(_) => {
                                let msg = format!("C function `{}` is declared elsewhere with a different signature", f.name);
                                errors.push(Error::new(f.pos, msg));
                            }
                            None => externs.push((f.pos, e)),
                        }
                    }
                    Some(Err(e)) => errors.push(e),
                    None => {}
                }
            } else if env.in_prelude(&key) {
                // The prelude's functions are compiled when used (`Env::ir_name`).
            } else if (f.is_macro || env.is_code_fn(&key)) && env.generic(&key).is_none() {
                macros.push(key);
            } else if env.generic(&key).is_none() {
                plain.push(key);
            }
        }
    }
    for key in &plain {
        if let Some(Err(e)) = env.signature(key) {
            errors.push(e);
        }
    }
    if let Err(e) = env.check_entry(&mods[0].program) {
        errors.push(e);
    }
    let mut funcs = Vec::new();
    for key in &plain {
        if let Some(Ok(_)) = env.signature(key) {
            match env.function(key) {
                Ok(func) => funcs.push((*func).clone()),
                Err(e) => errors.push(e),
            }
        }
    }
    // Macros and functions on code are checked even when unused, but are not
    // part of the program.
    for key in &macros {
        match env.signature(key) {
            Some(Ok(_)) => {
                if let Err(e) = env.function(key) {
                    errors.push(e);
                }
            }
            Some(Err(e)) => errors.push(e),
            None => {}
        }
    }
    // Compiling functions asks for generic instances, which may ask for more.
    while let Some(name) = env.next_pending() {
        match env.function(&name) {
            Ok(func) => funcs.push((*func).clone()),
            Err(e) => errors.push(e),
        }
    }
    funcs.extend(env.lambdas());

    // The types sema makes (generic instances, tuples, paths) are part of the
    // program only when its code or its declared types reach them: compile-time
    // code may make types the program cannot have, such as `(str, i64)` in a
    // macro of a freestanding program.
    let declared = mods.iter().enumerate().flat_map(|(m, module)| {
        let env = &env;
        module.program.structs.iter().filter(|s| s.params.is_empty()).map(move |s| {
            let key = env.key(m, &s.name).unwrap();
            if s.is_enum() { Type::Enum(key) } else { Type::Struct(key) }
        })
    });
    let used_prelude = env.used_prelude_types();
    let in_code = funcs.iter().flat_map(|f| f.regs.iter().chain([&f.ret]).cloned());
    let in_externs = externs.iter().flat_map(|(_, e)| e.params.iter().chain([&e.ret]).cloned());
    // Types that hold code only exist while compiling (see `define`).
    let declared = declared.filter(|t| !env.holds_code(t));
    let reached = reached_types(&env, declared.chain(used_prelude.iter().cloned()).chain(in_code).chain(in_externs).collect());
    let reaches = |t: &Type| matches!(t, Type::Struct(n) | Type::Enum(n) if reached.contains(n));

    for t in used_prelude {
        define(t, Pos::new(1, 1), &mut errors);
    }
    for t in env.struct_instances().into_iter().filter(reaches) {
        define(t, Pos::new(1, 1), &mut errors);
    }
    for t in env.tuple_types().into_iter().filter(reaches) {
        define(t, Pos::new(1, 1), &mut errors);
    }
    for t in env.path_types().into_iter().filter(reaches) {
        let def = env.path_def(&t).unwrap();
        match env.path_steps(def.pos, &def) {
            Ok(_) => define(t, def.pos, &mut errors),
            Err(e) => errors.push(e),
        }
    }

    // Only the C functions the program uses are part of it, so declaring a
    // library of them (`lib/libc.jh`) costs nothing and takes no names.
    externs.retain(|(_, e)| env.extern_used(&e.name));
    // Exported functions are C symbols too, one definition each.
    let mut exported: HashMap<&str, Pos> = HashMap::new();
    for f in mods.iter().flat_map(|m| &m.program.funcs).filter(|f| f.is_export) {
        if exported.insert(&f.name, f.pos).is_some() {
            errors.push(Error::new(f.pos, format!("C symbol `{}` is exported twice", f.name)));
        }
        if externs.iter().any(|(_, e)| e.name == f.name) {
            let msg = format!("`{}` is exported here and declared as an extern C function the program calls", f.name);
            errors.push(Error::new(f.pos, msg));
        }
    }
    // A C symbol and a jihoo function would share one JIR name (an exported
    // one is reported above).
    for (pos, e) in &externs {
        if funcs.iter().any(|f| f.name == e.name) && !exported.contains_key(e.name.as_str()) {
            let msg = format!("C function `{}` has the same name as the function `{}`; rename the function", e.name, e.name);
            errors.push(Error::new(*pos, msg));
        }
    }
    let externs = externs.into_iter().map(|(_, e)| e).collect();

    if errors.is_empty() {
        let files = match opts.locs {
            true => vec![String::new(); mods.iter().map(|m| m.file as usize + 1).max().unwrap_or(0)],
            false => vec![],
        };
        let mut m = ir::Module { profile, target: ir::Target::host(), structs, enums, externs, funcs, files };
        // Native lists have no GC: decide where values move, are copied and are freed.
        if profile == Profile::Native {
            ir::ownership::insert(&mut m);
        }
        Ok(m)
    } else {
        // One failing item can surface as the same error through several others.
        let mut seen = std::collections::HashSet::new();
        errors.retain(|e| seen.insert((e.pos, e.msg.clone())));
        Err(errors)
    }
}

/// The names of the structs and enums that `seeds` mention, and of those
/// that their parts mention in turn.
fn reached_types(env: &Env, mut work: Vec<Type>) -> HashSet<String> {
    let mut seen = HashSet::new();
    while let Some(t) = work.pop() {
        match t {
            Type::Struct(n) if seen.insert(n.clone()) => {
                let fields = env.tuple_named(&n).or_else(|| env.struct_fields(Pos::default(), &n).ok());
                work.extend(fields.iter().flat_map(|fs| fs.iter().map(|(_, t)| t.clone())));
            }
            Type::Enum(n) if seen.insert(n.clone()) => {
                if let Ok(variants) = env.enum_variants(Pos::default(), &n) {
                    work.extend(variants.iter().flat_map(|(_, ts)| ts.iter().cloned()));
                }
            }
            Type::Ptr(t) | Type::Array(t, _) | Type::List(t) | Type::Ref(t) | Type::Chan(t) | Type::Cell(t) => work.push(*t),
            Type::Fn(ps, r) => {
                work.extend(ps);
                work.push(*r);
            }
            _ => {}
        }
    }
    seen
}

/// Bound on rounds of item macro expansion (an expansion may call item macros).
const MAX_ITEM_ROUNDS: usize = 16;

/// Replaces every top-level `name!(...)` with the items the macro returns.
///
/// Each round analyzes the program as it is (lazily, so only what the macros
/// need), runs every item macro, and adds the produced items to the calling
/// module. Produced items may call item macros themselves, which the next round
/// expands.
fn expand_item_macros(profile: Profile, mods: &[Module]) -> Result<Vec<Module>, Vec<Error>> {
    let mut mods = mods.to_vec();
    for _ in 0..MAX_ITEM_ROUNDS {
        if mods.iter().all(|m| m.program.macro_calls.is_empty()) {
            return Ok(mods);
        }
        let mut produced = Vec::new();
        let mut errors = Vec::new();
        {
            let (env, name_errors) = Env::new(profile, &mods);
            if !name_errors.is_empty() {
                return Err(name_errors);
            }
            for (m, module) in mods.iter().enumerate() {
                for call in &module.program.macro_calls {
                    let sig = Rc::new(Sig { params: vec![], ret: Type::Unit });
                    let mut cx = FnCx::new(&env, sig, env.root(m).clone());
                    let items = cx.expand_code(call.pos, &call.name, &call.args).and_then(|(kind, code, expansion)| {
                        if kind != Type::Items {
                            let msg = format!("`{}!` produces {kind}; only `items` macros can be used at the top level", call.name);
                            return Err(Error::new(call.pos, msg));
                        }
                        let mut p = jihoo_syntax::parse_items(&code).map_err(|e| {
                            Error::new(call.pos, format!("`{}!` produced code that does not parse: {} in `{code}`", call.name, e.msg))
                        })?;
                        expansion.resolve(&env, hygiene::Code::Items(&mut p), &|_| false);
                        set_program_pos(&mut p, call.pos);
                        Ok(p)
                    });
                    match items {
                        Ok(p) => produced.push((m, p)),
                        Err(e) => errors.push(e),
                    }
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        for m in &mut mods {
            m.program.macro_calls.clear();
        }
        for (m, p) in produced {
            let prog = &mut mods[m].program;
            prog.structs.extend(p.structs);
            prog.consts.extend(p.consts);
            prog.funcs.extend(p.funcs);
            prog.traits.extend(p.traits);
            prog.impls.extend(p.impls);
            prog.macro_calls.extend(p.macro_calls);
        }
    }
    let pos = mods.iter().flat_map(|m| &m.program.macro_calls).map(|c| c.pos).next().unwrap_or_default();
    Err(vec![Error::new(pos, format!("item macros still produce item macros after {MAX_ITEM_ROUNDS} rounds"))])
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "print"
            | "syscall"
            | "len"
            | "size_of"
            | "align_of"
            | "stringify"
            | "code_text"
            | "to_code"
            | "type_of"
            | "type_shape"
            | "to_items"
            | "to_str"
            | "unique"
            | "ident"
            | "path_index"
    )
}

/// Builtins whose names a module may take: a module that defines a function
/// of that name calls its own (as `alloc.push` is). Floats, atomics, `trap`,
/// and the hosted builtins for lists, input and channels.
pub(crate) fn is_takeable_builtin(name: &str) -> bool {
    floats::is_float_builtin(name)
        || atomics::is_atomic_builtin(name)
        || matches!(name, "trap" | "read_line" | "push" | "pop" | "from_bytes" | "send" | "recv")
}

/// What `FnCx::save` keeps.
struct Saved {
    blocks: Vec<BlockBuf>,
    cur: BlockId,
    regs: usize,
    scopes: Vec<HashMap<String, Reg>>,
    const_regs: HashMap<String, Reg>,
    hoisted: usize,
    head: Option<BlockId>,
    captured: HashSet<Reg>,
    closure_regs: HashMap<String, Vec<Reg>>,
    loops: Vec<Loop>,
}

#[derive(Clone)]
struct BlockBuf {
    insts: Vec<Inst>,
    term: Option<Terminator>,
}

struct FnCx<'a> {
    env: &'a Env<'a>,
    sig: Rc<Sig>,
    /// Comptime parameters of the instance being compiled.
    bindings: Rc<Bindings>,
    /// Lowering a `comptime` helper: there are no local variables to refer to.
    in_comptime: bool,
    /// Lowering a macro body: `quote`, `expr` and `str` are available.
    in_macro: bool,
    /// How many macro expansions we are inside of.
    macro_depth: u32,
    /// Registers holding constants already built at the start of the function.
    const_regs: HashMap<String, Reg>,
    /// Number of hoisted instructions at the front of the entry block.
    hoisted: usize,
    blocks: Vec<BlockBuf>,
    /// The JIR name of the function being lowered, whose calls of itself in
    /// tail position become jumps (`tail_calls.rs`); `None` for helpers.
    name: Option<String>,
    /// Where those jumps go, once there is one.
    head: Option<BlockId>,
    cur: BlockId,
    regs: Vec<Type>,
    scopes: Vec<HashMap<String, Reg>>,
    /// Registers holding captured values, in a closure body: they cannot be
    /// assigned.
    captured: HashSet<Reg>,
    /// For each closure this instance received as a comptime argument, the
    /// registers holding its captured values.
    closure_regs: HashMap<String, Vec<Reg>>,
    /// The `while` loops around the current statement, innermost last: where
    /// `continue` and `break` jump to.
    loops: Vec<Loop>,
    /// The position the last `loc` gave, and its block, with `Env::locs`.
    last_loc: Option<(BlockId, Pos)>,
}

#[derive(Clone, Copy)]
struct Loop {
    /// The block that tests the condition, where `continue` goes.
    next: BlockId,
    /// The block after the loop.
    end: BlockId,
}

/// The JIR operator for a binary operator other than `&&` and `||`, and how it
/// is written.
fn ir_binop(op: BinOp) -> (ir::BinOp, &'static str) {
    match op {
        BinOp::Add => (ir::BinOp::Add, "+"),
        BinOp::Sub => (ir::BinOp::Sub, "-"),
        BinOp::Mul => (ir::BinOp::Mul, "*"),
        BinOp::Div => (ir::BinOp::Div, "/"),
        BinOp::Rem => (ir::BinOp::Rem, "%"),
        BinOp::Eq => (ir::BinOp::Eq, "=="),
        BinOp::Ne => (ir::BinOp::Ne, "!="),
        BinOp::Lt => (ir::BinOp::Lt, "<"),
        BinOp::Le => (ir::BinOp::Le, "<="),
        BinOp::Gt => (ir::BinOp::Gt, ">"),
        BinOp::Ge => (ir::BinOp::Ge, ">="),
        BinOp::BitAnd => (ir::BinOp::And, "&"),
        BinOp::BitOr => (ir::BinOp::Or, "|"),
        BinOp::BitXor => (ir::BinOp::Xor, "^"),
        BinOp::Shl => (ir::BinOp::Shl, "<<"),
        BinOp::Shr => (ir::BinOp::Shr, ">>"),
        BinOp::And | BinOp::Or => unreachable!("`&&` and `||` short-circuit"),
    }
}

/// A number literal, possibly negated: its type comes from context.
#[derive(Clone, Copy, PartialEq)]
enum Literal {
    Int,
    Float,
}

fn number_literal(e: &Expr) -> Option<Literal> {
    match &e.kind {
        ExprKind::Int(_) => Some(Literal::Int),
        ExprKind::Float(_) => Some(Literal::Float),
        ExprKind::Unary(UnOp::Neg, inner) => match inner.kind {
            ExprKind::Int(_) | ExprKind::Float(_) => number_literal(inner),
            _ => None,
        },
        _ => None,
    }
}

impl<'a> FnCx<'a> {
    fn new(env: &'a Env<'a>, sig: Rc<Sig>, bindings: Rc<Bindings>) -> Self {
        FnCx {
            env,
            sig,
            bindings,
            in_comptime: false,
            in_macro: false,
            macro_depth: 0,
            const_regs: HashMap::new(),
            hoisted: 0,
            blocks: vec![BlockBuf { insts: vec![], term: None }],
            name: None,
            head: None,
            cur: BlockId(0),
            regs: Vec::new(),
            scopes: vec![HashMap::new()],
            captured: HashSet::new(),
            closure_regs: HashMap::new(),
            loops: Vec::new(),
            last_loc: None,
        }
    }

    /// With `Env::locs`, says that the code from here on was written at
    /// `pos`, unless the block says so already.
    fn note(&mut self, pos: Pos) {
        if !self.env.locs || pos.line == 0 || self.last_loc == Some((self.cur, pos)) {
            return;
        }
        self.last_loc = Some((self.cur, pos));
        self.emit(Inst::Loc { file: u32::from(pos.file), line: pos.line, col: pos.col });
    }

    fn profile(&self) -> Profile {
        self.env.profile
    }

    /// Lowers `f` as the JIR function `name` (an instance name for generics).
    fn lower_fn(mut self, f: &FnDecl, name: &str) -> Result<ir::Function, Error> {
        self.name = Some(name.to_string());
        self.note(f.pos);
        let sig = self.sig.clone();
        self.in_macro =
            f.is_compile_time() || self.env.sig_holds_code(&sig) || self.env.module_is_comptime(self.bindings.module);
        // Comptime parameters are bindings, not registers.
        for (p, ty) in f.params.iter().filter(|p| !p.comptime).zip(&sig.params) {
            let r = self.new_reg(ty.clone());
            if self.scopes[0].insert(p.name.clone(), r).is_some() {
                return Err(Error::new(p.pos, format!("duplicate parameter `{}`", p.name)));
            }
        }
        // Then the hidden parameters: the captured values of comptime closures.
        let bindings = self.bindings.clone();
        for (name, captures) in bindings.closures() {
            let regs = captures.iter().map(|t| self.new_reg(t.clone())).collect();
            self.closure_regs.insert(name.to_string(), regs);
        }

        self.block(&f.body)?;
        let mut func = self.finish(name, sig.ret.clone(), f.body.end)?;
        func.export = f.is_export.then(|| f.name.clone());
        Ok(func)
    }

    /// Closes open blocks, drops unreachable ones, and builds the function.
    /// `end` is where a missing `return` is reported.
    fn finish(mut self, name: &str, ret: Type, end: Pos) -> Result<ir::Function, Error> {
        // Code only exists while compiling, also inside a struct or an enum.
        if !self.in_macro {
            if let Some(t) = self.regs.iter().find(|t| self.env.holds_code(t)) {
                let msg = format!("`{name}` uses {t}, which holds code and so only exists while compiling");
                return Err(Error::new(end, ir::plain_names(&msg)));
            }
        }
        let reachable = self.reachable();
        // A reachable block that is still open falls off the end of the function.
        for &b in &reachable {
            if self.blocks[b].term.is_some() {
                continue;
            }
            if ret != Type::Unit {
                return Err(Error::new(end, format!("missing `return`: `{name}` must return {ret}")));
            }
            let unit = self.new_reg(Type::Unit);
            self.blocks[b].insts.push(Inst::Unit { dst: unit });
            self.blocks[b].term = Some(Terminator::Ret(unit));
        }

        // Drop unreachable blocks and renumber the rest.
        let mut new_id = vec![None; self.blocks.len()];
        for (i, &b) in reachable.iter().enumerate() {
            new_id[b] = Some(BlockId(i as u32));
        }
        let mut old: Vec<Option<BlockBuf>> = self.blocks.into_iter().map(Some).collect();
        let blocks = reachable
            .iter()
            .map(|&b| {
                let buf = old[b].take().unwrap();
                let mut term = buf.term.unwrap();
                for s in term.successors_mut() {
                    *s = new_id[s.0 as usize].unwrap();
                }
                ir::Block { insts: buf.insts, term }
            })
            .collect();

        Ok(ir::Function { name: name.to_string(), export: None, params: self.sig.params.clone(), ret, regs: self.regs, blocks })
    }

    /// Blocks reachable from the entry, in breadth-first order (entry first).
    fn reachable(&self) -> Vec<usize> {
        let mut seen = vec![false; self.blocks.len()];
        let mut order = Vec::new();
        let mut queue = VecDeque::from([0usize]);
        seen[0] = true;
        while let Some(b) = queue.pop_front() {
            order.push(b);
            if let Some(t) = &self.blocks[b].term {
                for s in t.successors() {
                    let s = s.0 as usize;
                    if !seen[s] {
                        seen[s] = true;
                        queue.push_back(s);
                    }
                }
            }
        }
        order
    }

    // ---- builder ----

    fn new_reg(&mut self, ty: Type) -> Reg {
        self.regs.push(ty);
        Reg(self.regs.len() as u32 - 1)
    }

    fn ty(&self, r: Reg) -> &Type {
        &self.regs[r.0 as usize]
    }

    fn new_block(&mut self) -> BlockId {
        self.blocks.push(BlockBuf { insts: vec![], term: None });
        BlockId(self.blocks.len() as u32 - 1)
    }

    fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
    }

    fn emit(&mut self, inst: Inst) {
        self.blocks[self.cur.0 as usize].insts.push(inst);
    }

    /// Allocates a register of type `ty` and emits `make(dst)` into it.
    fn emit_to(&mut self, ty: Type, make: impl FnOnce(Reg) -> Inst) -> Reg {
        let dst = self.new_reg(ty);
        self.emit(make(dst));
        dst
    }

    /// Terminates the current block. No-op if it already ended (e.g. after `return`).
    fn terminate(&mut self, t: Terminator) {
        let b = &mut self.blocks[self.cur.0 as usize];
        if b.term.is_none() {
            b.term = Some(t);
        }
    }

    fn konst(&mut self, ty: Type, value: i64) -> Reg {
        self.emit_to(ty, |dst| Inst::Const { dst, value })
    }

    /// `value`, rounded to `t`.
    pub(crate) fn fconst(&mut self, t: FloatTy, value: f64) -> Reg {
        let value = t.round(value);
        self.emit_to(Type::Float(t), |dst| Inst::FConst { dst, value })
    }

    /// A float literal: `f64`, or `f32` where that is expected.
    fn float_literal(&mut self, x: f64, expected: Option<&Type>) -> Reg {
        match expected {
            Some(&Type::Float(t)) => self.fconst(t, x),
            _ => self.fconst(FloatTy::F64, x),
        }
    }

    fn unit(&mut self) -> Reg {
        self.emit_to(Type::Unit, |dst| Inst::Unit { dst })
    }

    fn resolve(&self, t: &TypeExpr) -> Result<Type, Error> {
        self.env.resolve_in(t, &self.bindings, self.in_macro)
    }

    fn local(&self, name: &str) -> Option<Reg> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn lookup(&self, pos: Pos, name: &str) -> Result<Reg, Error> {
        self.local(name).ok_or_else(|| self.unknown_name(pos, name))
    }

    fn unknown_name(&self, pos: Pos, name: &str) -> Error {
        if self.in_comptime && !self.env.has_function(self.bindings.module, name) {
            Error::new(
                pos,
                format!("`{name}` is not known at compile time; `comptime` code can only use constants and functions"),
            )
        } else {
            Error::new(pos, format!("unknown variable `{name}`"))
        }
    }

    /// A variable: a local, a comptime parameter, a top-level constant, or a
    /// function used as a value. All but locals are spliced in as constants.
    fn var(&mut self, pos: Pos, name: &str) -> Result<Reg, Error> {
        if let Some(r) = self.local(name) {
            return Ok(r);
        }
        if let Some(&r) = self.const_regs.get(name) {
            return Ok(r);
        }
        match self.bindings.get(name).cloned() {
            Some(Binding::Value(ty, v)) => {
                let r = self.hoist(|cx| cx.splice(&ty, &v));
                self.const_regs.insert(name.to_string(), r);
                return Ok(r);
            }
            Some(Binding::Type(t)) => {
                return Err(Error::new(pos, format!("`{name}` is a type ({t}), not a value")));
            }
            // A closure used as a value, not called: make a closure value.
            Some(Binding::Closure { ty, func, .. }) => {
                let captures = self.closure_regs[name].clone();
                let what = format!("`{name}` was given a function that captures values");
                return self.closure_value(pos, &what, func, captures, ty);
            }
            None => {}
        }
        let key = self.env.key_or_err(pos, &self.bindings, name)?;
        match self.env.constant(&key) {
            Some(c) => {
                self.env.check_visible(pos, self.env.viewer(self.bindings.module, name), &key)?;
                let c = c?;
                let r = self.hoist(|cx| cx.splice(&c.0, &c.1));
                self.const_regs.insert(name.to_string(), r);
                Ok(r)
            }
            None => self.func_value(pos, name, &key),
        }
    }

    /// A function that works on code runs only while compiling: only macros
    /// and other such functions may use it.
    fn check_code_fn(&self, pos: Pos, name: &str, key: &str) -> Result<(), Error> {
        if self.env.is_code_fn(key) && !self.in_macro {
            let msg = format!("`{name}` works on code, so it only runs while compiling: use it from a macro or a function on code");
            return Err(Error::new(pos, msg));
        }
        Ok(())
    }

    /// Function `key`, written `name`, as a value of type `fn(...) -> R`.
    fn func_value(&mut self, pos: Pos, name: &str, key: &str) -> Result<Reg, Error> {
        if is_builtin(name) {
            return Err(Error::new(pos, format!("`{name}` is a builtin, not a function, so it cannot be used as a value")));
        }
        if self.env.macro_decl(key).is_some() {
            return Err(Error::new(pos, format!("`{name}` is a macro; macros cannot be used as values")));
        }
        self.check_code_fn(pos, name, key)?;
        if self.env.generic(key).is_some() {
            return Err(Error::new(
                pos,
                format!("`{name}` has comptime parameters, so it cannot be used as a value; wrap a call to it in a function"),
            ));
        }
        let Some(sig) = self.env.signature(key) else {
            if floats::is_float_builtin(hygiene::unqualified(name)) {
                let msg = format!("`{name}` is a builtin, not a function, so it cannot be used as a value");
                return Err(Error::new(pos, msg));
            }
            return Err(self.unknown_name(pos, name));
        };
        self.env.check_visible(pos, self.env.viewer(self.bindings.module, name), key)?;
        let sig = sig?;
        if self.env.extern_decl(key).is_some_and(|d| d.variadic) {
            return Err(Error::new(pos, format!("`{name}` takes variable arguments, so it cannot be used as a value")));
        }
        let ty = Type::Fn(sig.params.clone(), Box::new(sig.ret.clone()));
        let func = self.env.ir_name(key);
        let r = self.hoist(|cx| cx.emit_to(ty, |dst| Inst::FuncRef { dst, func }));
        self.const_regs.insert(name.to_string(), r);
        Ok(r)
    }

    /// The function that `name` names at compile time, if it is a comptime
    /// parameter or a constant holding a function: `(JIR name, arguments it
    /// takes first, params, ret)`. The first arguments are the captured values
    /// of a comptime closure. Calls to it are direct calls.
    #[allow(clippy::type_complexity)]
    fn known_func(&self, pos: Pos, name: &str) -> Result<Option<(String, Vec<Reg>, Vec<Type>, Type)>, Error> {
        let found = match self.bindings.get(name) {
            Some(Binding::Value(ty, v)) => Some((ty.clone(), v.clone())),
            Some(Binding::Closure { ty: Type::Fn(params, ret), func, .. }) => {
                let first = self.closure_regs[name].clone();
                return Ok(Some((func.clone(), first, params.clone(), (**ret).clone())));
            }
            Some(Binding::Type(_) | Binding::Closure { .. }) => None,
            None => {
                let key = self.env.key_or_err(pos, &self.bindings, name)?;
                match self.env.constant(&key) {
                    Some(c) => {
                        self.env.check_visible(pos, self.env.viewer(self.bindings.module, name), &key)?;
                        Some(c?.as_ref().clone())
                    }
                    None => None,
                }
            }
        };
        Ok(match found {
            Some((Type::Fn(params, ret), ConstValue::Func(f))) => Some((f, vec![], params, *ret)),
            _ => None,
        })
    }

    /// Evaluates the arguments of a call to `what` and checks them against `params`.
    fn call_args(&mut self, pos: Pos, what: &str, params: &[Type], args: &[Expr]) -> Result<Vec<Reg>, Error> {
        if params.len() != args.len() {
            return Err(Error::new(pos, format!("{what} takes {} arguments, {} given", params.len(), args.len())));
        }
        let mut regs = Vec::with_capacity(args.len());
        for (i, (a, want)) in args.iter().zip(params).enumerate() {
            let r = self.expr(a, Some(want))?;
            self.expect(a.pos, r, want, &format!("argument {} of {what}", i + 1))?;
            regs.push(r);
        }
        Ok(regs)
    }

    /// Calls the function value in `callee`; `what` names it in errors.
    fn call_value(&mut self, pos: Pos, callee: Reg, what: &str, args: &[Expr]) -> Result<Reg, Error> {
        let Type::Fn(params, ret) = self.ty(callee).clone() else {
            return Err(Error::new(pos, format!("{what} is not a function; it has type {}", self.ty(callee))));
        };
        let regs = self.call_args(pos, what, &params, args)?;
        Ok(self.emit_to(*ret, |dst| Inst::CallIndirect { dst, callee, args: regs }))
    }

    /// Emits `build` at the start of the entry block instead of here, so a
    /// constant used in a loop is built once per call, not once per iteration.
    /// `build` must emit straight-line code without side effects; its result
    /// register must never be written again (constants are not assignable).
    fn hoist(&mut self, build: impl FnOnce(&mut Self) -> Reg) -> Reg {
        let cur = self.cur.0 as usize;
        let start = self.blocks[cur].insts.len();
        let r = build(self);
        let moved: Vec<Inst> = self.blocks[cur].insts.drain(start..).collect();
        let n = moved.len();
        let at = self.hoisted;
        self.blocks[0].insts.splice(at..at, moved);
        self.hoisted += n;
        r
    }

    fn expect(&self, pos: Pos, r: Reg, want: &Type, what: &str) -> Result<(), Error> {
        let got = self.ty(r);
        if got == want {
            Ok(())
        } else {
            Err(Error::new(pos, format!("{what} must be {want}, found {got}")))
        }
    }

    // ---- statements ----

    /// Errors for assigning to a name that is not a variable.
    fn check_assign_target(&self, target: &Expr) -> Result<(), Error> {
        if let ExprKind::Var(name) = &target.kind {
            if self.local(name).is_none() {
                if self.bindings.get(name).is_some() {
                    return Err(Error::new(target.pos, format!("cannot assign to comptime parameter `{name}`")));
                }
                if self.env.key(self.bindings.module, name).and_then(|k| self.env.constant(&k)).is_some() {
                    return Err(Error::new(target.pos, format!("cannot assign to constant `{name}`")));
                }
            }
        }
        Ok(())
    }

    fn block(&mut self, b: &Block) -> Result<(), Error> {
        self.scopes.push(HashMap::new());
        let r = b.stmts.iter().try_for_each(|s| self.stmt(s));
        self.scopes.pop();
        r
    }

    /// The state lowering code changes, to take that code out again (for
    /// `type_of`, which only wants its type).
    fn save(&self) -> Saved {
        Saved {
            blocks: self.blocks.clone(),
            cur: self.cur,
            regs: self.regs.len(),
            scopes: self.scopes.clone(),
            const_regs: self.const_regs.clone(),
            hoisted: self.hoisted,
            head: self.head,
            captured: self.captured.clone(),
            closure_regs: self.closure_regs.clone(),
            loops: self.loops.clone(),
        }
    }

    fn restore(&mut self, s: Saved) {
        self.blocks = s.blocks;
        self.cur = s.cur;
        self.regs.truncate(s.regs);
        self.scopes = s.scopes;
        self.const_regs = s.const_regs;
        self.hoisted = s.hoisted;
        self.head = s.head;
        self.captured = s.captured;
        self.closure_regs = s.closure_regs;
        self.loops = s.loops;
    }

    /// `{ statements... value }`: the statements in a scope of their own, and
    /// the value of the last one if it is an expression (a statement macro is
    /// not), or unit.
    fn block_expr(&mut self, b: &Block, expected: Option<&Type>) -> Result<Reg, Error> {
        self.scopes.push(HashMap::new());
        let r = self.block_value(b, expected);
        self.scopes.pop();
        r
    }

    fn block_value(&mut self, b: &Block, expected: Option<&Type>) -> Result<Reg, Error> {
        let (init, last) = match b.stmts.split_last() {
            Some((Stmt::Expr(e), init)) if !matches!(&e.kind, ExprKind::MacroCall(m, _) if self.macro_kind(m) == Some(Type::Stmts)) => {
                (init, Some(e))
            }
            _ => (&b.stmts[..], None),
        };
        init.iter().try_for_each(|s| self.stmt(s))?;
        match last {
            Some(e) => self.expr(e, expected),
            None => Ok(self.unit()),
        }
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), Error> {
        match s {
            Stmt::Let { pos, name, ty, value } => {
                let want = ty.as_ref().map(|t| self.resolve(t)).transpose()?;
                let v = self.expr(value, want.as_ref())?;
                if let Some(want) = &want {
                    self.expect(value.pos, v, want, &format!("the value of `{name}`"))?;
                }
                let vty = self.ty(v).clone();
                if vty == Type::Unit {
                    return Err(Error::new(*pos, format!("`{name}` would have type unit; this expression has no value")));
                }
                let dst = self.emit_to(vty, |dst| Inst::Copy { dst, src: v });
                self.scopes.last_mut().unwrap().insert(name.clone(), dst);
            }
            Stmt::LetPattern { pos, pattern, ty, value } => self.let_pattern(*pos, pattern, ty.as_ref(), value)?,
            Stmt::Assign { target, value } => {
                self.check_assign_target(target)?;
                let place = self.place(target)?;
                let want = place.ty().clone();
                let v = self.expr(value, Some(&want))?;
                self.expect(value.pos, v, &want, "the assigned value")?;
                self.write(target.pos, place, v)?;
            }
            // `a[f()] += 1` is `a[f()] = a[f()] + 1` with the place evaluated
            // once: `f` runs one time, and the place is read before the value.
            Stmt::OpAssign { pos, op, target, value } => {
                self.check_assign_target(target)?;
                let (op, sym) = ir_binop(*op);
                let place = self.place(target)?;
                let ty = place.ty().clone();
                let current = self.read(place.clone());
                // The value takes its type from the target; pointers move by an i64.
                let hint = if matches!(ty, Type::Ptr(_)) { Type::I64 } else { ty.clone() };
                let v = self.expr(value, Some(&hint))?;
                let vt = self.ty(v).clone();
                // The result must fit back into the target: `p += 1` on a pointer
                // does, `x += p` on an integer does not.
                let result = types::binary(op, &ty, &vt)
                    .filter(|t| *t == ty)
                    .ok_or_else(|| Error::new(*pos, format!("cannot apply `{sym}=` to {ty} and {vt}")))?;
                let r = self.emit_to(result, |dst| Inst::Binary { dst, op, lhs: current, rhs: v });
                self.write(target.pos, place, r)?;
            }
            Stmt::Return { pos, value } => {
                let ret = self.sig.ret.clone();
                let r = match value {
                    Some(e) => {
                        let r = self.expr(e, Some(&ret))?;
                        self.expect(e.pos, r, &ret, "the return value")?;
                        // `return g(...)`: a jump or a tail call, not a call.
                        if self.tail_call(r) {
                            let dead = self.new_block();
                            self.switch_to(dead);
                            return Ok(());
                        }
                        r
                    }
                    None if ret == Type::Unit => self.unit(),
                    None => return Err(Error::new(*pos, format!("missing return value of type {ret}"))),
                };
                self.terminate(Terminator::Ret(r));
                // Anything after this goes into an unreachable block.
                let dead = self.new_block();
                self.switch_to(dead);
            }
            Stmt::If { cond, then, els } => {
                let c = self.expr(cond, Some(&Type::Bool))?;
                self.expect(cond.pos, c, &Type::Bool, "an `if` condition")?;
                let then_bb = self.new_block();
                let end_bb = self.new_block();
                let else_bb = if els.is_some() { self.new_block() } else { end_bb };
                self.terminate(Terminator::Branch { cond: c, then: then_bb, els: else_bb });

                self.switch_to(then_bb);
                self.block(then)?;
                self.terminate(Terminator::Jump(end_bb));

                if let Some(els) = els {
                    self.switch_to(else_bb);
                    self.block(els)?;
                    self.terminate(Terminator::Jump(end_bb));
                }
                self.switch_to(end_bb);
            }
            Stmt::Match { pos, value, arms } => self.match_stmt(*pos, value, arms)?,
            Stmt::Go { pos, call } => self.go_stmt(*pos, call)?,
            Stmt::Select { pos, arms } => self.select_stmt(*pos, arms)?,
            Stmt::While { cond, body } => {
                let cond_bb = self.new_block();
                let body_bb = self.new_block();
                let end_bb = self.new_block();
                self.terminate(Terminator::Jump(cond_bb));

                self.switch_to(cond_bb);
                let c = self.expr(cond, Some(&Type::Bool))?;
                self.expect(cond.pos, c, &Type::Bool, "a `while` condition")?;
                self.terminate(Terminator::Branch { cond: c, then: body_bb, els: end_bb });

                self.switch_to(body_bb);
                self.loops.push(Loop { next: cond_bb, end: end_bb });
                let r = self.block(body);
                self.loops.pop();
                r?;
                self.terminate(Terminator::Jump(cond_bb));

                self.switch_to(end_bb);
            }
            Stmt::For { pos, index, name, iter, end, body } => {
                self.for_stmt(*pos, index.as_deref(), name, iter, end.as_ref(), body)?
            }
            Stmt::Break { pos } | Stmt::Continue { pos } => {
                let is_break = matches!(s, Stmt::Break { .. });
                let Some(l) = self.loops.last().copied() else {
                    let word = if is_break { "break" } else { "continue" };
                    // A closure body is a function of its own, so the loops
                    // around the closure do not count.
                    return Err(Error::new(*pos, format!("`{word}` outside of a loop")));
                };
                self.terminate(Terminator::Jump(if is_break { l.end } else { l.next }));
                // Anything after this goes into an unreachable block.
                let dead = self.new_block();
                self.switch_to(dead);
            }
            // A statement macro adds its statements to this block.
            Stmt::Expr(Expr { pos, kind: ExprKind::MacroCall(name, args) })
                if self.macro_kind(name) == Some(Type::Stmts) =>
            {
                self.macro_stmts(*pos, name, args)?;
            }
            Stmt::Expr(e) => {
                self.expr(e, None)?;
            }
        }
        Ok(())
    }

    // ---- expressions ----

    /// Lowers `e`. `expected` is only a hint for integer literals; callers still
    /// check the resulting type themselves.
    fn expr(&mut self, e: &Expr, expected: Option<&Type>) -> Result<Reg, Error> {
        self.note(e.pos);
        Ok(match &e.kind {
            ExprKind::Int(n) => self.int_literal(e.pos, *n as i128, expected)?,
            ExprKind::Unary(UnOp::Neg, inner) if matches!(inner.kind, ExprKind::Int(_)) => {
                let ExprKind::Int(n) = inner.kind else { unreachable!() };
                self.int_literal(e.pos, -(n as i128), expected)?
            }
            ExprKind::Float(x) => self.float_literal(*x, expected),
            ExprKind::Unary(UnOp::Neg, inner) if matches!(inner.kind, ExprKind::Float(_)) => {
                let ExprKind::Float(x) = inner.kind else { unreachable!() };
                self.float_literal(-x, expected)
            }
            ExprKind::Bool(b) => self.konst(Type::Bool, *b as i64),
            ExprKind::Str(s) => {
                // Macros run on the VM, where strings are always `str`.
                // Natively, a literal is a `str` unless a C string is expected.
                let ty = match expected {
                    _ if self.in_macro => Type::Str,
                    Some(Type::Ptr(_)) => types::str_literal(self.profile()),
                    _ if self.profile() == Profile::Native => Type::Str,
                    _ => types::str_literal(self.profile()),
                };
                self.emit_to(ty, |dst| Inst::Str { dst, value: s.clone() })
            }
            ExprKind::Var(name) => self.var(e.pos, name)?,
            ExprKind::Asm(a) => self.inline_asm(e.pos, a)?,
            ExprKind::MacroCall(name, args) => self.macro_call(e.pos, name, args, expected)?,
            ExprKind::Quote(kind, pieces, holes) => self.quote(e.pos, *kind, pieces, holes)?,
            ExprKind::Hole(_) => unreachable!("holes only exist inside quote templates"),
            ExprKind::Comptime(inner) => {
                let (ty, v) = self.env.comptime(inner, expected, &self.bindings)?;
                self.hoist(|cx| cx.splice(&ty, &v))
            }
            ExprKind::Unary(op, inner) => {
                let (op, sym, hint) = match op {
                    UnOp::Neg => (ir::UnOp::Neg, "-", expected),
                    // `!` is logical on bools and bitwise on integers.
                    UnOp::Not => (ir::UnOp::Not, "!", expected),
                };
                let src = self.expr(inner, hint)?;
                let ty = types::unary(op, self.ty(src)).ok_or_else(|| {
                    Error::new(e.pos, format!("cannot apply `{sym}` to {}", self.ty(src)))
                })?;
                self.emit_to(ty, |dst| Inst::Unary { dst, op, src })
            }
            ExprKind::Binary(BinOp::And, l, r) => self.short_circuit(true, l, r)?,
            ExprKind::Binary(BinOp::Or, l, r) => self.short_circuit(false, l, r)?,
            ExprKind::Binary(op, l, r) => self.binary(e.pos, *op, l, r, expected)?,
            ExprKind::Call(name, args) => self.call(e.pos, name, args, expected)?,
            ExprKind::CallExpr(f, args) => {
                // `geo.Shape.Circle(1)`, `Option(i64).Some(1)`: a variant.
                if let ExprKind::Field(base, variant) = &f.kind {
                    if let Some(en) = self.enum_path(base)? {
                        // `Option(i64).compare(a, b)`: a trait function of the enum.
                        if let (false, crate::enums::EnumRef::Known(ty)) = (self.is_variant_call(&en, variant), &en) {
                            return self.type_call(e.pos, ty.clone(), variant, args);
                        }
                        return self.construct(e.pos, en, variant, Some(args), expected);
                    }
                    // `shapes.Shape.area(s)`: a function of another module's
                    // trait or type.
                    if let ExprKind::Field(m, item) = &base.kind {
                        if let ExprKind::Var(alias) = &m.kind {
                            if self.local(alias).is_none() && self.env.is_alias(self.bindings.module, alias) {
                                if let Some(r) = self.path_call(e.pos, &format!("{alias}.{item}"), variant, args)? {
                                    return Ok(r);
                                }
                            }
                        }
                    }
                    // `xs[0].less(y)`: maybe a trait function.
                    if let Some(r) = self.method_call(e.pos, base, variant, args)? {
                        return Ok(r);
                    }
                }
                let callee = self.expr(f, None)?;
                self.call_value(e.pos, callee, "this function", args)?
            }
            ExprKind::Lambda(l) => self.lambda(e.pos, l, expected)?,
            ExprKind::Match(value, arms) => self.match_expr(e.pos, value, arms, expected)?,
            ExprKind::Block(b) => self.block_expr(b, expected)?,
            ExprKind::NewChan(t, cap) => self.new_chan(e.pos, t, cap.as_deref())?,
            ExprKind::Type(t) => {
                return Err(Error::new(t.pos, "`fn(...)` is a type, not a value"));
            }
            ExprKind::StructLit(t, inits) => {
                let Type::Struct(name) = self.resolve(t)? else {
                    return Err(Error::new(e.pos, "only structs can be built with `{ ... }`"));
                };
                self.struct_literal(e.pos, &name, inits)?
            }
            ExprKind::ArrayLit(items) => self.array_literal(e.pos, items, expected)?,
            ExprKind::Tuple(items) => self.tuple(items, expected)?,
            // `[v; n]` where a list is expected: `n` copies, `n` known when it runs.
            ExprKind::ArrayRepeat(value, n) if matches!(expected, Some(Type::List(_))) => {
                let Some(Type::List(elem)) = expected else { unreachable!() };
                let value = self.expr(value, Some(elem))?;
                let count = self.expr(n, Some(&Type::I64))?;
                self.expect(n.pos, count, &Type::I64, "the length of a list")?;
                let ty = Type::List(Box::new(self.ty(value).clone()));
                self.emit_to(ty, |dst| Inst::Repeat { dst, value, count })
            }
            ExprKind::ArrayRepeat(value, n) => {
                let hint = match expected {
                    Some(Type::Array(elem, _)) => Some(&**elem),
                    _ => None,
                };
                let n = self.env.array_len(n, &self.bindings)?;
                let v = self.expr(value, hint)?;
                let ty = Type::array(self.ty(v).clone(), n);
                self.emit_to(ty, |dst| Inst::Splat { dst, value: v })
            }
            ExprKind::SizeOf(t) | ExprKind::AlignOf(t) => {
                let ty = self.resolve(t)?;
                let l = self.env.layout(t.pos, &ty)?;
                let n = if matches!(e.kind, ExprKind::SizeOf(_)) { l.size } else { l.align };
                let n = i64::try_from(n).map_err(|_| Error::new(e.pos, format!("{ty} is too large")))?;
                self.konst(Type::I64, n)
            }
            ExprKind::PathRoot(t) => self.path_root(e.pos, t)?,
            ExprKind::Slice(base, lo, hi) => self.slice(e.pos, base, lo.as_deref(), hi.as_deref())?,
            ExprKind::Field(..) | ExprKind::Index(..) | ExprKind::Deref(_) => {
                // `Shape.Empty`: a variant without a payload.
                if let ExprKind::Field(base, variant) = &e.kind {
                    if let Some(en) = self.enum_path(base)? {
                        return self.construct(e.pos, en, variant, None, expected);
                    }
                }
                let place = self.place(e)?;
                self.read(place)
            }
            ExprKind::NewCell(inner) => {
                if self.profile() == Profile::Freestanding && !self.in_macro {
                    return Err(Error::new(e.pos, "`cell` allocates memory, which freestanding programs cannot"));
                }
                let hint = match expected {
                    Some(Type::Cell(t)) => Some(&**t),
                    _ => None,
                };
                let value = self.expr(inner, hint)?;
                let ty = Type::Cell(Box::new(self.ty(value).clone()));
                self.emit_to(ty, |dst| Inst::NewCell { dst, value })
            }
            ExprKind::NewRef(inner) => {
                if self.profile() == Profile::Freestanding && !self.in_macro {
                    return Err(Error::new(e.pos, "`ref` allocates memory, which freestanding programs cannot"));
                }
                let hint = match expected {
                    Some(Type::Ref(t)) => Some(&**t),
                    _ => None,
                };
                let src = self.expr(inner, hint)?;
                let ty = Type::Ref(Box::new(self.ty(src).clone()));
                self.emit_to(ty, |dst| Inst::Ref { dst, src })
            }
            ExprKind::AddrOf(inner) => {
                if !self.profile().is_compiled() {
                    return Err(Error::new(e.pos, "`&` makes a pointer; pointers are only available in native and freestanding mode"));
                }
                let place = self.place(inner)?;
                self.addr_of(e.pos, place)?
            }
            ExprKind::Cast(inner, ty) => {
                let to = self.resolve(ty)?;
                let src = self.expr(inner, None)?;
                let from = self.ty(src).clone();
                if !types::can_cast(&from, &to) {
                    return Err(Error::new(e.pos, format!("cannot cast {from} to {to}")));
                }
                if from == to {
                    src
                } else {
                    self.emit_to(to, |dst| Inst::Cast { dst, src })
                }
            }
        })
    }

    fn int_literal(&mut self, pos: Pos, n: i128, expected: Option<&Type>) -> Result<Reg, Error> {
        // Where a float is expected, `2` means `2.0`, as long as that is exact.
        if let Some(&Type::Float(t)) = expected {
            let x = t.round(n as f64);
            if x as i128 != n {
                let msg = format!("integer literal {n} is not exactly representable as {}; write it as a float", t.name());
                return Err(Error::new(pos, msg));
            }
            return Ok(self.fconst(t, x));
        }
        let t = expected.and_then(Type::as_int).unwrap_or(IntTy::I64);
        if n < t.min() || n > t.max() {
            return Err(Error::new(pos, format!("integer literal {n} does not fit in {}", t.name())));
        }
        Ok(self.konst(Type::Int(t), n as i64))
    }

    fn binary(&mut self, pos: Pos, op: BinOp, l: &Expr, r: &Expr, expected: Option<&Type>) -> Result<Reg, Error> {
        let (op, sym) = ir_binop(op);
        let is_cmp = matches!(
            op,
            ir::BinOp::Eq | ir::BinOp::Ne | ir::BinOp::Lt | ir::BinOp::Le | ir::BinOp::Gt | ir::BinOp::Ge
        );
        // Arithmetic passes the expected type down; comparisons produce bool, so
        // their operands get no hint from outside.
        let hint = if is_cmp { None } else { expected };

        let (ll, rl) = (number_literal(l), number_literal(r));
        let (lhs, rhs) = if ll.is_some() && rl.is_none() {
            // `1 + x`: type the literal after `x`. Literals have no side effects,
            // so evaluating the right side first is not observable.
            let rhs = self.expr(r, hint)?;
            let rt = self.ty(rhs).clone();
            (self.expr(l, Some(&rt))?, rhs)
        } else if ll.is_some() && rl.is_some() && (ll == Some(Literal::Float) || rl == Some(Literal::Float)) {
            // `1 + 2.5`: both float, of the expected type if that is one.
            let t = match hint {
                Some(t @ Type::Float(_)) => t.clone(),
                _ => Type::F64,
            };
            (self.expr(l, Some(&t))?, self.expr(r, Some(&t))?)
        } else {
            let lhs = self.expr(l, hint)?;
            let rt = match self.ty(lhs) {
                Type::Ptr(_) => Type::I64, // pointer offsets are i64
                t => t.clone(),
            };
            (lhs, self.expr(r, Some(&rt))?)
        };

        let (lt, rt) = (self.ty(lhs).clone(), self.ty(rhs).clone());
        // Cells go there too, for an error that says what to compare instead.
        if matches!(op, ir::BinOp::Eq | ir::BinOp::Ne) && lt == rt && (equality::is_structural(&lt) || matches!(lt, Type::Cell(_))) {
            return self.structural_eq(pos, lhs, rhs, op == ir::BinOp::Ne);
        }
        // `a < b` on a type with an `Ord` and no order of its own: `compare(a, b) < 0`.
        let ordering = matches!(op, ir::BinOp::Lt | ir::BinOp::Le | ir::BinOp::Gt | ir::BinOp::Ge);
        if ordering && lt == rt && types::binary(op, &lt, &rt).is_none() {
            if let Some(r) = self.ordered(pos, op, lhs, rhs)? {
                return Ok(r);
            }
            if matches!(lt, Type::Struct(_) | Type::Enum(_) | Type::List(_)) {
                return Err(Error::new(pos, format!("cannot apply `{sym}` to {lt} and {rt}; give the type an `Ord`")));
            }
        }
        let ty = types::binary(op, &lt, &rt)
            .ok_or_else(|| Error::new(pos, format!("cannot apply `{sym}` to {lt} and {rt}")))?;
        Ok(self.emit_to(ty, |dst| Inst::Binary { dst, op, lhs, rhs }))
    }

    /// `s[lo:hi]`: bytes `lo` up to `hi` of a `str`; `lo` defaults to 0 and
    /// `hi` to the length.
    fn slice(&mut self, pos: Pos, base: &Expr, lo: Option<&Expr>, hi: Option<&Expr>) -> Result<Reg, Error> {
        let src = self.expr(base, None)?;
        if !matches!(self.ty(src), Type::Str | Type::List(_)) {
            return Err(Error::new(pos, format!("cannot slice {}; only a str or list can be sliced", self.ty(src))));
        }
        let bound = |cx: &mut Self, e: Option<&Expr>, default: &dyn Fn(&mut Self) -> Reg| match e {
            Some(e) => {
                let r = cx.expr(e, Some(&Type::I64))?;
                cx.expect(e.pos, r, &Type::I64, "a slice bound")?;
                Ok(r)
            }
            None => Ok(default(cx)),
        };
        let lo = bound(self, lo, &|cx| cx.konst(Type::I64, 0))?;
        let hi = bound(self, hi, &|cx| cx.emit_to(Type::I64, |dst| Inst::Len { dst, src }))?;
        Ok(self.emit_to(self.ty(src).clone(), |dst| Inst::Slice { dst, src, lo, hi }))
    }

    /// `trap()`: stops the program. Code after it is unreachable, so a
    /// function may end with it instead of a `return`.
    fn trap(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        if !args.is_empty() {
            return Err(Error::new(pos, "`trap` takes no arguments"));
        }
        self.emit(Inst::Trap);
        self.terminate(Terminator::Unreachable);
        let dead = self.new_block();
        self.switch_to(dead);
        Ok(self.unit())
    }

    /// `read_line()`: the next line of standard input with its `\n`, or `""`
    /// at the end.
    fn read_line(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        if self.profile() == Profile::Freestanding {
            return Err(Error::new(pos, "`read_line` makes a `str`, which freestanding programs do not have"));
        }
        if !args.is_empty() {
            return Err(Error::new(pos, "`read_line` takes no arguments"));
        }
        Ok(self.emit_to(Type::Str, |dst| Inst::ReadLine { dst }))
    }

    /// `from_bytes(b)`: the `str` made of the bytes in the `[u8]` `b`.
    fn str_from_bytes(&mut self, pos: Pos, args: &[Expr]) -> Result<Reg, Error> {
        let [arg] = args else {
            return Err(Error::new(pos, "`from_bytes` takes exactly 1 argument, a `[u8]`"));
        };
        let bytes = Type::List(Box::new(Type::U8));
        let src = self.expr(arg, Some(&bytes))?;
        self.expect(arg.pos, src, &bytes, "the argument of `from_bytes`")?;
        Ok(self.emit_to(Type::Str, |dst| Inst::FromBytes { dst, src }))
    }

    /// `a && b` / `a || b` on bools, evaluating `b` only when needed.
    fn short_circuit(&mut self, is_and: bool, l: &Expr, r: &Expr) -> Result<Reg, Error> {
        let sym = if is_and { "&&" } else { "||" };
        let lhs = self.expr(l, Some(&Type::Bool))?;
        self.expect(l.pos, lhs, &Type::Bool, &format!("the left side of `{sym}`"))?;
        // Result when the right side is skipped: false for `&&`, true for `||`.
        let res = self.konst(Type::Bool, (!is_and) as i64);
        let rhs_bb = self.new_block();
        let end_bb = self.new_block();
        let (then, els) = if is_and { (rhs_bb, end_bb) } else { (end_bb, rhs_bb) };
        self.terminate(Terminator::Branch { cond: lhs, then, els });

        self.switch_to(rhs_bb);
        let rhs = self.expr(r, Some(&Type::Bool))?;
        self.expect(r.pos, rhs, &Type::Bool, &format!("the right side of `{sym}`"))?;
        self.emit(Inst::Copy { dst: res, src: rhs });
        self.terminate(Terminator::Jump(end_bb));

        self.switch_to(end_bb);
        Ok(res)
    }

    /// `[a, b, c]`: the element type comes from the expected type if there is one,
    /// otherwise from the first element.
    fn array_literal(&mut self, pos: Pos, items: &[Expr], expected: Option<&Type>) -> Result<Reg, Error> {
        // Where a list is expected, `[a, b]` is one; otherwise it is an array.
        let list = matches!(expected, Some(Type::List(_)));
        let mut elem = match expected {
            Some(Type::Array(elem, _) | Type::List(elem)) => Some((**elem).clone()),
            _ => None,
        };
        let mut regs = Vec::with_capacity(items.len());
        for item in items {
            let r = self.expr(item, elem.as_ref())?;
            match &elem {
                Some(t) => self.expect(item.pos, r, t, "an array element")?,
                None => elem = Some(self.ty(r).clone()),
            }
            regs.push(r);
        }
        let Some(elem) = elem else {
            return Err(Error::new(pos, "cannot infer the element type of `[]`; give the variable a type"));
        };
        let ty = if list { Type::List(Box::new(elem)) } else { Type::array(elem, regs.len() as u64) };
        Ok(self.emit_to(ty, |dst| Inst::Array { dst, items: regs }))
    }

    fn struct_literal(&mut self, pos: Pos, name: &str, inits: &[FieldInit]) -> Result<Reg, Error> {
        let env = self.env;
        let fields = env.struct_fields(pos, name)?;
        let mut values: Vec<Option<Reg>> = vec![None; fields.len()];
        // Evaluate in source order, store in declaration order.
        for init in inits {
            let (index, ty) = env.field(init.pos, &Type::Struct(name.to_string()), &init.name, self.bindings.module)?;
            let field = &fields[index as usize].0;
            if values[index as usize].is_some() {
                return Err(Error::new(init.pos, format!("field `{field}` is given twice")));
            }
            let v = self.expr(&init.value, Some(&ty))?;
            self.expect(init.value.pos, v, &ty, &format!("field `{field}`"))?;
            values[index as usize] = Some(v);
        }
        let missing: Vec<&str> = fields
            .iter()
            .zip(&values)
            .filter(|(_, v)| v.is_none())
            .map(|((n, _), _)| n.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(Error::new(pos, format!("missing fields in `{name}`: {}", missing.join(", "))));
        }
        let fields = values.into_iter().map(Option::unwrap).collect();
        Ok(self.emit_to(Type::Struct(name.to_string()), |dst| Inst::Struct {
            dst,
            name: name.to_string(),
            fields,
        }))
    }

    fn call(&mut self, pos: Pos, name: &str, args: &[Expr], expected: Option<&Type>) -> Result<Reg, Error> {
        match name {
            "print" => {
                // Compile-time code runs on the VM, which can always print.
                if !self.in_macro && !Type::can_print(self.profile()) {
                    let msg = "`print` needs the VM or the C library, so it is not available in freestanding mode \
                               (write to a file descriptor instead, as `io.puts` does)";
                    return Err(Error::new(pos, msg));
                }
                let [arg] = args else {
                    return Err(Error::new(pos, "`print` takes exactly 1 argument"));
                };
                let mut r = self.expr(arg, None)?;
                // Macros run on the VM, where text is a `str`.
                let profile = if self.in_macro { Profile::Hosted } else { self.profile() };
                if !self.ty(r).is_printable(profile) {
                    // A value with a `Show` prints as its text.
                    match self.show_value(arg.pos, r)? {
                        Some(text) => r = text,
                        None => {
                            let msg = format!("cannot print a value of type {}; give the type a `Show`", self.ty(r));
                            return Err(Error::new(arg.pos, msg));
                        }
                    }
                }
                self.emit(Inst::Print { src: r });
                Ok(self.unit())
            }
            "stringify" => self.stringify(pos, args),
            "code_text" | "to_code" => self.code_text(pos, name, args),
            "type_of" => self.type_of(pos, args),
            "type_shape" => self.type_shape(pos, args),
            "to_items" => self.text_to_items(pos, args),
            "path_index" => self.path_index(pos, args),
            "to_str" => {
                if self.profile() == Profile::Freestanding && !self.in_macro {
                    return Err(Error::new(pos, "`to_str` makes a `str`, which freestanding programs do not have"));
                }
                let [arg] = args else {
                    return Err(Error::new(pos, "`to_str` takes exactly 1 argument"));
                };
                let r = self.expr(arg, None)?;
                if !matches!(self.ty(r), Type::Int(_) | Type::Float(_) | Type::Bool) {
                    return Err(Error::new(arg.pos, format!("`to_str` takes a number or a bool, not {}", self.ty(r))));
                }
                Ok(self.emit_to(Type::Str, |dst| Inst::ToStr { dst, src: r }))
            }
            // `ident(name)`: a name as code, to use a generated name in an expression.
            "ident" => {
                if !self.in_macro {
                    return Err(Error::new(pos, "`ident` can only be used inside a macro"));
                }
                let [arg] = args else {
                    return Err(Error::new(pos, "`ident` takes exactly 1 argument"));
                };
                let r = self.expr(arg, Some(&Type::Str))?;
                self.expect(arg.pos, r, &Type::Str, "the argument of `ident`")?;
                // A one-hole quote in a name position checks it is an identifier.
                Ok(self.emit_to(Type::Expr, |dst| Inst::Quote {
                    dst,
                    pieces: vec![String::new(), String::new()],
                    holes: vec![r],
                    kinds: vec![ir::HoleKind::Ident],
                }))
            }
            "unique" => {
                if !self.in_macro {
                    return Err(Error::new(pos, "`unique` can only be used inside a macro"));
                }
                let [arg] = args else {
                    return Err(Error::new(pos, "`unique` takes exactly 1 argument"));
                };
                let r = self.expr(arg, Some(&Type::Str))?;
                self.expect(arg.pos, r, &Type::Str, "the argument of `unique`")?;
                Ok(self.emit_to(Type::Str, |dst| Inst::Unique { dst, prefix: r }))
            }
            "len" => {
                let [arg] = args else {
                    return Err(Error::new(pos, "`len` takes exactly 1 argument"));
                };
                let r = self.expr(arg, None)?;
                let n = match self.ty(r) {
                    Type::Str | Type::List(_) => return Ok(self.emit_to(Type::I64, |dst| Inst::Len { dst, src: r })),
                    Type::Array(_, n) => *n,
                    Type::Ptr(inner) if matches!(**inner, Type::Array(..)) => {
                        let Type::Array(_, n) = **inner else { unreachable!() };
                        n
                    }
                    t => return Err(Error::new(arg.pos, format!("`len` needs an array, list or str, found {t}"))),
                };
                let n = i64::try_from(n).map_err(|_| Error::new(arg.pos, "array is too long"))?;
                Ok(self.konst(Type::I64, n))
            }
            "syscall" => {
                if !self.profile().is_compiled() {
                    return Err(Error::new(pos, "`syscall` is only available in native and freestanding mode"));
                }
                if args.is_empty() || args.len() > 7 {
                    return Err(Error::new(pos, "`syscall` takes 1 to 7 arguments"));
                }
                let mut regs = Vec::with_capacity(args.len());
                // A string literal is a C string here, as for C's variable arguments.
                let c_string = Type::ptr(Type::U8);
                for a in args {
                    let hint = matches!(a.kind, ExprKind::Str(_)).then_some(&c_string);
                    let r = self.expr(a, hint)?;
                    if !self.ty(r).is_syscall_arg() {
                        return Err(Error::new(
                            a.pos,
                            format!("syscall arguments must be integers or pointers, found {}", self.ty(r)),
                        ));
                    }
                    regs.push(r);
                }
                Ok(self.emit_to(Type::I64, |dst| Inst::Syscall { dst, args: regs }))
            }
            _ => {
                // A local holding a function value. Locals of other types do not
                // hide functions: `let len = len(a)` keeps working.
                if let Some(r) = self.local(name).filter(|&r| matches!(self.ty(r), Type::Fn(..))) {
                    return self.call_value(pos, r, &format!("`{name}`"), args);
                }
                if let Some((base, field)) = name.split_once('.') {
                    let base_expr = Expr { pos, kind: ExprKind::Var(base.to_string()) };
                    // `s.f(x)`: a function stored in a field of a local, or a
                    // trait function of its type.
                    if self.local(base).is_some() {
                        if let Some(r) = self.method_call(pos, &base_expr, field, args)? {
                            return Ok(r);
                        }
                        let callee = Expr { pos, kind: ExprKind::Field(Box::new(base_expr), field.to_string()) };
                        let r = self.expr(&callee, None)?;
                        return self.call_value(pos, r, &format!("`{name}`"), args);
                    }
                    // `Shape.Circle(1)`: a variant of an enum.
                    if let Some(en) = self.enum_path(&base_expr)?.filter(|en| self.is_variant_call(en, field)) {
                        return self.construct(pos, en, field, Some(args), expected);
                    }
                    // `Ord.compare(a, b)`, `T.zero()`: a function of a trait or type.
                    if let Some(r) = self.path_call(pos, base, field, args)? {
                        return Ok(r);
                    }
                }
                // A comptime parameter or constant naming a function: a direct call.
                if self.local(name).is_none() {
                    if let Some((func, mut first, params, ret)) = self.known_func(pos, name)? {
                        first.extend(self.call_args(pos, &format!("`{name}`"), &params, args)?);
                        return Ok(self.emit_to(ret, |dst| Inst::Call { dst, func, args: first }));
                    }
                }
                // A takeable builtin, unless the module has a function of that name.
                let (module, base) = hygiene::qualified(name).unwrap_or((self.bindings.module, name));
                if is_takeable_builtin(base) && !self.env.has_function(module, base) {
                    return match base {
                        "trap" => self.trap(pos, args),
                        "read_line" => self.read_line(pos, args),
                        "push" => self.list_push(pos, args),
                        "pop" => self.list_pop(pos, args),
                        "from_bytes" => self.str_from_bytes(pos, args),
                        "send" => self.chan_send(pos, args),
                        "recv" => self.chan_recv(pos, args),
                        _ if atomics::is_atomic_builtin(base) => self.atomic_builtin(pos, base, args),
                        _ => self.float_builtin(pos, base, args, expected),
                    };
                }
                let key = self.env.key_or_err(pos, &self.bindings, name)?;
                self.env.check_visible(pos, self.env.viewer(self.bindings.module, name), &key)?;
                if let Some(decl) = self.env.generic(&key) {
                    return self.call_generic(pos, &key, decl, args);
                }
                if self.env.macro_decl(&key).is_some() {
                    return Err(Error::new(pos, format!("`{name}` is a macro; call it as `{name}!(...)`")));
                }
                self.check_code_fn(pos, name, &key)?;
                let sig = self.env.signature(&key).ok_or_else(|| match self.local(name) {
                    Some(r) => Error::new(pos, format!("`{name}` is not a function; it has type {}", self.ty(r))),
                    None => Error::new(pos, format!("unknown function `{name}`")),
                })??;
                let variadic = self.env.extern_decl(&key).is_some_and(|d| d.variadic);
                let regs = if variadic && args.len() >= sig.params.len() {
                    let (fixed, rest) = args.split_at(sig.params.len());
                    let mut regs = self.call_args(pos, &format!("`{name}`"), &sig.params, fixed)?;
                    // A string literal there is a C string, as for `printf("%s", "x")`.
                    let c_string = Type::ptr(Type::U8);
                    for a in rest {
                        let hint = matches!(a.kind, ExprKind::Str(_)).then_some(&c_string);
                        let r = self.expr(a, hint)?;
                        // Through `...` C takes only what fits a register.
                        if !ir::types::c_compatible(self.ty(r), false, &|_| None) {
                            let msg = format!("{} cannot be passed to C as a variable argument; pass a pointer to it", self.ty(r));
                            return Err(Error::new(a.pos, msg));
                        }
                        regs.push(r);
                    }
                    regs
                } else {
                    self.call_args(pos, &format!("`{name}`"), &sig.params, args)?
                };
                // C calls a function pointer without the values a closure
                // captured: a closure made right here is an error (one in a
                // variable stops the program when passed).
                if self.env.extern_decl(&key).is_some() {
                    for (a, &r) in args.iter().zip(&regs) {
                        let made_here = self.blocks[self.cur.0 as usize]
                            .insts
                            .iter()
                            .any(|i| matches!(i, Inst::Closure { dst, .. } if *dst == r));
                        if made_here {
                            let msg = "C cannot call a closure: this function captures values; pass them as arguments";
                            return Err(Error::new(a.pos, msg));
                        }
                    }
                }
                let ret = sig.ret.clone();
                let func = self.env.ir_name(&key);
                Ok(self.emit_to(ret, |dst| Inst::Call { dst, func, args: regs }))
            }
        }
    }
}

#[cfg(test)]
mod tests;
