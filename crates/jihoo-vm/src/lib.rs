//! Register VM that executes JIR directly (the hosted profile).

pub mod gc;

use std::collections::HashMap;
use std::fmt;
use std::io::Write;

use gc::{GcRef, Heap};
use jihoo_ir::{BinOp, Inst, Module, Profile, Reg, Terminator, UnOp};

const MAX_CALL_DEPTH: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Str(GcRef),
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

pub struct Vm<'m> {
    module: &'m Module,
    fn_index: HashMap<&'m str, usize>,
    heap: Heap,
    stack: Vec<Frame>,
}

/// Runs `main` of a hosted module and returns its result.
pub fn run(module: &Module, out: &mut dyn Write) -> Result<i64, VmError> {
    Vm::new(module).run_main(out)
}

impl<'m> Vm<'m> {
    pub fn new(module: &'m Module) -> Self {
        let fn_index = module.funcs.iter().enumerate().map(|(i, f)| (f.name.as_str(), i)).collect();
        Vm { module, fn_index, heap: Heap::default(), stack: Vec::new() }
    }

    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    pub fn run_main(&mut self, out: &mut dyn Write) -> Result<i64, VmError> {
        let err = |msg: &str| VmError { func: "main".into(), msg: msg.into() };
        if self.module.profile != Profile::Hosted {
            return Err(err("freestanding modules cannot run on the VM; use `jihoo build`"));
        }
        let main = *self.fn_index.get("main").ok_or_else(|| err("no `main` function"))?;
        match self.call(main, &[], out)? {
            Value::Int(n) => Ok(n),
            Value::Str(_) => Err(err("`main` must return an integer")),
        }
    }

    fn call(&mut self, func: usize, args: &[Value], out: &mut dyn Write) -> Result<Value, VmError> {
        self.push_frame(func, args, None)?;
        let base = self.stack.len() - 1;
        let module = self.module;

        loop {
            let frame = self.stack.last_mut().unwrap();
            let f = &module.funcs[frame.func];
            let block = &f.blocks[frame.block];

            if frame.ip < block.insts.len() {
                let inst = &block.insts[frame.ip];
                frame.ip += 1;
                self.exec(inst, out)?;
                continue;
            }

            match &block.term {
                Terminator::Jump(b) => {
                    frame.block = b.0 as usize;
                    frame.ip = 0;
                }
                Terminator::Branch { cond, then, els } => {
                    let c = self.int(*cond)?;
                    let frame = self.stack.last_mut().unwrap();
                    frame.block = if c != 0 { then.0 } else { els.0 } as usize;
                    frame.ip = 0;
                }
                Terminator::Ret(r) => {
                    let v = frame.regs[r.0 as usize];
                    let done = self.stack.pop().unwrap();
                    if self.stack.len() == base {
                        return Ok(v);
                    }
                    let caller = self.stack.last_mut().unwrap();
                    caller.regs[done.ret_dst.unwrap().0 as usize] = v;
                }
            }
        }
    }

    fn push_frame(&mut self, func: usize, args: &[Value], ret_dst: Option<Reg>) -> Result<(), VmError> {
        if self.stack.len() >= MAX_CALL_DEPTH {
            return Err(self.error("stack overflow"));
        }
        let f = &self.module.funcs[func];
        let mut regs = vec![Value::Int(0); f.num_regs as usize];
        regs[..args.len()].copy_from_slice(args);
        self.stack.push(Frame { func, block: 0, ip: 0, regs, ret_dst });
        Ok(())
    }

    fn exec(&mut self, inst: &'m Inst, out: &mut dyn Write) -> Result<(), VmError> {
        match inst {
            Inst::Const { dst, value } => self.set(*dst, Value::Int(*value)),
            Inst::Str { dst, value } => {
                let r = self.alloc_str(value);
                self.set(*dst, Value::Str(r));
            }
            Inst::Copy { dst, src } => {
                let v = self.get(*src);
                self.set(*dst, v);
            }
            Inst::Unary { dst, op, src } => {
                let x = self.int(*src)?;
                let v = match op {
                    UnOp::Neg => x.wrapping_neg(),
                    UnOp::Not => (x == 0) as i64,
                };
                self.set(*dst, Value::Int(v));
            }
            Inst::Binary { dst, op, lhs, rhs } => {
                let v = self.binary(*op, self.get(*lhs), self.get(*rhs))?;
                self.set(*dst, v);
            }
            Inst::Call { dst, func, args } => {
                let callee = self.fn_index[func.as_str()];
                let args: Vec<Value> = args.iter().map(|r| self.get(*r)).collect();
                self.push_frame(callee, &args, Some(*dst))?;
            }
            Inst::Print { src } => {
                let line = match self.get(*src) {
                    Value::Int(n) => n.to_string(),
                    Value::Str(r) => self.heap.str(r).to_string(),
                };
                writeln!(out, "{line}").map_err(|e| self.error(&format!("print failed: {e}")))?;
            }
            Inst::Syscall { .. } => return Err(self.error("`syscall` is not available on the VM")),
        }
        Ok(())
    }

    fn binary(&mut self, op: BinOp, a: Value, b: Value) -> Result<Value, VmError> {
        use Value::*;
        let v = match (op, a, b) {
            (BinOp::Add, Int(x), Int(y)) => x.wrapping_add(y),
            (BinOp::Sub, Int(x), Int(y)) => x.wrapping_sub(y),
            (BinOp::Mul, Int(x), Int(y)) => x.wrapping_mul(y),
            (BinOp::Div | BinOp::Rem, Int(_), Int(0)) => return Err(self.error("division by zero")),
            (BinOp::Div, Int(x), Int(y)) => x.wrapping_div(y),
            (BinOp::Rem, Int(x), Int(y)) => x.wrapping_rem(y),
            (BinOp::Eq, Int(x), Int(y)) => (x == y) as i64,
            (BinOp::Ne, Int(x), Int(y)) => (x != y) as i64,
            (BinOp::Lt, Int(x), Int(y)) => (x < y) as i64,
            (BinOp::Le, Int(x), Int(y)) => (x <= y) as i64,
            (BinOp::Gt, Int(x), Int(y)) => (x > y) as i64,
            (BinOp::Ge, Int(x), Int(y)) => (x >= y) as i64,
            (BinOp::Add, Str(x), Str(y)) => {
                let s = format!("{}{}", self.heap.str(x), self.heap.str(y));
                return Ok(Str(self.alloc_str(&s)));
            }
            (BinOp::Eq, Str(x), Str(y)) => (self.heap.str(x) == self.heap.str(y)) as i64,
            (BinOp::Ne, Str(x), Str(y)) => (self.heap.str(x) != self.heap.str(y)) as i64,
            _ => {
                return Err(self.error(&format!(
                    "`{}` is not supported for {} and {}",
                    op.mnemonic(),
                    type_name(a),
                    type_name(b)
                )))
            }
        };
        Ok(Int(v))
    }

    /// Allocates on the GC heap, collecting first if the heap is over its threshold.
    /// Every live value is in some frame's registers, so the frames are the roots.
    fn alloc_str(&mut self, s: &str) -> GcRef {
        if self.heap.should_collect() {
            self.heap.collect(self.stack.iter().flat_map(|f| f.regs.iter()));
        }
        self.heap.alloc_str(s)
    }

    fn get(&self, r: Reg) -> Value {
        self.stack.last().unwrap().regs[r.0 as usize]
    }

    fn set(&mut self, r: Reg, v: Value) {
        self.stack.last_mut().unwrap().regs[r.0 as usize] = v;
    }

    fn int(&self, r: Reg) -> Result<i64, VmError> {
        match self.get(r) {
            Value::Int(n) => Ok(n),
            v => Err(self.error(&format!("expected an integer, found {}", type_name(v)))),
        }
    }

    fn error(&self, msg: &str) -> VmError {
        let func = match self.stack.last() {
            Some(f) => self.module.funcs[f.func].name.clone(),
            None => "?".into(),
        };
        VmError { func, msg: msg.into() }
    }
}

fn type_name(v: Value) -> &'static str {
    match v {
        Value::Int(_) => "int",
        Value::Str(_) => "str",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(src: &str) -> Module {
        let m = jihoo_lower::lower(&jihoo_syntax::parse(src).unwrap()).unwrap();
        jihoo_ir::verify(&m).unwrap();
        m
    }

    fn run_src(src: &str) -> (i64, String) {
        let m = compile(src);
        let mut out = Vec::new();
        let code = run(&m, &mut out).unwrap();
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
}
