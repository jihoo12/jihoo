//! Recursive descent parser.
//!
//! Semicolons are optional. A newline ends a statement, and a binary operator or a
//! postfix `(`, `[`, `.` or `as` at the start of the next line does not continue the
//! expression (similar to Go).
//!
//! As in Rust, struct literals are not allowed directly in `if`/`while` conditions,
//! so that `if x == y { ... }` is not read as a literal `y { ... }`. Parenthesize
//! them there.

use crate::ast::*;
use crate::lexer::{lex, Tok, Token};
use crate::{Error, Pos};

pub fn parse(src: &str) -> Result<Program, Error> {
    let toks = lex(src)?;
    Parser { toks, i: 0, no_struct_lit: false }.program()
}

struct Parser {
    toks: Vec<Token>,
    i: usize,
    /// Set while parsing an `if`/`while` condition.
    no_struct_lit: bool,
}

type PResult<T> = Result<T, Error>;

impl Parser {
    fn cur(&self) -> &Token {
        &self.toks[self.i]
    }

    fn peek(&self) -> &Tok {
        &self.cur().tok
    }

    fn pos(&self) -> Pos {
        self.cur().pos
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.i].clone();
        if t.tok != Tok::Eof {
            self.i += 1;
        }
        t
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Tok, what: &str) -> PResult<Token> {
        if self.peek() == t {
            Ok(self.bump())
        } else {
            Err(self.unexpected(what))
        }
    }

    fn unexpected(&self, what: &str) -> Error {
        Error::new(self.pos(), format!("expected {what}, found {}", describe(self.peek())))
    }

    fn ident(&mut self, what: &str) -> PResult<(Pos, String)> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                let pos = self.bump().pos;
                Ok((pos, name))
            }
            _ => Err(self.unexpected(what)),
        }
    }

    /// True if the current token is `t` on the same line as the previous token.
    fn same_line(&self, t: &Tok) -> bool {
        self.peek() == t && !self.cur().newline_before
    }

    /// Runs `f` with struct literals allowed or not, restoring the old setting.
    fn with_struct_lit<T>(&mut self, allowed: bool, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let saved = std::mem::replace(&mut self.no_struct_lit, !allowed);
        let r = f(self);
        self.no_struct_lit = saved;
        r
    }

    /// Parses `{ item, item ... }` where items are separated by `,` or newlines.
    fn braced_list<T>(&mut self, mut item: impl FnMut(&mut Self) -> PResult<T>) -> PResult<Vec<T>> {
        self.expect(&Tok::LBrace, "`{`")?;
        let mut items = Vec::new();
        loop {
            if self.eat(&Tok::RBrace) {
                return Ok(items);
            }
            items.push(item(self)?);
            if !self.eat(&Tok::Comma) && *self.peek() != Tok::RBrace && !self.cur().newline_before {
                return Err(self.unexpected("`,` or `}`"));
            }
        }
    }

    // ---- items ----

    fn program(&mut self) -> PResult<Program> {
        let mut attrs = Vec::new();
        while let Tok::InnerAttr(name) = self.peek().clone() {
            let pos = self.bump().pos;
            attrs.push((pos, name));
        }
        let mut structs = Vec::new();
        let mut consts = Vec::new();
        let mut funcs = Vec::new();
        while *self.peek() != Tok::Eof {
            match self.peek() {
                Tok::Fn => funcs.push(self.fn_decl()?),
                Tok::Struct => structs.push(self.struct_decl()?),
                Tok::Const => consts.push(self.const_decl()?),
                Tok::InnerAttr(_) => {
                    return Err(Error::new(self.pos(), "`#![...]` must come before any item"))
                }
                _ => return Err(self.unexpected("`fn`, `struct` or `const`")),
            }
        }
        Ok(Program { attrs, structs, consts, funcs })
    }

    fn const_decl(&mut self) -> PResult<ConstDecl> {
        let pos = self.expect(&Tok::Const, "`const`")?.pos;
        let (_, name) = self.ident("constant name")?;
        let ty = if self.eat(&Tok::Colon) { Some(self.type_expr()?) } else { None };
        self.expect(&Tok::Assign, "`=`")?;
        let value = self.expr()?;
        Ok(ConstDecl { pos, name, ty, value })
    }

    fn struct_decl(&mut self) -> PResult<StructDecl> {
        let pos = self.expect(&Tok::Struct, "`struct`")?.pos;
        let (_, name) = self.ident("struct name")?;
        let fields = self.braced_list(|p| {
            let (pos, name) = p.ident("field name")?;
            p.expect(&Tok::Colon, "`:` and a field type")?;
            let ty = p.type_expr()?;
            Ok(FieldDecl { pos, name, ty })
        })?;
        Ok(StructDecl { pos, name, fields })
    }

    fn fn_decl(&mut self) -> PResult<FnDecl> {
        let pos = self.expect(&Tok::Fn, "`fn`")?.pos;
        let (_, name) = self.ident("function name")?;
        self.expect(&Tok::LParen, "`(`")?;
        let mut params = Vec::new();
        while *self.peek() != Tok::RParen {
            let comptime = self.eat(&Tok::Comptime);
            let (ppos, pname) = self.ident("parameter name")?;
            self.expect(&Tok::Colon, "`:` and a parameter type")?;
            let ty = self.type_expr()?;
            params.push(Param { pos: ppos, comptime, name: pname, ty });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "`)`")?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.type_expr()?) } else { None };
        let body = self.block()?;
        Ok(FnDecl { pos, name, params, ret, body })
    }

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let pos = self.pos();
        if self.eat(&Tok::Star) {
            let inner = self.type_expr()?;
            return Ok(TypeExpr { pos, kind: TypeExprKind::Ptr(Box::new(inner)) });
        }
        if self.eat(&Tok::LBracket) {
            let elem = self.type_expr()?;
            self.expect(&Tok::Semi, "`;` and an array length")?;
            let len = self.with_struct_lit(true, |p| p.expr())?;
            self.expect(&Tok::RBracket, "`]`")?;
            return Ok(TypeExpr { pos, kind: TypeExprKind::Array(Box::new(elem), Box::new(len)) });
        }
        let (pos, name) = self.ident("a type")?;
        Ok(TypeExpr { pos, kind: TypeExprKind::Named(name) })
    }

    // ---- statements ----

    fn block(&mut self) -> PResult<Block> {
        self.expect(&Tok::LBrace, "`{`")?;
        self.with_struct_lit(true, |p| {
            let mut stmts = Vec::new();
            loop {
                while p.eat(&Tok::Semi) {}
                if *p.peek() == Tok::RBrace {
                    let end = p.bump().pos;
                    return Ok(Block { stmts, end });
                }
                if *p.peek() == Tok::Eof {
                    return Err(p.unexpected("`}`"));
                }
                stmts.push(p.stmt()?);
                p.end_of_stmt()?;
            }
        })
    }

    /// A statement must be followed by `;`, `}`, or a newline.
    fn end_of_stmt(&mut self) -> PResult<()> {
        if self.eat(&Tok::Semi) || *self.peek() == Tok::RBrace || self.cur().newline_before {
            Ok(())
        } else {
            Err(self.unexpected("end of statement"))
        }
    }

    fn cond(&mut self) -> PResult<Expr> {
        self.with_struct_lit(false, |p| p.expr())
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        match self.peek() {
            Tok::Let => {
                let pos = self.bump().pos;
                let (_, name) = self.ident("variable name")?;
                let ty = if self.eat(&Tok::Colon) { Some(self.type_expr()?) } else { None };
                self.expect(&Tok::Assign, "`=`")?;
                let value = self.expr()?;
                Ok(Stmt::Let { pos, name, ty, value })
            }
            Tok::Return => {
                let pos = self.bump().pos;
                let ends = matches!(self.peek(), Tok::Semi | Tok::RBrace | Tok::Eof)
                    || self.cur().newline_before;
                let value = if ends { None } else { Some(self.expr()?) };
                Ok(Stmt::Return { pos, value })
            }
            Tok::If => self.if_stmt(),
            Tok::While => {
                self.bump();
                let cond = self.cond()?;
                let body = self.block()?;
                Ok(Stmt::While { cond, body })
            }
            _ => {
                let e = self.expr()?;
                if self.same_line(&Tok::Assign) {
                    self.bump();
                    let value = self.expr()?;
                    Ok(Stmt::Assign { target: e, value })
                } else {
                    Ok(Stmt::Expr(e))
                }
            }
        }
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        self.expect(&Tok::If, "`if`")?;
        let cond = self.cond()?;
        let then = self.block()?;
        let els = if self.eat(&Tok::Else) {
            if *self.peek() == Tok::If {
                let inner = self.if_stmt()?;
                let end = self.toks[self.i - 1].pos;
                Some(Block { stmts: vec![inner], end })
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        Ok(Stmt::If { cond, then, els })
    }

    // ---- expressions ----

    fn expr(&mut self) -> PResult<Expr> {
        self.binary(0)
    }

    fn binary(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = self.cast()?;
        loop {
            if self.cur().newline_before {
                break;
            }
            let Some((op, prec)) = binop(self.peek()) else { break };
            if prec < min_prec {
                break;
            }
            let pos = self.bump().pos;
            let rhs = self.binary(prec + 1)?;
            lhs = Expr { pos, kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)) };
        }
        Ok(lhs)
    }

    /// `unary (as T)*` — binds tighter than any binary operator, like in Rust.
    fn cast(&mut self) -> PResult<Expr> {
        let mut e = self.unary()?;
        while self.same_line(&Tok::As) {
            let pos = self.bump().pos;
            let ty = self.type_expr()?;
            e = Expr { pos, kind: ExprKind::Cast(Box::new(e), ty) };
        }
        Ok(e)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        let wrap: fn(Box<Expr>) -> ExprKind = match self.peek() {
            Tok::Minus => |e| ExprKind::Unary(UnOp::Neg, e),
            Tok::Bang => |e| ExprKind::Unary(UnOp::Not, e),
            Tok::Star => ExprKind::Deref,
            Tok::Amp => ExprKind::AddrOf,
            Tok::Comptime => ExprKind::Comptime,
            _ => return self.postfix(),
        };
        self.bump();
        let inner = self.unary()?;
        Ok(Expr { pos, kind: wrap(Box::new(inner)) })
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.same_line(&Tok::Dot) {
                let pos = self.bump().pos;
                let (_, field) = self.ident("a field name")?;
                e = Expr { pos, kind: ExprKind::Field(Box::new(e), field) };
            } else if self.same_line(&Tok::LBracket) {
                let pos = self.bump().pos;
                let index = self.with_struct_lit(true, |p| p.expr())?;
                self.expect(&Tok::RBracket, "`]`")?;
                e = Expr { pos, kind: ExprKind::Index(Box::new(e), Box::new(index)) };
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        let kind = match self.peek().clone() {
            Tok::Int(n) => {
                self.bump();
                ExprKind::Int(n)
            }
            Tok::True => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::False => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::Ident(name) if matches!(name.as_str(), "size_of" | "align_of") && self.toks[self.i + 1].tok == Tok::LParen => {
                self.bump();
                self.bump();
                let ty = self.type_expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                if name == "size_of" {
                    ExprKind::SizeOf(ty)
                } else {
                    ExprKind::AlignOf(ty)
                }
            }
            Tok::LBracket => {
                self.bump();
                self.with_struct_lit(true, |p| {
                    if p.eat(&Tok::RBracket) {
                        return Ok(ExprKind::ArrayLit(vec![]));
                    }
                    let first = p.expr()?;
                    if p.eat(&Tok::Semi) {
                        let len = p.expr()?;
                        p.expect(&Tok::RBracket, "`]`")?;
                        return Ok(ExprKind::ArrayRepeat(Box::new(first), Box::new(len)));
                    }
                    let mut items = vec![first];
                    while p.eat(&Tok::Comma) {
                        if *p.peek() == Tok::RBracket {
                            break;
                        }
                        items.push(p.expr()?);
                    }
                    p.expect(&Tok::RBracket, "`,` or `]`")?;
                    Ok(ExprKind::ArrayLit(items))
                })?
            }
            Tok::Ident(name) => {
                self.bump();
                if self.same_line(&Tok::LParen) {
                    self.bump();
                    let args = self.with_struct_lit(true, |p| {
                        let mut args = Vec::new();
                        while *p.peek() != Tok::RParen {
                            args.push(p.expr()?);
                            if !p.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        Ok(args)
                    })?;
                    self.expect(&Tok::RParen, "`)`")?;
                    ExprKind::Call(name, args)
                } else if self.same_line(&Tok::LBrace) && !self.no_struct_lit {
                    let fields = self.with_struct_lit(true, |p| {
                        p.braced_list(|p| {
                            let (pos, name) = p.ident("a field name")?;
                            p.expect(&Tok::Colon, "`:`")?;
                            let value = p.expr()?;
                            Ok(FieldInit { pos, name, value })
                        })
                    })?;
                    ExprKind::StructLit(name, fields)
                } else {
                    ExprKind::Var(name)
                }
            }
            Tok::LParen => {
                self.bump();
                let e = self.with_struct_lit(true, |p| p.expr())?;
                self.expect(&Tok::RParen, "`)`")?;
                return Ok(e);
            }
            _ => return Err(self.unexpected("an expression")),
        };
        Ok(Expr { pos, kind })
    }
}

fn binop(t: &Tok) -> Option<(BinOp, u8)> {
    Some(match t {
        Tok::OrOr => (BinOp::Or, 1),
        Tok::AndAnd => (BinOp::And, 2),
        Tok::EqEq => (BinOp::Eq, 3),
        Tok::NotEq => (BinOp::Ne, 3),
        Tok::Lt => (BinOp::Lt, 4),
        Tok::Le => (BinOp::Le, 4),
        Tok::Gt => (BinOp::Gt, 4),
        Tok::Ge => (BinOp::Ge, 4),
        Tok::Plus => (BinOp::Add, 5),
        Tok::Minus => (BinOp::Sub, 5),
        Tok::Star => (BinOp::Mul, 6),
        Tok::Slash => (BinOp::Div, 6),
        Tok::Percent => (BinOp::Rem, 6),
        _ => return None,
    })
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => format!("identifier `{s}`"),
        Tok::Int(n) => format!("integer `{n}`"),
        Tok::Str(_) => "string literal".into(),
        Tok::InnerAttr(a) => format!("`#![{a}]`"),
        Tok::Eof => "end of file".into(),
        other => format!("`{}`", punct(other)),
    }
}

fn punct(t: &Tok) -> &'static str {
    match t {
        Tok::Fn => "fn",
        Tok::Let => "let",
        Tok::Return => "return",
        Tok::If => "if",
        Tok::Else => "else",
        Tok::While => "while",
        Tok::True => "true",
        Tok::False => "false",
        Tok::Struct => "struct",
        Tok::As => "as",
        Tok::Const => "const",
        Tok::Comptime => "comptime",
        Tok::LParen => "(",
        Tok::RParen => ")",
        Tok::LBrace => "{",
        Tok::RBrace => "}",
        Tok::LBracket => "[",
        Tok::RBracket => "]",
        Tok::Dot => ".",
        Tok::Comma => ",",
        Tok::Colon => ":",
        Tok::Semi => ";",
        Tok::Arrow => "->",
        Tok::Assign => "=",
        Tok::EqEq => "==",
        Tok::NotEq => "!=",
        Tok::Lt => "<",
        Tok::Le => "<=",
        Tok::Gt => ">",
        Tok::Ge => ">=",
        Tok::Plus => "+",
        Tok::Minus => "-",
        Tok::Star => "*",
        Tok::Slash => "/",
        Tok::Percent => "%",
        Tok::Bang => "!",
        Tok::Amp => "&",
        Tok::AndAnd => "&&",
        Tok::OrOr => "||",
        Tok::Ident(_) | Tok::Int(_) | Tok::Str(_) | Tok::InnerAttr(_) | Tok::Eof => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(src: &str) -> Vec<Stmt> {
        parse(&format!("fn main() {{\n{src}\n}}")).unwrap().funcs.remove(0).body.stmts
    }

    #[test]
    fn parses_function_with_control_flow() {
        let p = parse(
            "fn fib(n: i64) -> i64 {\n  if n < 2 { return n }\n  return fib(n - 1) + fib(n - 2)\n}",
        )
        .unwrap();
        assert_eq!(p.funcs.len(), 1);
        assert_eq!(p.funcs[0].params.len(), 1);
        assert_eq!(p.funcs[0].body.stmts.len(), 2);
    }

    #[test]
    fn newline_ends_expression() {
        // `-1` does not continue `x` from the previous line, and `*p` is a new statement.
        assert_eq!(body("let x = 1\n-1").len(), 2);
        assert_eq!(body("let x = 1\n*p = 2").len(), 2);
    }

    #[test]
    fn reads_inner_attrs() {
        let p = parse("#![freestanding]\nfn _start() {}").unwrap();
        assert_eq!(p.attrs[0].1, "freestanding");
    }

    #[test]
    fn reports_position() {
        let err = parse("fn main() {\n  let = 3\n}").unwrap_err();
        assert_eq!(err.pos, Pos { line: 2, col: 7 });
    }

    #[test]
    fn structs() {
        let p = parse("struct P {\n  x: i64\n  next: *P,\n}\nfn main() { let p = P { x: 1, next: n } }")
            .unwrap();
        assert_eq!(p.structs[0].fields.len(), 2);
        assert!(matches!(p.structs[0].fields[1].ty.kind, TypeExprKind::Ptr(_)));
        let Stmt::Let { value, .. } = &p.funcs[0].body.stmts[0] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::StructLit(name, f) if name == "P" && f.len() == 2));
    }

    #[test]
    fn no_struct_literal_in_conditions() {
        let s = body("if x == y { }");
        let Stmt::If { cond, then, .. } = &s[0] else { panic!() };
        assert!(matches!(cond.kind, ExprKind::Binary(BinOp::Eq, _, _)));
        assert!(then.stmts.is_empty());
    }

    #[test]
    fn arrays_and_size_of() {
        let s = body("let a: [u8; 4] = [1, 2, 3, 4,]\nlet b = [0; 16]\nlet n = size_of([*u8; 2]) + align_of(i64)\nlet c = []");
        let Stmt::Let { ty: Some(ty), value, .. } = &s[0] else { panic!() };
        assert!(matches!(&ty.kind, TypeExprKind::Array(_, n) if matches!(n.kind, ExprKind::Int(4))));
        assert!(matches!(&value.kind, ExprKind::ArrayLit(items) if items.len() == 4));
        let Stmt::Let { value, .. } = &s[1] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::ArrayRepeat(_, n) if matches!(n.kind, ExprKind::Int(16))));
        let Stmt::Let { value, .. } = &s[2] else { panic!() };
        let ExprKind::Binary(_, l, r) = &value.kind else { panic!() };
        assert!(matches!(l.kind, ExprKind::SizeOf(_)) && matches!(r.kind, ExprKind::AlignOf(_)));
        let Stmt::Let { value, .. } = &s[3] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::ArrayLit(items) if items.is_empty()));
    }

    #[test]
    fn comptime_and_consts() {
        let p = parse("const N: i64 = 4 * 4\nconst M = f(N)\nfn main() { let a: [u8; N + 1] = [0; comptime f(2)] }").unwrap();
        assert_eq!(p.consts.len(), 2);
        assert!(p.consts[0].ty.is_some());
        let Stmt::Let { ty: Some(ty), value, .. } = &p.funcs[0].body.stmts[0] else { panic!() };
        assert!(matches!(&ty.kind, TypeExprKind::Array(_, n) if matches!(n.kind, ExprKind::Binary(..))));
        let ExprKind::ArrayRepeat(_, n) = &value.kind else { panic!() };
        // `comptime` binds like a prefix operator: `comptime f(2) + 1` is `(comptime f(2)) + 1`.
        assert!(matches!(n.kind, ExprKind::Comptime(_)));
        let s = body("let x = comptime f(2) + 1");
        let Stmt::Let { value, .. } = &s[0] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::Binary(_, l, _) if matches!(l.kind, ExprKind::Comptime(_))));
    }

    #[test]
    fn comptime_params() {
        let p = parse("fn max(comptime T: type, a: T, b: T) -> T { return a }").unwrap();
        let params = &p.funcs[0].params;
        assert!(params[0].comptime && !params[1].comptime);
        assert!(matches!(&params[0].ty.kind, TypeExprKind::Named(n) if n == "type"));
    }

    #[test]
    fn postfix_and_prefix() {
        let s = body("*p.next[2] = &q.x as *u8");
        let Stmt::Assign { target, value } = &s[0] else { panic!() };
        // `*` applies to the whole postfix chain.
        let ExprKind::Deref(inner) = &target.kind else { panic!() };
        assert!(matches!(inner.kind, ExprKind::Index(_, _)));
        // Prefix operators bind tighter than `as`.
        let ExprKind::Cast(inner, _) = &value.kind else { panic!() };
        assert!(matches!(inner.kind, ExprKind::AddrOf(_)));
    }
}
