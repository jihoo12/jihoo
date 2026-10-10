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

pub use types::{FloatTy, IntTy, Type};
pub use verify::verify;

/// Version of the JIR text format, written in the `jir N` header. The LLVM
/// backend reads exactly one version (`jir::kVersion` in
/// `backend-llvm/src/jir.h`), so a `jihoo-llc` from another build refuses the
/// module instead of compiling it with other rules. Bump it whenever the
/// format or the meaning of anything in it changes (see "Versions" in
/// `docs/jir.md`); the `version` tests check that both sides and the spec agree.
pub const JIR_VERSION: u32 = 1;

/// Language profile, selected with `#![native]` or `#![freestanding]` at the
/// top of a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Default: runs on the VM, garbage collected, std available.
    Hosted,
    /// Compiled natively via LLVM to a program for the operating system: links
    /// with libc, calls C functions (`extern fn`), starts at a C `main`. No GC.
    Native,
    /// Compiled natively via LLVM with no OS libraries at all: core only,
    /// `syscall`/asm, starts at `_start`. No GC.
    Freestanding,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Hosted => "hosted",
            Profile::Native => "native",
            Profile::Freestanding => "freestanding",
        }
    }

    /// Name of the entry function.
    pub fn entry(self) -> &'static str {
        match self {
            Profile::Hosted | Profile::Native => "main",
            Profile::Freestanding => "_start",
        }
    }

    /// Compiled by LLVM rather than run on the VM: no GC, but raw pointers,
    /// `syscall` and inline asm.
    pub fn is_compiled(self) -> bool {
        self != Profile::Hosted
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
    pub enums: Vec<EnumDef>,
    /// C functions the program calls (native only), by symbol name.
    pub externs: Vec<ExternFn>,
    pub funcs: Vec<Function>,
}

impl Module {
    pub fn func(&self, name: &str) -> Option<&Function> {
        self.funcs.iter().find(|f| f.name == name)
    }

    pub fn extern_fn(&self, name: &str) -> Option<&ExternFn> {
        self.externs.iter().find(|f| f.name == name)
    }

    pub fn struct_def(&self, name: &str) -> Option<&StructDef> {
        self.structs.iter().find(|s| s.name == name)
    }

    pub fn enum_def(&self, name: &str) -> Option<&EnumDef> {
        self.enums.iter().find(|e| e.name == name)
    }

    /// What a struct or enum type holds, for [`layout::of`].
    pub fn members(&self, t: &Type) -> Option<Vec<Vec<Type>>> {
        match t {
            Type::Struct(name) => Some(vec![self.struct_def(name)?.fields.iter().map(|(_, t)| t.clone()).collect()]),
            Type::Enum(name) => Some(self.enum_def(name)?.variants.iter().map(|(_, ts)| ts.clone()).collect()),
            _ => None,
        }
    }
}

/// A function defined outside the program and called with the C calling
/// convention. `call` and `funcref` name it like any other function. Only
/// C-compatible types appear in it (see [`types::c_compatible`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ExternFn {
    /// The C symbol.
    pub name: String,
    pub params: Vec<Type>,
    pub ret: Type,
    /// Takes more arguments after `params`, like `printf`. Those are passed
    /// with C's default promotions.
    pub variadic: bool,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    /// Field names are kept for readability; instructions address fields by index.
    pub fields: Vec<(String, Type)>,
}

/// A sum type: a value is one of the variants, each with its own payload.
/// Instructions address variants by index.
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<(String, Vec<Type>)>,
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
    /// A float constant, of the type of `dst`. For an `f32`, `value` is exactly
    /// representable as `f32`.
    FConst { dst: Reg, value: f64 },
    /// The unit value.
    Unit { dst: Reg },
    /// String literal: `str` when hosted, `ptr` to constant
    /// NUL-terminated bytes when compiled.
    Str { dst: Reg, value: String },
    Copy { dst: Reg, src: Reg },
    Unary { dst: Reg, op: UnOp, src: Reg },
    Binary { dst: Reg, op: BinOp, lhs: Reg, rhs: Reg },
    /// Converts between integer and float types, bool to integer, and pointers.
    Cast { dst: Reg, src: Reg },
    Call { dst: Reg, func: String, args: Vec<Reg> },
    /// Function `func` as a value, of type `fn(params) -> ret`.
    FuncRef { dst: Reg, func: String },
    /// Calls the function value in `callee`.
    CallIndirect { dst: Reg, callee: Reg, args: Vec<Reg> },
    /// Hosted only: a closure, the function value that calls `func` with
    /// `captures` followed by its own arguments. `func`'s leading parameters
    /// take the captured values.
    Closure { dst: Reg, func: String, captures: Vec<Reg> },
    /// Builds a struct value from all of its fields, in declaration order.
    Struct { dst: Reg, name: String, fields: Vec<Reg> },
    /// Reads field `index` of a struct value.
    Field { dst: Reg, src: Reg, index: u32 },
    /// A copy of struct `src` with field `index` replaced by `value`.
    SetField { dst: Reg, src: Reg, index: u32, value: Reg },
    /// Native and freestanding only: `*ptr`.
    Load { dst: Reg, ptr: Reg },
    /// Native and freestanding only: `*ptr = value`.
    Store { ptr: Reg, value: Reg },
    /// Native and freestanding only: the address of register `src`.
    Addr { dst: Reg, src: Reg },
    /// Native and freestanding only: the address of field `index` of the struct `ptr` points to.
    FieldPtr { dst: Reg, ptr: Reg, index: u32 },
    /// Builds variant `index` of the enum type of `dst` from its payload values.
    Variant { dst: Reg, index: u32, fields: Vec<Reg> },
    /// The variant index of enum value `src`, as a `u32`.
    Tag { dst: Reg, src: Reg },
    /// Payload value `index` of enum value `src`, which must be variant `variant`.
    Payload { dst: Reg, src: Reg, variant: u32, index: u32 },
    /// Hosted only: a new `ref T` holding a copy of `src`.
    Ref { dst: Reg, src: Reg },
    /// Hosted only: the value `ref T` `src` refers to.
    Deref { dst: Reg, src: Reg },
    /// Hosted only: a new channel of the type of `dst`, buffering up to `cap`
    /// (an `i64`) values; 0 makes sender and receiver meet.
    NewChan { dst: Reg, cap: Reg },
    /// Hosted only: sends `value` on channel `chan`, waiting while it is full.
    Send { chan: Reg, value: Reg },
    /// Hosted only: receives a value from channel `chan`, waiting until there is one.
    Recv { dst: Reg, chan: Reg },
    /// Hosted only: starts a task that calls the function value `callee` with
    /// `args`. Its result is dropped.
    Spawn { callee: Reg, args: Vec<Reg> },
    /// Hosted only: does the first of `cases` that can go ahead, waiting until
    /// one can, and sets `dst` (an `i64`) to its index. With `default`, it does
    /// not wait: if none can go ahead, `dst` is `cases.len()`.
    Select { dst: Reg, cases: Vec<SelectCase>, default: bool },
    /// Hosted only: a new cell holding `value`.
    NewCell { dst: Reg, value: Reg },
    /// Hosted only: reads what cell `cell` holds, or the part of it at `path`.
    CellGet { dst: Reg, cell: Reg, path: Vec<PathStep> },
    /// Hosted only: replaces what cell `cell` holds, or the part of it at
    /// `path`, by `value`. Everything holding the cell sees the change.
    CellSet { cell: Reg, path: Vec<PathStep>, value: Reg },
    /// Reads the part of aggregate `src` at `path` (fields and elements, outside
    /// in). Elements are bounds-checked.
    GetPath { dst: Reg, src: Reg, path: Vec<PathStep> },
    /// A copy of aggregate `src` with the part at `path` replaced by `value`.
    /// Elements are bounds-checked.
    SetPath { dst: Reg, src: Reg, path: Vec<PathStep>, value: Reg },
    /// Builds an array from all of its elements.
    Array { dst: Reg, items: Vec<Reg> },
    /// An array with every element set to `value`.
    Splat { dst: Reg, value: Reg },
    /// Reads element `index` (an `i64`) of an array value. Bounds-checked.
    Elem { dst: Reg, src: Reg, index: Reg },
    /// A copy of array `src` with element `index` replaced by `value`. Bounds-checked.
    SetElem { dst: Reg, src: Reg, index: Reg, value: Reg },
    /// Native and freestanding only: the address of element `index` of the array `ptr` points
    /// to. Bounds-checked.
    ElemPtr { dst: Reg, ptr: Reg, index: Reg },
    /// Native and freestanding only. First argument is the syscall number, then up to 6 more.
    Syscall { dst: Reg, args: Vec<Reg> },
    /// Hosted-only builtin.
    Print { src: Reg },
    /// Hosted (and macros): a number or bool as text.
    ToStr { dst: Reg, src: Reg },
    /// Compile time only (macros): `prefix` plus a number never handed out before
    /// in this compilation, for names that cannot clash.
    Unique { dst: Reg, prefix: Reg },
    /// Native and freestanding only: inline assembly. `template` uses LLVM operand syntax
    /// (`$0` is the output if there is one, then the inputs) and `constraints` is an
    /// LLVM constraint string with one input entry per register in `args`. `dst`
    /// is the output, or `unit`.
    Asm { dst: Reg, template: String, constraints: String, args: Vec<Reg> },
    /// Compile time only (macros): builds code (`expr`, `stmts` or `items`, by the
    /// type of `dst`) from template text and holes, with
    /// `pieces.len() == holes.len() + 1`. `kinds` says where each hole sits.
    Quote { dst: Reg, pieces: Vec<String>, holes: Vec<Reg>, kinds: Vec<HoleKind> },
}

/// One step into an aggregate, for `getpath` and `setpath`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStep {
    /// Field `N` of a struct.
    Field(u32),
    /// The element of an array at the index in this `i64` register.
    Elem(Reg),
}

/// One way a `select` can go ahead.
#[derive(Debug, Clone)]
pub enum SelectCase {
    /// Receive from `chan` into `dst`.
    Recv { dst: Reg, chan: Reg },
    /// Send `value` on `chan`.
    Send { chan: Reg, value: Reg },
}

fn path_regs(path: &mut [PathStep]) -> impl Iterator<Item = &mut Reg> {
    path.iter_mut().filter_map(|s| match s {
        PathStep::Elem(r) => Some(r),
        PathStep::Field(_) => None,
    })
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

    /// Every register the terminator reads.
    pub fn regs_mut(&mut self) -> Vec<&mut Reg> {
        match self {
            Terminator::Branch { cond, .. } => vec![cond],
            Terminator::Ret(r) => vec![r],
            Terminator::Jump(_) | Terminator::Unreachable => vec![],
        }
    }
}

impl Inst {
    /// Every register the instruction reads or writes, for passes that renumber
    /// registers.
    pub fn regs_mut(&mut self) -> Vec<&mut Reg> {
        use Inst::*;
        match self {
            Const { dst, .. } | FConst { dst, .. } | Unit { dst } | Str { dst, .. } | FuncRef { dst, .. } => vec![dst],
            Copy { dst, src }
            | Unary { dst, src, .. }
            | Cast { dst, src }
            | Field { dst, src, .. }
            | Tag { dst, src }
            | Payload { dst, src, .. }
            | Ref { dst, src }
            | Deref { dst, src }
            | NewChan { dst, cap: src }
            | Recv { dst, chan: src }
            | ToStr { dst, src }
            | Addr { dst, src }
            | Unique { dst, prefix: src }
            | Load { dst, ptr: src }
            | FieldPtr { dst, ptr: src, .. }
            | Splat { dst, value: src } => vec![dst, src],
            Binary { dst, lhs, rhs, .. }
            | Elem { dst, src: lhs, index: rhs }
            | SetField { dst, src: lhs, value: rhs, .. }
            | ElemPtr { dst, ptr: lhs, index: rhs } => vec![dst, lhs, rhs],
            SetElem { dst, src, index, value } => vec![dst, src, index, value],
            Store { ptr, value } | Send { chan: ptr, value } => vec![ptr, value],
            Spawn { callee, args } => std::iter::once(callee).chain(args).collect(),
            GetPath { dst, src, path } | CellGet { dst, cell: src, path } => {
                [dst, src].into_iter().chain(path_regs(path)).collect()
            }
            NewCell { dst, value } => vec![dst, value],
            CellSet { cell, path, value } => [cell, value].into_iter().chain(path_regs(path)).collect(),
            SetPath { dst, src, path, value } => [dst, src, value].into_iter().chain(path_regs(path)).collect(),
            Select { dst, cases, .. } => std::iter::once(dst)
                .chain(cases.iter_mut().flat_map(|c| match c {
                    SelectCase::Recv { dst, chan } => [dst, chan],
                    SelectCase::Send { chan, value } => [chan, value],
                }))
                .collect(),
            Print { src } => vec![src],
            Call { dst, args, .. }
            | Struct { dst, fields: args, .. }
            | Variant { dst, fields: args, .. }
            | Array { dst, items: args }
            | Syscall { dst, args }
            | Asm { dst, args, .. }
            | Quote { dst, holes: args, .. }
            | Closure { dst, captures: args, .. } => std::iter::once(dst).chain(args).collect(),
            CallIndirect { dst, callee, args } => [dst, callee].into_iter().chain(args).collect(),
        }
    }
}
