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
    parse_file(src, 0)
}

/// Parses a source file; positions in it carry file number `file`.
pub fn parse_file(src: &str, file: u16) -> Result<Program, Error> {
    Parser::new(src, file)?.program()
}

/// Parses statements, such as the code a statement macro produced.
pub fn parse_stmts(src: &str) -> Result<Vec<Stmt>, Error> {
    let mut p = Parser::new(src, 0)?;
    let mut stmts = Vec::new();
    loop {
        while p.eat(&Tok::Semi) {}
        if *p.peek() == Tok::Eof {
            return Ok(stmts);
        }
        stmts.push(p.stmt()?);
        if *p.peek() != Tok::Eof {
            p.end_of_stmt()?;
        }
    }
}

/// Parses items (no imports or attributes), such as an item macro's output.
pub fn parse_items(src: &str) -> Result<Program, Error> {
    let mut p = Parser::new(src, 0)?;
    let prog = p.items(false)?;
    if !prog.imports.is_empty() {
        return Err(Error::new(prog.imports[0].pos, "code made by a macro cannot `import`"));
    }
    Ok(prog)
}

/// Parses a single expression, such as the code a macro produced.
pub fn parse_expr(src: &str) -> Result<Expr, Error> {
    let mut p = Parser::new(src, 0)?;
    let e = p.expr()?;
    if *p.peek() != Tok::Eof {
        return Err(p.unexpected("end of expression"));
    }
    Ok(e)
}

struct Parser {
    src: String,
    toks: Vec<Token>,
    i: usize,
    /// Set while parsing an `if`/`while` condition.
    no_struct_lit: bool,
    /// Set while parsing a quote template: the holes found so far, with their
    /// byte ranges in the source and where they sit.
    holes: Option<Vec<(usize, usize, Expr, HoleKind)>>,
}

type PResult<T> = Result<T, Error>;

impl Parser {
    fn new(src: &str, file: u16) -> Result<Self, Error> {
        let toks = lex(src, file)?;
        Ok(Parser { src: src.to_string(), toks, i: 0, no_struct_lit: false, holes: None })
    }

    /// Byte offset where the previous token ended.
    fn prev_end(&self) -> usize {
        self.toks[self.i.saturating_sub(1)].end
    }

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
            // `fn $name()` in a template: a name filled in by the macro.
            Tok::Dollar if self.holes.is_some() => {
                let pos = self.pos();
                self.hole(HoleKind::Ident)?;
                Ok((pos, "__hole__".into()))
            }
            _ => Err(self.unexpected(what)),
        }
    }

    /// `$x` or `$(expr)` in a template, recorded as a hole of `kind`.
    fn hole(&mut self, kind: HoleKind) -> PResult<Expr> {
        let pos = self.pos();
        let start = self.cur().start;
        self.expect(&Tok::Dollar, "`$`")?;
        if self.holes.is_none() {
            return Err(Error::new(pos, "`$` can only be used inside `quote(...)`"));
        }
        // The hole itself is ordinary code; it may not contain holes.
        let saved = self.holes.take();
        let inner = self.hole_body();
        self.holes = saved;
        let inner = inner?;
        let end = self.prev_end();
        self.holes.as_mut().unwrap().push((start, end, inner.clone(), kind));
        Ok(Expr { pos, kind: ExprKind::Hole(Box::new(inner)) })
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
        let mut prog = self.items(false)?;
        prog.attrs = attrs;
        Ok(prog)
    }

    /// Items up to the end of the file, or up to a `}` if `in_braces`.
    fn items(&mut self, in_braces: bool) -> PResult<Program> {
        let mut imports = Vec::new();
        let mut structs = Vec::new();
        let mut consts = Vec::new();
        let mut funcs = Vec::new();
        let mut macro_calls = Vec::new();
        loop {
            match self.peek() {
                Tok::Eof if !in_braces => break,
                Tok::RBrace if in_braces => break,
                // `$items` in `quote items { ... }`.
                Tok::Dollar if self.holes.is_some() => {
                    self.hole(HoleKind::Items)?;
                }
                // `name!(...)` / `module.name!(...)`: an item macro.
                Tok::Ident(_) if self.item_macro_follows() => {
                    let pos = self.pos();
                    let mut name = self.ident("a macro name")?.1;
                    if self.eat(&Tok::Dot) {
                        name = format!("{name}.{}", self.ident("a macro name")?.1);
                    }
                    self.expect(&Tok::Bang, "`!`")?;
                    let args = self.macro_args()?;
                    macro_calls.push(ItemMacro { pos, name, args });
                }
                Tok::Import => imports.push(self.import()?),
                Tok::Fn | Tok::Macro => funcs.push(self.fn_decl(false)?),
                Tok::Struct => structs.push(self.struct_decl(false)?),
                Tok::Const => consts.push(self.const_decl(false)?),
                Tok::Pub => {
                    self.bump();
                    match self.peek() {
                        Tok::Fn | Tok::Macro => funcs.push(self.fn_decl(true)?),
                        Tok::Struct => structs.push(self.struct_decl(true)?),
                        Tok::Const => consts.push(self.const_decl(true)?),
                        _ => return Err(self.unexpected("`fn`, `macro`, `struct` or `const` after `pub`")),
                    }
                }
                Tok::InnerAttr(_) => {
                    return Err(Error::new(self.pos(), "`#![...]` must come before any item"))
                }
                _ => return Err(self.unexpected("`fn`, `macro`, `struct`, `const` or `import`")),
            }
        }
        Ok(Program { attrs: vec![], imports, structs, consts, funcs, macro_calls })
    }

    fn item_macro_follows(&self) -> bool {
        let t = |k: usize| self.toks.get(self.i + k).map(|x| &x.tok);
        matches!(
            (t(1), t(2), t(3), t(4)),
            (Some(Tok::Bang), Some(Tok::LParen), _, _)
                | (Some(Tok::Dot), Some(Tok::Ident(_)), Some(Tok::Bang), Some(Tok::LParen))
        )
    }

    fn import(&mut self) -> PResult<Import> {
        let pos = self.expect(&Tok::Import, "`import`")?.pos;
        let mut path = vec![self.ident("a module name")?.1];
        while self.eat(&Tok::Dot) {
            path.push(self.ident("a module name")?.1);
        }
        let alias = if self.eat(&Tok::As) { self.ident("a module alias")?.1 } else { path.last().unwrap().clone() };
        Ok(Import { pos, path, alias })
    }

    /// After a name: is this `module.item` used as a call, macro call or struct
    /// literal? Plain `a.b` stays a field access; the checker resolves module
    /// constants from it.
    fn qualified_follows(&self) -> bool {
        let t = |k: usize| self.toks.get(self.i + k);
        let same_line = |k: usize, tok: &Tok| t(k).is_some_and(|x| x.tok == *tok && !x.newline_before);
        if !same_line(0, &Tok::Dot) || !matches!(t(1).map(|x| &x.tok), Some(Tok::Ident(_))) {
            return false;
        }
        same_line(2, &Tok::LParen)
            || (same_line(2, &Tok::Bang) && same_line(3, &Tok::LParen))
            || (same_line(2, &Tok::LBrace) && !self.no_struct_lit)
    }

    fn const_decl(&mut self, is_pub: bool) -> PResult<ConstDecl> {
        let pos = self.expect(&Tok::Const, "`const`")?.pos;
        let (_, name) = self.ident("constant name")?;
        let ty = if self.eat(&Tok::Colon) { Some(self.type_expr()?) } else { None };
        self.expect(&Tok::Assign, "`=`")?;
        let value = self.expr()?;
        Ok(ConstDecl { pos, is_pub, name, ty, value })
    }

    fn struct_decl(&mut self, is_pub: bool) -> PResult<StructDecl> {
        let pos = self.expect(&Tok::Struct, "`struct`")?.pos;
        let (_, name) = self.ident("struct name")?;
        let mut params = Vec::new();
        if self.eat(&Tok::LParen) {
            while *self.peek() != Tok::RParen {
                // Every struct parameter is compile-time; `comptime` is optional.
                self.eat(&Tok::Comptime);
                let (ppos, pname) = self.ident("parameter name")?;
                self.expect(&Tok::Colon, "`:` and a parameter type")?;
                let ty = self.type_expr()?;
                params.push(Param { pos: ppos, comptime: true, name: pname, ty });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen, "`)`")?;
        }
        let fields = self.braced_list(|p| {
            let (pos, name) = p.ident("field name")?;
            p.expect(&Tok::Colon, "`:` and a field type")?;
            let ty = p.type_expr()?;
            Ok(FieldDecl { pos, name, ty })
        })?;
        Ok(StructDecl { pos, is_pub, name, params, fields })
    }

    fn fn_decl(&mut self, is_pub: bool) -> PResult<FnDecl> {
        let is_macro = *self.peek() == Tok::Macro;
        let pos = self.bump().pos; // `fn` or `macro`
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
        Ok(FnDecl { pos, is_pub, is_macro, name, params, ret, body })
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
        let (pos, mut name) = self.ident("a type")?;
        // `module.Type`
        if self.same_line(&Tok::Dot) && matches!(self.toks[self.i + 1].tok, Tok::Ident(_)) {
            self.bump();
            name = format!("{name}.{}", self.ident("a type name")?.1);
        }
        if self.same_line(&Tok::LParen) {
            let args = self.call_args()?;
            return Ok(TypeExpr { pos, kind: TypeExprKind::Generic(name, args) });
        }
        Ok(TypeExpr { pos, kind: TypeExprKind::Named(name) })
    }

    /// `(a, b, ...)`
    fn call_args(&mut self) -> PResult<Vec<Expr>> {
        self.expect(&Tok::LParen, "`(`")?;
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
        Ok(args)
    }

    /// `{ field: value, ... }` of a struct literal.
    fn field_inits(&mut self) -> PResult<Vec<FieldInit>> {
        self.with_struct_lit(true, |p| {
            p.braced_list(|p| {
                let (pos, name) = p.ident("a field name")?;
                p.expect(&Tok::Colon, "`:`")?;
                let value = p.expr()?;
                Ok(FieldInit { pos, name, value })
            })
        })
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
                    // A hole on its own is a place for statements.
                    if matches!(e.kind, ExprKind::Hole(_)) {
                        if let Some(h) = self.holes.as_mut().and_then(|h| h.last_mut()) {
                            h.3 = HoleKind::Stmts;
                        }
                    }
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
            Tok::Ident(first) => {
                self.bump();
                let mut name = first;
                if self.qualified_follows() {
                    self.bump();
                    name = format!("{name}.{}", self.ident("a name")?.1);
                }
                if self.same_line(&Tok::Bang) && self.toks[self.i + 1].tok == Tok::LParen {
                    self.bump();
                    return Ok(Expr { pos, kind: ExprKind::MacroCall(name, self.macro_args()?) });
                }
                if self.same_line(&Tok::LParen) {
                    let args = self.call_args()?;
                    // `Pair(i64) { ... }`: a literal of a generic struct. A call is
                    // never directly followed by `{` otherwise.
                    if self.same_line(&Tok::LBrace) && !self.no_struct_lit {
                        let ty = TypeExpr { pos, kind: TypeExprKind::Generic(name, args) };
                        ExprKind::StructLit(ty, self.field_inits()?)
                    } else {
                        ExprKind::Call(name, args)
                    }
                } else if self.same_line(&Tok::LBrace) && !self.no_struct_lit {
                    let ty = TypeExpr { pos, kind: TypeExprKind::Named(name) };
                    ExprKind::StructLit(ty, self.field_inits()?)
                } else {
                    ExprKind::Var(name)
                }
            }
            Tok::Asm => {
                self.bump();
                ExprKind::Asm(Box::new(self.asm_args()?))
            }
            Tok::Quote => {
                self.bump();
                self.quote()?
            }
            Tok::Dollar => return self.hole(HoleKind::Expr),
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

impl Parser {
    /// What follows `$`: a name, or a parenthesized expression.
    fn hole_body(&mut self) -> PResult<Expr> {
        if self.eat(&Tok::LParen) {
            let e = self.with_struct_lit(true, |p| p.expr())?;
            self.expect(&Tok::RParen, "`)`")?;
            Ok(e)
        } else {
            let (pos, n) = self.ident("a name or `(` after `$`")?;
            Ok(Expr { pos, kind: ExprKind::Var(n) })
        }
    }

    /// `(arg, arg, ...)` of a macro call; each argument keeps its source text.
    fn macro_args(&mut self) -> PResult<Vec<MacroArg>> {
        self.expect(&Tok::LParen, "`(`")?;
        let args = self.with_struct_lit(true, |p| {
            let mut args = Vec::new();
            while *p.peek() != Tok::RParen {
                let start = p.cur().start;
                let expr = p.expr()?;
                let text = p.src[start..p.prev_end()].to_string();
                args.push(MacroArg { text, expr });
                if !p.eat(&Tok::Comma) {
                    break;
                }
            }
            Ok(args)
        })?;
        self.expect(&Tok::RParen, "`)`")?;
        Ok(args)
    }

    /// `quote(expr)`, `quote { stmts }` or `quote items { items }`. The template
    /// must parse as that kind of code, with `$x` and `$(expr)` holes standing in
    /// for code inserted when the macro runs.
    fn quote(&mut self) -> PResult<ExprKind> {
        let pos = self.pos();
        if self.holes.is_some() {
            return Err(Error::new(pos, "`quote` cannot be nested"));
        }
        let kind = match self.peek().clone() {
            Tok::LParen => CodeKind::Expr,
            Tok::LBrace => CodeKind::Stmts,
            Tok::Ident(w) if w == "items" => {
                self.bump();
                CodeKind::Items
            }
            _ => return Err(self.unexpected("`(`, `{` or `items {` after `quote`")),
        };
        let close = if kind == CodeKind::Expr { Tok::RParen } else { Tok::RBrace };
        self.bump(); // `(` or `{`
        let start = self.cur().start;
        self.holes = Some(Vec::new());
        let parsed = self.with_struct_lit(true, |p| match kind {
            CodeKind::Expr => p.expr().map(|_| ()),
            CodeKind::Items => p.items(true).map(|_| ()),
            CodeKind::Stmts => loop {
                while p.eat(&Tok::Semi) {}
                if *p.peek() == Tok::RBrace {
                    break Ok(());
                }
                if let Err(e) = p.stmt().and_then(|_| p.end_of_stmt()) {
                    break Err(e);
                }
            },
        });
        let holes = self.holes.take().unwrap();
        parsed?;
        // An empty template has no last token of its own.
        let end = if self.cur().start > start { self.prev_end().max(start) } else { start };
        self.expect(&close, if kind == CodeKind::Expr { "`)`" } else { "`}`" })?;

        let mut pieces = Vec::new();
        let mut exprs = Vec::new();
        let mut at = start;
        for (s, e, expr, hole) in holes {
            pieces.push(self.src[at..s].to_string());
            exprs.push((hole, expr));
            at = e;
        }
        pieces.push(self.src[at..end].to_string());
        Ok(ExprKind::Quote(kind, pieces, exprs))
    }

    /// `("line", "line", out(reg) T, in("rdi") x, clobber("rcx", "memory"))`
    fn asm_args(&mut self) -> PResult<AsmExpr> {
        self.expect(&Tok::LParen, "`(`")?;
        self.with_struct_lit(true, |p| {
            let mut lines = Vec::new();
            while let Tok::Str(s) = p.peek().clone() {
                p.bump();
                lines.push(s);
                if !p.eat(&Tok::Comma) {
                    break;
                }
            }
            if lines.is_empty() {
                return Err(p.unexpected("an asm template string"));
            }
            let mut asm = AsmExpr { template: lines.join("\n"), output: None, inputs: vec![], clobbers: vec![] };
            while *p.peek() != Tok::RParen {
                let (pos, kind) = p.ident("`in`, `out` or `clobber`")?;
                p.expect(&Tok::LParen, "`(`")?;
                match kind.as_str() {
                    "in" | "out" => {
                        let reg = match p.peek().clone() {
                            Tok::Str(r) => AsmReg::Named(r),
                            Tok::Ident(w) if w == "reg" => AsmReg::Any,
                            Tok::Ident(w) if w == "out" && kind == "in" => AsmReg::Out,
                            _ => return Err(p.unexpected("a register name string or `reg`")),
                        };
                        p.bump();
                        p.expect(&Tok::RParen, "`)`")?;
                        if kind == "in" {
                            asm.inputs.push((reg, p.expr()?));
                        } else if asm.output.is_some() {
                            return Err(Error::new(pos, "an asm block can have at most one `out`"));
                        } else {
                            asm.output = Some((reg, p.type_expr()?));
                        }
                    }
                    "clobber" => {
                        while let Tok::Str(c) = p.peek().clone() {
                            p.bump();
                            asm.clobbers.push(c);
                            if !p.eat(&Tok::Comma) {
                                break;
                            }
                        }
                        p.expect(&Tok::RParen, "`)`")?;
                    }
                    _ => return Err(Error::new(pos, format!("expected `in`, `out` or `clobber`, found `{kind}`"))),
                }
                if !p.eat(&Tok::Comma) {
                    break;
                }
            }
            p.expect(&Tok::RParen, "`)`")?;
            Ok(asm)
        })
    }
}

/// Binary operators and their precedence (higher binds tighter), as in Rust.
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
        Tok::Pipe => (BinOp::BitOr, 5),
        Tok::Caret => (BinOp::BitXor, 6),
        Tok::Amp => (BinOp::BitAnd, 7),
        Tok::Shl => (BinOp::Shl, 8),
        Tok::Shr => (BinOp::Shr, 8),
        Tok::Plus => (BinOp::Add, 9),
        Tok::Minus => (BinOp::Sub, 9),
        Tok::Star => (BinOp::Mul, 10),
        Tok::Slash => (BinOp::Div, 10),
        Tok::Percent => (BinOp::Rem, 10),
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
        Tok::Asm => "asm",
        Tok::Macro => "macro",
        Tok::Quote => "quote",
        Tok::Import => "import",
        Tok::Pub => "pub",
        Tok::Dollar => "$",
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
        Tok::Pipe => "|",
        Tok::Caret => "^",
        Tok::Shl => "<<",
        Tok::Shr => ">>",
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
        assert_eq!(err.pos, Pos::new(2, 7));
    }

    #[test]
    fn structs() {
        let p = parse("struct P {\n  x: i64\n  next: *P,\n}\nfn main() { let p = P { x: 1, next: n } }")
            .unwrap();
        assert_eq!(p.structs[0].fields.len(), 2);
        assert!(matches!(p.structs[0].fields[1].ty.kind, TypeExprKind::Ptr(_)));
        let Stmt::Let { value, .. } = &p.funcs[0].body.stmts[0] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::StructLit(t, f)
            if matches!(&t.kind, TypeExprKind::Named(n) if n == "P") && f.len() == 2));
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
    fn integer_literals() {
        let lit = |src: &str| {
            let s = body(&format!("let x = {src}"));
            let Stmt::Let { value, .. } = &s[0] else { panic!() };
            let ExprKind::Int(n) = value.kind else { panic!() };
            n
        };
        assert_eq!(lit("1_000"), 1000);
        assert_eq!(lit("0xff"), 255);
        assert_eq!(lit("0x7fff_ffff_ffff_ffff"), i64::MAX);
        assert_eq!(lit("0b1010"), 10);
        assert!(parse("fn f() { let x = 0x1_0000_0000_0000_0000 }").unwrap_err().msg.contains("too large"));
        assert!(parse("fn f() { let x = 0xg }").unwrap_err().msg.contains("not a valid number"));
        assert!(parse("fn f() { let x = 12ab }").unwrap_err().msg.contains("not a valid number"));
    }

    #[test]
    fn visibility() {
        let p = parse("pub fn a() {}\nfn b() {}\npub struct S { x: i64 }\npub const C = 1\npub macro m() -> expr { return quote(1) }").unwrap();
        assert!(p.funcs[0].is_pub && !p.funcs[1].is_pub && p.funcs[2].is_pub && p.funcs[2].is_macro);
        assert!(p.structs[0].is_pub && p.consts[0].is_pub);
        assert!(parse("pub import x").is_err());
    }

    #[test]
    fn imports_and_paths() {
        let p = parse(
            "import alloc\nimport std.io as out\nfn f(v: *alloc.Vec(i64)) -> alloc.Arena {\n  out.print(alloc.MAX)\n  let a = alloc.Arena { base: p, used: 0 }\n  let b = alloc.Pair(u8) { a: 1, b: 2 }\n  let s = p.x\n  return alloc.make!(1)\n}",
        )
        .unwrap();
        assert_eq!(p.imports[0].path, ["alloc"]);
        assert_eq!((p.imports[1].path.join("."), p.imports[1].alias.as_str()), ("std.io".into(), "out"));
        let TypeExprKind::Ptr(inner) = &p.funcs[0].params[0].ty.kind else { panic!() };
        assert!(matches!(&inner.kind, TypeExprKind::Generic(n, _) if n == "alloc.Vec"));
        assert!(matches!(&p.funcs[0].ret.as_ref().unwrap().kind, TypeExprKind::Named(n) if n == "alloc.Arena"));
        let stmts = &p.funcs[0].body.stmts;
        let Stmt::Expr(call) = &stmts[0] else { panic!() };
        let ExprKind::Call(name, args) = &call.kind else { panic!() };
        assert_eq!(name, "out.print");
        // `alloc.MAX` alone stays a field access.
        assert!(matches!(&args[0].kind, ExprKind::Field(b, f) if matches!(&b.kind, ExprKind::Var(m) if m == "alloc") && f == "MAX"));
        let Stmt::Let { value, .. } = &stmts[1] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::StructLit(t, _) if matches!(&t.kind, TypeExprKind::Named(n) if n == "alloc.Arena")));
        let Stmt::Let { value, .. } = &stmts[2] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::StructLit(t, _) if matches!(&t.kind, TypeExprKind::Generic(n, _) if n == "alloc.Pair")));
        let Stmt::Let { value, .. } = &stmts[3] else { panic!() };
        assert!(matches!(value.kind, ExprKind::Field(..)));
        let Stmt::Return { value: Some(v), .. } = &stmts[4] else { panic!() };
        assert!(matches!(&v.kind, ExprKind::MacroCall(n, _) if n == "alloc.make"));
        // In conditions, `a.b { ... }` is a field followed by the body.
        let s = body("if p.ok { g() }");
        assert!(matches!(&s[0], Stmt::If { cond, .. } if matches!(cond.kind, ExprKind::Field(..))));
        assert_eq!(parse_file("fn f() {", 3).unwrap_err().pos.file, 3);
    }

    #[test]
    fn bitwise_precedence() {
        // a | b ^ c & d << 1 + 2  ==  a | (b ^ (c & (d << (1 + 2))))
        let s = body("let x = a | b ^ c & d << 1 + 2\nlet y = p & q == r\nlet z = &p & q");
        let Stmt::Let { value, .. } = &s[0] else { panic!() };
        let ExprKind::Binary(BinOp::BitOr, _, r) = &value.kind else { panic!("{value:?}") };
        let ExprKind::Binary(BinOp::BitXor, _, r) = &r.kind else { panic!() };
        let ExprKind::Binary(BinOp::BitAnd, _, r) = &r.kind else { panic!() };
        let ExprKind::Binary(BinOp::Shl, _, r) = &r.kind else { panic!() };
        assert!(matches!(r.kind, ExprKind::Binary(BinOp::Add, _, _)));
        // Comparisons bind looser than `&`, unlike C.
        let Stmt::Let { value, .. } = &s[1] else { panic!() };
        assert!(matches!(value.kind, ExprKind::Binary(BinOp::Eq, _, _)));
        // A leading `&` is still address-of.
        let Stmt::Let { value, .. } = &s[2] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::Binary(BinOp::BitAnd, l, _) if matches!(l.kind, ExprKind::AddrOf(_))));
    }

    #[test]
    fn generic_structs() {
        let p = parse("struct Pair(T: type, comptime N: i64) { a: [T; N] }\nfn f(p: *Pair(u8, 4)) -> Pair(i64, 1) { return Pair(i64, 1) { a: [0] } }")
            .unwrap();
        assert_eq!(p.structs[0].params.len(), 2);
        assert!(p.structs[0].params.iter().all(|p| p.comptime));
        let TypeExprKind::Ptr(inner) = &p.funcs[0].params[0].ty.kind else { panic!() };
        assert!(matches!(&inner.kind, TypeExprKind::Generic(n, args) if n == "Pair" && args.len() == 2));
        let Stmt::Return { value: Some(v), .. } = &p.funcs[0].body.stmts[0] else { panic!() };
        assert!(matches!(&v.kind, ExprKind::StructLit(t, _) if matches!(t.kind, TypeExprKind::Generic(..))));
        // In a condition, `f(x) { ... }` is a call followed by the body.
        let s = body("if f(x) { g() }");
        assert!(matches!(&s[0], Stmt::If { cond, .. } if matches!(cond.kind, ExprKind::Call(..))));
    }

    #[test]
    fn macros() {
        let p = parse("macro twice(x: expr) -> expr { return quote($x + $(x) * 2) }\nfn main() { let y = twice!(a + 1, f(b, c)) }")
            .unwrap();
        assert!(p.funcs[0].is_macro && !p.funcs[1].is_macro);
        let Stmt::Return { value: Some(q), .. } = &p.funcs[0].body.stmts[0] else { panic!() };
        let ExprKind::Quote(CodeKind::Expr, pieces, holes) = &q.kind else { panic!() };
        assert_eq!(pieces, &["", " + ", " * 2"]);
        assert_eq!(holes.len(), 2);
        let Stmt::Let { value, .. } = &p.funcs[1].body.stmts[0] else { panic!() };
        let ExprKind::MacroCall(name, args) = &value.kind else { panic!() };
        assert_eq!(name, "twice");
        assert_eq!(args.iter().map(|a| a.text.as_str()).collect::<Vec<_>>(), ["a + 1", "f(b, c)"]);
        // `x != y` is not a macro call.
        assert!(matches!(body("let z = x != y")[0], Stmt::Let { .. }));
        assert!(parse("fn f() { let x = $y }").unwrap_err().msg.contains("only be used inside `quote"));
        assert!(parse("macro m() -> expr { return quote(quote(1)) }").unwrap_err().msg.contains("cannot be nested"));
        assert!(parse("macro m() -> expr { return quote(1 +) }").is_err());
        assert_eq!(parse_expr("(1) * (2)").unwrap().pos, Pos::new(1, 5));
    }

    #[test]
    fn statement_and_item_quotes() {
        let p = parse(
            "macro m(a: expr, body: stmts, name: str) -> items {\n\
             let s = quote {\n  let t = $a\n  $body\n  $a = t\n}\n\
             let e = quote {}\n\
             return quote items {\n  $prev\n  pub fn $name() -> i64 { $body\n return $a }\n}\n}\n\
             m!(x, y, \"f\")\nother.m!(1)",
        )
        .unwrap();
        let stmts = &p.funcs[0].body.stmts;
        let Stmt::Let { value, .. } = &stmts[0] else { panic!() };
        let ExprKind::Quote(CodeKind::Stmts, pieces, holes) = &value.kind else { panic!("{value:?}") };
        let kinds: Vec<HoleKind> = holes.iter().map(|h| h.0).collect();
        assert_eq!(kinds, [HoleKind::Expr, HoleKind::Stmts, HoleKind::Expr]);
        assert_eq!(pieces[0], "let t = "); // from the first token
        let Stmt::Let { value, .. } = &stmts[1] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::Quote(CodeKind::Stmts, p, h) if p == &[""] && h.is_empty()));
        let Stmt::Return { value: Some(v), .. } = &stmts[2] else { panic!() };
        let ExprKind::Quote(CodeKind::Items, _, holes) = &v.kind else { panic!() };
        let kinds: Vec<HoleKind> = holes.iter().map(|h| h.0).collect();
        assert_eq!(kinds, [HoleKind::Items, HoleKind::Ident, HoleKind::Stmts, HoleKind::Expr]);
        assert_eq!(p.macro_calls.len(), 2);
        assert_eq!(p.macro_calls[1].name, "other.m");
        assert_eq!(parse_stmts("let a = 1\nb = a").unwrap().len(), 2);
        assert_eq!(parse_items("pub fn f() {}\nstruct S { x: i64 }").unwrap().funcs.len(), 1);
        assert!(parse_items("import x").is_err());
    }

    #[test]
    fn inline_asm() {
        let s = body("let r = asm(\"mov {out}, {0}\", \"inc {out}\", out(reg) i64, in(\"rdi\") x + 1, clobber(\"cc\", \"memory\"))");
        let Stmt::Let { value, .. } = &s[0] else { panic!() };
        let ExprKind::Asm(a) = &value.kind else { panic!() };
        assert_eq!(a.template, "mov {out}, {0}\ninc {out}");
        assert!(matches!(&a.output, Some((AsmReg::Any, _))));
        assert_eq!(a.inputs[0].0, AsmReg::Named("rdi".into()));
        assert_eq!(a.clobbers, ["cc", "memory"]);
        assert!(parse("fn f() { asm(\"nop\", out(reg) i64, out(reg) i64) }").is_err());
        assert!(parse("fn f() { asm(in(reg) 1) }").is_err());
        let s = body("let r = asm(\"inc {out}\", out(reg) i64, in(out) 41)");
        let Stmt::Let { value, .. } = &s[0] else { panic!() };
        let ExprKind::Asm(a) = &value.kind else { panic!() };
        assert_eq!(a.inputs[0].0, AsmReg::Out);
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
