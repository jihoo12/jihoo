//! A module prepared for the VM.
//!
//! The VM runs JIR as it is, but some of what an instruction needs would
//! otherwise be worked out every time it runs: which function a call names
//! (a lookup by name), what value a constant is (it depends on the register's
//! type), which operation a binary instruction is (it depends on the operands'
//! type). `Program` works that out once per function, as an `Op` per
//! instruction, and leaves the other instructions to run from the JIR.
//!
//! A program can grow with its module: compile-time evaluation adds functions
//! to one module as it needs them (`Program::extend`).

use std::collections::HashMap;

use jihoo_ir::{BinOp, Function, Inst, IntTy, Module, Reg, Type};

use crate::Value;

#[derive(Debug, Clone, Default)]
pub struct Program {
    /// Function index by name.
    index: HashMap<String, usize>,
    /// For each function, for each block, an `Op` per instruction.
    code: Vec<Box<[Box<[Op]>]>>,
}

#[derive(Debug, Clone)]
pub(crate) enum Op {
    /// `const` or `fconst`, as the value it puts in the register.
    Const { dst: Reg, value: Value },
    Copy { dst: Reg, src: Reg },
    /// A binary operation on integers of type `ty`.
    Int { dst: Reg, op: BinOp, ty: IntTy, lhs: Reg, rhs: Reg },
    /// A call of the function with index `func`.
    Call { dst: Reg, func: u32, args: Box<[Reg]> },
    FuncRef { dst: Reg, func: u32 },
    /// Any other instruction: the VM runs the JIR instruction at this place.
    Inst,
}

impl Program {
    pub fn of(module: &Module) -> Program {
        let mut p = Program::default();
        p.extend(module);
        p
    }

    /// Prepares the functions `module` has gained since this program was made
    /// from it (all of them, for a new program). Every function they call must
    /// be in `module`.
    pub fn extend(&mut self, module: &Module) {
        let from = self.code.len();
        for (i, f) in module.funcs.iter().enumerate().skip(from) {
            self.index.insert(f.name.clone(), i);
        }
        for f in &module.funcs[from..] {
            let code = f.blocks.iter().map(|b| b.insts.iter().map(|i| self.op(f, i)).collect()).collect();
            self.code.push(code);
        }
    }

    pub fn get(&self, name: &str) -> Option<usize> {
        self.index.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.code.len()
    }

    pub fn is_empty(&self) -> bool {
        self.code.is_empty()
    }

    /// The ops of block `block` of function `func`.
    #[inline]
    pub(crate) fn block(&self, func: usize, block: usize) -> &[Op] {
        &self.code[func][block]
    }

    fn func(&self, name: &str) -> u32 {
        let i = self.get(name).unwrap_or_else(|| panic!("no function `{name}` in the module"));
        i as u32
    }

    fn op(&self, f: &Function, inst: &Inst) -> Op {
        match inst {
            Inst::Const { dst, value } => {
                let value = match f.reg_type(*dst) {
                    Type::Bool => Value::Bool(*value != 0),
                    _ => Value::Int(*value),
                };
                Op::Const { dst: *dst, value }
            }
            Inst::FConst { dst, value } => Op::Const { dst: *dst, value: Value::float(*value) },
            Inst::Copy { dst, src } => Op::Copy { dst: *dst, src: *src },
            Inst::Binary { dst, op, lhs, rhs } => match f.reg_type(*lhs) {
                &Type::Int(ty) => Op::Int { dst: *dst, op: *op, ty, lhs: *lhs, rhs: *rhs },
                _ => Op::Inst,
            },
            Inst::Call { dst, func, args } => Op::Call { dst: *dst, func: self.func(func), args: args.clone().into() },
            Inst::FuncRef { dst, func } => Op::FuncRef { dst: *dst, func: self.func(func) },
            _ => Op::Inst,
        }
    }
}

/// `x op y` for integers of type `t`, in canonical form; `None` for a division
/// by zero.
#[inline]
pub(crate) fn int_binary(op: BinOp, t: IntTy, x: i64, y: i64) -> Option<Value> {
    use Value::{Bool, Int};
    let (ux, uy) = (x as u64, y as u64);
    let signed = t.signed();
    Some(match op {
        BinOp::Add => Int(t.wrap(x.wrapping_add(y))),
        BinOp::Sub => Int(t.wrap(x.wrapping_sub(y))),
        BinOp::Mul => Int(t.wrap(x.wrapping_mul(y))),
        BinOp::Div | BinOp::Rem if y == 0 => return None,
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
        // Canonical values of one type have the same high bits, so these
        // stay canonical.
        BinOp::And => Int(x & y),
        BinOp::Or => Int(x | y),
        BinOp::Xor => Int(x ^ y),
        BinOp::Shl => Int(t.wrap(x.wrapping_shl(y as u32 & (t.bits() - 1)))),
        // Canonical form already sign- or zero-extends, so a 64-bit shift of
        // the right kind gives the right result for every width.
        BinOp::Shr if signed => Int(x >> (y as u32 & (t.bits() - 1))),
        BinOp::Shr => Int((ux >> (y as u32 & (t.bits() - 1))) as i64),
    })
}
