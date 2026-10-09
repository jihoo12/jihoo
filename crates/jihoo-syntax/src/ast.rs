use crate::Pos;

#[derive(Debug, Clone)]
pub struct Program {
    /// File-level attributes such as `#![freestanding]`.
    pub attrs: Vec<(Pos, String)>,
    pub imports: Vec<Import>,
    pub structs: Vec<StructDecl>,
    pub consts: Vec<ConstDecl>,
    pub funcs: Vec<FnDecl>,
    /// `name!(...)` at the top level: replaced by the items the macro returns.
    pub macro_calls: Vec<ItemMacro>,
}

#[derive(Debug, Clone)]
pub struct ItemMacro {
    pub pos: Pos,
    pub name: String,
    pub args: Vec<MacroArg>,
}

/// `import a.b` (the file `a/b.jh`, used as `b.item`) or `import a.b as c`.
#[derive(Debug, Clone)]
pub struct Import {
    pub pos: Pos,
    pub path: Vec<String>,
    pub alias: String,
}

/// `const NAME: T = value`, evaluated at compile time.
#[derive(Debug, Clone)]
pub struct ConstDecl {
    pub pos: Pos,
    /// `pub`: usable from other modules.
    pub is_pub: bool,
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub pos: Pos,
    /// `pub`: usable from other modules, fields included.
    pub is_pub: bool,
    pub name: String,
    /// `struct Pair(T: type)`: compile-time parameters, which make the struct
    /// generic. Each distinct set of arguments is its own struct type.
    pub params: Vec<Param>,
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
    /// `pub`: usable from other modules.
    pub is_pub: bool,
    /// `macro name(...) -> expr { ... }`: run at compile time by `name!(...)`.
    pub is_macro: bool,
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
    /// `Pair(i64)`: an instance of a generic struct. Arguments are written as
    /// expressions; type arguments are read back as types.
    Generic(String, Vec<Expr>),
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
    /// `Point { x: 1, y: 2 }` or `Pair(i64) { a: 1, b: 2 }`
    StructLit(TypeExpr, Vec<FieldInit>),
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
    /// `asm("template", out(...) T, in(...) x, clobber(...))`
    Asm(Box<AsmExpr>),
    /// `name!(args)`: a macro call, replaced by the code the macro returns.
    MacroCall(String, Vec<MacroArg>),
    /// Code as a value, inside macros: `quote(a + $x)` (an expression),
    /// `quote { ... }` (statements) or `quote items { ... }` (items). `pieces` is
    /// the template text around the holes, so `pieces.len() == holes.len() + 1`.
    Quote(CodeKind, Vec<String>, Vec<(HoleKind, Expr)>),
    /// `$x` or `$(expr)` inside a quote template; only appears in the template's
    /// parsed form, which exists to check that the template is an expression.
    Hole(Box<Expr>),
    /// `comptime expr`: evaluated while compiling, then used as a constant.
    Comptime(Box<Expr>),
    /// `size_of(T)`
    SizeOf(TypeExpr),
    /// `align_of(T)`
    AlignOf(TypeExpr),
}

/// What kind of code a quote, or a macro, produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    Expr,
    Stmts,
    Items,
}

/// Where a hole sits in a quote template, which decides how a value is inserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleKind {
    /// In place of an expression.
    Expr,
    /// In place of a name: `fn $name()`, `let $v = ...`.
    Ident,
    /// As a statement of its own: `$body`.
    Stmts,
    /// As an item of its own, in `quote items { ... }`.
    Items,
}

/// An argument of a macro call: its source text and its parsed form.
#[derive(Debug, Clone)]
pub struct MacroArg {
    pub text: String,
    pub expr: Expr,
}

/// Inline assembly. Freestanding only.
#[derive(Debug, Clone)]
pub struct AsmExpr {
    /// The template lines, joined with newlines. `{0}`, `{1}`, ... are the inputs
    /// and `{out}` is the output.
    pub template: String,
    pub output: Option<(AsmReg, TypeExpr)>,
    pub inputs: Vec<(AsmReg, Expr)>,
    pub clobbers: Vec<String>,
}

/// Where an asm operand lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmReg {
    /// `"rdi"`: that register.
    Named(String),
    /// `reg`: any general-purpose register.
    Any,
    /// `in(out) x`: the same register as the output, which starts out holding `x`.
    Out,
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
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

/// Sets the position of `e` and everything inside it to `pos`. Code produced by a
/// macro has no source of its own, so errors in it point at the macro call.
pub fn set_pos(e: &mut Expr, pos: Pos) {
    e.pos = pos;
    let ty = |t: &mut TypeExpr| set_type_pos(t, pos);
    match &mut e.kind {
        ExprKind::Int(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Var(_) => {}
        ExprKind::Unary(_, x)
        | ExprKind::Field(x, _)
        | ExprKind::Deref(x)
        | ExprKind::AddrOf(x)
        | ExprKind::Comptime(x)
        | ExprKind::Hole(x) => set_pos(x, pos),
        ExprKind::Binary(_, l, r) | ExprKind::Index(l, r) | ExprKind::ArrayRepeat(l, r) => {
            set_pos(l, pos);
            set_pos(r, pos);
        }
        ExprKind::Call(_, args) | ExprKind::ArrayLit(args) => args.iter_mut().for_each(|a| set_pos(a, pos)),
        ExprKind::Quote(_, _, holes) => holes.iter_mut().for_each(|(_, a)| set_pos(a, pos)),
        ExprKind::MacroCall(_, args) => args.iter_mut().for_each(|a| set_pos(&mut a.expr, pos)),
        ExprKind::StructLit(t, fields) => {
            ty(t);
            fields.iter_mut().for_each(|f| {
                f.pos = pos;
                set_pos(&mut f.value, pos);
            })
        }
        ExprKind::Cast(x, t) => {
            set_pos(x, pos);
            ty(t);
        }
        ExprKind::SizeOf(t) | ExprKind::AlignOf(t) => ty(t),
        ExprKind::Asm(a) => {
            if let Some((_, t)) = &mut a.output {
                ty(t);
            }
            a.inputs.iter_mut().for_each(|(_, x)| set_pos(x, pos));
        }
    }
}

fn set_type_pos(t: &mut TypeExpr, pos: Pos) {
    t.pos = pos;
    match &mut t.kind {
        TypeExprKind::Named(_) => {}
        TypeExprKind::Ptr(inner) => set_type_pos(inner, pos),
        TypeExprKind::Array(elem, n) => {
            set_type_pos(elem, pos);
            set_pos(n, pos);
        }
        TypeExprKind::Generic(_, args) => args.iter_mut().for_each(|a| set_pos(a, pos)),
    }
}

/// Like [`set_pos`], for every statement of a block.
pub fn set_block_pos(b: &mut Block, pos: Pos) {
    b.end = pos;
    b.stmts.iter_mut().for_each(|s| set_stmt_pos(s, pos));
}

pub fn set_stmt_pos(s: &mut Stmt, pos: Pos) {
    match s {
        Stmt::Let { pos: p, ty, value, .. } => {
            *p = pos;
            if let Some(t) = ty {
                set_type_pos(t, pos);
            }
            set_pos(value, pos);
        }
        Stmt::Assign { target, value } => {
            set_pos(target, pos);
            set_pos(value, pos);
        }
        Stmt::Return { pos: p, value } => {
            *p = pos;
            if let Some(v) = value {
                set_pos(v, pos);
            }
        }
        Stmt::If { cond, then, els } => {
            set_pos(cond, pos);
            set_block_pos(then, pos);
            if let Some(e) = els {
                set_block_pos(e, pos);
            }
        }
        Stmt::While { cond, body } => {
            set_pos(cond, pos);
            set_block_pos(body, pos);
        }
        Stmt::Expr(e) => set_pos(e, pos),
    }
}

/// Like [`set_pos`], for every item of a program (the output of an item macro).
pub fn set_program_pos(p: &mut Program, pos: Pos) {
    for s in &mut p.structs {
        s.pos = pos;
        for prm in &mut s.params {
            prm.pos = pos;
            set_type_pos(&mut prm.ty, pos);
        }
        for f in &mut s.fields {
            f.pos = pos;
            set_type_pos(&mut f.ty, pos);
        }
    }
    for c in &mut p.consts {
        c.pos = pos;
        if let Some(t) = &mut c.ty {
            set_type_pos(t, pos);
        }
        set_pos(&mut c.value, pos);
    }
    for f in &mut p.funcs {
        f.pos = pos;
        for prm in &mut f.params {
            prm.pos = pos;
            set_type_pos(&mut prm.ty, pos);
        }
        if let Some(t) = &mut f.ret {
            set_type_pos(t, pos);
        }
        set_block_pos(&mut f.body, pos);
    }
    for m in &mut p.macro_calls {
        m.pos = pos;
        m.args.iter_mut().for_each(|a| set_pos(&mut a.expr, pos));
    }
}
