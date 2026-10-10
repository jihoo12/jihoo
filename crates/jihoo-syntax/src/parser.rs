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
use crate::lexer::{lex, lex_with, Tok, Token};
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
    let mut p = Parser::output(src)?;
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
    let mut p = Parser::output(src)?;
    let prog = p.items(false)?;
    if !prog.imports.is_empty() {
        return Err(Error::new(prog.imports[0].pos, "code made by a macro cannot `import`"));
    }
    Ok(prog)
}

/// Parses a single expression, such as the code a macro produced.
pub fn parse_expr(src: &str) -> Result<Expr, Error> {
    let mut p = Parser::output(src)?;
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
    /// Set while parsing a quote template: where to add text to it, by byte
    /// offset, to give the names it writes hygiene marks (see `mark`).
    marks: Option<Vec<(usize, String)>>,
    /// How deeply the syntax tree being built is nested here (see `MAX_NESTING`).
    depth: u32,
}

type PResult<T> = Result<T, Error>;

/// How deeply expressions, blocks, types and patterns may nest, counting each
/// operand of an operator chain like `1 + 1 + 1` as one level deeper than the
/// last. Every later stage walks the tree recursively, so this keeps a program
/// from running the compiler out of stack.
const MAX_NESTING: u32 = 1000;

impl Parser {
    fn new(src: &str, file: u16) -> Result<Self, Error> {
        let toks = lex(src, file)?;
        Ok(Parser::with_tokens(src, toks))
    }

    /// A parser for code a macro produced, whose names may carry hygiene marks.
    fn output(src: &str) -> Result<Self, Error> {
        Ok(Parser::with_tokens(src, lex_with(src, 0, true)?))
    }

    fn with_tokens(src: &str, toks: Vec<Token>) -> Self {
        Parser { src: src.to_string(), toks, i: 0, no_struct_lit: false, holes: None, marks: None, depth: 0 }
    }

    /// In a quote template, marks the name just read (if it was a name, not a
    /// hole): `tmp` becomes `tmp#` in the template, and each expansion numbers
    /// it (`tmp#7`, see `number_marks`). Names a template writes in these places
    /// are marked: variables, bindings, parameters, called functions and
    /// macros, and types. Field names, variant names, item names and the
    /// built-in types are not, and neither are names that holes insert. The
    /// compiler keeps marked locals apart from the caller's and resolves other
    /// marked names where the macro is defined (`hygiene.rs` in the compiler).
    fn mark(&mut self) {
        self.mark_with("#".into());
    }

    fn mark_with(&mut self, text: String) {
        let prev = &self.toks[self.i - 1];
        // `$name`: a hole, whose name is the macro's, not the template's.
        let in_hole = self.i >= 2 && self.toks[self.i - 2].tok == Tok::Dollar;
        if let (Some(marks), Tok::Ident(_), false) = (&mut self.marks, &prev.tok, in_hole) {
            marks.push((prev.end, text));
        }
    }

    /// Goes one level deeper into the tree; `shallower` comes back up.
    fn deeper(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(Error::new(self.pos(), format!("code is nested too deeply (more than {MAX_NESTING} levels)")));
        }
        Ok(())
    }

    fn shallower(&mut self, levels: u32) {
        self.depth -= levels;
    }

    /// Runs `f` one level deeper.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        self.deeper()?;
        let r = f(self);
        self.shallower(1);
        r
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
        // The hole itself is ordinary code; it may not contain holes, and its
        // names are the macro's own, not the template's.
        let saved = (self.holes.take(), self.marks.take());
        let inner = self.hole_body();
        (self.holes, self.marks) = saved;
        let inner = inner?;
        let end = self.prev_end();
        self.holes.as_mut().unwrap().push((start, end, inner.clone(), kind));
        Ok(Expr { pos, kind: ExprKind::Hole(Box::new(inner)) })
    }

    /// The operator of a `+=`-style token on the same line as the previous token.
    fn op_assign_follows(&self) -> Option<BinOp> {
        match self.peek() {
            Tok::OpAssign(op) if !self.cur().newline_before => Some(*op),
            _ => None,
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
                    self.mark();
                    if self.eat(&Tok::Dot) {
                        name = format!("{name}.{}", self.ident("a macro name")?.1);
                    }
                    self.expect(&Tok::Bang, "`!`")?;
                    let args = self.macro_args()?;
                    macro_calls.push(ItemMacro { pos, name, args });
                }
                Tok::Import => imports.push(self.import()?),
                Tok::Fn | Tok::Macro | Tok::Extern => funcs.push(self.fn_decl(false)?),
                Tok::Struct | Tok::Enum => structs.push(self.struct_decl(false)?),
                Tok::Const => consts.push(self.const_decl(false)?),
                Tok::Pub => {
                    self.bump();
                    match self.peek() {
                        Tok::Fn | Tok::Macro | Tok::Extern => funcs.push(self.fn_decl(true)?),
                        Tok::Struct | Tok::Enum => structs.push(self.struct_decl(true)?),
                        Tok::Const => consts.push(self.const_decl(true)?),
                        _ => return Err(self.unexpected("`fn`, `extern`, `macro`, `struct`, `enum` or `const` after `pub`")),
                    }
                }
                Tok::InnerAttr(_) => {
                    return Err(Error::new(self.pos(), "`#![...]` must come before any item"))
                }
                _ => return Err(self.unexpected("`fn`, `extern`, `macro`, `struct`, `enum`, `const` or `import`")),
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

    /// `struct Name(params) { fields }` or `enum Name(params) { variants }`.
    fn struct_decl(&mut self, is_pub: bool) -> PResult<StructDecl> {
        let is_enum = *self.peek() == Tok::Enum;
        let pos = self.bump().pos; // `struct` or `enum`
        let (_, name) = self.ident("struct name")?;
        let mut params = Vec::new();
        if self.eat(&Tok::LParen) {
            while *self.peek() != Tok::RParen {
                // Every struct parameter is compile-time; `comptime` is optional.
                self.eat(&Tok::Comptime);
                let (ppos, pname) = self.ident("parameter name")?;
                self.mark();
                self.expect(&Tok::Colon, "`:` and a parameter type")?;
                let ty = self.type_expr()?;
                params.push(Param { pos: ppos, comptime: true, name: pname, ty });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen, "`)`")?;
        }
        if is_enum {
            let variants = self.braced_list(|p| {
                let (pos, name) = p.ident("variant name")?;
                let mut fields = Vec::new();
                if p.same_line(&Tok::LParen) {
                    p.bump();
                    while *p.peek() != Tok::RParen {
                        fields.push(p.type_expr()?);
                        if !p.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    p.expect(&Tok::RParen, "`,` or `)`")?;
                }
                Ok(VariantDecl { pos, name, fields })
            })?;
            return Ok(StructDecl { pos, is_pub, name, params, fields: vec![], variants: Some(variants) });
        }
        let fields = self.braced_list(|p| {
            let (pos, name) = p.ident("field name")?;
            p.expect(&Tok::Colon, "`:` and a field type")?;
            let ty = p.type_expr()?;
            Ok(FieldDecl { pos, name, ty })
        })?;
        Ok(StructDecl { pos, is_pub, name, params, fields, variants: None })
    }

    /// `fn`, `macro`, or `extern fn` (a C function: no body, and `...` may end
    /// its parameters).
    fn fn_decl(&mut self, is_pub: bool) -> PResult<FnDecl> {
        let is_macro = *self.peek() == Tok::Macro;
        let is_extern = *self.peek() == Tok::Extern;
        let pos = self.bump().pos; // `fn`, `macro` or `extern`
        if is_extern {
            self.expect(&Tok::Fn, "`fn` after `extern`")?;
        }
        let (_, name) = self.ident("function name")?;
        self.expect(&Tok::LParen, "`(`")?;
        let mut params = Vec::new();
        let mut variadic = false;
        while *self.peek() != Tok::RParen {
            if is_extern && self.eat(&Tok::Ellipsis) {
                variadic = true;
                break;
            }
            let comptime = self.eat(&Tok::Comptime);
            if comptime && is_extern {
                return Err(Error::new(self.pos(), "extern functions cannot have `comptime` parameters"));
            }
            let (ppos, pname) = self.ident("parameter name")?;
            self.mark();
            self.expect(&Tok::Colon, "`:` and a parameter type")?;
            let ty = self.type_expr()?;
            params.push(Param { pos: ppos, comptime, name: pname, ty });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "`)`")?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.type_expr()?) } else { None };
        if is_extern {
            let body = Block { stmts: vec![], end: pos };
            return Ok(FnDecl { pos, is_pub, is_macro, is_extern, variadic, name, params, ret, body });
        }
        let body = self.block()?;
        Ok(FnDecl { pos, is_pub, is_macro, is_extern, variadic, name, params, ret, body })
    }

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        self.nested(|p| p.type_expr_body())
    }

    fn type_expr_body(&mut self) -> PResult<TypeExpr> {
        let pos = self.pos();
        if self.eat(&Tok::Fn) {
            self.expect(&Tok::LParen, "`(` and parameter types")?;
            let mut params = Vec::new();
            while *self.peek() != Tok::RParen {
                params.push(self.type_expr()?);
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen, "`,` or `)`")?;
            // `-> R` must be on the same line, so a field type ends at the newline.
            let ret = if self.same_line(&Tok::Arrow) {
                self.bump();
                Some(Box::new(self.type_expr()?))
            } else {
                None
            };
            return Ok(TypeExpr { pos, kind: TypeExprKind::Fn(params, ret) });
        }
        if self.eat(&Tok::Star) {
            let inner = self.type_expr()?;
            return Ok(TypeExpr { pos, kind: TypeExprKind::Ptr(Box::new(inner)) });
        }
        if self.eat(&Tok::Ref) {
            let inner = self.type_expr()?;
            return Ok(TypeExpr { pos, kind: TypeExprKind::Ref(Box::new(inner)) });
        }
        if self.eat(&Tok::Chan) {
            let inner = self.type_expr()?;
            return Ok(TypeExpr { pos, kind: TypeExprKind::Chan(Box::new(inner)) });
        }
        if self.eat(&Tok::Cell) {
            let inner = self.type_expr()?;
            return Ok(TypeExpr { pos, kind: TypeExprKind::Cell(Box::new(inner)) });
        }
        if self.eat(&Tok::LBracket) {
            let elem = self.type_expr()?;
            self.expect(&Tok::Semi, "`;` and an array length")?;
            let len = self.with_struct_lit(true, |p| p.expr())?;
            self.expect(&Tok::RBracket, "`]`")?;
            return Ok(TypeExpr { pos, kind: TypeExprKind::Array(Box::new(elem), Box::new(len)) });
        }
        let (pos, mut name) = self.ident("a type")?;
        if !is_builtin_type(&name) {
            self.mark();
        }
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
        self.nested(|p| p.block_body())
    }

    fn block_body(&mut self) -> PResult<Block> {
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
                self.mark();
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
            Tok::Go => {
                let pos = self.bump().pos;
                let call = self.expr()?;
                if !matches!(call.kind, ExprKind::Call(..) | ExprKind::CallExpr(..)) {
                    return Err(Error::new(call.pos, "`go` needs a function call: `go f(x)`"));
                }
                Ok(Stmt::Go { pos, call })
            }
            Tok::Select => {
                let pos = self.bump().pos;
                let arms = self.braced_list(|p| p.select_arm())?;
                Ok(Stmt::Select { pos, arms })
            }
            Tok::Match => {
                let pos = self.bump().pos;
                let value = self.cond()?;
                let arms = self.braced_list(|p| p.match_arm())?;
                Ok(Stmt::Match { pos, value, arms })
            }
            Tok::Break => Ok(Stmt::Break { pos: self.bump().pos }),
            Tok::Continue => Ok(Stmt::Continue { pos: self.bump().pos }),
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
                } else if let Some(op) = self.op_assign_follows() {
                    let pos = self.bump().pos;
                    let value = self.expr()?;
                    Ok(Stmt::OpAssign { pos, op, target: e, value })
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

    /// `pattern => { ... }` or `pattern => statement`, with an optional
    /// `if guard` before `=>`.
    fn match_arm(&mut self) -> PResult<MatchArm> {
        let pos = self.pos();
        let pattern = self.pattern()?;
        let guard = self.guard()?;
        let body = self.arm_body()?;
        Ok(MatchArm { pos, pattern, guard, body })
    }

    /// `pattern => value` in a `match` expression.
    fn match_expr_arm(&mut self) -> PResult<MatchExprArm> {
        let pos = self.pos();
        let pattern = self.pattern()?;
        let guard = self.guard()?;
        self.expect(&Tok::FatArrow, "`=>`")?;
        let value = self.with_struct_lit(true, |p| p.expr())?;
        Ok(MatchExprArm { pos, pattern, guard, value })
    }

    fn guard(&mut self) -> PResult<Option<Expr>> {
        if self.eat(&Tok::If) {
            Ok(Some(self.cond()?))
        } else {
            Ok(None)
        }
    }

    /// `=> { ... }` or `=> statement`, after the head of an arm.
    fn arm_body(&mut self) -> PResult<Block> {
        self.expect(&Tok::FatArrow, "`=>`")?;
        if *self.peek() == Tok::LBrace {
            return self.block();
        }
        let s = self.stmt()?;
        Ok(Block { stmts: vec![s], end: self.toks[self.i - 1].pos })
    }

    /// `let v = recv(c) => ...`, `recv(c) => ...`, `send(c, x) => ...` or `_ => ...`.
    fn select_arm(&mut self) -> PResult<SelectArm> {
        let pos = self.pos();
        let op = if matches!(self.peek(), Tok::Ident(n) if n == "_") {
            self.bump();
            SelectOp::Default
        } else {
            let bind = if self.eat(&Tok::Let) {
                let (_, name) = self.ident("a variable name")?;
                self.mark();
                self.expect(&Tok::Assign, "`=`")?;
                Some(name)
            } else {
                None
            };
            let e = self.expr()?;
            match (e.kind, bind) {
                (ExprKind::Call(f, mut args), bind) if f == "recv" && args.len() == 1 => {
                    SelectOp::Recv { bind, chan: args.remove(0) }
                }
                (ExprKind::Call(f, mut args), None) if f == "send" && args.len() == 2 => {
                    let value = args.pop().unwrap();
                    SelectOp::Send { chan: args.pop().unwrap(), value }
                }
                _ => {
                    let msg = "a `select` arm starts with `let x = recv(c)`, `recv(c)`, `send(c, v)` or `_`";
                    return Err(Error::new(e.pos, msg));
                }
            }
        };
        let body = self.arm_body()?;
        Ok(SelectArm { pos, op, body })
    }

    /// A pattern, possibly with alternatives: `p | q | ...`.
    fn pattern(&mut self) -> PResult<Pattern> {
        let first = self.single_pattern()?;
        if *self.peek() != Tok::Pipe {
            return Ok(first);
        }
        let pos = first.pos;
        let mut alts = vec![first];
        while self.eat(&Tok::Pipe) {
            alts.push(self.single_pattern()?);
        }
        Ok(Pattern { pos, kind: PatternKind::Or(alts) })
    }

    fn single_pattern(&mut self) -> PResult<Pattern> {
        self.nested(|p| p.single_pattern_body())
    }

    fn single_pattern_body(&mut self) -> PResult<Pattern> {
        let pos = self.pos();
        let negative = self.eat(&Tok::Minus);
        let kind = match self.peek().clone() {
            Tok::Int(n) => {
                self.bump();
                PatternKind::Int(if negative { -(n as i128) } else { n as i128 })
            }
            Tok::Float(_) => {
                return Err(Error::new(pos, "floats cannot be patterns (rounding makes exact matches fragile); compare with `<`, `==` in a guard"))
            }
            _ if negative => return Err(self.unexpected("an integer")),
            Tok::True | Tok::False => PatternKind::Bool(self.bump().tok == Tok::True),
            Tok::Ident(name) if name == "_" => {
                self.bump();
                PatternKind::Wild
            }
            Tok::Ident(name) => {
                self.bump();
                // A lowercase name alone binds; the others name variants or structs.
                let binds = !name.starts_with(|c: char| c.is_ascii_uppercase());
                let struct_name = self.same_line(&Tok::LBrace) && !self.same_line(&Tok::LParen);
                if binds && !self.same_line(&Tok::LParen) || struct_name {
                    self.mark();
                }
                if self.same_line(&Tok::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    while *self.peek() != Tok::RParen {
                        args.push(self.pattern()?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(&Tok::RParen, "`,` or `)`")?;
                    PatternKind::Variant(name, args)
                } else if self.same_line(&Tok::LBrace) {
                    self.struct_pattern(name)?
                } else {
                    PatternKind::Name(name)
                }
            }
            _ => return Err(self.unexpected("a pattern (a variant, a name, a literal or `_`)")),
        };
        Ok(Pattern { pos, kind })
    }

    /// `Name { x, y: pattern, .. }`, after the name.
    fn struct_pattern(&mut self, name: String) -> PResult<PatternKind> {
        let mut rest = false;
        let mut fields = Vec::new();
        self.braced_list(|p| {
            if rest {
                return Err(p.unexpected("`}` after `..`"));
            }
            if p.eat(&Tok::Dot) {
                p.expect(&Tok::Dot, "`..`")?;
                rest = true;
                return Ok(());
            }
            let (pos, field) = p.ident("a field name or `..`")?;
            let pattern = if p.eat(&Tok::Colon) {
                p.pattern()?
            } else {
                // `x` is `x: x`; in a template, the binding is marked, the field not.
                p.mark_with(format!(": {field}#"));
                Pattern { pos, kind: PatternKind::Name(field.clone()) }
            };
            fields.push((pos, field, pattern));
            Ok(())
        })?;
        Ok(PatternKind::Struct(name, fields, rest))
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        self.expect(&Tok::If, "`if`")?;
        let cond = self.cond()?;
        let then = self.block()?;
        let els = if self.eat(&Tok::Else) {
            if *self.peek() == Tok::If {
                let inner = self.nested(|p| p.if_stmt())?;
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
        let mut levels = 0;
        loop {
            if self.cur().newline_before {
                break;
            }
            let Some((op, prec)) = binop(self.peek()) else { break };
            if prec < min_prec {
                break;
            }
            self.deeper()?;
            levels += 1;
            let pos = self.bump().pos;
            let rhs = self.binary(prec + 1)?;
            lhs = Expr { pos, kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)) };
        }
        self.shallower(levels);
        Ok(lhs)
    }

    /// `unary (as T)*` — binds tighter than any binary operator, like in Rust.
    fn cast(&mut self) -> PResult<Expr> {
        let mut e = self.unary()?;
        let mut levels = 0;
        while self.same_line(&Tok::As) {
            self.deeper()?;
            levels += 1;
            let pos = self.bump().pos;
            let ty = self.type_expr()?;
            e = Expr { pos, kind: ExprKind::Cast(Box::new(e), ty) };
        }
        self.shallower(levels);
        Ok(e)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let pos = self.pos();
        let wrap: fn(Box<Expr>) -> ExprKind = match self.peek() {
            Tok::Minus => |e| ExprKind::Unary(UnOp::Neg, e),
            Tok::Bang => |e| ExprKind::Unary(UnOp::Not, e),
            Tok::Star => ExprKind::Deref,
            Tok::Amp => ExprKind::AddrOf,
            Tok::Ref => ExprKind::NewRef,
            Tok::Comptime => ExprKind::Comptime,
            _ => return self.postfix(),
        };
        self.bump();
        let inner = self.nested(|p| p.unary())?;
        Ok(Expr { pos, kind: wrap(Box::new(inner)) })
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let e = self.nested(|p| p.primary())?;
        let depth = self.depth;
        let r = self.postfix_ops(e);
        self.depth = depth;
        r
    }

    /// The field accesses, indexes and calls after `e`.
    fn postfix_ops(&mut self, mut e: Expr) -> PResult<Expr> {
        loop {
            let postfix = [Tok::Dot, Tok::LBracket, Tok::LParen].iter().any(|t| self.same_line(t));
            if !postfix {
                return Ok(e);
            }
            self.deeper()?;
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
                let pos = self.pos();
                let args = self.call_args()?;
                e = Expr { pos, kind: ExprKind::CallExpr(Box::new(e), args) };
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
            Tok::Float(x) => {
                self.bump();
                ExprKind::Float(x)
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
                self.mark();
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
            Tok::Fn => return self.fn_expr(),
            Tok::Match => {
                self.bump();
                let value = self.cond()?;
                let arms = self.braced_list(|p| p.match_expr_arm())?;
                ExprKind::Match(Box::new(value), arms)
            }
            // `chan(T, n)` makes a channel; `chan T` is the type.
            Tok::Chan if self.toks[self.i + 1].tok == Tok::LParen => {
                self.bump();
                self.bump();
                let ty = self.type_expr()?;
                let cap = if self.eat(&Tok::Comma) { Some(Box::new(self.with_struct_lit(true, |p| p.expr())?)) } else { None };
                self.expect(&Tok::RParen, "`,` or `)`")?;
                ExprKind::NewChan(ty, cap)
            }
            Tok::Chan => ExprKind::Type(self.type_expr()?),
            // `cell(value)` makes a cell; `cell T` is the type.
            Tok::Cell if self.toks[self.i + 1].tok == Tok::LParen => {
                self.bump();
                self.bump();
                let value = self.with_struct_lit(true, |p| p.expr())?;
                self.expect(&Tok::RParen, "`)`")?;
                ExprKind::NewCell(Box::new(value))
            }
            Tok::Cell => ExprKind::Type(self.type_expr()?),
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
    /// `fn(...)` in an expression: an anonymous function if a body follows on the
    /// same line, else a function type (a type argument).
    fn fn_expr(&mut self) -> PResult<Expr> {
        let pos = self.expect(&Tok::Fn, "`fn`")?.pos;
        self.expect(&Tok::LParen, "`(`")?;
        // Each parameter is `name: T`, or just `T` (a type, or a lambda
        // parameter's name whose type comes from context).
        let mut params: Vec<(Pos, Option<String>, TypeExpr)> = Vec::new();
        while *self.peek() != Tok::RParen {
            let ppos = self.pos();
            let named = matches!(self.peek(), Tok::Ident(_)) && self.toks[self.i + 1].tok == Tok::Colon;
            let name = if named {
                let (_, n) = self.ident("a parameter name")?;
                self.mark();
                self.bump(); // `:`
                Some(n)
            } else {
                None
            };
            params.push((ppos, name, self.type_expr()?));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "`,` or `)`")?;
        let ret = if self.same_line(&Tok::Arrow) {
            self.bump();
            Some(self.type_expr()?)
        } else {
            None
        };
        if !self.same_line(&Tok::LBrace) {
            if let Some((p, _, _)) = params.iter().find(|(_, n, _)| n.is_some()) {
                return Err(Error::new(*p, "expected the body of the anonymous function: `{ ... }`"));
            }
            let params = params.into_iter().map(|(_, _, t)| t).collect();
            let ty = TypeExpr { pos, kind: TypeExprKind::Fn(params, ret.map(Box::new)) };
            return Ok(Expr { pos, kind: ExprKind::Type(ty) });
        }
        let params = params
            .into_iter()
            .map(|(p, name, ty)| match (name, ty.kind) {
                (Some(n), kind) => Ok((p, n, Some(TypeExpr { pos: ty.pos, kind }))),
                (None, TypeExprKind::Named(n)) if !n.contains('.') => Ok((p, n, None)),
                (None, _) => Err(Error::new(p, "a parameter of an anonymous function needs a name: `x` or `x: T`")),
            })
            .collect::<PResult<Vec<_>>>()?;
        let body = self.with_struct_lit(true, |p| p.block())?;
        Ok(Expr { pos, kind: ExprKind::Lambda(Box::new(Lambda { params, ret, body })) })
    }

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
        self.marks = Some(Vec::new());
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
        let marks = self.marks.take().unwrap();
        parsed?;
        // An empty template has no last token of its own.
        let end = if self.cur().start > start { self.prev_end().max(start) } else { start };
        self.expect(&close, if kind == CodeKind::Expr { "`)`" } else { "`}`" })?;

        // The template's text between the holes, with the marks added.
        let piece = |from: usize, to: usize| {
            let mut s = String::new();
            let mut at = from;
            for (offset, text) in marks.iter().filter(|(o, _)| (from..=to).contains(o)) {
                s.push_str(&self.src[at..*offset]);
                s.push_str(text);
                at = *offset;
            }
            s.push_str(&self.src[at..to]);
            s
        };
        let mut pieces = Vec::new();
        let mut exprs = Vec::new();
        let mut at = start;
        for (s, e, expr, hole) in holes {
            pieces.push(piece(at, s));
            exprs.push((hole, expr));
            at = e;
        }
        pieces.push(piece(at, end));
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
        Tok::Float(x) => format!("float `{x:?}`"),
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
        Tok::Break => "break",
        Tok::Continue => "continue",
        Tok::True => "true",
        Tok::False => "false",
        Tok::Struct => "struct",
        Tok::Enum => "enum",
        Tok::Match => "match",
        Tok::Ref => "ref",
        Tok::Go => "go",
        Tok::Chan => "chan",
        Tok::Select => "select",
        Tok::Cell => "cell",
        Tok::FatArrow => "=>",
        Tok::As => "as",
        Tok::Const => "const",
        Tok::Comptime => "comptime",
        Tok::Asm => "asm",
        Tok::Macro => "macro",
        Tok::Quote => "quote",
        Tok::Import => "import",
        Tok::Pub => "pub",
        Tok::Extern => "extern",
        Tok::Ellipsis => "...",
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
        Tok::OpAssign(op) => match op {
            BinOp::Add => "+=",
            BinOp::Sub => "-=",
            BinOp::Mul => "*=",
            BinOp::Div => "/=",
            BinOp::Rem => "%=",
            BinOp::BitAnd => "&=",
            BinOp::BitOr => "|=",
            BinOp::BitXor => "^=",
            BinOp::Shl => "<<=",
            BinOp::Shr => ">>=",
            _ => "?=",
        },
        Tok::Ident(_) | Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::InnerAttr(_) | Tok::Eof => "?",
    }
}

/// The types that are built in, whose names a template does not mark.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32" | "f64" | "bool" | "str" | "unit"
            | "type" | "expr" | "stmts" | "items"
    )
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
    fn lexes_compound_assignment() {
        let toks = |src: &str| -> Vec<Tok> { crate::lexer::lex(src, 0).unwrap().into_iter().map(|t| t.tok).collect() };
        let x = || Tok::Ident("x".into());
        assert_eq!(toks("x += 1"), [x(), Tok::OpAssign(BinOp::Add), Tok::Int(1), Tok::Eof]);
        assert_eq!(toks("x<<=x"), [x(), Tok::OpAssign(BinOp::Shl), x(), Tok::Eof]);
        assert_eq!(toks("x >>= 1"), [x(), Tok::OpAssign(BinOp::Shr), Tok::Int(1), Tok::Eof]);
        assert_eq!(toks("x>=1"), [x(), Tok::Ge, Tok::Int(1), Tok::Eof]);
        assert_eq!(toks("x && y"), [x(), Tok::AndAnd, Tok::Ident("y".into()), Tok::Eof]);
        assert_eq!(toks("x -> -=")[1..3], [Tok::Arrow, Tok::OpAssign(BinOp::Sub)]);
        let s = body("a[i].x |= 4");
        assert!(matches!(&s[0], Stmt::OpAssign { op: BinOp::BitOr, .. }));
    }

    #[test]
    fn lexes_float_literals() {
        let toks = |src: &str| -> Vec<Tok> { crate::lexer::lex(src, 0).unwrap().into_iter().map(|t| t.tok).collect() };
        assert_eq!(toks("1.5 2e3 1_000.25e-2 7E+1"), [Tok::Float(1.5), Tok::Float(2e3), Tok::Float(10.0025), Tok::Float(70.0), Tok::Eof]);
        // `1.` and `1.x` are an integer and a `.`; hex digits are never exponents.
        assert_eq!(toks("1.x"), [Tok::Int(1), Tok::Dot, Tok::Ident("x".into()), Tok::Eof]);
        assert_eq!(toks("0x1e3"), [Tok::Int(0x1e3), Tok::Eof]);
        assert_eq!(toks("xs[0].y"), [Tok::Ident("xs".into()), Tok::LBracket, Tok::Int(0), Tok::RBracket, Tok::Dot, Tok::Ident("y".into()), Tok::Eof]);
        assert!(crate::lexer::lex("1e999", 0).unwrap_err().msg.contains("too large"));
        assert!(crate::lexer::lex("1.5x", 0).unwrap_err().msg.contains("not a valid number"));
        assert!(parse("fn f(x: f64) { match x { 1.5 => {} } }").unwrap_err().msg.contains("floats cannot be patterns"));
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
    fn nesting_is_bounded() {
        // As in the CLI: the default stack of a test thread is too small for
        // the deepest trees allowed in a debug build.
        std::thread::Builder::new().stack_size(256 << 20).spawn(check_nesting).unwrap().join().unwrap();
    }

    fn check_nesting() {
        let deep = |open: &str, mid: &str, close: &str, n: usize| {
            format!("fn f() -> i64 {{ {}{mid}{} }}", open.repeat(n), close.repeat(n))
        };
        let too_deep = |src: String| parse(&src).unwrap_err().msg.contains("nested too deeply");
        // Up to the limit parses; past it is an error, not a stack overflow.
        assert!(parse(&deep("(", "1", ")", 900)).is_ok());
        assert!(too_deep(deep("(", "1", ")", 5000)));
        assert!(too_deep(deep("-", "1", "", 5000)));
        assert!(too_deep(deep("", "1", " + 1", 5000)));
        assert!(too_deep(deep("", "x", ".y", 5000)));
        assert!(too_deep(deep("if c { ", "", "}", 5000)));
        assert!(too_deep(format!("fn f(p: {}i64) {{}}", "*".repeat(5000))));
    }

    #[test]
    fn non_ascii_characters_in_errors() {
        assert!(parse("fn 한() {}").unwrap_err().msg.contains("unexpected character `한`"));
        assert!(parse("fn f() { let s = \"\\é\" }").unwrap_err().msg.contains("unknown escape `\\é`"));
        assert!(parse("fn f() { let s = \"\\").unwrap_err().msg.contains("unterminated string"));
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
        assert_eq!(lit("0x7fff_ffff_ffff_ffff"), i64::MAX as u64);
        assert_eq!(lit("0xffff_ffff_ffff_ffff"), u64::MAX);
        assert_eq!(lit("18446744073709551615"), u64::MAX);
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

    /// The text of a template, with the holes as `$`.
    fn template(src: &str) -> String {
        let e = parse_expr(src).unwrap();
        let ExprKind::Quote(_, pieces, _) = e.kind else { panic!("{e:?}") };
        pieces.join("$")
    }

    #[test]
    fn templates_mark_the_names_they_write() {
        // Variables, bindings, parameters, called functions and macros, and
        // types; not fields, variants, built-in types or what holes insert.
        assert_eq!(
            template("quote { let t: i64 = helper($a) + n.len + util.f(1) + m!(t)\n let p = P { x: t, y: $(b) }\n print(p.x) }"),
            "let t#: i64 = helper#($) + n#.len + util#.f(1) + m#!(t#)\n let p# = P# { x: t#, y: $ }\n print#(p#.x)"
        );
        assert_eq!(
            template("quote { match v {\n Some(x) => {}\n P { x, y: yy } => {}\n None => {}\n k => {}\n } }"),
            "match v# {\n Some(x#) => {}\n P# { x: x#, y: yy# } => {}\n None => {}\n k# => {}\n }"
        );
        assert_eq!(
            template("quote(fn(k: [Point; 2], s: str) -> u8 { return $x })"),
            "fn(k#: [Point#; 2], s#: str) -> u8 { return $ }"
        );
        // A name hole is the macro's; so is everything inside `$( )`.
        assert_eq!(template("quote { let $name = $(f(a)) }"), "let $ = $");
        assert_eq!(template("quote items { fn api(x: T) -> T { return x } }"), "fn api(x#: T#) -> T# { return x# }");
    }

    #[test]
    fn macro_output_has_numbered_and_qualified_names() {
        assert_eq!(crate::number_marks("let t# = \"a#\" // b#\n f#(t#2)", 7), "let t#7 = \"a#\" // b#\n f#7(t#2)");
        let e = parse_expr("#3:helper(t#7) + util#7.f(1)").unwrap();
        let ExprKind::Binary(_, l, r) = e.kind else { panic!() };
        assert!(matches!(&l.kind, ExprKind::Call(n, args) if n == "#3:helper" && matches!(&args[0].kind, ExprKind::Var(v) if v == "t#7")));
        assert!(matches!(&r.kind, ExprKind::Call(n, _) if n == "util#7.f"));
        // Only macro output may use them.
        assert!(parse("fn f() { let t#7 = 1 }").is_err());
        assert!(parse("fn f() { #3:helper() }").is_err());
        let renamed = crate::mark_names("t#7 + \"t#7\" + x.y + u#7.v", &mut |n| Some(format!("<{n}>")));
        assert_eq!(renamed, "<t#7> + \"t#7\" + x.y + <u#7>.v");
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
        assert_eq!(pieces[0], "let t# = "); // from the first token, with `t` marked
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
    fn function_types_and_calls() {
        let p = parse("struct S {\n  f: fn(i64, *u8) -> i64\n  g: fn()\n}\nfn h() -> fn(i64) -> i64 { return s.f(1)(2) }").unwrap();
        let TypeExprKind::Fn(params, Some(ret)) = &p.structs[0].fields[0].ty.kind else { panic!() };
        assert!(params.len() == 2 && matches!(&ret.kind, TypeExprKind::Named(n) if n == "i64"));
        // Without `->` on the same line, the type ends: `g` returns unit.
        assert!(matches!(&p.structs[0].fields[1].ty.kind, TypeExprKind::Fn(ps, None) if ps.is_empty()));
        assert!(matches!(&p.funcs[0].ret.as_ref().unwrap().kind, TypeExprKind::Fn(_, Some(_))));
        let Stmt::Return { value: Some(v), .. } = &p.funcs[0].body.stmts[0] else { panic!() };
        let ExprKind::CallExpr(callee, args) = &v.kind else { panic!("{v:?}") };
        assert!(matches!(&callee.kind, ExprKind::Call(n, _) if n == "s.f") && args.len() == 1);
        // A call never continues on the next line.
        assert_eq!(body("f\n(1)").len(), 2);
        assert!(matches!(&body("let t = Vec(fn(u8) -> u8)")[0], Stmt::Let { value, .. }
            if matches!(&value.kind, ExprKind::Call(_, a) if matches!(a[0].kind, ExprKind::Type(_)))));
    }

    #[test]
    fn enums_and_match() {
        let p = parse("pub enum Option(T: type) {\n  Some(T)\n  None\n}\nfn f() {\n match o {\n  Some(x, _) => return x\n  None => { g() }\n  -1 => {}\n  _ => {}\n }\n}").unwrap();
        let e = &p.structs[0];
        assert!(e.is_pub && e.is_enum() && e.params.len() == 1);
        let vs = e.variants.as_ref().unwrap();
        assert!(vs[0].name == "Some" && vs[0].fields.len() == 1 && vs[1].fields.is_empty());
        let Stmt::Match { arms, .. } = &p.funcs[0].body.stmts[0] else { panic!() };
        let PatternKind::Variant(n, args) = &arms[0].pattern.kind else { panic!() };
        assert!(n == "Some" && matches!(&args[0].kind, PatternKind::Name(x) if x == "x"));
        assert!(matches!(args[1].kind, PatternKind::Wild));
        assert!(matches!(&arms[1].pattern.kind, PatternKind::Name(n) if n == "None"));
        assert!(matches!(arms[2].pattern.kind, PatternKind::Int(-1)) && matches!(arms[3].pattern.kind, PatternKind::Wild));
        // The value is not a struct literal: `o {` starts the arms.
        assert!(parse("fn f() { match p { _ => {} } }").is_ok());
        assert!(parse("fn f() { match p { 1 {} } }").unwrap_err().msg.contains("expected `=>`"));
    }

    #[test]
    fn anonymous_functions() {
        let s = body("let f = fn(x, y: u8) -> u8 { return y }\nlet t = Vec(fn(x) -> u8)\ng(fn() { h() })");
        let Stmt::Let { value, .. } = &s[0] else { panic!() };
        let ExprKind::Lambda(l) = &value.kind else { panic!("{value:?}") };
        assert!(l.params[0].1 == "x" && l.params[0].2.is_none() && l.params[1].2.is_some() && l.ret.is_some());
        // Without a body, `fn(x) -> u8` is a type: `x` names a type.
        let Stmt::Let { value, .. } = &s[1] else { panic!() };
        assert!(matches!(&value.kind, ExprKind::Call(_, a) if matches!(a[0].kind, ExprKind::Type(_))));
        let Stmt::Expr(call) = &s[2] else { panic!() };
        assert!(matches!(&call.kind, ExprKind::Call(_, a) if matches!(a[0].kind, ExprKind::Lambda(_))));
    }

    #[test]
    fn tasks_and_channels() {
        let s = body("let c: chan u8 = chan(u8, 4)\ngo f(c)\ngo fn() { g() }()\nlet t = V(chan u8)");
        assert!(matches!(&s[0], Stmt::Let { ty: Some(t), value, .. }
            if matches!(t.kind, TypeExprKind::Chan(_)) && matches!(value.kind, ExprKind::NewChan(_, Some(_)))));
        assert!(matches!(&s[1], Stmt::Go { call, .. } if matches!(call.kind, ExprKind::Call(..))));
        assert!(matches!(&s[2], Stmt::Go { call, .. } if matches!(call.kind, ExprKind::CallExpr(..))));
        assert!(matches!(&s[3], Stmt::Let { value, .. } if matches!(&value.kind, ExprKind::Call(_, a) if matches!(a[0].kind, ExprKind::Type(_)))));
    }

    #[test]
    fn cells() {
        let s = body("let c: cell u8 = cell(1)\n*c = 2\nlet t = V(cell u8)");
        assert!(matches!(&s[0], Stmt::Let { ty: Some(t), value, .. }
            if matches!(t.kind, TypeExprKind::Cell(_)) && matches!(value.kind, ExprKind::NewCell(_))));
        assert!(matches!(&s[1], Stmt::Assign { target, .. } if matches!(target.kind, ExprKind::Deref(_))));
        assert!(matches!(&s[2], Stmt::Let { value, .. } if matches!(&value.kind, ExprKind::Call(_, a) if matches!(a[0].kind, ExprKind::Type(_)))));
    }

    #[test]
    fn select_arms() {
        let s = body("select {\n let v = recv(c) => f(v)\n recv(d) => {}\n send(e, 1) => {}\n _ => {}\n}");
        let Stmt::Select { arms, .. } = &s[0] else { panic!() };
        assert!(matches!(&arms[0].op, SelectOp::Recv { bind: Some(v), .. } if v == "v"));
        assert!(matches!(&arms[1].op, SelectOp::Recv { bind: None, .. }));
        assert!(matches!(&arms[2].op, SelectOp::Send { .. }));
        assert!(matches!(&arms[3].op, SelectOp::Default));
    }

    #[test]
    fn nested_patterns_and_match_expressions() {
        let s = body("let a = match s {\n Circle(Point { x, y: 0, .. }, r) if r > 1 => r\n _ => 0\n}");
        let Stmt::Let { value, .. } = &s[0] else { panic!() };
        let ExprKind::Match(_, arms) = &value.kind else { panic!("{value:?}") };
        assert!(arms.len() == 2 && arms[0].guard.is_some());
        let PatternKind::Variant(_, args) = &arms[0].pattern.kind else { panic!() };
        let PatternKind::Struct(n, fields, rest) = &args[0].kind else { panic!() };
        assert!(n == "Point" && *rest && fields.len() == 2);
        assert!(matches!(&fields[0].2.kind, PatternKind::Name(x) if x == "x"));
        assert!(matches!(fields[1].2.kind, PatternKind::Int(0)));
        assert!(parse("fn f() { match p { P { .., x } => {} } }").unwrap_err().msg.contains("`}` after `..`"));
        let s = body("match s {\n A(1 | 2) | B => {}\n}");
        let Stmt::Match { arms, .. } = &s[0] else { panic!() };
        let PatternKind::Or(alts) = &arms[0].pattern.kind else { panic!() };
        assert!(alts.len() == 2 && matches!(&alts[0].kind, PatternKind::Variant(_, a) if matches!(a[0].kind, PatternKind::Or(_))));
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
