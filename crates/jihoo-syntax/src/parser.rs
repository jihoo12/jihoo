//! Recursive descent parser.
//!
//! Semicolons are optional. A newline ends a statement, and a binary operator or a call's
//! `(` at the start of the next line does not continue the expression (similar to Go).

use crate::ast::*;
use crate::lexer::{lex, Tok, Token};
use crate::{Error, Pos};

pub fn parse(src: &str) -> Result<Program, Error> {
    let toks = lex(src)?;
    Parser { toks, i: 0 }.program()
}

struct Parser {
    toks: Vec<Token>,
    i: usize,
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

    // ---- items ----

    fn program(&mut self) -> PResult<Program> {
        let mut attrs = Vec::new();
        while let Tok::InnerAttr(name) = self.peek().clone() {
            let pos = self.bump().pos;
            attrs.push((pos, name));
        }
        let mut funcs = Vec::new();
        while *self.peek() != Tok::Eof {
            match self.peek() {
                Tok::Fn => funcs.push(self.fn_decl()?),
                Tok::InnerAttr(_) => {
                    return Err(Error::new(self.pos(), "`#![...]` must come before any item"))
                }
                _ => return Err(self.unexpected("`fn`")),
            }
        }
        Ok(Program { attrs, funcs })
    }

    fn fn_decl(&mut self) -> PResult<FnDecl> {
        let pos = self.expect(&Tok::Fn, "`fn`")?.pos;
        let (_, name) = self.ident("function name")?;
        self.expect(&Tok::LParen, "`(`")?;
        let mut params = Vec::new();
        while *self.peek() != Tok::RParen {
            let (ppos, pname) = self.ident("parameter name")?;
            self.expect(&Tok::Colon, "`:` and a parameter type")?;
            let ty = self.type_expr()?;
            params.push(Param { pos: ppos, name: pname, ty });
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
        let (pos, name) = self.ident("a type")?;
        Ok(TypeExpr { pos, name })
    }

    // ---- statements ----

    fn block(&mut self) -> PResult<Block> {
        self.expect(&Tok::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        loop {
            while self.eat(&Tok::Semi) {}
            if self.eat(&Tok::RBrace) {
                return Ok(Block { stmts });
            }
            if *self.peek() == Tok::Eof {
                return Err(self.unexpected("`}`"));
            }
            stmts.push(self.stmt()?);
            self.end_of_stmt()?;
        }
    }

    /// A statement must be followed by `;`, `}`, or a newline.
    fn end_of_stmt(&mut self) -> PResult<()> {
        if self.eat(&Tok::Semi) || *self.peek() == Tok::RBrace || self.cur().newline_before {
            Ok(())
        } else {
            Err(self.unexpected("end of statement"))
        }
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
                let cond = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::While { cond, body })
            }
            Tok::Ident(_) if self.toks[self.i + 1].tok == Tok::Assign => {
                let (pos, name) = self.ident("variable name")?;
                self.bump(); // `=`
                let value = self.expr()?;
                Ok(Stmt::Assign { pos, name, value })
            }
            _ => Ok(Stmt::Expr(self.expr()?)),
        }
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        self.expect(&Tok::If, "`if`")?;
        let cond = self.expr()?;
        let then = self.block()?;
        let els = if self.eat(&Tok::Else) {
            if *self.peek() == Tok::If {
                Some(Block { stmts: vec![self.if_stmt()?] })
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
        let mut lhs = self.unary()?;
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

    fn unary(&mut self) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Minus => UnOp::Neg,
            Tok::Bang => UnOp::Not,
            _ => return self.primary(),
        };
        let pos = self.bump().pos;
        let inner = self.unary()?;
        Ok(Expr { pos, kind: ExprKind::Unary(op, Box::new(inner)) })
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
            Tok::Ident(name) => {
                self.bump();
                if *self.peek() == Tok::LParen && !self.cur().newline_before {
                    self.bump();
                    let mut args = Vec::new();
                    while *self.peek() != Tok::RParen {
                        args.push(self.expr()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen, "`)`")?;
                    ExprKind::Call(name, args)
                } else {
                    ExprKind::Var(name)
                }
            }
            Tok::LParen => {
                self.bump();
                let e = self.expr()?;
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
        Tok::LParen => "(",
        Tok::RParen => ")",
        Tok::LBrace => "{",
        Tok::RBrace => "}",
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
        Tok::AndAnd => "&&",
        Tok::OrOr => "||",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // `-1` does not continue `x` from the previous line.
        let p = parse("fn main() {\n  let x = 1\n  -1\n}").unwrap();
        assert_eq!(p.funcs[0].body.stmts.len(), 2);
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
}
