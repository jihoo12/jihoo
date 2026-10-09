use crate::Pos;

#[derive(Debug, Clone)]
pub struct Program {
    /// File-level attributes such as `#![freestanding]`.
    pub attrs: Vec<(Pos, String)>,
    pub structs: Vec<StructDecl>,
    pub consts: Vec<ConstDecl>,
    pub funcs: Vec<FnDecl>,
}

/// `const NAME: T = value`, evaluated at compile time.
#[derive(Debug, Clone)]
pub struct ConstDecl {
    pub pos: Pos,
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub pos: Pos,
    pub name: String,
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub pos: Pos,
    pub name: String,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub pos: Pos,
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub pos: Pos,
    /// `comptime x: T`: the argument is evaluated at compile time and the
    /// function is instantiated once per distinct value (`comptime T: type` makes
    /// it generic over a type).
    pub comptime: bool,
    pub name: String,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone)]
pub struct TypeExpr {
    pub pos: Pos,
    pub kind: TypeExprKind,
}

#[derive(Debug, Clone)]
pub enum TypeExprKind {
    /// `i64`, `bool`, `Point`, ...
    Named(String),
    /// `*T`
    Ptr(Box<TypeExpr>),
    /// `[T; N]`, where `N` is evaluated at compile time.
    Array(Box<TypeExpr>, Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Position of the closing `}`.
    pub end: Pos,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        pos: Pos,
        name: String,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    /// `target = value`. The target is any expression; the checker makes sure it
    /// is a place (variable, field, `*ptr`, `ptr[i]`).
    Assign {
        target: Expr,
        value: Expr,
    },
    Return {
        pos: Pos,
        value: Option<Expr>,
    },
    If {
        cond: Expr,
        then: Block,
        /// `else if` is a block holding a single `If`.
        els: Option<Block>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub pos: Pos,
    pub kind: ExprKind,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Bool(bool),
    Str(String),
    Var(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    /// `Point { x: 1, y: 2 }`
    StructLit(String, Vec<FieldInit>),
    /// `base.field`
    Field(Box<Expr>, String),
    /// `array[index]` or `ptr[index]`
    Index(Box<Expr>, Box<Expr>),
    /// `*ptr`
    Deref(Box<Expr>),
    /// `&place`
    AddrOf(Box<Expr>),
    /// `value as T`
    Cast(Box<Expr>, TypeExpr),
    /// `[a, b, c]`
    ArrayLit(Vec<Expr>),
    /// `[value; N]`, where `N` is evaluated at compile time.
    ArrayRepeat(Box<Expr>, Box<Expr>),
    /// `comptime expr`: evaluated while compiling, then used as a constant.
    Comptime(Box<Expr>),
    /// `size_of(T)`
    SizeOf(TypeExpr),
    /// `align_of(T)`
    AlignOf(TypeExpr),
}

#[derive(Debug, Clone)]
pub struct FieldInit {
    pub pos: Pos,
    pub name: String,
    pub value: Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
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
}
