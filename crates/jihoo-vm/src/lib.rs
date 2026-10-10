//! Register VM that executes JIR directly (the hosted profile).
//!
//! Tasks (`go f(x)`) run on one OS thread, interleaved by a deterministic
//! round-robin scheduler: a task runs until it waits on a channel, finishes, or
//! has run `TIME_SLICE` instructions and reaches a *safepoint*: a call, or a
//! jump back to an earlier block. The frontend numbers blocks in breadth-first
//! order, so every loop has such a jump. Code without calls and loops therefore
//! never interleaves with other tasks: `*c = *c + 1` on a cell is atomic. The
//! same program always prints the same thing. When the first task (`main`)
//! returns, the run ends, whatever the other tasks are doing; when every task
//! waits on a channel, it is a deadlock.

pub mod gc;
mod program;

use std::collections::VecDeque;
use std::fmt;
use std::io::Write;
use std::rc::Rc;

use gc::{GcRef, Heap, Waiter};
use program::{int_binary, Op};
pub use program::Program;
use jihoo_ir::{BinOp, FloatTy, Function, Inst, IntTy, Module, PathStep, Profile, Reg, SelectCase, Terminator, Type, UnOp};

const MAX_CALL_DEPTH: usize = 10_000;
/// Instructions a task runs before the next ready task gets its turn, at the
/// next safepoint.
const TIME_SLICE: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Unit,
    /// Every integer type, in canonical form (see `IntTy::wrap`).
    Int(i64),
    /// Every float type, as the bits of an `f64` (so values stay `Eq`); an
    /// `f32` is an `f64` exactly representable as `f32` (see `FloatTy::round`).
    Float(u64),
    Bool(bool),
    Str(GcRef),
    /// A struct or an array.
    Agg(GcRef),
    /// A function value: an index into the module's functions.
    Func(u32),
    Chan(GcRef),
}

impl Value {
    pub fn float(v: f64) -> Value {
        Value::Float(v.to_bits())
    }

    /// The heap object this value refers to, if any.
    pub fn gc_ref(&self) -> Option<GcRef> {
        match self {
            Value::Str(r) | Value::Agg(r) | Value::Chan(r) => Some(*r),
            Value::Unit | Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Func(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VmError {
    pub func: String,
    pub msg: String,
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime error in @{}: {}", self.func, self.msg)
    }
}

impl std::error::Error for VmError {}

struct Frame {
    func: usize,
    block: usize,
    ip: usize,
    regs: Vec<Value>,
    /// Caller register that receives the return value.
    ret_dst: Option<Reg>,
}

/// A task that is not running. The running task's frames are in `Vm::stack`.
#[derive(Default)]
struct Task {
    stack: Vec<Frame>,
    /// What the task waits for, if it does.
    waiting: Option<Wait>,
}

/// A task waiting on channels: its waiters there carry `token`.
struct Wait {
    token: u64,
    chans: Vec<GcRef>,
}

pub struct Vm<'m> {
    module: &'m Module,
    /// The module prepared to run (see `program.rs`).
    program: Rc<Program>,
    heap: Heap,
    /// The frames of the running task.
    stack: Vec<Frame>,
    /// Every task by number; the entry of the running one has an empty stack.
    tasks: Vec<Task>,
    current: usize,
    /// Tasks ready to run, in the order they get their turn.
    ready: VecDeque<usize>,
    /// Instructions left in the running task's time slice.
    slice: u32,
    /// Set when the running task starts waiting on a channel.
    blocked: bool,
    /// The token of the next wait.
    next_token: u64,
    /// Objects made in the middle of an instruction that are not in a register
    /// yet; they are roots until the instruction ends.
    temp_roots: Vec<Value>,
    /// Values handed out to the embedder (`alloc_string`), kept alive for as long
    /// as the VM lives: between allocating an argument and passing it to
    /// `call_named`, no frame holds it.
    pinned: Vec<Value>,
    /// Register vectors of returned frames, reused for new ones so that a
    /// call allocates nothing.
    reg_pool: Vec<Vec<Value>>,
    /// Check every in-place update (`JIHOO_VM_CHECK`, see `check_in_place`).
    check: bool,
    /// Instructions left before execution is stopped, if limited.
    fuel: Option<u64>,
    /// The next number `unique` hands out.
    uniques: u64,
}

/// Runs `main` of a hosted module and returns its result.
pub fn run(module: &Module, out: &mut dyn Write) -> Result<i64, VmError> {
    Vm::new(module).run_main(out)
}

impl<'m> Vm<'m> {
    pub fn new(module: &'m Module) -> Self {
        Vm::with_program(module, Rc::new(Program::of(module)))
    }

    /// A VM for `module` that runs `program`, which must be made from exactly
    /// its functions. Compile-time evaluation keeps one that grows with its
    /// module, so that a new VM costs nothing per function.
    pub fn with_program(module: &'m Module, program: Rc<Program>) -> Self {
        debug_assert_eq!(program.len(), module.funcs.len(), "the program does not match the module");
        Vm {
            module,
            program,
            heap: Heap::default(),
            stack: Vec::new(),
            tasks: Vec::new(),
            current: 0,
            ready: VecDeque::new(),
            slice: TIME_SLICE,
            blocked: false,
            next_token: 0,
            temp_roots: Vec::new(),
            pinned: Vec::new(),
            reg_pool: Vec::new(),
            check: std::env::var_os("JIHOO_VM_CHECK").is_some_and(|v| !v.is_empty() && v != "0"),
            fuel: None,
            uniques: 0,
        }
    }

    /// Starts `unique` at `next`, so that several runs never repeat a name.
    pub fn with_uniques(mut self, next: u64) -> Self {
        self.uniques = next;
        self
    }

    /// The next number `unique` would hand out.
    pub fn uniques(&self) -> u64 {
        self.uniques
    }

    /// Checks every in-place update, whatever `JIHOO_VM_CHECK` says.
    pub fn with_check(mut self, on: bool) -> Self {
        self.check = on;
        self
    }

    /// Stops execution with an error after `steps` instructions. Used for
    /// compile-time evaluation, so an endless loop cannot hang the compiler.
    pub fn with_fuel(mut self, steps: u64) -> Self {
        self.fuel = Some(steps);
        self
    }

    /// Allocates a string on this VM's heap, to pass as an argument. It stays
    /// alive until the VM is dropped.
    pub fn alloc_string(&mut self, s: &str) -> Value {
        let v = Value::Str(self.alloc_str(s));
        self.pinned.push(v);
        v
    }

    /// Calls the function `name` with `args`, regardless of the module's profile.
    /// Instructions the VM cannot run (pointers, `syscall`) are runtime errors.
    pub fn call_named(&mut self, name: &str, args: &[Value], out: &mut dyn Write) -> Result<Value, VmError> {
        let func = self
            .program
            .get(name)
            .ok_or_else(|| VmError { func: name.into(), msg: "no such function".into() })?;
        self.call(func, args, out)
    }

    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }

    /// The function value for function `name`, if the module has it.
    pub fn func_value(&self, name: &str) -> Option<Value> {
        self.program.get(name).map(|i| Value::Func(i as u32))
    }

    /// The index of function `name`, which an instruction names. Every function
    /// a module calls is in it: the verifier checks a program, and compile-time
    /// evaluation adds what it calls.
    #[inline]
    fn func_index(&self, name: &str) -> usize {
        self.program.get(name).unwrap_or_else(|| panic!("no function `{name}` in the module"))
    }

    /// The name of the function a `Value::Func` refers to.
    pub fn func_name(&self, index: u32) -> &'m str {
        &self.module.funcs[index as usize].name
    }

    pub fn run_main(&mut self, out: &mut dyn Write) -> Result<i64, VmError> {
        let err = |msg: &str| VmError { func: "main".into(), msg: msg.into() };
        if self.module.profile != Profile::Hosted {
            let msg = format!("{} modules cannot run on the VM; use `jihoo build`", self.module.profile.as_str());
            return Err(err(&msg));
        }
        let main = self.program.get("main").ok_or_else(|| err("no `main` function"))?;
        match self.call(main, &[], out)? {
            Value::Int(n) => Ok(n),
            Value::Unit => Ok(0),
            _ => Err(err("`main` must return unit or an integer")),
        }
    }

    /// Runs `func` as the first task until it returns. Tasks it starts run
    /// interleaved with it and are dropped when it returns.
    fn call(&mut self, func: usize, args: &[Value], out: &mut dyn Write) -> Result<Value, VmError> {
        self.stack.clear();
        self.tasks = vec![Task::default()];
        self.current = 0;
        self.ready.clear();
        self.slice = TIME_SLICE;
        self.push_frame(func, args, None)?;
        let module = self.module;
        let program = self.program.clone();

        loop {
            if let Some(fuel) = &mut self.fuel {
                if *fuel == 0 {
                    return Err(self.error("evaluation did not finish (step limit reached)"));
                }
                *fuel -= 1;
            }
            self.slice = self.slice.saturating_sub(1);

            let frame = self.stack.last_mut().unwrap();
            let ops = program.block(frame.func, frame.block);
            if frame.ip < ops.len() {
                let ip = frame.ip;
                frame.ip += 1;
                match &ops[ip] {
                    Op::Const { dst, value } => self.set(*dst, *value),
                    Op::Copy { dst, src } => {
                        let v = self.get(*src);
                        if dst != src {
                            self.share(v);
                        }
                        self.set(*dst, v);
                    }
                    Op::Int { dst, op, ty, lhs, rhs } => {
                        let (x, y) = (self.int(*lhs)?, self.int(*rhs)?);
                        let Some(v) = int_binary(*op, *ty, x, y) else {
                            return Err(self.error("division by zero"));
                        };
                        self.set(*dst, v);
                    }
                    Op::Call { dst, func, args } => {
                        let frame = self.call_frame(*func as usize, &[], args, Some(*dst));
                        self.push(frame)?;
                        self.safepoint()?;
                    }
                    Op::FuncRef { dst, func } => self.set(*dst, Value::Func(*func)),
                    Op::Inst => {
                        let f = &module.funcs[frame.func];
                        let inst = &f.blocks[frame.block].insts[ip];
                        self.exec(f, inst, out)?;
                        if self.blocked {
                            self.blocked = false;
                            self.switch_task()?;
                        } else if matches!(inst, Inst::CallIndirect { .. }) {
                            self.safepoint()?;
                        }
                    }
                }
                continue;
            }

            let from = frame.block;
            match &module.funcs[frame.func].blocks[from].term {
                Terminator::Jump(b) => {
                    frame.block = b.0 as usize;
                    frame.ip = 0;
                    if frame.block <= from {
                        self.safepoint()?;
                    }
                }
                Terminator::Branch { cond, then, els } => {
                    let c = self.bool(*cond)?;
                    let frame = self.stack.last_mut().unwrap();
                    frame.block = if c { then.0 } else { els.0 } as usize;
                    frame.ip = 0;
                    if frame.block <= from {
                        self.safepoint()?;
                    }
                }
                Terminator::Unreachable => return Err(self.error("reached `unreachable`")),
                Terminator::Ret(r) => {
                    let v = frame.regs[r.0 as usize];
                    let done = self.stack.pop().unwrap();
                    self.reg_pool.push(done.regs);
                    if let Some(caller) = self.stack.last_mut() {
                        caller.regs[done.ret_dst.unwrap().0 as usize] = v;
                    } else if self.current == 0 {
                        self.tasks.clear();
                        self.ready.clear();
                        return Ok(v);
                    } else {
                        // A started task is done; its result is dropped.
                        self.switch_task()?;
                    }
                }
            }
        }
    }

    /// Gives way to the next ready task if the running one has used its time
    /// slice. Called only at safepoints: calls and jumps back.
    fn safepoint(&mut self) -> Result<(), VmError> {
        if self.slice == 0 {
            self.slice = TIME_SLICE;
            if !self.ready.is_empty() {
                self.ready.push_back(self.current);
                self.switch_task()?;
            }
        }
        Ok(())
    }

    /// Puts the running task aside and runs the next ready one.
    fn switch_task(&mut self) -> Result<(), VmError> {
        let Some(next) = self.ready.pop_front() else {
            return Err(self.error("deadlock: every task is waiting on a channel"));
        };
        std::mem::swap(&mut self.stack, &mut self.tasks[self.current].stack);
        self.current = next;
        std::mem::swap(&mut self.stack, &mut self.tasks[next].stack);
        self.slice = TIME_SLICE;
        Ok(())
    }

    /// Makes the running task wait: as a receiver on each channel in `recvs`
    /// (with the register the value goes to) and as a sender on each one in
    /// `sends` (with the value). `choice` is the register that gets the index
    /// of the case that goes ahead, for a `select`; recvs come first, then sends.
    fn wait(&mut self, recvs: &[(GcRef, Reg, i64)], sends: &[(GcRef, Value, i64)], choice: Option<Reg>) {
        let token = self.next_token;
        self.next_token += 1;
        let task = self.current;
        let waiter = |dst, i| Waiter { task, token, dst, choice: choice.map(|r| (r, i)) };
        for &(c, dst, i) in recvs {
            self.heap.chan_mut(c).receivers.push_back(waiter(Some(dst), i));
        }
        for &(c, v, i) in sends {
            self.heap.chan_mut(c).senders.push_back((waiter(None, i), v));
        }
        let chans = recvs.iter().map(|r| r.0).chain(sends.iter().map(|s| s.0)).collect();
        self.tasks[task].waiting = Some(Wait { token, chans });
        self.blocked = true;
    }

    /// Lets waiter `w` go ahead (with the value it receives, if any) and makes
    /// its task ready to run again. Its waiters on other channels are removed.
    fn complete(&mut self, w: Waiter, value: Option<Value>) {
        let frame = self.tasks[w.task].stack.last_mut().unwrap();
        if let (Some(dst), Some(v)) = (w.dst, value) {
            frame.regs[dst.0 as usize] = v;
        }
        if let Some((r, i)) = w.choice {
            frame.regs[r.0 as usize] = Value::Int(i);
        }
        if let Some(wait) = self.tasks[w.task].waiting.take() {
            for c in wait.chans {
                let ch = self.heap.chan_mut(c);
                ch.receivers.retain(|r| r.token != wait.token);
                ch.senders.retain(|(s, _)| s.token != wait.token);
            }
        }
        self.ready.push_back(w.task);
    }

    /// Sends `v` on `c` if that needs no waiting.
    fn try_send(&mut self, c: GcRef, v: Value) -> bool {
        let ch = self.heap.chan_mut(c);
        if let Some(w) = ch.receivers.pop_front() {
            // A receiver is waiting: hand the value over.
            self.complete(w, Some(v));
        } else if ch.buf.len() < ch.cap {
            ch.buf.push_back(v);
        } else {
            return false;
        }
        true
    }

    /// Receives from `c` if that needs no waiting.
    fn try_recv(&mut self, c: GcRef) -> Option<Value> {
        let ch = self.heap.chan_mut(c);
        if let Some(v) = ch.buf.pop_front() {
            // Room in the buffer: the first waiting sender puts its value in.
            if let Some((w, sv)) = ch.senders.pop_front() {
                ch.buf.push_back(sv);
                self.complete(w, None);
            }
            Some(v)
        } else if let Some((w, v)) = ch.senders.pop_front() {
            // No buffer: take the value straight from a waiting sender.
            self.complete(w, None);
            Some(v)
        } else {
            None
        }
    }

    fn chan(&self, r: Reg, what: &str) -> Result<GcRef, VmError> {
        match self.get(r) {
            Value::Chan(c) => Ok(c),
            _ => Err(self.error(&format!("`{what}` needs a channel"))),
        }
    }

    fn frame(&mut self, func: usize, args: &[Value], ret_dst: Option<Reg>) -> Frame {
        let mut regs = self.regs_for(func);
        regs[..args.len()].copy_from_slice(args);
        Frame { func, block: 0, ip: 0, regs, ret_dst }
    }

    /// Registers for a new frame of `func`, all unit, from the pool if it can.
    #[inline]
    fn regs_for(&mut self, func: usize) -> Vec<Value> {
        let mut regs = self.reg_pool.pop().unwrap_or_default();
        regs.clear();
        regs.resize(self.module.funcs[func].regs.len(), Value::Unit);
        regs
    }

    fn push_frame(&mut self, func: usize, args: &[Value], ret_dst: Option<Reg>) -> Result<(), VmError> {
        let frame = self.frame(func, args, ret_dst);
        self.push(frame)
    }

    #[inline]
    fn push(&mut self, frame: Frame) -> Result<(), VmError> {
        if self.stack.len() >= MAX_CALL_DEPTH {
            return Err(self.error("stack overflow"));
        }
        self.stack.push(frame);
        Ok(())
    }

    /// A frame calling `func` with `first` (a closure's captured values) and
    /// then the values of `args`, which get a second reference.
    #[inline]
    fn call_frame(&mut self, func: usize, first: &[Value], args: &[Reg], ret_dst: Option<Reg>) -> Frame {
        let mut regs = self.regs_for(func);
        regs[..first.len()].copy_from_slice(first);
        for (i, r) in args.iter().enumerate() {
            regs[first.len() + i] = self.shared(*r);
        }
        Frame { func, block: 0, ip: 0, regs, ret_dst }
    }

    /// The function a function value calls, and the arguments it passes first
    /// (a closure's captured values).
    fn callee(&mut self, callee: Reg) -> Result<(usize, Vec<Value>), VmError> {
        match self.get(callee) {
            Value::Func(i) => Ok((i as usize, Vec::new())),
            Value::Agg(r) => {
                let items = self.heap.items(r);
                let Value::Func(i) = items[0] else { return Err(self.error("malformed closure")) };
                // The closure keeps its captured values: the call gets copies.
                let captured = items[1..].to_vec();
                for v in &captured {
                    self.share(*v);
                }
                Ok((i as usize, captured))
            }
            _ => Err(self.error("called a value that is not a function")),
        }
    }

    fn exec(&mut self, f: &'m Function, inst: &'m Inst, out: &mut dyn Write) -> Result<(), VmError> {
        match inst {
            // Prepared as `Op`s and run by `call`.
            Inst::Const { .. } | Inst::FConst { .. } | Inst::Copy { .. } | Inst::Call { .. } | Inst::FuncRef { .. } => {
                unreachable!("prepared as an `Op` and run by `call`")
            }
            Inst::Unit { dst } => self.set(*dst, Value::Unit),
            Inst::Str { dst, value } => {
                let r = self.alloc_str(value);
                self.set(*dst, Value::Str(r));
            }
            Inst::Unary { dst, op, src } => {
                let v = match (op, f.reg_type(*dst)) {
                    (UnOp::Neg, Type::Int(t)) => Value::Int(t.wrap(self.int(*src)?.wrapping_neg())),
                    (UnOp::Neg, Type::Float(_)) => Value::float(-self.float(*src)?),
                    (UnOp::Not, Type::Int(t)) => Value::Int(t.wrap(!self.int(*src)?)),
                    (UnOp::Not, _) => Value::Bool(!self.bool(*src)?),
                    (UnOp::Neg, t) => return Err(self.error(&format!("cannot negate {t}"))),
                };
                self.set(*dst, v);
            }
            Inst::Binary { dst, op, lhs, rhs } => {
                let v = self.binary(*op, f.reg_type(*lhs), self.get(*lhs), self.get(*rhs))?;
                self.set(*dst, v);
            }
            Inst::Cast { dst, src } => {
                let v = match (f.reg_type(*dst), self.get(*src)) {
                    (Type::Int(t), Value::Int(n)) => Value::Int(t.wrap(n)),
                    (Type::Int(_), Value::Bool(b)) => Value::Int(b as i64),
                    (&Type::Float(to), Value::Int(n)) => Value::float(int_to_float(f.reg_type(*src), n, to)),
                    (&Type::Float(to), Value::Float(x)) => Value::float(to.round(f64::from_bits(x))),
                    (&Type::Int(to), Value::Float(x)) => Value::Int(float_to_int(f64::from_bits(x), to)),
                    (_, v) => v, // a cast to the same type
                };
                self.set(*dst, v);
            }
            Inst::Struct { dst, fields, .. } => {
                let values = fields.iter().map(|r| self.shared(*r)).collect();
                let r = self.alloc_agg(values);
                self.set(*dst, Value::Agg(r));
            }
            Inst::Field { dst, src, index } => {
                let r = self.agg_ref(*src)?;
                let v = self.heap.items(r)[*index as usize];
                let v = self.share(v);
                self.set(*dst, v);
            }
            Inst::SetField { dst, src, index, value } => {
                let r = self.agg_ref(*src)?;
                let v = self.shared(*value);
                self.update(*dst, *src, r, *index as usize, v);
            }
            Inst::GetPath { dst, src, path } => {
                let v = self.get_in(self.get(*src), path)?;
                self.set(*dst, v);
            }
            Inst::SetPath { dst, src, path, value } => {
                let v = self.shared(*value);
                let new = self.set_in(self.get(*src), path, v, dst == src)?;
                self.set(*dst, new);
            }
            // A cell is a one-element aggregate that is changed in place.
            Inst::NewCell { dst, value } => {
                let v = self.shared(*value);
                let r = self.alloc_agg(vec![v]);
                self.set(*dst, Value::Agg(r));
            }
            Inst::CellGet { dst, cell, path } => {
                let c = self.agg_ref(*cell)?;
                let v = self.get_in(self.heap.items(c)[0], path)?;
                self.set(*dst, v);
            }
            Inst::CellSet { cell, path, value } => {
                let c = self.agg_ref(*cell)?;
                let v = self.shared(*value);
                // The cell is the one reference to what it holds, which may be
                // updated in place unless shared. The cell keeps everything rooted.
                let new = if path.is_empty() { v } else { self.set_in(self.heap.items(c)[0], path, v, true)? };
                self.heap.items_mut(c)[0] = new;
            }
            Inst::Array { dst, items } => {
                let values = items.iter().map(|r| self.shared(*r)).collect();
                let r = self.alloc_agg(values);
                self.set(*dst, Value::Agg(r));
            }
            Inst::Splat { dst, value } => {
                let Type::Array(_, n) = f.reg_type(*dst) else {
                    return Err(self.error("`splat` needs an array register"));
                };
                let v = self.shared(*value);
                let r = self.alloc_agg(vec![v; *n as usize]);
                self.set(*dst, Value::Agg(r));
            }
            // An enum is an aggregate of its tag followed by the variant's payload.
            Inst::Variant { dst, index, fields } => {
                let mut values = Vec::with_capacity(fields.len() + 1);
                values.push(Value::Int(*index as i64));
                values.extend(fields.iter().map(|r| self.shared(*r)));
                let r = self.alloc_agg(values);
                self.set(*dst, Value::Agg(r));
            }
            // A ref is a one-element aggregate: immutable, like every heap object.
            Inst::Ref { dst, src } => {
                let v = self.shared(*src);
                let r = self.alloc_agg(vec![v]);
                self.set(*dst, Value::Agg(r));
            }
            Inst::Deref { dst, src } => {
                let v = self.heap.items(self.agg_ref(*src)?)[0];
                let v = self.share(v);
                self.set(*dst, v);
            }
            Inst::Tag { dst, src } => {
                let tag = self.heap.items(self.agg_ref(*src)?)[0];
                self.set(*dst, tag);
            }
            Inst::Payload { dst, src, variant, index } => {
                let items = self.heap.items(self.agg_ref(*src)?);
                if items[0] != Value::Int(*variant as i64) {
                    return Err(self.error(&format!("read the payload of variant {variant} from another variant")));
                }
                let v = items[1 + *index as usize];
                let v = self.share(v);
                self.set(*dst, v);
            }
            Inst::Elem { dst, src, index } => {
                let r = self.agg_ref(*src)?;
                let i = self.bounds_check(r, *index)?;
                let v = self.heap.items(r)[i];
                let v = self.share(v);
                self.set(*dst, v);
            }
            Inst::SetElem { dst, src, index, value } => {
                let r = self.agg_ref(*src)?;
                let i = self.bounds_check(r, *index)?;
                let v = self.shared(*value);
                self.update(*dst, *src, r, i, v);
            }
            Inst::Load { .. }
            | Inst::Store { .. }
            | Inst::Addr { .. }
            | Inst::FieldPtr { .. }
            | Inst::ElemPtr { .. } => {
                return Err(self.error("pointers are not available on the VM"))
            }
            // A closure is an aggregate of the function and the captured values,
            // which become its first arguments.
            Inst::Closure { dst, func, captures } => {
                let mut values = vec![Value::Func(self.func_index(func) as u32)];
                values.extend(captures.iter().map(|r| self.shared(*r)));
                let r = self.alloc_agg(values);
                self.set(*dst, Value::Agg(r));
            }
            Inst::CallIndirect { dst, callee, args } => {
                let (callee, captured) = self.callee(*callee)?;
                let frame = self.call_frame(callee, &captured, args, Some(*dst));
                self.push(frame)?;
            }
            Inst::Spawn { callee, args } => {
                let (callee, captured) = self.callee(*callee)?;
                let frame = self.call_frame(callee, &captured, args, None);
                self.tasks.push(Task { stack: vec![frame], waiting: None });
                self.ready.push_back(self.tasks.len() - 1);
            }
            Inst::NewChan { dst, cap } => {
                let cap = self.int(*cap)?;
                let cap = usize::try_from(cap).map_err(|_| self.error(&format!("channel capacity {cap} is negative")))?;
                let r = self.alloc_chan(cap);
                self.set(*dst, Value::Chan(r));
            }
            Inst::Send { chan, value } => {
                let c = self.chan(*chan, "send")?;
                let v = self.shared(*value);
                if !self.try_send(c, v) {
                    self.wait(&[], &[(c, v, 0)], None);
                }
            }
            Inst::Recv { dst, chan } => {
                let c = self.chan(*chan, "recv")?;
                match self.try_recv(c) {
                    Some(v) => self.set(*dst, v),
                    None => self.wait(&[(c, *dst, 0)], &[], None),
                }
            }
            Inst::Select { dst, cases, default } => {
                // The first case that can go ahead now does, in source order.
                let mut recvs = Vec::new();
                let mut sends = Vec::new();
                for (i, case) in cases.iter().enumerate() {
                    let i = i as i64;
                    match case {
                        SelectCase::Recv { dst: to, chan } => {
                            let c = self.chan(*chan, "select")?;
                            if let Some(v) = self.try_recv(c) {
                                self.set(*to, v);
                                self.set(*dst, Value::Int(i));
                                return Ok(());
                            }
                            recvs.push((c, *to, i));
                        }
                        SelectCase::Send { chan, value } => {
                            let c = self.chan(*chan, "select")?;
                            let v = self.shared(*value);
                            if self.try_send(c, v) {
                                self.set(*dst, Value::Int(i));
                                return Ok(());
                            }
                            sends.push((c, v, i));
                        }
                    }
                }
                if *default {
                    self.set(*dst, Value::Int(cases.len() as i64));
                } else {
                    // With no cases, this waits forever, like Go's `select {}`.
                    self.wait(&recvs, &sends, Some(*dst));
                }
            }
            Inst::ToStr { dst, src } => {
                let text = match (f.reg_type(*src), self.get(*src)) {
                    (Type::Int(jihoo_ir::IntTy::U64), Value::Int(n)) => (n as u64).to_string(),
                    (_, Value::Int(n)) => n.to_string(),
                    (&Type::Float(t), Value::Float(x)) => float_text(t, f64::from_bits(x)),
                    (_, Value::Bool(b)) => b.to_string(),
                    (_, v) => return Err(self.error(&format!("`to_str` cannot take {}", type_name(v)))),
                };
                let r = self.alloc_str(&text);
                self.set(*dst, Value::Str(r));
            }
            Inst::Unique { dst, prefix } => {
                let Value::Str(p) = self.get(*prefix) else {
                    return Err(self.error("`unique` needs a str"));
                };
                let name = format!("{}__{}", self.heap.str(p), self.uniques);
                self.uniques += 1;
                let r = self.alloc_str(&name);
                self.set(*dst, Value::Str(r));
            }
            Inst::Print { src } => {
                let line = match self.get(*src) {
                    Value::Int(n) if f.reg_type(*src) == &Type::Int(jihoo_ir::IntTy::U64) => (n as u64).to_string(),
                    Value::Int(n) => n.to_string(),
                    Value::Float(x) => match f.reg_type(*src) {
                        &Type::Float(t) => float_text(t, f64::from_bits(x)),
                        t => return Err(self.error(&format!("a float in a {t} register"))),
                    },
                    Value::Bool(b) => b.to_string(),
                    Value::Str(r) => self.heap.str(r).to_string(),
                    v @ (Value::Unit | Value::Agg(_) | Value::Func(_) | Value::Chan(_)) => {
                        return Err(self.error(&format!("cannot print {}", type_name(v))))
                    }
                };
                writeln!(out, "{line}").map_err(|e| self.error(&format!("print failed: {e}")))?;
            }
            Inst::Syscall { .. } => return Err(self.error("`syscall` is not available on the VM")),
            Inst::Asm { .. } => return Err(self.error("inline asm is not available on the VM")),
            Inst::Quote { dst, pieces, holes, kinds } => {
                use jihoo_ir::HoleKind;
                let mut code = pieces[0].clone();
                for ((hole, kind), piece) in holes.iter().zip(kinds).zip(&pieces[1..]) {
                    let ty = f.reg_type(*hole);
                    let text = match (kind, ty, self.get(*hole)) {
                        // Parenthesized, so `$x * 2` keeps its meaning whatever `x` is.
                        (HoleKind::Expr, Type::Expr, Value::Str(r)) => format!("({})", self.heap.str(r)),
                        (HoleKind::Expr, Type::Str, Value::Str(r)) => string_literal(self.heap.str(r)),
                        (HoleKind::Expr, Type::Int(jihoo_ir::IntTy::U64), Value::Int(n)) => (n as u64).to_string(),
                        // Parenthesized so that a negative number stays one operand.
                        (HoleKind::Expr, _, Value::Int(n)) if n < 0 => format!("({n})"),
                        (HoleKind::Expr, _, Value::Int(n)) => n.to_string(),
                        (HoleKind::Expr, &Type::Float(t), Value::Float(x)) => {
                            let x = f64::from_bits(x);
                            if !x.is_finite() {
                                return Err(self.error(&format!("{} has no literal to insert as code", float_text(t, x))));
                            }
                            // Parenthesized so that a negative number stays one operand.
                            let text = float_text(t, x);
                            if x.is_sign_negative() { format!("({text})") } else { text }
                        }
                        (HoleKind::Expr, _, Value::Bool(b)) => b.to_string(),
                        (HoleKind::Ident, Type::Str | Type::Expr, Value::Str(r)) => {
                            let name = self.heap.str(r).trim().to_string();
                            if !is_identifier(&name) {
                                return Err(self.error(&format!("`{name}` is not a valid name")));
                            }
                            name
                        }
                        // Statements and items on lines of their own.
                        (HoleKind::Stmts, Type::Stmts | Type::Expr, Value::Str(r))
                        | (HoleKind::Items, Type::Items, Value::Str(r)) => format!("\n{}\n", self.heap.str(r)),
                        (kind, t, _) => {
                            let place = format!("{kind:?}").to_lowercase();
                            return Err(self.error(&format!("cannot insert a value of type {t} where a {place} goes")));
                        }
                    };
                    code.push_str(&text);
                    code.push_str(piece);
                }
                let r = self.alloc_str(&code);
                self.set(*dst, Value::Str(r));
            }
        }
        Ok(())
    }

    /// `operand` is the type of `a` (and `b`): integer ops depend on width and signedness.
    fn binary(&mut self, op: BinOp, operand: &Type, a: Value, b: Value) -> Result<Value, VmError> {
        use Value::*;
        if let (&Type::Float(t), Float(x), Float(y)) = (operand, a, b) {
            let (x, y) = (f64::from_bits(x), f64::from_bits(y));
            // Each result is rounded to `t`: for `f32`, the f64 result of one
            // operation on two f32s, rounded once, is the correctly rounded f32.
            let num = |v: f64| Value::float(t.round(v));
            return Ok(match op {
                BinOp::Add => num(x + y),
                BinOp::Sub => num(x - y),
                BinOp::Mul => num(x * y),
                BinOp::Div => num(x / y),
                BinOp::Rem => num(x % y),
                BinOp::Eq => Bool(x == y),
                BinOp::Ne => Bool(x != y),
                BinOp::Lt => Bool(x < y),
                BinOp::Le => Bool(x <= y),
                BinOp::Gt => Bool(x > y),
                BinOp::Ge => Bool(x >= y),
                BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Shl | BinOp::Shr => {
                    return Err(self.error(&format!("`{}` is not supported for floats", op.mnemonic())))
                }
            });
        }
        if let (&Type::Int(t), Int(x), Int(y)) = (operand, a, b) {
            return int_binary(op, t, x, y).ok_or_else(|| self.error("division by zero"));
        }
        Ok(match (op, a, b) {
            (BinOp::Add, Str(x), Str(y)) => {
                let s = format!("{}{}", self.heap.str(x), self.heap.str(y));
                Str(self.alloc_str(&s))
            }
            (BinOp::Eq | BinOp::Ne, Str(x), Str(y)) => {
                Bool((self.heap.str(x) == self.heap.str(y)) == (op == BinOp::Eq))
            }
            (BinOp::Eq | BinOp::Ne, Bool(x), Bool(y)) => Bool((x == y) == (op == BinOp::Eq)),
            (BinOp::And, Bool(x), Bool(y)) => Bool(x & y),
            (BinOp::Or, Bool(x), Bool(y)) => Bool(x | y),
            (BinOp::Xor, Bool(x), Bool(y)) => Bool(x ^ y),
            _ => {
                return Err(self.error(&format!(
                    "`{}` is not supported for {} and {}",
                    op.mnemonic(),
                    type_name(a),
                    type_name(b)
                )))
            }
        })
    }

    /// Allocates on the GC heap, collecting first if the heap is over its threshold.
    /// Whatever an instruction allocates from must already be reachable from
    /// [`Vm::roots`]: values read from registers are, values only held in Rust
    /// locals are not.
    fn alloc_str(&mut self, s: &str) -> GcRef {
        self.maybe_collect();
        self.heap.alloc_str(s)
    }

    fn alloc_agg(&mut self, fields: Vec<Value>) -> GcRef {
        self.maybe_collect();
        self.heap.alloc_agg(fields)
    }

    fn alloc_chan(&mut self, cap: usize) -> GcRef {
        self.maybe_collect();
        self.heap.alloc_chan(cap)
    }

    fn maybe_collect(&mut self) {
        if self.heap.should_collect() {
            let extra = self.extra_roots();
            let roots = roots(&self.stack, &self.tasks, &self.pinned, &extra);
            self.heap.collect(roots);
        }
    }

    /// The roots besides registers and pinned values: the channels tasks wait
    /// on, and objects made in the middle of an instruction.
    fn extra_roots(&self) -> Vec<Value> {
        let mut extra: Vec<Value> =
            self.tasks.iter().filter_map(|t| t.waiting.as_ref()).flat_map(|w| w.chans.iter().map(|c| Value::Chan(*c))).collect();
        extra.extend(&self.temp_roots);
        extra
    }

    /// With `JIHOO_VM_CHECK`, makes sure that `obj`, about to be updated in
    /// place, has no reference but the one the update replaces: anything else
    /// that refers to it would see the change, which breaks value semantics.
    /// The shared bit is meant to rule that out (see `share`); this checks it
    /// against the heap itself, which is slow, so only when asked.
    fn check_in_place(&self, obj: GcRef) {
        if !self.check {
            return;
        }
        let extra = self.extra_roots();
        let n = self.heap.references_to(roots(&self.stack, &self.tasks, &self.pinned, &extra), obj);
        assert!(
            n == 1,
            "JIHOO_VM_CHECK: {obj:?} is updated in place in {}, but {n} references to it are live",
            self.module.funcs[self.stack.last().unwrap().func].name
        );
    }

    #[inline]
    fn bounds_check(&self, agg: GcRef, index: Reg) -> Result<usize, VmError> {
        let i = self.int(index)?;
        let len = self.heap.items(agg).len();
        if i < 0 || i as usize >= len {
            return Err(self.error(&format!("index {i} out of bounds for length {len}")));
        }
        Ok(i as usize)
    }

    #[inline(always)]
    fn agg_ref(&self, r: Reg) -> Result<GcRef, VmError> {
        match self.get(r) {
            Value::Agg(s) => Ok(s),
            v => Err(self.type_error("a struct or array", v)),
        }
    }

    /// Notes that `v`, if it is an object, may now have another reference, so
    /// it must not be updated in place any more.
    #[inline(always)]
    fn share(&mut self, v: Value) -> Value {
        if let Value::Agg(r) = v {
            self.heap.mark_shared(r);
        }
        v
    }

    /// The value of `r`, which is about to be copied somewhere while `r` keeps it.
    #[inline(always)]
    fn shared(&mut self, r: Reg) -> Value {
        let v = self.get(r);
        self.share(v)
    }

    /// The part of `v` at `path`, which gets a second reference: the parts on
    /// the way do not.
    fn get_in(&mut self, mut v: Value, path: &[PathStep]) -> Result<Value, VmError> {
        for step in path {
            let r = self.agg_of(v)?;
            let i = self.step_index(r, *step)?;
            v = self.heap.items(r)[i];
        }
        Ok(self.share(v))
    }

    /// `root` with the part at `path` replaced by `v`. `in_place` says whether
    /// the result replaces the only reference to `root`: then objects are
    /// updated in place down to the first shared one, and copied from there.
    /// `root` must be reachable from a root of the GC.
    fn set_in(&mut self, root: Value, path: &[PathStep], v: Value, in_place: bool) -> Result<Value, VmError> {
        // The objects along the path, outside in, and the index taken in each.
        let mut objs = Vec::with_capacity(path.len());
        let mut at = Vec::with_capacity(path.len());
        let mut cur = root;
        for step in path {
            let r = self.agg_of(cur)?;
            let i = self.step_index(r, *step)?;
            objs.push(r);
            at.push(i);
            cur = self.heap.items(r)[i];
        }
        let mut in_place = in_place;
        let places: Vec<bool> = objs
            .iter()
            .map(|r| {
                in_place = in_place && !self.heap.is_shared(*r);
                in_place
            })
            .collect();
        for (k, &place) in places.iter().enumerate() {
            if place {
                self.check_in_place(objs[k]);
            }
        }
        // Rebuild inside out. Nothing is changed in place before every copy is
        // made, so the old objects stay reachable from `root`.
        let mut new = v;
        for k in (0..objs.len()).rev() {
            if places[k] {
                self.heap.items_mut(objs[k])[at[k]] = new;
                new = Value::Agg(objs[k]);
            } else {
                let mut values = self.copy_items(objs[k]);
                values[at[k]] = new;
                self.temp_roots.push(new);
                new = Value::Agg(self.alloc_agg(values));
            }
        }
        self.temp_roots.clear();
        Ok(new)
    }

    /// The parts of aggregate `obj`, for a copy of it. The copy and `obj` both
    /// refer to each part, so none may be updated in place any more.
    fn copy_items(&mut self, obj: GcRef) -> Vec<Value> {
        let values = self.heap.items(obj).to_vec();
        for v in &values {
            self.share(*v);
        }
        values
    }

    fn agg_of(&self, v: Value) -> Result<GcRef, VmError> {
        match v {
            Value::Agg(r) => Ok(r),
            v => Err(self.error(&format!("expected a struct or array, found {}", type_name(v)))),
        }
    }

    /// The index of the part of `obj` that `step` takes; bounds-checked.
    fn step_index(&self, obj: GcRef, step: PathStep) -> Result<usize, VmError> {
        match step {
            PathStep::Field(i) => Ok(i as usize),
            PathStep::Elem(r) => self.bounds_check(obj, r),
        }
    }

    /// `dst = src` with part `i` of aggregate `obj` (the value of `src`) set to
    /// `v`. When the result replaces the only reference to `obj`, `obj` is
    /// updated in place; otherwise it is copied.
    fn update(&mut self, dst: Reg, src: Reg, obj: GcRef, i: usize, v: Value) {
        if dst == src && !self.heap.is_shared(obj) {
            self.check_in_place(obj);
            self.heap.items_mut(obj)[i] = v;
            return;
        }
        let mut values = self.copy_items(obj);
        values[i] = v;
        // `src` and `v`'s register still hold their values, so everything in
        // `values` stays rooted if this allocation collects.
        let new = self.alloc_agg(values);
        self.set(dst, Value::Agg(new));
    }

    #[inline(always)]
    fn get(&self, r: Reg) -> Value {
        self.stack.last().unwrap().regs[r.0 as usize]
    }

    #[inline(always)]
    fn set(&mut self, r: Reg, v: Value) {
        self.stack.last_mut().unwrap().regs[r.0 as usize] = v;
    }

    #[inline(always)]
    fn float(&self, r: Reg) -> Result<f64, VmError> {
        match self.get(r) {
            Value::Float(x) => Ok(f64::from_bits(x)),
            v => Err(self.type_error("a float", v)),
        }
    }

    #[inline(always)]
    fn int(&self, r: Reg) -> Result<i64, VmError> {
        match self.get(r) {
            Value::Int(n) => Ok(n),
            v => Err(self.type_error("i64", v)),
        }
    }

    #[inline(always)]
    fn bool(&self, r: Reg) -> Result<bool, VmError> {
        match self.get(r) {
            Value::Bool(b) => Ok(b),
            v => Err(self.type_error("bool", v)),
        }
    }

    /// A register held a value of the wrong kind: only for a broken module,
    /// since the verifier checks types, so kept out of the way of the fast path.
    #[cold]
    #[inline(never)]
    fn type_error(&self, expected: &str, v: Value) -> VmError {
        self.error(&format!("expected {expected}, found {}", type_name(v)))
    }

    #[cold]
    fn error(&self, msg: &str) -> VmError {
        let func = match self.stack.last() {
            Some(f) => self.module.funcs[f.func].name.clone(),
            None => "?".into(),
        };
        VmError { func, msg: msg.into() }
    }
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `s` as jihoo source: a string literal that reads back as `s`.
fn string_literal(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Every value the program can still reach without going through the heap: the
/// registers of the running task (`stack`) and of every other task, values
/// handed to the embedder (`pinned`), and `extra`: the channels tasks wait on
/// and the objects an instruction is still building. Values in a channel are
/// reached through the channel.
fn roots<'a>(
    stack: &'a [Frame],
    tasks: &'a [Task],
    pinned: &'a [Value],
    extra: &'a [Value],
) -> impl Iterator<Item = &'a Value> {
    let stacks = std::iter::once(stack).chain(tasks.iter().map(|t| t.stack.as_slice()));
    stacks.flatten().flat_map(|f| f.regs.iter()).chain(pinned).chain(extra)
}

/// Integer `n` (in canonical form for type `from`) as the nearest `to`, rounded
/// once, as LLVM's `sitofp`/`uitofp` do.
fn int_to_float(from: &Type, n: i64, to: FloatTy) -> f64 {
    let unsigned = matches!(from, Type::Int(t) if !t.signed());
    match (to, unsigned) {
        (FloatTy::F32, false) => n as f32 as f64,
        (FloatTy::F32, true) => n as u64 as f32 as f64,
        (FloatTy::F64, false) => n as f64,
        (FloatTy::F64, true) => n as u64 as f64,
    }
}

/// `x` as integer type `to`: the fraction dropped, saturating at the limits,
/// NaN as 0, like LLVM's `fptosi.sat`/`fptoui.sat` (and Rust's `as`).
fn float_to_int(x: f64, to: IntTy) -> i64 {
    match to {
        IntTy::I8 => x as i8 as i64,
        IntTy::I16 => x as i16 as i64,
        IntTy::I32 => x as i32 as i64,
        IntTy::I64 => x as i64,
        IntTy::U8 => x as u8 as i64,
        IntTy::U16 => x as u16 as i64,
        IntTy::U32 => x as u32 as i64,
        IntTy::U64 => x as u64 as i64,
    }
}

/// How `print` and string conversion show a float: the shortest decimal that
/// reads back as the same value of type `t`, always with a `.` or exponent
/// (`1.0`, `0.1`, `1e100`), or `inf`, `-inf`, `NaN`.
pub fn float_text(t: FloatTy, x: f64) -> String {
    match t {
        FloatTy::F32 => format!("{:?}", x as f32),
        FloatTy::F64 => format!("{x:?}"),
    }
}

fn type_name(v: Value) -> &'static str {
    match v {
        Value::Unit => "unit",
        Value::Int(_) => "i64",
        Value::Float(_) => "float",
        Value::Bool(_) => "bool",
        Value::Str(_) => "str",
        Value::Agg(_) => "aggregate",
        Value::Func(_) => "function",
        Value::Chan(_) => "channel",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(src: &str) -> Module {
        let m = jihoo_sema::analyze(&jihoo_syntax::parse(src).unwrap()).unwrap();
        jihoo_ir::verify(&m).unwrap();
        m
    }

    fn run_src(src: &str) -> (i64, String) {
        let m = compile(src);
        let mut out = Vec::new();
        let code = Vm::new(&m).with_check(true).run_main(&mut out).unwrap();
        (code, String::from_utf8(out).unwrap())
    }

    #[test]
    fn recursion() {
        let src = "
fn fib(n: i64) -> i64 {
    if n < 2 { return n }
    return fib(n - 1) + fib(n - 2)
}
fn main() -> i64 { return fib(20) }";
        assert_eq!(run_src(src).0, 6765);
    }

    #[test]
    fn loops_and_short_circuit() {
        let src = "
fn main() {
    let i = 0
    let n = 0
    while i < 10 {
        if i % 2 == 0 && i != 4 || i == 9 { n = n + 1 }
        i = i + 1
    }
    print(n)
}";
        // 0, 2, 6, 8, 9
        assert_eq!(run_src(src).1, "5\n");
    }

    #[test]
    fn strings() {
        let (_, out) = run_src("fn main() { print(\"hello, \" + \"jihoo\") }");
        assert_eq!(out, "hello, jihoo\n");
    }

    #[test]
    fn bools() {
        let (_, out) = run_src("fn main() { print(1 < 2 && !(3 == 4))\n print(\"a\" != \"a\") }");
        assert_eq!(out, "true\nfalse\n");
    }

    #[test]
    fn sized_integers_wrap() {
        let src = "
fn main() {
    let a: u8 = 250
    a = a + 10
    print(a)                    // 4
    let b: i8 = 127
    print(b + 1)                // -128
    let c: u32 = 7
    print(c / 2 * 2 == 6)       // true
    let d: u64 = 0
    print(d - 1)                // 18446744073709551615
    print((d - 1) / 2 > 0)      // unsigned compare: true
    let e: i16 = -1
    print(e as u16)             // 65535
    print(300 as u8)            // 44
    print(true as i32 + 1)      // 2
}";
        let (_, out) = run_src(src);
        assert_eq!(out, "4\n-128\ntrue\n18446744073709551615\ntrue\n65535\n44\n2\n");
    }

    #[test]
    fn structs_are_values() {
        let src = "
struct Point { x: i64, y: i64 }
struct Line { a: Point, b: Point }

fn moved(p: Point) -> Point {
    p.x = p.x + 100
    return p
}

fn main() {
    let p = Point { x: 1, y: 2 }
    let q = p
    q.x = 10
    print(p.x)                  // 1: `q` is a copy
    print(moved(p).x)           // 101
    print(p.x)                  // 1: arguments are copies
    let l = Line { a: p, b: q }
    l.b.y = 7
    print(l.b.y + l.a.y)        // 9
    print(q.y)                  // 2
}";
        let (_, out) = run_src(src);
        assert_eq!(out, "1\n101\n1\n9\n2\n");
    }

    #[test]
    fn gc_traces_struct_fields() {
        // Lots of garbage structs, while one struct keeps a string alive across
        // collections.
        let src = "
struct Named { name: str, n: i64 }
fn main() {
    let keep = Named { name: \"kept\" + \"!\", n: 0 }
    let chunk = \"................................................................\"
    let i = 0
    while i < 3000 {
        let tmp = Named { name: chunk + chunk + chunk + chunk + chunk, n: i }
        keep.n = keep.n + tmp.n
        i = i + 1
    }
    print(keep.name)
    print(keep.n)
}";
        let m = compile(src);
        let mut vm = Vm::new(&m);
        let mut out = Vec::new();
        vm.run_main(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "kept!\n4498500\n");
        assert!(vm.heap().stats().collections > 0, "{:?}", vm.heap().stats());
    }

    #[test]
    fn arrays() {
        let src = "
struct Grid { cells: [[u8; 3]; 2] }

fn sort(xs: [i64; 6]) -> [i64; 6] {
    let i = 0
    while i < len(xs) {
        let j = 0
        while j < len(xs) - 1 - i {
            if xs[j] > xs[j + 1] {
                let t = xs[j]
                xs[j] = xs[j + 1]
                xs[j + 1] = t
            }
            j = j + 1
        }
        i = i + 1
    }
    return xs
}

fn main() {
    let xs = [5, 3, 9, 1, 4, 1]
    let sorted = sort(xs)
    print(sorted[0] * 100 + sorted[5])   // 109
    print(xs[0])                          // 5: `sort` got a copy
    let g = Grid { cells: [[0; 3]; 2] }
    g.cells[1][2] = 7
    let row = g.cells[1]
    row[0] = 1
    print(g.cells[1][0] + g.cells[1][2]) // 7: `row` is a copy
    print(len(g.cells) * len(row))       // 6
}";
        let (_, out) = run_src(src);
        assert_eq!(out, "109\n5\n7\n6\n");
    }

    #[test]
    fn array_index_out_of_bounds_is_an_error() {
        let m = compile("fn main() { let a = [1, 2, 3]\n let i = 3\n print(a[i]) }");
        let err = run(&m, &mut Vec::new()).unwrap_err();
        assert_eq!(err.msg, "index 3 out of bounds for length 3");
        let m = compile("fn main() { let a = [1, 2, 3]\n a[-1] = 0 }");
        assert!(run(&m, &mut Vec::new()).unwrap_err().msg.contains("index -1 out of bounds"));
    }

    #[test]
    fn division_by_zero_is_an_error() {
        let m = compile("fn main() { let z = 0\n print(1 / z) }");
        let err = run(&m, &mut Vec::new()).unwrap_err();
        assert!(err.msg.contains("division by zero"));
    }

    #[test]
    fn gc_reclaims_garbage() {
        // Each iteration makes a ~1KiB string and drops the previous one.
        let src = "
fn main() {
    let chunk = \"................................................................\"
    let i = 0
    while i < 5000 {
        let s = chunk + chunk + chunk + chunk + chunk + chunk + chunk + chunk
        s = s + s
        i = i + 1
    }
}";
        let m = compile(src);
        let mut vm = Vm::new(&m);
        vm.run_main(&mut Vec::new()).unwrap();
        let stats = vm.heap().stats();
        assert!(stats.collections > 0, "{stats:?}");
        assert!(stats.live_bytes < 4 << 20, "{stats:?}");
    }

    #[test]
    fn arguments_survive_collection() {
        // No frame holds the first argument while the second is allocated.
        let m = compile("fn join(a: str, b: str) -> str { return a + b }\nfn main() {}");
        let mut vm = Vm::new(&m);
        vm.heap_mut().set_stress(true);
        let a = vm.alloc_string("left ");
        let b = vm.alloc_string("right");
        let Value::Str(r) = vm.call_named("join", &[a, b], &mut Vec::new()).unwrap() else { panic!() };
        assert_eq!(vm.heap().str(r), "left right");
        assert!(vm.heap().stats().collections >= 2);
    }

    /// Runs `src`, with the in-place check on (`JIHOO_VM_CHECK`), so that every
    /// test also checks that the VM never updates in place what another live
    /// reference can see.
    fn run_limited(src: &str, stress: bool) -> (Result<i64, VmError>, String) {
        let m = compile(src);
        let mut vm = Vm::new(&m).with_fuel(1_000_000).with_check(true);
        vm.heap_mut().set_stress(stress);
        let mut out = Vec::new();
        let r = vm.run_main(&mut out);
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn tasks_and_channels() {
        // Unbuffered: every send waits for its receive, so the order is fixed.
        let src = "
fn ping(c: chan i64, back: chan i64) {
    let i = 0
    while i < 3 {
        send(c, i)
        print(\"ping \" + to_str(recv(back)))
        i = i + 1
    }
}
fn main() {
    let c = chan(i64)
    let back = chan(i64)
    go ping(c, back)
    let i = 0
    while i < 3 {
        let v = recv(c)
        print(\"pong \" + to_str(v))
        send(back, v * 10)
        i = i + 1
    }
}";
        let (r, out) = run_limited(src, false);
        r.unwrap();
        // After the last `send`, `main` keeps running and returns before `ping`
        // gets its turn: the run ends with `main`, so "ping 20" never prints.
        assert_eq!(out, "pong 0\nping 0\npong 1\nping 10\npong 2\n");
        // The same under a collection at every allocation: queued values are roots.
        assert_eq!(run_limited(src, true).1, out);
    }

    #[test]
    fn buffered_channels_and_deadlock() {
        let (r, out) = run_limited("fn main() { let c = chan(str, 2)\n send(c, \"a\")\n send(c, \"b\")\n print(recv(c) + recv(c)) }", true);
        r.unwrap();
        assert_eq!(out, "ab\n");
        let (r, _) = run_limited("fn main() { let c = chan(i64, 1)\n send(c, 1)\n send(c, 2) }", false);
        assert!(r.unwrap_err().msg.contains("deadlock"));
        let (r, _) = run_limited("fn main() { let c = chan(i64, 0 - 1) }", false);
        assert!(r.unwrap_err().msg.contains("capacity -1 is negative"));
    }

    #[test]
    fn select() {
        // The first ready case wins, in source order; `_` runs when none is ready.
        let src = "
fn main() {
    let a = chan(i64, 1)
    let b = chan(i64, 1)
    send(a, 1)
    send(b, 2)
    select {
        let x = recv(b) => print(\"b \" + to_str(x))
        let y = recv(a) => print(\"a \" + to_str(y))
    }
    select {
        recv(b) => print(\"b again\")
        send(b, 3) => print(\"sent 3\")
        _ => print(\"none\")
    }
    select {
        recv(b) => print(\"b now\")
        _ => print(\"none\")
    }
}";
        let (r, out) = run_limited(src, false);
        r.unwrap();
        assert_eq!(out, "b 2\nsent 3\nb now\n");

        // A select in a loop waits on `never` every time. Its waiter there is
        // removed whenever `tick` goes ahead instead, so the final send on
        // `never` reaches the final receive, not a stale select.
        let src = "
fn main() {
    let tick = chan(i64)
    let never = chan(i64)
    go fn() {
        let i = 0
        while i < 200 {
            send(tick, i)
            i = i + 1
        }
    }()
    let sum = 0
    let i = 0
    while i < 200 {
        select {
            let t = recv(tick) => sum = sum + t
            recv(never) => print(\"wrong\")
        }
        i = i + 1
    }
    go fn() { send(never, 7) }()
    print(sum + recv(never))
}";
        let (r, out) = run_limited(src, true);
        r.unwrap();
        assert_eq!(out, "19907\n");
        let (r, _) = run_limited("fn main() { let c = chan(i64)\n select { recv(c) => {} } }", false);
        assert!(r.unwrap_err().msg.contains("deadlock"));
    }

    #[test]
    fn cells_are_shared_and_updates_without_calls_are_atomic() {
        // Four tasks add to one cell. Each `*total = *total + 1` has no call or
        // loop inside, so no other task runs between its read and its write.
        // `slow` makes the second counter's update span a call: then a channel
        // with room for one value serves as a lock around it.
        let src = "
fn slow(x: i64) -> i64 {
    let i = 0
    while i < 3 {
        i = i + 1
    }
    return x + 1
}
fn main() {
    let total = cell(0)
    let locked = cell(0)
    let lock = chan(bool, 1)
    let done = chan(bool)
    let w = 0
    while w < 4 {
        go fn() {
            let i = 0
            while i < 500 {
                *total = *total + 1
                send(lock, true)
                *locked = slow(*locked)
                recv(lock)
                i = i + 1
            }
            send(done, true)
        }()
        w = w + 1
    }
    let k = 0
    while k < 4 {
        recv(done)
        k = k + 1
    }
    print(*total)
    print(*locked)
}";
        let (r, out) = run_limited(src, false);
        r.unwrap();
        assert_eq!(out, "2000\n2000\n");
        // Without the lock, updates spanning a call get lost.
        let racy = src.replace("send(lock, true)", "").replace("recv(lock)", "");
        let (r, out) = run_limited(&racy, false);
        r.unwrap();
        assert!(out.starts_with("2000\n") && out != "2000\n2000\n", "{out}");
    }

    #[test]
    fn tasks_are_preempted_and_dropped_with_main() {
        // A task that never stops does not keep `main` from running or ending.
        let (r, out) = run_limited("fn spin() { while true {} }\nfn main() { go spin()\n go spin()\n let i = 0\n while i < 5000 { i = i + 1 }\n print(i) }", false);
        r.unwrap();
        assert_eq!(out, "5000\n");
        // An error in any task stops the run.
        let (r, _) = run_limited("fn bad(x: i64) -> i64 { return 1 / x }\nfn main() { go bad(0)\n let c = chan(i64)\n print(recv(c)) }", false);
        assert!(r.unwrap_err().msg.contains("division by zero"));
    }

    #[test]
    fn lists_survive_stress_collection() {
        // Each cell is only reachable through the cell after it.
        let src = "
enum List { Cons(i64, ref List), Nil }
fn main() {
    let l = List.Nil
    let i = 0
    while i < 300 {
        l = List.Cons(i, ref l)
        i = i + 1
    }
    let total = 0
    let done = false
    while !done {
        match l {
            Cons(x, rest) => {
                total = total + x
                l = *rest
            }
            Nil => done = true
        }
    }
    print(total)
}";
        let m = compile(src);
        let mut vm = Vm::new(&m);
        vm.heap_mut().set_stress(true);
        let mut out = Vec::new();
        vm.run_main(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "44850\n");
    }

    #[test]
    fn in_place_updates_keep_value_semantics() {
        // Updates may happen in place, but never where another copy can see it.
        let src = "
struct S { arr: [i64; 3], n: i64 }
const T = [1, 2, 3]
fn modify(a: [i64; 3]) -> i64 {
    a[0] = 100
    return a[0]
}
fn main() {
    let a = [1, 2, 3]
    let b = a
    a[0] = 9
    print(b[0])                         // 1: a copy
    print(modify(a) + a[0])             // 109: the callee has its own
    let s = S { arr: a, n: 0 }
    a[1] = 8
    print(s.arr[1])                     // 2
    s.arr[2] = 7
    print(a[2])                         // 3
    let row = s.arr
    s.arr[0] = 5
    print(row[0])                       // 9
    let grid = [[0; 3]; 2]
    let r = grid[0]
    grid[0][0] = 4
    print(r[0] + grid[1][0])            // 0: rows were one shared value
    let f = fn() -> i64 { return a[0] }
    a[0] = 50
    print(f())                          // 9: captured when made
    let c = chan([i64; 3], 1)
    send(c, a)
    a[0] = 60
    print(recv(c)[0])                   // 50
    let rf = ref a
    a[0] = 70
    print((*rf)[0])                     // 60
    let i = 0
    while i < 2 {
        let t = T
        print(t[0])                     // 1, 1: the constant never changes
        t[0] = 99
        i = i + 1
    }
    let ss = [s; 2]
    ss[0].arr[0] = 1
    print(ss[1].arr[0])                 // 5
    let e = Option.Some(a)
    a[0] = 80
    match e {
        Some(x) => print(x[0])          // 70
        None => {}
    }
    // Copying an outer value must not leave an inner one updatable in place:
    // the copy and the original both hold the inner rows.
    let g = [[1], [2]]
    g[1][0] = 3                         // g[1] is a fresh, unshared row
    let h = g
    g[0][0] = 4                         // copies g's outer array
    g[1][0] = 5
    print(h[1][0])                      // 3
    g[1][0] = 6
    let k = fn() -> i64 { return g[1][0] }
    g[0][0] = 7
    g[1][0] = 8
    print(k())                          // 6
    let c2 = chan([[i64; 1]; 2], 1)
    send(c2, g)
    g[0][0] = 9
    g[1][0] = 10
    print(recv(c2)[1][0])               // 8
}
enum Option(T: type) { Some(T), None }";
        let expected = "1\n109\n2\n3\n9\n0\n9\n50\n60\n1\n1\n5\n70\n3\n6\n8\n";
        let (r, out) = run_limited(src, false);
        r.unwrap();
        assert_eq!(out, expected);
        let (r, out) = run_limited(src, true);
        r.unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn updates_in_a_loop_are_in_place() {
        // Without in-place updates this allocates a 10000-element array per step.
        let src = "fn main() {\n let a = [0; 10000]\n let i = 0\n while i < 10000 {\n a[i] = i\n i = i + 1\n }\n print(a[9999]) }";
        let m = compile(src);
        let mut vm = Vm::new(&m);
        vm.heap_mut().set_stress(false); // this counts collections
        let mut out = Vec::new();
        vm.run_main(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "9999\n");
        assert!(vm.heap().stats().collections <= 1, "{:?}", vm.heap().stats());
    }

    #[test]
    fn programs_survive_stress_collection() {
        let src = "
struct Named { name: str, tags: [str; 2] }
fn rename(n: Named, s: str) -> Named { n.name = s + n.name\n return n }
fn main() {
    let n = Named { name: \"a\", tags: [\"x\", \"y\"] }
    let i = 0
    while i < 50 {
        n = rename(n, \"b\")
        n.tags[1] = n.tags[0] + n.tags[1]
        i = i + 1
    }
    print(n.tags[1])
}";
        let m = compile(src);
        let mut vm = Vm::new(&m);
        vm.heap_mut().set_stress(true);
        let mut out = Vec::new();
        vm.run_main(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), format!("{}y\n", "x".repeat(50)));
        assert!(vm.heap().stats().collections > 100);
    }
}
