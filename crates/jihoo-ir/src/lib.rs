//! jihoo IR (JIR).
//!
//! The contract between the VM and the LLVM backend. The text format is defined in
//! `docs/jir.md`, and the C++ backend (`backend-llvm`) reads that text.
//!
//! Every register has a type, declared once per function. Registers are not strict
//! SSA: a register may be assigned many times (a `let` variable is a register).
//! The LLVM backend gives each register an alloca and lets mem2reg clean up.

pub mod layout;
mod print;
pub mod types;
mod verify;

pub use types::{IntTy, Type};
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

    /// Name of the entry function.
    pub fn entry(self) -> &'static str {
        match self {
            Profile::Hosted => "main",
            Profile::Freestanding => "_start",
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
    pub structs: Vec<StructDef>,
    pub funcs: Vec<Function>,
}

impl Module {
    pub fn func(&self, name: &str) -> Option<&Function> {
        self.funcs.iter().find(|f| f.name == name)
    }

    pub fn struct_def(&self, name: &str) -> Option<&StructDef> {
        self.structs.iter().find(|s| s.name == name)
    }
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    /// Field names are kept for readability; instructions address fields by index.
    pub fields: Vec<(String, Type)>,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Type>,
    pub ret: Type,
    /// Type of every register. Arguments arrive in `%0 .. %(params-1)`, so this
    /// starts with `params`.
    pub regs: Vec<Type>,
    /// `blocks[0]` is the entry block.
    pub blocks: Vec<Block>,
}

impl Function {
    pub fn reg_type(&self, r: Reg) -> &Type {
        &self.regs[r.0 as usize]
    }
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
    And,
    Or,
    Xor,
    /// Shift amounts are taken modulo the bit width, so every shift is defined.
    Shl,
    /// Arithmetic for signed types, logical for unsigned ones.
    Shr,
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
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Xor => "xor",
            BinOp::Shl => "shl",
            BinOp::Shr => "shr",
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
    /// An integer or `bool` (0/1) constant, depending on the type of `dst`.
    /// Integers are stored in canonical form (see [`IntTy::wrap`]).
    Const { dst: Reg, value: i64 },
    /// The unit value.
    Unit { dst: Reg },
    /// String literal: `str` when hosted, `ptr` to constant bytes when freestanding.
    Str { dst: Reg, value: String },
    Copy { dst: Reg, src: Reg },
    Unary { dst: Reg, op: UnOp, src: Reg },
    Binary { dst: Reg, op: BinOp, lhs: Reg, rhs: Reg },
    /// Converts between integer types, bool to integer, and pointers.
    Cast { dst: Reg, src: Reg },
    Call { dst: Reg, func: String, args: Vec<Reg> },
    /// Function `func` as a value, of type `fn(params) -> ret`.
    FuncRef { dst: Reg, func: String },
    /// Calls the function value in `callee`.
    CallIndirect { dst: Reg, callee: Reg, args: Vec<Reg> },
    /// Builds a struct value from all of its fields, in declaration order.
    Struct { dst: Reg, name: String, fields: Vec<Reg> },
    /// Reads field `index` of a struct value.
    Field { dst: Reg, src: Reg, index: u32 },
    /// A copy of struct `src` with field `index` replaced by `value`.
    SetField { dst: Reg, src: Reg, index: u32, value: Reg },
    /// Freestanding only: `*ptr`.
    Load { dst: Reg, ptr: Reg },
    /// Freestanding only: `*ptr = value`.
    Store { ptr: Reg, value: Reg },
    /// Freestanding only: the address of register `src`.
    Addr { dst: Reg, src: Reg },
    /// Freestanding only: the address of field `index` of the struct `ptr` points to.
    FieldPtr { dst: Reg, ptr: Reg, index: u32 },
    /// Builds an array from all of its elements.
    Array { dst: Reg, items: Vec<Reg> },
    /// An array with every element set to `value`.
    Splat { dst: Reg, value: Reg },
    /// Reads element `index` (an `i64`) of an array value. Bounds-checked.
    Elem { dst: Reg, src: Reg, index: Reg },
    /// A copy of array `src` with element `index` replaced by `value`. Bounds-checked.
    SetElem { dst: Reg, src: Reg, index: Reg, value: Reg },
    /// Freestanding only: the address of element `index` of the array `ptr` points
    /// to. Bounds-checked.
    ElemPtr { dst: Reg, ptr: Reg, index: Reg },
    /// Freestanding only. First argument is the syscall number, then up to 6 more.
    Syscall { dst: Reg, args: Vec<Reg> },
    /// Hosted-only builtin.
    Print { src: Reg },
    /// Hosted (and macros): an integer or bool as text.
    ToStr { dst: Reg, src: Reg },
    /// Compile time only (macros): `prefix` plus a number never handed out before
    /// in this compilation, for names that cannot clash.
    Unique { dst: Reg, prefix: Reg },
    /// Freestanding only: inline assembly. `template` uses LLVM operand syntax
    /// (`$0` is the output if there is one, then the inputs) and `constraints` is an
    /// LLVM constraint string with one input entry per register in `args`. `dst`
    /// is the output, or `unit`.
    Asm { dst: Reg, template: String, constraints: String, args: Vec<Reg> },
    /// Compile time only (macros): builds code (`expr`, `stmts` or `items`, by the
    /// type of `dst`) from template text and holes, with
    /// `pieces.len() == holes.len() + 1`. `kinds` says where each hole sits.
    Quote { dst: Reg, pieces: Vec<String>, holes: Vec<Reg>, kinds: Vec<HoleKind> },
}

/// Where a hole of a `quote` sits, which decides how its value is inserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleKind {
    /// An expression: code in parentheses, other values as literals.
    Expr,
    /// A name: a `str` (or `expr`) that must be an identifier, inserted as is.
    Ident,
    /// A statement of its own: `stmts` or `expr` code.
    Stmts,
    /// An item of its own: `items` code.
    Items,
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Jump(BlockId),
    /// Goes to `then` when the `bool` `cond` is true.
    Branch { cond: Reg, then: BlockId, els: BlockId },
    Ret(Reg),
    /// Control never gets here.
    Unreachable,
}

impl Terminator {
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Jump(b) => vec![*b],
            Terminator::Branch { then, els, .. } => vec![*then, *els],
            Terminator::Ret(_) | Terminator::Unreachable => vec![],
        }
    }

    pub fn successors_mut(&mut self) -> Vec<&mut BlockId> {
        match self {
            Terminator::Jump(b) => vec![b],
            Terminator::Branch { then, els, .. } => vec![then, els],
            Terminator::Ret(_) | Terminator::Unreachable => vec![],
        }
    }
}
