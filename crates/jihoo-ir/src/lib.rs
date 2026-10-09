//! jihoo IR (JIR).
//!
//! The contract between the VM and the LLVM backend. The text format is defined in
//! `docs/jir.md`, and the C++ backend (`backend-llvm`) reads that text.
//!
//! To keep v0 simple:
//! - Every value is 64 bits wide (ints; bools are 0/1; strings are GC strings when hosted
//!   and plain addresses when freestanding).
//! - It is not strict SSA: a register may be assigned many times (a `let` variable is a
//!   register).
//!   The LLVM backend gives each register an alloca and lets mem2reg clean up.

mod print;
mod verify;

pub use verify::verify;

/// Language profile, selected with `#![freestanding]` at the top of a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Default: runs on the VM, garbage collected, std available.
    Hosted,
    /// No GC, core only, `syscall`/asm allowed. Compiled natively via LLVM.
    Freestanding,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Hosted => "hosted",
            Profile::Freestanding => "freestanding",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reg(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

#[derive(Debug, Clone)]
pub struct Module {
    pub profile: Profile,
    pub funcs: Vec<Function>,
}

impl Module {
    pub fn func(&self, name: &str) -> Option<&Function> {
        self.funcs.iter().find(|f| f.name == name)
    }
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    /// Arguments arrive in registers `%0 .. %(params-1)`.
    pub params: u32,
    pub num_regs: u32,
    /// `blocks[0]` is the entry block.
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub term: Terminator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn mnemonic(self) -> &'static str {
        match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::Div => "div",
            BinOp::Rem => "rem",
            BinOp::Eq => "eq",
            BinOp::Ne => "ne",
            BinOp::Lt => "lt",
            BinOp::Le => "le",
            BinOp::Gt => "gt",
            BinOp::Ge => "ge",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

impl UnOp {
    pub fn mnemonic(self) -> &'static str {
        match self {
            UnOp::Neg => "neg",
            UnOp::Not => "not",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Inst {
    Const { dst: Reg, value: i64 },
    Str { dst: Reg, value: String },
    Copy { dst: Reg, src: Reg },
    Unary { dst: Reg, op: UnOp, src: Reg },
    Binary { dst: Reg, op: BinOp, lhs: Reg, rhs: Reg },
    Call { dst: Reg, func: String, args: Vec<Reg> },
    /// Freestanding only. First argument is the syscall number, then up to 6 more.
    Syscall { dst: Reg, args: Vec<Reg> },
    /// Hosted-only builtin.
    Print { src: Reg },
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Jump(BlockId),
    /// Goes to `then` when `cond != 0`.
    Branch { cond: Reg, then: BlockId, els: BlockId },
    Ret(Reg),
}

impl Terminator {
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Jump(b) => vec![*b],
            Terminator::Branch { then, els, .. } => vec![*then, *els],
            Terminator::Ret(_) => vec![],
        }
    }
}
