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
    /// `Some` for an `enum`, whose `fields` are empty.
    pub variants: Option<Vec<VariantDecl>>,
}

impl StructDecl {
    pub fn is_enum(&self) -> bool {
        self.variants.is_some()
    }

    /// `struct` or `enum`, for messages.
    pub fn kind(&self) -> &'static str {
        if self.is_enum() {
            "enum"
        } else {
            "struct"
        }
    }
}

/// A variant of an enum: `Circle(i64)`, or `Empty` without a payload.
#[derive(Debug, Clone)]
pub struct VariantDecl {
    pub pos: Pos,
    pub name: String,
    pub fields: Vec<TypeExpr>,
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
    /// `extern fn name(...)`: a C function, defined elsewhere (native mode).
    /// Its `body` is empty.
    pub is_extern: bool,
    /// An extern function ending in `...`, like `printf`.
    pub variadic: bool,
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
    /// `ref T`: an immutable GC reference.
    Ref(Box<TypeExpr>),
    /// `chan T`: a channel between tasks.
    Chan(Box<TypeExpr>),
    /// `cell T`: a shared, mutable `T`.
    Cell(Box<TypeExpr>),
    /// `fn(A, B) -> R`; without `-> R` the function returns unit.
    Fn(Vec<TypeExpr>, Option<Box<TypeExpr>>),
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
    /// `target op= value`, such as `i += 1`: the target is evaluated once.
    OpAssign {
        pos: Pos,
        op: BinOp,
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
    /// `break`: leaves the innermost `while`.
    Break {
        pos: Pos,
    },
    /// `continue`: goes on with the innermost `while`'s next test.
    Continue {
        pos: Pos,
    },
    /// `go f(x)`: runs the call in a new task.
    Go {
        pos: Pos,
        call: Expr,
    },
    /// `select { let v = recv(c) => ..., send(c, x) => ..., _ => ... }`
    Select {
        pos: Pos,
        arms: Vec<SelectArm>,
    },
    /// `match value { pattern => body ... }`
    Match {
        pos: Pos,
        value: Expr,
        arms: Vec<MatchArm>,
    },
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct SelectArm {
    pub pos: Pos,
    pub op: SelectOp,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub enum SelectOp {
    /// `recv(chan)`, or `let name = recv(chan)` to keep the value.
    Recv { bind: Option<String>, chan: Expr },
    /// `send(chan, value)`
    Send { chan: Expr, value: Expr },
    /// `_`: when no other arm can go ahead right away.
    Default,
}

/// An arm of a `match` statement: `pattern if guard => body`.
#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pos: Pos,
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Block,
}

/// An arm of a `match` expression: `pattern if guard => value`.
#[derive(Debug, Clone)]
pub struct MatchExprArm {
    pub pos: Pos,
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub pos: Pos,
    pub kind: PatternKind,
}

#[derive(Debug, Clone)]
pub enum PatternKind {
    /// `_`: anything.
    Wild,
    /// An integer literal, possibly negative.
    Int(i128),
    Bool(bool),
    /// A name: a variant without a payload if the matched enum has one by that
    /// name, else a new variable bound to the value. Names starting with an
    /// uppercase letter are always variants.
    Name(String),
    /// `Circle(p, _)`: a variant and patterns for its payload values.
    Variant(String, Vec<Pattern>),
    /// `Point { x, y: 0, .. }`: a struct and patterns for some of its fields
    /// (`x` alone is `x: x`); `true` with `..`, which ignores the other fields.
    Struct(String, Vec<(Pos, String, Pattern)>, bool),
    /// `p | q`: either pattern. Both bind the same names.
    Or(Vec<Pattern>),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub pos: Pos,
    pub kind: ExprKind,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    /// An integer literal, up to `u64::MAX`; `-5` is a negation of `5`.
    Int(u64),
    /// A float literal: `f64` unless its context wants `f32`.
    Float(f64),
    Bool(bool),
    Str(String),
    Var(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    /// A call of a function value that is not a plain name: `s.f(x)`,
    /// `fs[0](x)`, `make()(x)`.
    CallExpr(Box<Expr>, Vec<Expr>),
    /// A type that does not parse as an expression, where an expression is
    /// expected: a type argument, as in `Vec(fn(i64) -> i64)` or `Vec(chan u8)`.
    Type(TypeExpr),
    /// `chan(T)` or `chan(T, capacity)`: a new channel.
    NewChan(TypeExpr, Option<Box<Expr>>),
    /// `fn(x: i64, y) -> R { ... }`: an anonymous function, which may capture
    /// local variables.
    Lambda(Box<Lambda>),
    /// `match value { pattern => value ... }` where a value is expected.
    Match(Box<Expr>, Vec<MatchExprArm>),
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
    /// `ref value`: a new reference to a copy of `value`.
    NewRef(Box<Expr>),
    /// `cell(value)`: a new cell holding a copy of `value`.
    NewCell(Box<Expr>),
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

/// An anonymous function. Parameter and return types may be left out when the
/// expected function type gives them.
#[derive(Debug, Clone)]
pub struct Lambda {
    pub params: Vec<(Pos, String, Option<TypeExpr>)>,
    pub ret: Option<TypeExpr>,
    pub body: Block,
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
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Var(_) => {}
        ExprKind::Unary(_, x)
        | ExprKind::Field(x, _)
        | ExprKind::Deref(x)
        | ExprKind::AddrOf(x)
        | ExprKind::NewRef(x)
        | ExprKind::NewCell(x)
        | ExprKind::Comptime(x)
        | ExprKind::Hole(x) => set_pos(x, pos),
        ExprKind::Binary(_, l, r) | ExprKind::Index(l, r) | ExprKind::ArrayRepeat(l, r) => {
            set_pos(l, pos);
            set_pos(r, pos);
        }
        ExprKind::Call(_, args) | ExprKind::ArrayLit(args) => args.iter_mut().for_each(|a| set_pos(a, pos)),
        ExprKind::CallExpr(f, args) => {
            set_pos(f, pos);
            args.iter_mut().for_each(|a| set_pos(a, pos));
        }
        ExprKind::Type(t) => ty(t),
        ExprKind::Match(value, arms) => {
            set_pos(value, pos);
            for arm in arms {
                arm.pos = pos;
                set_pattern_pos(&mut arm.pattern, pos);
                if let Some(g) = &mut arm.guard {
                    set_pos(g, pos);
                }
                set_pos(&mut arm.value, pos);
            }
        }
        ExprKind::NewChan(t, cap) => {
            ty(t);
            if let Some(c) = cap {
                set_pos(c, pos);
            }
        }
        ExprKind::Lambda(l) => {
            for (p, _, t) in &mut l.params {
                *p = pos;
                if let Some(t) = t {
                    ty(t);
                }
            }
            if let Some(t) = &mut l.ret {
                ty(t);
            }
            set_block_pos(&mut l.body, pos);
        }
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
        TypeExprKind::Ptr(inner) | TypeExprKind::Ref(inner) | TypeExprKind::Chan(inner) | TypeExprKind::Cell(inner) => {
            set_type_pos(inner, pos)
        }
        TypeExprKind::Array(elem, n) => {
            set_type_pos(elem, pos);
            set_pos(n, pos);
        }
        TypeExprKind::Generic(_, args) => args.iter_mut().for_each(|a| set_pos(a, pos)),
        TypeExprKind::Fn(params, ret) => {
            params.iter_mut().for_each(|t| set_type_pos(t, pos));
            if let Some(r) = ret {
                set_type_pos(r, pos);
            }
        }
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
        Stmt::OpAssign { pos: p, target, value, .. } => {
            *p = pos;
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
        Stmt::Break { pos: p } | Stmt::Continue { pos: p } => *p = pos,
        Stmt::Go { pos: p, call } => {
            *p = pos;
            set_pos(call, pos);
        }
        Stmt::Select { pos: p, arms } => {
            *p = pos;
            for arm in arms {
                arm.pos = pos;
                match &mut arm.op {
                    SelectOp::Recv { chan, .. } => set_pos(chan, pos),
                    SelectOp::Send { chan, value } => {
                        set_pos(chan, pos);
                        set_pos(value, pos);
                    }
                    SelectOp::Default => {}
                }
                set_block_pos(&mut arm.body, pos);
            }
        }
        Stmt::Match { pos: p, value, arms } => {
            *p = pos;
            set_pos(value, pos);
            for arm in arms {
                arm.pos = pos;
                set_pattern_pos(&mut arm.pattern, pos);
                if let Some(g) = &mut arm.guard {
                    set_pos(g, pos);
                }
                set_block_pos(&mut arm.body, pos);
            }
        }
        Stmt::Expr(e) => set_pos(e, pos),
    }
}

fn set_pattern_pos(p: &mut Pattern, pos: Pos) {
    p.pos = pos;
    match &mut p.kind {
        PatternKind::Wild | PatternKind::Int(_) | PatternKind::Bool(_) | PatternKind::Name(_) => {}
        PatternKind::Variant(_, args) | PatternKind::Or(args) => args.iter_mut().for_each(|a| set_pattern_pos(a, pos)),
        PatternKind::Struct(_, fields, _) => fields.iter_mut().for_each(|(p, _, f)| {
            *p = pos;
            set_pattern_pos(f, pos);
        }),
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
        for v in s.variants.iter_mut().flatten() {
            v.pos = pos;
            v.fields.iter_mut().for_each(|t| set_type_pos(t, pos));
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

/// What a name does where [`names_in_expr`] and the like find it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRole {
    /// Declares a local: `let`, a parameter, a pattern binding.
    Binds,
    /// Refers to something: a variable, function, macro or type. A path such
    /// as `alias.item` is one name.
    Uses,
    /// Not a name: the source text of a macro argument, code that the macro
    /// will get as text.
    Code,
}

/// Calls `f` on every name in `e` that declares or refers to something
/// (field and variant names do not), with what it does. The compiler uses it
/// for macro hygiene.
pub fn names_in_expr(e: &mut Expr, f: &mut dyn FnMut(&mut String, NameRole)) {
    match &mut e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) => {}
        ExprKind::Var(n) => f(n, NameRole::Uses),
        ExprKind::Unary(_, x)
        | ExprKind::Field(x, _)
        | ExprKind::Deref(x)
        | ExprKind::AddrOf(x)
        | ExprKind::NewRef(x)
        | ExprKind::NewCell(x)
        | ExprKind::Comptime(x)
        | ExprKind::Hole(x) => names_in_expr(x, f),
        ExprKind::Binary(_, l, r) | ExprKind::Index(l, r) | ExprKind::ArrayRepeat(l, r) => {
            names_in_expr(l, f);
            names_in_expr(r, f);
        }
        ExprKind::Call(n, args) => {
            f(n, NameRole::Uses);
            args.iter_mut().for_each(|a| names_in_expr(a, f));
        }
        ExprKind::ArrayLit(args) => args.iter_mut().for_each(|a| names_in_expr(a, f)),
        ExprKind::CallExpr(callee, args) => {
            names_in_expr(callee, f);
            args.iter_mut().for_each(|a| names_in_expr(a, f));
        }
        ExprKind::Type(t) | ExprKind::SizeOf(t) | ExprKind::AlignOf(t) => names_in_type(t, f),
        ExprKind::Match(value, arms) => {
            names_in_expr(value, f);
            for arm in arms {
                names_in_pattern(&mut arm.pattern, f);
                if let Some(g) = &mut arm.guard {
                    names_in_expr(g, f);
                }
                names_in_expr(&mut arm.value, f);
            }
        }
        ExprKind::NewChan(t, cap) => {
            names_in_type(t, f);
            if let Some(c) = cap {
                names_in_expr(c, f);
            }
        }
        ExprKind::Lambda(l) => {
            for (_, n, t) in &mut l.params {
                f(n, NameRole::Binds);
                if let Some(t) = t {
                    names_in_type(t, f);
                }
            }
            if let Some(t) = &mut l.ret {
                names_in_type(t, f);
            }
            names_in_block(&mut l.body, f);
        }
        ExprKind::Quote(_, _, holes) => holes.iter_mut().for_each(|(_, h)| names_in_expr(h, f)),
        ExprKind::MacroCall(n, args) => {
            f(n, NameRole::Uses);
            for a in args {
                f(&mut a.text, NameRole::Code);
                names_in_expr(&mut a.expr, f);
            }
        }
        ExprKind::StructLit(t, fields) => {
            names_in_type(t, f);
            fields.iter_mut().for_each(|fi| names_in_expr(&mut fi.value, f));
        }
        ExprKind::Cast(x, t) => {
            names_in_expr(x, f);
            names_in_type(t, f);
        }
        ExprKind::Asm(a) => {
            if let Some((_, t)) = &mut a.output {
                names_in_type(t, f);
            }
            a.inputs.iter_mut().for_each(|(_, x)| names_in_expr(x, f));
        }
    }
}

fn names_in_type(t: &mut TypeExpr, f: &mut dyn FnMut(&mut String, NameRole)) {
    match &mut t.kind {
        TypeExprKind::Named(n) => f(n, NameRole::Uses),
        TypeExprKind::Generic(n, args) => {
            f(n, NameRole::Uses);
            args.iter_mut().for_each(|a| names_in_expr(a, f));
        }
        TypeExprKind::Ptr(inner) | TypeExprKind::Ref(inner) | TypeExprKind::Chan(inner) | TypeExprKind::Cell(inner) => {
            names_in_type(inner, f)
        }
        TypeExprKind::Array(elem, n) => {
            names_in_type(elem, f);
            names_in_expr(n, f);
        }
        TypeExprKind::Fn(params, ret) => {
            params.iter_mut().for_each(|p| names_in_type(p, f));
            if let Some(r) = ret {
                names_in_type(r, f);
            }
        }
    }
}

fn names_in_pattern(p: &mut Pattern, f: &mut dyn FnMut(&mut String, NameRole)) {
    match &mut p.kind {
        PatternKind::Wild | PatternKind::Int(_) | PatternKind::Bool(_) => {}
        PatternKind::Name(n) => f(n, NameRole::Binds),
        PatternKind::Variant(_, args) | PatternKind::Or(args) => args.iter_mut().for_each(|a| names_in_pattern(a, f)),
        PatternKind::Struct(n, fields, _) => {
            f(n, NameRole::Uses);
            fields.iter_mut().for_each(|(_, _, fp)| names_in_pattern(fp, f));
        }
    }
}

pub fn names_in_block(b: &mut Block, f: &mut dyn FnMut(&mut String, NameRole)) {
    b.stmts.iter_mut().for_each(|s| names_in_stmt(s, f));
}

pub fn names_in_stmt(s: &mut Stmt, f: &mut dyn FnMut(&mut String, NameRole)) {
    match s {
        Stmt::Let { name, ty, value, .. } => {
            f(name, NameRole::Binds);
            if let Some(t) = ty {
                names_in_type(t, f);
            }
            names_in_expr(value, f);
        }
        Stmt::Assign { target, value } | Stmt::OpAssign { target, value, .. } => {
            names_in_expr(target, f);
            names_in_expr(value, f);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                names_in_expr(v, f);
            }
        }
        Stmt::If { cond, then, els } => {
            names_in_expr(cond, f);
            names_in_block(then, f);
            if let Some(e) = els {
                names_in_block(e, f);
            }
        }
        Stmt::While { cond, body } => {
            names_in_expr(cond, f);
            names_in_block(body, f);
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Go { call, .. } => names_in_expr(call, f),
        Stmt::Select { arms, .. } => {
            for arm in arms {
                match &mut arm.op {
                    SelectOp::Recv { bind, chan } => {
                        if let Some(b) = bind {
                            f(b, NameRole::Binds);
                        }
                        names_in_expr(chan, f);
                    }
                    SelectOp::Send { chan, value } => {
                        names_in_expr(chan, f);
                        names_in_expr(value, f);
                    }
                    SelectOp::Default => {}
                }
                names_in_block(&mut arm.body, f);
            }
        }
        Stmt::Match { value, arms, .. } => {
            names_in_expr(value, f);
            for arm in arms {
                names_in_pattern(&mut arm.pattern, f);
                if let Some(g) = &mut arm.guard {
                    names_in_expr(g, f);
                }
                names_in_block(&mut arm.body, f);
            }
        }
        Stmt::Expr(e) => names_in_expr(e, f),
    }
}

/// Like [`names_in_expr`], for every item of a program. Item names are not
/// visited: items are not hygienic.
pub fn names_in_program(p: &mut Program, f: &mut dyn FnMut(&mut String, NameRole)) {
    for s in &mut p.structs {
        for prm in &mut s.params {
            f(&mut prm.name, NameRole::Binds);
            names_in_type(&mut prm.ty, f);
        }
        s.fields.iter_mut().for_each(|fd| names_in_type(&mut fd.ty, f));
        for v in s.variants.iter_mut().flatten() {
            v.fields.iter_mut().for_each(|t| names_in_type(t, f));
        }
    }
    for c in &mut p.consts {
        if let Some(t) = &mut c.ty {
            names_in_type(t, f);
        }
        names_in_expr(&mut c.value, f);
    }
    for func in &mut p.funcs {
        for prm in &mut func.params {
            f(&mut prm.name, NameRole::Binds);
            names_in_type(&mut prm.ty, f);
        }
        if let Some(t) = &mut func.ret {
            names_in_type(t, f);
        }
        names_in_block(&mut func.body, f);
    }
    for m in &mut p.macro_calls {
        f(&mut m.name, NameRole::Uses);
        for a in &mut m.args {
            f(&mut a.text, NameRole::Code);
            names_in_expr(&mut a.expr, f);
        }
    }
}
