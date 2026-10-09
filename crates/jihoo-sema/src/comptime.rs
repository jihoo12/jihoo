//! Compile-time evaluation.
//!
//! `comptime e` (and every `const` and non-literal array length) is lowered to a
//! helper function `comptime.N` that returns `e`. That function, together with
//! every function it can call, is run on the VM — for freestanding programs too.
//! The result is turned back into IR constants where the expression was used.
//!
//! Compile-time code cannot use what the VM does not have: pointers and `syscall`.
//! `print` works and writes to the compiler's stderr.

use std::collections::HashSet;
use std::rc::Rc;

use jihoo_ir::{Inst, Reg, Terminator, Type};
use jihoo_syntax::ast::Expr;
use jihoo_syntax::{Error, Pos};
use jihoo_vm::{Value, Vm};

use crate::env::{Env, Sig};
use crate::generic::Bindings;
use crate::FnCx;

/// Instruction budget for one compile-time evaluation.
const FUEL: u64 = if cfg!(test) { 1_000_000 } else { 100_000_000 };

/// A value computed at compile time, independent of the VM that computed it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConstValue {
    Unit,
    Int(i64),
    Bool(bool),
    Str(String),
    /// Fields of a struct or elements of an array.
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
    /// `ty`. `funcs` are functions that exist only for this run (such as a
    /// `comptime` helper); everything they call is looked up and compiled.
    pub fn run(
        &self,
        pos: Pos,
        mut funcs: Vec<jihoo_ir::Function>,
        entry: &str,
        args: &[ConstValue],
        ty: &Type,
    ) -> Result<ConstValue, Error> {
        if funcs.is_empty() {
            funcs.push((*self.function(entry)?).clone());
        }
        let mut seen: HashSet<String> = funcs.iter().map(|f| f.name.clone()).collect();
        let mut i = 0;
        while i < funcs.len() {
            for callee in callees(&funcs[i]) {
                if !seen.insert(callee.clone()) {
                    continue;
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

        let module = jihoo_ir::Module { profile: self.profile, structs: vec![], funcs };
        let mut vm = Vm::new(&module).with_fuel(FUEL).with_uniques(self.uniques.get());
        let mut values = Vec::new();
        for a in args {
            values.push(match a {
                ConstValue::Unit => Value::Unit,
                ConstValue::Int(n) => Value::Int(*n),
                ConstValue::Bool(b) => Value::Bool(*b),
                ConstValue::Str(s) => vm.alloc_string(s),
                ConstValue::Agg(_) | ConstValue::Func(_) => unreachable!("macro arguments are code, integers, bools or str"),
            });
        }
        let mut out = Vec::new();
        let result = vm.call_named(entry, &values, &mut out);
        self.uniques.set(vm.uniques());
        if !out.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&out));
        }
        let v = result.map_err(|err| {
            let at = if err.func.starts_with("comptime.") { String::new() } else { format!(" in `{}`", err.func) };
            Error::new(pos, format!("compile-time evaluation failed{at}: {}", err.msg))
        })?;
        self.to_const(pos, v, ty, &vm)
    }

    fn to_const(&self, pos: Pos, v: Value, ty: &Type, vm: &Vm) -> Result<ConstValue, Error> {
        let heap = vm.heap();
        Ok(match (v, ty) {
            (Value::Unit, Type::Unit) => ConstValue::Unit,
            (Value::Int(n), Type::Int(_)) => ConstValue::Int(n),
            (Value::Bool(b), Type::Bool) => ConstValue::Bool(b),
            (Value::Str(r), t) if *t == Type::Str || t.is_code() => ConstValue::Str(heap.str(r).to_string()),
            (Value::Agg(r), Type::Struct(_)) => {
                let fields = heap.items(r).to_vec();
                let tys: Vec<Type> = (0..fields.len() as u32).map(|i| self.field_type(ty, i)).collect();
                let items = fields.into_iter().zip(&tys).map(|(v, t)| self.to_const(pos, v, t, vm));
                ConstValue::Agg(items.collect::<Result<_, _>>()?)
            }
            (Value::Agg(r), Type::Array(elem, _)) => {
                let items = heap.items(r).to_vec().into_iter().map(|v| self.to_const(pos, v, elem, vm));
                ConstValue::Agg(items.collect::<Result<_, _>>()?)
            }
            (Value::Func(i), Type::Fn(..)) => ConstValue::Func(vm.func_name(i).to_string()),
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

fn callees(f: &jihoo_ir::Function) -> Vec<String> {
    let mut out = Vec::new();
    for b in &f.blocks {
        for inst in &b.insts {
            if let Inst::Call { func, .. } | Inst::FuncRef { func, .. } = inst {
                out.push(func.clone());
            }
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
            (Type::Array(elem, _), ConstValue::Agg(items)) => {
                // `[0; 4096]` stays one `splat` instead of 4096 constants.
                if items.len() > 1 && items.iter().all(|x| *x == items[0]) {
                    let value = self.splice(elem, &items[0]);
                    return self.emit_to(ty.clone(), |dst| Inst::Splat { dst, value });
                }
                let items = items.iter().map(|x| self.splice(elem, x)).collect();
                self.emit_to(ty.clone(), |dst| Inst::Array { dst, items })
            }
            (ty, v) => unreachable!("{v:?} is not a value of type {ty}"),
        }
    }
}
