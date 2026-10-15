//! Compile-time evaluation.
//!
//! `comptime e` (and every `const` and non-literal array length) is lowered to a
//! helper function `comptime.N` that returns `e`. That function, together with
//! every function it can call, is run on the VM — for freestanding programs too.
//! The result is turned back into IR constants where the expression was used.
//!
//! The functions that runs need are gathered in one module per compilation
//! (`ComptimeModule`), which only grows: a run copies in only what no earlier
//! run needed, so a program with many constants does not copy the functions
//! they share again for each.
//!
//! Compile-time code cannot use what the VM does not have: pointers and `syscall`.
//! `print` works and writes to the compiler's stderr.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use jihoo_ir::{Inst, Reg, Terminator, Type};
use jihoo_syntax::ast::Expr;
use jihoo_syntax::{Error, Pos};
use jihoo_vm::{Program, Value, Vm};

use crate::env::{Env, Sig};
use crate::generic::Bindings;
use crate::FnCx;

/// Instruction budget for one compile-time evaluation.
const FUEL: u64 = if cfg!(test) { 1_000_000 } else { 100_000_000 };

/// The functions compile-time runs have needed so far, each with everything
/// it calls, and that module prepared for the VM. Runs only add to it. A run holds its own `Rc`s,
/// so a run started while converting another's result (which can lower more
/// code) adds to a copy instead of changing what the first one runs.
pub(crate) struct ComptimeModule {
    module: Rc<jihoo_ir::Module>,
    program: Rc<Program>,
}

impl ComptimeModule {
    pub(crate) fn new(profile: jihoo_ir::Profile) -> Self {
        let module = jihoo_ir::Module {
            profile,
            target: jihoo_ir::Target::host(),
            structs: vec![],
            enums: vec![],
            externs: vec![],
            funcs: vec![],
            files: vec![],
        };
        ComptimeModule { module: Rc::new(module), program: Rc::default() }
    }

    fn has(&self, name: &str) -> bool {
        self.program.get(name).is_some()
    }

    fn add(&mut self, funcs: Vec<jihoo_ir::Function>) {
        let module = Rc::make_mut(&mut self.module);
        for f in funcs {
            // A run started while this one gathered may have added it already.
            if self.program.get(&f.name).is_none() {
                module.funcs.push(f);
            }
        }
        Rc::make_mut(&mut self.program).extend(module);
    }
}

/// A value computed at compile time, independent of the VM that computed it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConstValue {
    Unit,
    Int(i64),
    /// The bits of an `f64` (see `Value::Float`).
    Float(u64),
    Bool(bool),
    Str(String),
    /// Fields of a struct or elements of an array; for an enum, the variant
    /// index followed by the payload.
    Agg(Vec<ConstValue>),
    /// A function value: the JIR name of the function.
    Func(String),
}

impl Env<'_> {
    /// Evaluates `e` at compile time.
    /// `b` gives the comptime parameters in scope, which the expression may use.
    pub fn comptime(&self, e: &Expr, expected: Option<&Type>, b: &Rc<Bindings>) -> Result<(Type, ConstValue), Error> {
        self.comptime_in(e, expected, b, false)
    }

    /// Like `comptime`; `in_macro` evaluates `e` with the rules of macro bodies
    /// (strings are `str` even in freestanding programs), for macro arguments.
    pub fn comptime_in(
        &self,
        e: &Expr,
        expected: Option<&Type>,
        b: &Rc<Bindings>,
        in_macro: bool,
    ) -> Result<(Type, ConstValue), Error> {
        let id = self.comptime_ids.get();
        self.comptime_ids.set(id + 1);
        let name = format!("comptime.{id}"); // `.` keeps it apart from user functions

        // Lower `e` into `fn comptime.N() -> T { return e }`.
        let mut cx = FnCx::new(self, Rc::new(Sig { params: vec![], ret: Type::Unit }), b.clone());
        cx.in_comptime = true;
        cx.in_macro = in_macro;
        let r = cx.expr(e, expected)?;
        let ty = cx.ty(r).clone();
        cx.terminate(Terminator::Ret(r));
        let helper = cx.finish(&name, ty.clone(), e.pos)?;
        let v = self.run(e.pos, vec![helper], &name, &[], &ty)?;
        Ok((ty, v))
    }

    /// Runs function `entry` on the VM with `args` and returns its result of type
    /// `ty`. `funcs` are functions made for this run (such as a `comptime`
    /// helper); everything they call is looked up and compiled.
    pub fn run(
        &self,
        pos: Pos,
        funcs: Vec<jihoo_ir::Function>,
        entry: &str,
        args: &[ConstValue],
        ty: &Type,
    ) -> Result<ConstValue, Error> {
        let mut no_types = |_: &str| Err(Error::new(pos, "`type_of` only works in a macro"));
        self.run_typed(pos, funcs, entry, args, ty, &mut no_types)
    }

    /// Like `run`, for a macro: when `type_of` asks for the type of some code,
    /// `type_of` here works it out where the macro was called, and the macro
    /// runs again from the start with it, until it asks for nothing new. Macros
    /// are deterministic, and `unique` starts from the same number each time.
    pub fn run_typed(
        &self,
        pos: Pos,
        funcs: Vec<jihoo_ir::Function>,
        entry: &str,
        args: &[ConstValue],
        ty: &Type,
        type_of: &mut dyn FnMut(&str) -> Result<String, Error>,
    ) -> Result<ConstValue, Error> {
        let (module, program) = self.prepare(pos, funcs, entry)?;
        let uniques = self.uniques.get();
        let mut types = HashMap::new();
        loop {
            let mut vm = Vm::with_program(&module, program.clone())
                .with_fuel(FUEL)
                .with_uniques(uniques)
                .with_input(jihoo_vm::Input::Closed)
                .with_code_types(types.clone());
            let values: Vec<Value> = args.iter().map(|a| arg(&mut vm, a)).collect();
            let mut out = Vec::new();
            let result = vm.call_named(entry, &values, &mut out);
            if let (Err(_), Some(code)) = (&result, vm.wanted_type()) {
                let code = code.to_string();
                let t = type_of(&code)?;
                types.insert(code, t);
                continue;
            }
            self.uniques.set(vm.uniques());
            if !out.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&out));
            }
            let v = result.map_err(|err| {
                let at = if err.func.starts_with("comptime.") { String::new() } else { format!(" in `{}`", err.func) };
                Error::new(pos, format!("compile-time evaluation failed{at}: {}", err.msg))
            })?;
            return self.to_const(pos, v, ty, &vm);
        }
    }

    /// Gets function `entry` ready to run on the VM: `funcs` are functions made
    /// for this run, and everything they call is looked up and compiled. Fails
    /// if any of it cannot run while compiling.
    pub(crate) fn prepare(
        &self,
        pos: Pos,
        mut funcs: Vec<jihoo_ir::Function>,
        entry: &str,
    ) -> Result<(Rc<jihoo_ir::Module>, Rc<Program>), Error> {
        // Only what no earlier run needed: everything a function in the
        // compile-time module calls is in it already. Nothing is added unless
        // all of it can be, so that stays true when a callee has errors.
        let known = |name: &str| self.comptime.borrow().has(name);
        if funcs.is_empty() && !known(entry) {
            funcs.push((*self.function(entry)?).clone());
        }
        let mut seen: HashSet<String> = funcs.iter().map(|f| f.name.clone()).collect();
        let mut i = 0;
        while i < funcs.len() {
            for callee in callees(&funcs[i]) {
                if known(&callee) || !seen.insert(callee.clone()) {
                    continue;
                }
                if self.is_extern_symbol(&callee) {
                    return Err(Error::new(
                        pos,
                        format!("cannot call `{callee}` at compile time: it is a C function, and only exists in the built program"),
                    ));
                }
                if self.function_in_progress(&callee) {
                    return Err(Error::new(
                        pos,
                        format!("cannot call `{callee}` at compile time here: `{callee}` is still being compiled"),
                    ));
                }
                let f = self.function(&callee).map_err(|_| {
                    Error::new(pos, format!("cannot evaluate this at compile time: `{callee}` has errors"))
                })?;
                funcs.push((*f).clone());
            }
            i += 1;
        }

        self.comptime.borrow_mut().add(funcs);
        let ct = self.comptime.borrow();
        Ok((ct.module.clone(), ct.program.clone()))
    }

    pub(crate) fn to_const(&self, pos: Pos, v: Value, ty: &Type, vm: &Vm) -> Result<ConstValue, Error> {
        let heap = vm.heap();
        Ok(match (v, ty) {
            (Value::Unit, Type::Unit) => ConstValue::Unit,
            (Value::Int(n), Type::Int(_)) => ConstValue::Int(n),
            (Value::Float(x), Type::Float(_)) => ConstValue::Float(x),
            (Value::Bool(b), Type::Bool) => ConstValue::Bool(b),
            (Value::Str(r), t) if *t == Type::Str || t.is_code() => match std::str::from_utf8(heap.bytes(r)) {
                Ok(s) => ConstValue::Str(s.to_string()),
                // String literals are UTF-8, so a computed one has to be too.
                Err(_) => return Err(Error::new(pos, "a str computed at compile time must be valid UTF-8 (a slice cut a character)")),
            },
            (Value::Agg(r), Type::Struct(_)) => {
                let fields = heap.items(r).to_vec();
                let tys: Vec<Type> = (0..fields.len() as u32).map(|i| self.field_type(ty, i)).collect();
                let items = fields.into_iter().zip(&tys).map(|(v, t)| self.to_const(pos, v, t, vm));
                ConstValue::Agg(items.collect::<Result<_, _>>()?)
            }
            (Value::Agg(r), Type::Array(elem, _) | Type::List(elem)) => {
                let items = heap.items(r).to_vec().into_iter().map(|v| self.to_const(pos, v, elem, vm));
                ConstValue::Agg(items.collect::<Result<_, _>>()?)
            }
            (Value::Func(i), Type::Fn(..)) => ConstValue::Func(vm.func_name(i).to_string()),
            (Value::Chan(_), _) => {
                return Err(Error::new(pos, "a channel cannot be computed at compile time"));
            }
            (_, Type::Cell(_)) => {
                return Err(Error::new(pos, "a cell cannot be computed at compile time"));
            }
            (Value::Agg(_), Type::Fn(..)) => {
                return Err(Error::new(
                    pos,
                    "a closure that captures values cannot be computed at compile time; only functions can",
                ))
            }
            // A ref becomes a one-element aggregate; sharing is not kept, which
            // cannot be observed since refs are immutable.
            (Value::Agg(r), Type::Ref(inner)) => {
                let v = heap.items(r)[0];
                ConstValue::Agg(vec![self.to_const(pos, v, inner, vm)?])
            }
            (Value::Agg(r), Type::Enum(_)) => {
                let items = heap.items(r).to_vec();
                let Value::Int(tag) = items[0] else { unreachable!("enum without a tag") };
                let tys = self.payload_types(ty, tag as u32);
                let mut out = vec![ConstValue::Int(tag)];
                for (v, t) in items[1..].iter().zip(&tys) {
                    out.push(self.to_const(pos, *v, t, vm)?);
                }
                ConstValue::Agg(out)
            }
            (_, Type::Ptr(_)) => {
                return Err(Error::new(
                    pos,
                    format!("a value of type {ty} cannot be computed at compile time: pointers do not exist while compiling"),
                ))
            }
            (v, ty) => unreachable!("VM produced {v:?} for {ty}"),
        })
    }
}

/// `a` on `vm`'s heap, to pass as an argument: a macro's code arguments,
/// numbers, bools and `str`, and the values contracts and rules are tried
/// with.
pub(crate) fn arg(vm: &mut Vm, a: &ConstValue) -> Value {
    match a {
        ConstValue::Unit => Value::Unit,
        ConstValue::Int(n) => Value::Int(*n),
        ConstValue::Float(x) => Value::Float(*x),
        ConstValue::Bool(b) => Value::Bool(*b),
        ConstValue::Str(s) => vm.alloc_string(s),
        // Structs, arrays, lists, enums and refs are all aggregates.
        ConstValue::Agg(items) => {
            let items = items.iter().map(|x| arg(vm, x)).collect();
            vm.alloc_list(items)
        }
        // The functions rules are checked with (`rules.rs`).
        ConstValue::Func(name) => vm.func_value(name).expect("a function the run has"),
    }
}

fn callees(f: &jihoo_ir::Function) -> Vec<String> {
    let mut out = Vec::new();
    for b in &f.blocks {
        for inst in &b.insts {
            if let Inst::Call { func, .. } | Inst::FuncRef { func, .. } | Inst::Closure { func, .. } = inst {
                out.push(func.clone());
            }
        }
        if let jihoo_ir::Terminator::TailCall { func, .. } = &b.term {
            out.push(func.clone());
        }
    }
    out
}

impl FnCx<'_> {
    /// Emits IR that rebuilds the compile-time value `v` of type `ty`.
    pub(crate) fn splice(&mut self, ty: &Type, v: &ConstValue) -> Reg {
        match (ty, v) {
            (_, ConstValue::Unit) => self.unit(),
            (_, ConstValue::Int(n)) => self.konst(ty.clone(), *n),
            (&Type::Float(t), ConstValue::Float(x)) => self.fconst(t, f64::from_bits(*x)),
            (_, ConstValue::Bool(b)) => self.konst(Type::Bool, *b as i64),
            (_, ConstValue::Str(s)) => self.emit_to(Type::Str, |dst| Inst::Str { dst, value: s.clone() }),
            (_, ConstValue::Func(f)) => self.emit_to(ty.clone(), |dst| Inst::FuncRef { dst, func: f.clone() }),
            (Type::Struct(name), ConstValue::Agg(items)) => {
                let fields = (0..items.len() as u32)
                    .map(|i| {
                        let fty = self.env.field_type(ty, i);
                        self.splice(&fty, &items[i as usize])
                    })
                    .collect();
                self.emit_to(ty.clone(), |dst| Inst::Struct { dst, name: name.clone(), fields })
            }
            (Type::Ref(inner), ConstValue::Agg(items)) => {
                let src = self.splice(inner, &items[0]);
                self.emit_to(ty.clone(), |dst| Inst::Ref { dst, src })
            }
            (Type::Enum(_), ConstValue::Agg(items)) => {
                let ConstValue::Int(tag) = items[0] else { unreachable!("enum without a tag") };
                let tys = self.env.payload_types(ty, tag as u32);
                let fields = items[1..].iter().zip(&tys).map(|(v, t)| self.splice(t, v)).collect();
                self.emit_to(ty.clone(), |dst| Inst::Variant { dst, index: tag as u32, fields })
            }
            (Type::Array(elem, _), ConstValue::Agg(items)) => {
                // `[0; 4096]` stays one `splat` instead of 4096 constants.
                if items.len() > 1 && items.iter().all(|x| *x == items[0]) {
                    let value = self.splice(elem, &items[0]);
                    return self.emit_to(ty.clone(), |dst| Inst::Splat { dst, value });
                }
                let items = items.iter().map(|x| self.splice(elem, x)).collect();
                self.emit_to(ty.clone(), |dst| Inst::Array { dst, items })
            }
            (Type::List(elem), ConstValue::Agg(items)) => {
                let items = items.iter().map(|x| self.splice(elem, x)).collect();
                self.emit_to(ty.clone(), |dst| Inst::Array { dst, items })
            }
            (ty, v) => unreachable!("{v:?} is not a value of type {ty}"),
        }
    }
}
