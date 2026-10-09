//! Register VM that executes JIR directly (the hosted profile).

pub mod gc;

use std::collections::HashMap;
use std::fmt;
use std::io::Write;

use gc::{GcRef, Heap};
use jihoo_ir::{BinOp, Function, Inst, Module, Profile, Reg, Terminator, Type, UnOp};

const MAX_CALL_DEPTH: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Unit,
    /// Every integer type, in canonical form (see `IntTy::wrap`).
    Int(i64),
    Bool(bool),
    Str(GcRef),
    /// A struct or an array.
    Agg(GcRef),
}

impl Value {
    /// The heap object this value refers to, if any.
    pub fn gc_ref(&self) -> Option<GcRef> {
        match self {
            Value::Str(r) | Value::Agg(r) => Some(*r),
            Value::Unit | Value::Int(_) | Value::Bool(_) => None,
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

pub struct Vm<'m> {
    module: &'m Module,
    fn_index: HashMap<&'m str, usize>,
    heap: Heap,
    stack: Vec<Frame>,
    /// Instructions left before execution is stopped, if limited.
    fuel: Option<u64>,
}

/// Runs `main` of a hosted module and returns its result.
pub fn run(module: &Module, out: &mut dyn Write) -> Result<i64, VmError> {
    Vm::new(module).run_main(out)
}

impl<'m> Vm<'m> {
    pub fn new(module: &'m Module) -> Self {
        let fn_index = module.funcs.iter().enumerate().map(|(i, f)| (f.name.as_str(), i)).collect();
        Vm { module, fn_index, heap: Heap::default(), stack: Vec::new(), fuel: None }
    }

    /// Stops execution with an error after `steps` instructions. Used for
    /// compile-time evaluation, so an endless loop cannot hang the compiler.
    pub fn with_fuel(mut self, steps: u64) -> Self {
        self.fuel = Some(steps);
        self
    }

    /// Calls the function `name` with `args`, regardless of the module's profile.
    /// Instructions the VM cannot run (pointers, `syscall`) are runtime errors.
    pub fn call_named(&mut self, name: &str, args: &[Value], out: &mut dyn Write) -> Result<Value, VmError> {
        let func = *self
            .fn_index
            .get(name)
            .ok_or_else(|| VmError { func: name.into(), msg: "no such function".into() })?;
        self.call(func, args, out)
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
            Value::Unit => Ok(0),
            _ => Err(err("`main` must return unit or an integer")),
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

            if let Some(fuel) = &mut self.fuel {
                if *fuel == 0 {
                    return Err(self.error("evaluation did not finish (step limit reached)"));
                }
                *fuel -= 1;
            }
            let frame = self.stack.last_mut().unwrap();
            if frame.ip < block.insts.len() {
                let inst = &block.insts[frame.ip];
                frame.ip += 1;
                self.exec(f, inst, out)?;
                continue;
            }

            match &block.term {
                Terminator::Jump(b) => {
                    frame.block = b.0 as usize;
                    frame.ip = 0;
                }
                Terminator::Branch { cond, then, els } => {
                    let c = self.bool(*cond)?;
                    let frame = self.stack.last_mut().unwrap();
                    frame.block = if c { then.0 } else { els.0 } as usize;
                    frame.ip = 0;
                }
                Terminator::Unreachable => return Err(self.error("reached `unreachable`")),
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
        let mut regs = vec![Value::Unit; f.regs.len()];
        regs[..args.len()].copy_from_slice(args);
        self.stack.push(Frame { func, block: 0, ip: 0, regs, ret_dst });
        Ok(())
    }

    fn exec(&mut self, f: &'m Function, inst: &'m Inst, out: &mut dyn Write) -> Result<(), VmError> {
        match inst {
            Inst::Const { dst, value } => {
                let v = match f.reg_type(*dst) {
                    Type::Bool => Value::Bool(*value != 0),
                    _ => Value::Int(*value),
                };
                self.set(*dst, v);
            }
            Inst::Unit { dst } => self.set(*dst, Value::Unit),
            Inst::Str { dst, value } => {
                let r = self.alloc_str(value);
                self.set(*dst, Value::Str(r));
            }
            Inst::Copy { dst, src } => {
                let v = self.get(*src);
                self.set(*dst, v);
            }
            Inst::Unary { dst, op, src } => {
                let v = match (op, f.reg_type(*dst)) {
                    (UnOp::Neg, Type::Int(t)) => Value::Int(t.wrap(self.int(*src)?.wrapping_neg())),
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
                    (_, v) => v, // a cast to the same type
                };
                self.set(*dst, v);
            }
            Inst::Struct { dst, fields, .. } => {
                let values = fields.iter().map(|r| self.get(*r)).collect();
                let r = self.alloc_agg(values);
                self.set(*dst, Value::Agg(r));
            }
            Inst::Field { dst, src, index } => {
                let r = self.agg_ref(*src)?;
                let v = self.heap.items(r)[*index as usize];
                self.set(*dst, v);
            }
            Inst::SetField { dst, src, index, value } => {
                let r = self.agg_ref(*src)?;
                let mut values = self.heap.items(r).to_vec();
                values[*index as usize] = self.get(*value);
                // `src` and `value` are still in registers, so everything in
                // `values` stays rooted if this allocation collects.
                let new = self.alloc_agg(values);
                self.set(*dst, Value::Agg(new));
            }
            Inst::Array { dst, items } => {
                let values = items.iter().map(|r| self.get(*r)).collect();
                let r = self.alloc_agg(values);
                self.set(*dst, Value::Agg(r));
            }
            Inst::Splat { dst, value } => {
                let Type::Array(_, n) = f.reg_type(*dst) else {
                    return Err(self.error("`splat` needs an array register"));
                };
                let r = self.alloc_agg(vec![self.get(*value); *n as usize]);
                self.set(*dst, Value::Agg(r));
            }
            Inst::Elem { dst, src, index } => {
                let r = self.agg_ref(*src)?;
                let i = self.bounds_check(r, *index)?;
                let v = self.heap.items(r)[i];
                self.set(*dst, v);
            }
            Inst::SetElem { dst, src, index, value } => {
                let r = self.agg_ref(*src)?;
                let i = self.bounds_check(r, *index)?;
                let mut values = self.heap.items(r).to_vec();
                values[i] = self.get(*value);
                let new = self.alloc_agg(values);
                self.set(*dst, Value::Agg(new));
            }
            Inst::Load { .. }
            | Inst::Store { .. }
            | Inst::Addr { .. }
            | Inst::FieldPtr { .. }
            | Inst::ElemPtr { .. } => {
                return Err(self.error("pointers are not available on the VM"))
            }
            Inst::Call { dst, func, args } => {
                let callee = self.fn_index[func.as_str()];
                let args: Vec<Value> = args.iter().map(|r| self.get(*r)).collect();
                self.push_frame(callee, &args, Some(*dst))?;
            }
            Inst::Print { src } => {
                let line = match self.get(*src) {
                    Value::Int(n) if f.reg_type(*src) == &Type::Int(jihoo_ir::IntTy::U64) => (n as u64).to_string(),
                    Value::Int(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Str(r) => self.heap.str(r).to_string(),
                    v @ (Value::Unit | Value::Agg(_)) => {
                        return Err(self.error(&format!("cannot print {}", type_name(v))))
                    }
                };
                writeln!(out, "{line}").map_err(|e| self.error(&format!("print failed: {e}")))?;
            }
            Inst::Syscall { .. } => return Err(self.error("`syscall` is not available on the VM")),
        }
        Ok(())
    }

    /// `operand` is the type of `a` (and `b`): integer ops depend on width and signedness.
    fn binary(&mut self, op: BinOp, operand: &Type, a: Value, b: Value) -> Result<Value, VmError> {
        use Value::*;
        if let (Type::Int(t), Int(x), Int(y)) = (operand, a, b) {
            let (ux, uy) = (x as u64, y as u64);
            let signed = t.signed();
            return Ok(match op {
                BinOp::Add => Int(t.wrap(x.wrapping_add(y))),
                BinOp::Sub => Int(t.wrap(x.wrapping_sub(y))),
                BinOp::Mul => Int(t.wrap(x.wrapping_mul(y))),
                BinOp::Div | BinOp::Rem if y == 0 => return Err(self.error("division by zero")),
                BinOp::Div if signed => Int(t.wrap(x.wrapping_div(y))),
                BinOp::Div => Int(t.wrap((ux / uy) as i64)),
                BinOp::Rem if signed => Int(t.wrap(x.wrapping_rem(y))),
                BinOp::Rem => Int(t.wrap((ux % uy) as i64)),
                BinOp::Eq => Bool(x == y),
                BinOp::Ne => Bool(x != y),
                BinOp::Lt => Bool(if signed { x < y } else { ux < uy }),
                BinOp::Le => Bool(if signed { x <= y } else { ux <= uy }),
                BinOp::Gt => Bool(if signed { x > y } else { ux > uy }),
                BinOp::Ge => Bool(if signed { x >= y } else { ux >= uy }),
            });
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
    /// Every live value is in some frame's registers, so the frames are the roots.
    fn alloc_str(&mut self, s: &str) -> GcRef {
        self.maybe_collect();
        self.heap.alloc_str(s)
    }

    fn alloc_agg(&mut self, fields: Vec<Value>) -> GcRef {
        self.maybe_collect();
        self.heap.alloc_agg(fields)
    }

    fn maybe_collect(&mut self) {
        if self.heap.should_collect() {
            self.heap.collect(self.stack.iter().flat_map(|f| f.regs.iter()));
        }
    }

    fn bounds_check(&self, agg: GcRef, index: Reg) -> Result<usize, VmError> {
        let i = self.int(index)?;
        let len = self.heap.items(agg).len();
        if i < 0 || i as usize >= len {
            return Err(self.error(&format!("index {i} out of bounds for length {len}")));
        }
        Ok(i as usize)
    }

    fn agg_ref(&self, r: Reg) -> Result<GcRef, VmError> {
        match self.get(r) {
            Value::Agg(s) => Ok(s),
            v => Err(self.error(&format!("expected a struct or array, found {}", type_name(v)))),
        }
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
            v => Err(self.error(&format!("expected i64, found {}", type_name(v)))),
        }
    }

    fn bool(&self, r: Reg) -> Result<bool, VmError> {
        match self.get(r) {
            Value::Bool(b) => Ok(b),
            v => Err(self.error(&format!("expected bool, found {}", type_name(v)))),
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
        Value::Unit => "unit",
        Value::Int(_) => "i64",
        Value::Bool(_) => "bool",
        Value::Str(_) => "str",
        Value::Agg(_) => "aggregate",
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
}
