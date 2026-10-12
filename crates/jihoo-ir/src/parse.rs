//! JIR text format reader: the inverse of `print.rs`, for `jihoo run
//! file.jir`. The format is specified in `docs/jir.md`; the LLVM backend has
//! its own reader (`backend-llvm/src/jir_parser.cpp`) for the compiled
//! profiles.
//!
//! The reader checks the syntax and that the module is of this `JIR_VERSION`;
//! what the instructions mean is checked by [`crate::verify`].

use std::collections::HashSet;
use std::fmt;

use crate::types::Type;
use crate::*;

/// A module that does not read, and the line where it stopped (1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

impl std::error::Error for ParseError {}

type Result<T> = std::result::Result<T, ParseError>;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Reg(u32),
    /// `@name`
    Global(String),
    /// `$Name` or `$"Name(i64)"`
    Named(String),
    Int(i64),
    Float(f64),
    Str(String),
    Punct(&'static str),
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Word(w) => write!(f, "`{w}`"),
            Tok::Reg(r) => write!(f, "`%{r}`"),
            Tok::Global(g) => write!(f, "`@{g}`"),
            Tok::Named(n) => write!(f, "`${n}`"),
            Tok::Int(n) => write!(f, "`{n}`"),
            Tok::Float(x) => write!(f, "`{x:?}`"),
            Tok::Str(_) => f.write_str("a string"),
            Tok::Punct(p) => write!(f, "`{p}`"),
        }
    }
}

const PUNCT: [&str; 13] = ["...", "->", "=", ",", "(", ")", "{", "}", ":", "*", "[", "]", "|"];

fn name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The tokens of one line, up to a `;` comment.
fn tokenize(line: usize, src: &str) -> Result<Vec<Tok>> {
    let err = |msg: String| ParseError { line, msg };
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b';' {
            break;
        } else if c == b'"' || (c == b'$' && b.get(i + 1) == Some(&b'"')) {
            let named = c == b'$';
            i += if named { 2 } else { 1 };
            let mut bytes = Vec::new();
            loop {
                let Some(&d) = b.get(i) else { return Err(err("unterminated string".into())) };
                i += 1;
                match d {
                    b'"' => break,
                    b'\\' => {
                        let e = *b.get(i).ok_or_else(|| err("unterminated string".into()))?;
                        i += 1;
                        bytes.push(match e {
                            b'n' => b'\n',
                            b't' => b'\t',
                            b'\\' => b'\\',
                            b'"' => b'"',
                            b'x' => {
                                let hex = src.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok());
                                i += 2;
                                hex.ok_or_else(|| err("bad `\\x` escape".into()))?
                            }
                            _ => return Err(err(format!("unknown escape `\\{}`", e as char))),
                        });
                    }
                    _ => bytes.push(d),
                }
            }
            let s = String::from_utf8(bytes).map_err(|_| err("a string that is not UTF-8".into()))?;
            out.push(if named { Tok::Named(s) } else { Tok::Str(s) });
        } else if matches!(c, b'%' | b'@' | b'$') {
            let start = i + 1;
            i = start;
            // Function names may contain `.`: `@max.0`, `@alloc.push`.
            while i < b.len() && (name_char(b[i]) || (c != b'%' && b[i] == b'.')) {
                i += 1;
            }
            let name = &src[start..i];
            if name.is_empty() {
                return Err(err(format!("expected a name after `{}`", c as char)));
            }
            out.push(match c {
                b'%' => Tok::Reg(name.parse().map_err(|_| err(format!("bad register `%{name}`")))?),
                b'@' => Tok::Global(name.to_string()),
                _ => Tok::Named(name.to_string()),
            });
        } else if let Some(p) = PUNCT.iter().find(|p| src[i..].starts_with(**p)) {
            out.push(Tok::Punct(p));
            i += p.len();
        } else if src[i..].starts_with("-inf") {
            out.push(Tok::Word("-inf".into()));
            i += 4;
        } else if c.is_ascii_digit() || c == b'-' {
            let start = i;
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let mut float = false;
            if b.get(i) == Some(&b'.') {
                float = true;
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if matches!(b.get(i), Some(b'e' | b'E')) {
                float = true;
                i += 1;
                if matches!(b.get(i), Some(b'+' | b'-')) {
                    i += 1;
                }
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text = &src[start..i];
            out.push(if float {
                Tok::Float(text.parse().map_err(|_| err(format!("bad number `{text}`")))?)
            } else {
                Tok::Int(text.parse().map_err(|_| err(format!("bad number `{text}`")))?)
            });
        } else if name_char(c) {
            let start = i;
            while i < b.len() && name_char(b[i]) {
                i += 1;
            }
            out.push(Tok::Word(src[start..i].to_string()));
        } else {
            let ch = src[i..].chars().next().unwrap();
            return Err(err(format!("unexpected character `{ch}`")));
        }
    }
    Ok(out)
}

/// A cursor over one line's tokens.
struct Line<'a> {
    no: usize,
    toks: Vec<Tok>,
    i: usize,
    enums: &'a HashSet<String>,
}

impl Line<'_> {
    fn err<T>(&self, msg: impl Into<String>) -> Result<T> {
        Err(ParseError { line: self.no, msg: msg.into() })
    }

    fn done(&self) -> bool {
        self.i >= self.toks.len()
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }

    fn next(&mut self, what: &str) -> Result<Tok> {
        match self.toks.get(self.i) {
            Some(t) => {
                self.i += 1;
                Ok(t.clone())
            }
            None => self.err(format!("expected {what}, found the end of the line")),
        }
    }

    fn end(&self) -> Result<()> {
        match self.peek() {
            None => Ok(()),
            Some(t) => self.err(format!("unexpected {t} at the end of the line")),
        }
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(q)) if *q == p)
    }

    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(x)) if x == w)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        let yes = self.is_punct(p);
        self.i += yes as usize;
        yes
    }

    fn punct(&mut self, p: &str) -> Result<()> {
        match self.next(&format!("`{p}`"))? {
            Tok::Punct(q) if q == p => Ok(()),
            t => self.err(format!("expected `{p}`, found {t}")),
        }
    }

    fn word(&mut self, what: &str) -> Result<String> {
        match self.next(what)? {
            Tok::Word(w) => Ok(w),
            t => self.err(format!("expected {what}, found {t}")),
        }
    }

    fn keyword(&mut self, w: &str) -> Result<()> {
        match self.next(&format!("`{w}`"))? {
            Tok::Word(x) if x == w => Ok(()),
            t => self.err(format!("expected `{w}`, found {t}")),
        }
    }

    fn reg(&mut self) -> Result<Reg> {
        match self.next("a register")? {
            Tok::Reg(r) => Ok(Reg(r)),
            t => self.err(format!("expected a register like %0, found {t}")),
        }
    }

    fn global(&mut self, what: &str) -> Result<String> {
        match self.next(what)? {
            Tok::Global(g) => Ok(g),
            t => self.err(format!("expected {what} like @name, found {t}")),
        }
    }

    fn int(&mut self) -> Result<i64> {
        match self.next("an integer")? {
            Tok::Int(n) => Ok(n),
            t => self.err(format!("expected an integer, found {t}")),
        }
    }

    fn index(&mut self) -> Result<u32> {
        let n = self.int()?;
        u32::try_from(n).or_else(|_| self.err(format!("`{n}` is not an index")))
    }

    fn block(&mut self) -> Result<BlockId> {
        let w = self.word("a block like bb0")?;
        match w.strip_prefix("bb").and_then(|n| n.parse().ok()) {
            Some(n) => Ok(BlockId(n)),
            None => self.err(format!("expected a block like bb0, found `{w}`")),
        }
    }

    fn str(&mut self) -> Result<String> {
        match self.next("a string")? {
            Tok::Str(s) => Ok(s),
            t => self.err(format!("expected a string, found {t}")),
        }
    }

    /// `T, T, ...` up to `)` (not eaten).
    fn types_until_paren(&mut self) -> Result<Vec<Type>> {
        let mut ts = Vec::new();
        while !self.is_punct(")") {
            ts.push(self.ty()?);
            if !self.eat_punct(",") {
                break;
            }
        }
        Ok(ts)
    }

    fn ty(&mut self) -> Result<Type> {
        match self.next("a type")? {
            Tok::Punct("*") => Ok(Type::ptr(self.ty()?)),
            // `[T]`, a list.
            Tok::Punct("[") if !matches!(self.peek(), Some(Tok::Int(_))) => {
                let elem = self.ty()?;
                self.punct("]")?;
                Ok(Type::List(Box::new(elem)))
            }
            Tok::Punct("[") => {
                let n = self.int()?;
                let n = u64::try_from(n).or_else(|_| self.err("an array length must not be negative"))?;
                self.keyword("x")?;
                let elem = self.ty()?;
                self.punct("]")?;
                Ok(Type::array(elem, n))
            }
            Tok::Named(n) if self.enums.contains(&n) => Ok(Type::Enum(n)),
            Tok::Named(n) => Ok(Type::Struct(n)),
            Tok::Word(w) => Ok(match w.as_str() {
                "fn" => {
                    self.punct("(")?;
                    let params = self.types_until_paren()?;
                    self.punct(")")?;
                    self.punct("->")?;
                    Type::Fn(params, Box::new(self.ty()?))
                }
                "ref" => Type::Ref(Box::new(self.ty()?)),
                "chan" => Type::Chan(Box::new(self.ty()?)),
                "cell" => Type::Cell(Box::new(self.ty()?)),
                "expr" => Type::Expr,
                "stmts" => Type::Stmts,
                "items" => Type::Items,
                other => match Type::from_name(other) {
                    Some(t) => t,
                    None => return self.err(format!("unknown type `{other}`")),
                },
            }),
            t => self.err(format!("expected a type, found {t}")),
        }
    }

    /// `(%a, %b, ...)`
    fn regs(&mut self) -> Result<Vec<Reg>> {
        self.punct("(")?;
        let mut regs = Vec::new();
        while !self.is_punct(")") {
            regs.push(self.reg()?);
            if !self.eat_punct(",") {
                break;
            }
        }
        self.punct(")")?;
        Ok(regs)
    }

    /// `(field N, elem %i, ...)`, possibly empty.
    fn path(&mut self) -> Result<Vec<PathStep>> {
        self.punct("(")?;
        let mut path = Vec::new();
        while !self.is_punct(")") {
            match self.word("`field` or `elem`")?.as_str() {
                "field" => path.push(PathStep::Field(self.index()?)),
                "elem" => path.push(PathStep::Elem(self.reg()?)),
                w => return self.err(format!("expected `field N` or `elem %r`, found `{w}`")),
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        self.punct(")")?;
        Ok(path)
    }

    /// `%a, %b` after the opcode.
    fn two(&mut self) -> Result<(Reg, Reg)> {
        let a = self.reg()?;
        self.punct(",")?;
        Ok((a, self.reg()?))
    }

    /// `%a, %b, %c` after the opcode.
    fn three(&mut self) -> Result<(Reg, Reg, Reg)> {
        let (a, b) = self.two()?;
        self.punct(",")?;
        Ok((a, b, self.reg()?))
    }
}

fn unop(w: &str) -> Option<UnOp> {
    UnOp::ALL.into_iter().find(|op| op.mnemonic() == w)
}

fn binop(w: &str) -> Option<BinOp> {
    BinOp::ALL.into_iter().find(|op| op.mnemonic() == w)
}

/// `%d = ...`, after `%d =`.
fn assign(l: &mut Line, dst: Reg) -> Result<Inst> {
    let op = l.word("an opcode")?;
    Ok(match op.as_str() {
        "const" => Inst::Const { dst, value: l.int()? },
        "fconst" => {
            let value = match l.next("a float")? {
                Tok::Float(x) => x,
                Tok::Int(n) => n as f64,
                Tok::Word(w) if w == "inf" => f64::INFINITY,
                Tok::Word(w) if w == "-inf" => f64::NEG_INFINITY,
                Tok::Word(w) if w == "nan" => f64::NAN,
                t => return l.err(format!("expected a float, found {t}")),
            };
            Inst::FConst { dst, value }
        }
        "unit" => Inst::Unit { dst },
        "str" => Inst::Str { dst, value: l.str()? },
        "copy" => Inst::Copy { dst, src: l.reg()? },
        "clone" => Inst::Clone { dst, src: l.reg()? },
        "cast" => Inst::Cast { dst, src: l.reg()? },
        "bitcast" => Inst::Bitcast { dst, src: l.reg()? },
        "call" => match l.next("a function")? {
            Tok::Global(func) => Inst::Call { dst, func, args: l.regs()? },
            Tok::Reg(r) => Inst::CallIndirect { dst, callee: Reg(r), args: l.regs()? },
            t => return l.err(format!("expected a function like @name or %N, found {t}")),
        },
        "funcref" => Inst::FuncRef { dst, func: l.global("a function")? },
        "closure" => {
            let func = l.global("a function")?;
            Inst::Closure { dst, func, captures: l.regs()? }
        }
        "struct" => match l.next("a struct")? {
            Tok::Named(name) => Inst::Struct { dst, name, fields: l.regs()? },
            t => return l.err(format!("expected a struct like $Name, found {t}")),
        },
        "field" | "fieldptr" => {
            let src = l.reg()?;
            l.punct(",")?;
            let index = l.index()?;
            if op == "field" {
                Inst::Field { dst, src, index }
            } else {
                Inst::FieldPtr { dst, ptr: src, index }
            }
        }
        "setfield" => {
            let src = l.reg()?;
            l.punct(",")?;
            let index = l.index()?;
            l.punct(",")?;
            Inst::SetField { dst, src, index, value: l.reg()? }
        }
        "load" => Inst::Load { dst, ptr: l.reg()? },
        "atomic_load" => Inst::AtomicLoad { dst, ptr: l.reg()? },
        "atomic_cas" => {
            let (ptr, expected, new) = l.three()?;
            Inst::AtomicCas { dst, ptr, expected, new }
        }
        "addr" => Inst::Addr { dst, src: l.reg()? },
        "variant" => {
            let index = l.index()?;
            Inst::Variant { dst, index, fields: l.regs()? }
        }
        "tag" => Inst::Tag { dst, src: l.reg()? },
        "payload" => {
            let src = l.reg()?;
            l.punct(",")?;
            let variant = l.index()?;
            l.punct(",")?;
            Inst::Payload { dst, src, variant, index: l.index()? }
        }
        "ref" => Inst::Ref { dst, src: l.reg()? },
        "deref" => Inst::Deref { dst, src: l.reg()? },
        "chan" => Inst::NewChan { dst, cap: l.reg()? },
        "recv" => Inst::Recv { dst, chan: l.reg()? },
        "select" => {
            // `[recv %c -> %v | send %c, %v | default]`
            l.punct("[")?;
            let (mut cases, mut default) = (Vec::new(), false);
            while !l.is_punct("]") {
                match l.word("`recv`, `send` or `default`")?.as_str() {
                    "recv" => {
                        let chan = l.reg()?;
                        l.punct("->")?;
                        cases.push(SelectCase::Recv { dst: l.reg()?, chan });
                    }
                    "send" => {
                        let (chan, value) = l.two()?;
                        cases.push(SelectCase::Send { chan, value });
                    }
                    "default" => default = true,
                    w => return l.err(format!("expected a select case, found `{w}`")),
                }
                if !l.eat_punct("|") {
                    break;
                }
            }
            l.punct("]")?;
            Inst::Select { dst, cases, default }
        }
        "cell" => Inst::NewCell { dst, value: l.reg()? },
        "cellget" => {
            let cell = l.reg()?;
            l.punct(",")?;
            Inst::CellGet { dst, cell, path: l.path()? }
        }
        "getpath" => {
            let src = l.reg()?;
            l.punct(",")?;
            Inst::GetPath { dst, src, path: l.path()? }
        }
        "setpath" => {
            let src = l.reg()?;
            l.punct(",")?;
            let path = l.path()?;
            l.punct(",")?;
            Inst::SetPath { dst, src, path, value: l.reg()? }
        }
        "array" => Inst::Array { dst, items: l.regs()? },
        "splat" => Inst::Splat { dst, value: l.reg()? },
        "elem" => {
            let (src, index) = l.two()?;
            Inst::Elem { dst, src, index }
        }
        "elemptr" => {
            let (ptr, index) = l.two()?;
            Inst::ElemPtr { dst, ptr, index }
        }
        "setelem" => {
            let (src, index, value) = l.three()?;
            Inst::SetElem { dst, src, index, value }
        }
        "syscall" => Inst::Syscall { dst, args: l.regs()? },
        "to_str" => Inst::ToStr { dst, src: l.reg()? },
        "len" => Inst::Len { dst, src: l.reg()? },
        "strbyte" => {
            let (src, index) = l.two()?;
            Inst::StrByte { dst, src, index }
        }
        "slice" => {
            let (src, lo, hi) = l.three()?;
            Inst::Slice { dst, src, lo, hi }
        }
        "readline" => Inst::ReadLine { dst },
        "repeat" => {
            let (value, count) = l.two()?;
            Inst::Repeat { dst, value, count }
        }
        "pop" => {
            let root = l.reg()?;
            l.punct(",")?;
            Inst::Pop { dst, root, path: l.path()? }
        }
        "frombytes" => Inst::FromBytes { dst, src: l.reg()? },
        "unique" => Inst::Unique { dst, prefix: l.reg()? },
        "stringify" => Inst::Stringify { dst, code: l.reg()? },
        "codetext" => Inst::CodeText { dst, src: l.reg()? },
        "typeof" => Inst::TypeOf { dst, code: l.reg()? },
        "asm" => {
            let template = l.str()?;
            l.punct(",")?;
            let constraints = l.str()?;
            Inst::Asm { dst, template, constraints, args: l.regs()? }
        }
        "quote" => {
            // `["p0", "p1", ...](kind %h, ...)`
            l.punct("[")?;
            let mut pieces = Vec::new();
            while !l.is_punct("]") {
                pieces.push(l.str()?);
                if !l.eat_punct(",") {
                    break;
                }
            }
            l.punct("]")?;
            l.punct("(")?;
            let (mut holes, mut kinds) = (Vec::new(), Vec::new());
            while !l.is_punct(")") {
                kinds.push(match l.word("a hole kind")?.as_str() {
                    "expr" => HoleKind::Expr,
                    "ident" => HoleKind::Ident,
                    "stmts" => HoleKind::Stmts,
                    "items" => HoleKind::Items,
                    "type" => HoleKind::Type,
                    w => return l.err(format!("unknown hole kind `{w}`")),
                });
                holes.push(l.reg()?);
                if !l.eat_punct(",") {
                    break;
                }
            }
            l.punct(")")?;
            if pieces.len() != holes.len() + 1 {
                return l.err("a quote needs one more piece than holes");
            }
            Inst::Quote { dst, pieces, holes, kinds }
        }
        w => {
            if let Some(op) = unop(w) {
                Inst::Unary { dst, op, src: l.reg()? }
            } else if let Some(op) = binop(w) {
                let (lhs, rhs) = l.two()?;
                Inst::Binary { dst, op, lhs, rhs }
            } else if let Some(op) = AtomicOp::ALL.into_iter().find(|op| op.mnemonic() == w) {
                let (ptr, value) = l.two()?;
                Inst::AtomicRmw { dst, op, ptr, value }
            } else {
                return l.err(format!("unknown opcode `{w}`"));
            }
        }
    })
}

/// An instruction without a result, or a terminator.
enum Stmt {
    Inst(Inst),
    Term(Terminator),
}

fn statement(l: &mut Line) -> Result<Stmt> {
    let w = l.word("an instruction")?;
    Ok(match w.as_str() {
        "store" => {
            let (ptr, value) = l.two()?;
            Stmt::Inst(Inst::Store { ptr, value })
        }
        "atomic_store" => {
            let (ptr, value) = l.two()?;
            Stmt::Inst(Inst::AtomicStore { ptr, value })
        }
        "send" => {
            let (chan, value) = l.two()?;
            Stmt::Inst(Inst::Send { chan, value })
        }
        "spawn" => {
            let callee = l.reg()?;
            Stmt::Inst(Inst::Spawn { callee, args: l.regs()? })
        }
        "cellset" => {
            let cell = l.reg()?;
            l.punct(",")?;
            let path = l.path()?;
            l.punct(",")?;
            Stmt::Inst(Inst::CellSet { cell, path, value: l.reg()? })
        }
        "push" => {
            let root = l.reg()?;
            l.punct(",")?;
            let path = l.path()?;
            l.punct(",")?;
            Stmt::Inst(Inst::Push { root, path, value: l.reg()? })
        }
        "print" => Stmt::Inst(Inst::Print { src: l.reg()? }),
        "drop" => Stmt::Inst(Inst::Drop { src: l.reg()? }),
        "trap" => Stmt::Inst(Inst::Trap),
        "fence" => Stmt::Inst(Inst::Fence),
        "jmp" => Stmt::Term(Terminator::Jump(l.block()?)),
        "br" => {
            let cond = l.reg()?;
            l.punct(",")?;
            let then = l.block()?;
            l.punct(",")?;
            Stmt::Term(Terminator::Branch { cond, then, els: l.block()? })
        }
        "ret" => Stmt::Term(Terminator::Ret(l.reg()?)),
        "unreachable" => Stmt::Term(Terminator::Unreachable),
        _ => return l.err(format!("unknown instruction `{w}`")),
    })
}

/// `struct $S { a: T, ... } [size S align A]` and `enum $E { A, B(T), ... } [...]`,
/// after the keyword. The layout is the frontend's, which the backend
/// checks; it is recomputed when the module is printed.
fn layout_suffix(l: &mut Line) -> Result<()> {
    if l.is_word("size") {
        l.i += 1;
        l.int()?;
        l.keyword("align")?;
        l.int()?;
    }
    l.end()
}

fn struct_def(l: &mut Line) -> Result<StructDef> {
    let name = match l.next("a struct name")? {
        Tok::Named(n) => n,
        t => return l.err(format!("expected a struct like $Name, found {t}")),
    };
    l.punct("{")?;
    let mut fields = Vec::new();
    while !l.is_punct("}") {
        let field = l.word("a field name")?;
        l.punct(":")?;
        fields.push((field, l.ty()?));
        if !l.eat_punct(",") {
            break;
        }
    }
    l.punct("}")?;
    layout_suffix(l)?;
    Ok(StructDef { name, fields })
}

fn enum_def(l: &mut Line) -> Result<EnumDef> {
    let name = match l.next("an enum name")? {
        Tok::Named(n) => n,
        t => return l.err(format!("expected an enum like $Name, found {t}")),
    };
    l.punct("{")?;
    let mut variants = Vec::new();
    while !l.is_punct("}") {
        let variant = l.word("a variant name")?;
        let mut payload = Vec::new();
        if l.eat_punct("(") {
            payload = l.types_until_paren()?;
            l.punct(")")?;
        }
        variants.push((variant, payload));
        if !l.eat_punct(",") {
            break;
        }
    }
    l.punct("}")?;
    layout_suffix(l)?;
    Ok(EnumDef { name, variants })
}

fn extern_fn(l: &mut Line) -> Result<ExternFn> {
    let name = l.global("a function")?;
    l.punct("(")?;
    let (mut params, mut variadic) = (Vec::new(), false);
    while !l.is_punct(")") {
        if l.eat_punct("...") {
            variadic = true;
            break;
        }
        params.push(l.ty()?);
        if !l.eat_punct(",") {
            break;
        }
    }
    l.punct(")")?;
    l.punct("->")?;
    let ret = l.ty()?;
    l.end()?;
    Ok(ExternFn { name, params, ret, variadic })
}

/// `fn @name(T, ...) -> R [export @sym] {`, after `fn`.
fn fn_header(l: &mut Line) -> Result<Function> {
    let name = l.global("a function")?;
    l.punct("(")?;
    let params = l.types_until_paren()?;
    l.punct(")")?;
    l.punct("->")?;
    let ret = l.ty()?;
    let export = if l.is_word("export") {
        l.i += 1;
        Some(l.global("a C symbol")?)
    } else {
        None
    };
    l.punct("{")?;
    l.end()?;
    Ok(Function { name, export, params, ret, regs: vec![], blocks: vec![] })
}

/// Reads a module in the JIR text format of this version.
pub fn parse(text: &str) -> Result<Module> {
    // Structs and enums share the `$Name` syntax: a first look at the enum
    // lines tells their types apart wherever they appear.
    let mut enums = HashSet::new();
    for (i, raw) in text.lines().enumerate() {
        if let [Tok::Word(w), Tok::Named(name), ..] = &tokenize(i + 1, raw)?[..] {
            if w == "enum" {
                enums.insert(name.clone());
            }
        }
    }

    let mut m = Module {
        profile: Profile::Hosted,
        target: Target::host(),
        structs: vec![],
        enums: vec![],
        externs: vec![],
        funcs: vec![],
    };
    let (mut version, mut profile, mut target) = (false, false, false);
    // The function being read, whether its `regs` line is next, and whether
    // its last block still needs a terminator.
    let mut func: Option<Function> = None;
    let mut need_regs = false;
    let mut open_block = false;
    let mut last = 0;

    for (i, raw) in text.lines().enumerate() {
        let no = i + 1;
        last = no;
        let mut l = Line { no, toks: tokenize(no, raw)?, i: 0, enums: &enums };
        if l.done() {
            continue;
        }

        let Some(f) = &mut func else {
            let w = l.word("a header, `struct`, `enum`, `extern` or `fn`")?;
            // The version comes first, so that a module of another version is
            // refused for that, not for the first line that changed.
            if !version && w != "jir" {
                return l.err("expected the `jir N` header first");
            }
            match w.as_str() {
                "jir" => {
                    if version {
                        return l.err("a second `jir` header");
                    }
                    let v = l.int()?;
                    if v != i64::from(JIR_VERSION) {
                        return l.err(format!("this is JIR version {v}, but this jihoo reads version {JIR_VERSION}"));
                    }
                    version = true;
                    l.end()?;
                }
                "profile" => {
                    if profile {
                        return l.err("a second `profile` header");
                    }
                    m.profile = match l.word("a profile")?.as_str() {
                        "hosted" => Profile::Hosted,
                        "native" => Profile::Native,
                        "freestanding" => Profile::Freestanding,
                        p => return l.err(format!("unknown profile `{p}`")),
                    };
                    profile = true;
                    l.end()?;
                }
                "target" => {
                    if target {
                        return l.err("a second `target` header");
                    }
                    let t = l.word("a target")?;
                    m.target = Target::parse(&t).ok_or_else(|| ParseError {
                        line: no,
                        msg: format!("unknown target `{t}`; the targets are x86_64 and aarch64"),
                    })?;
                    target = true;
                    l.end()?;
                }
                "struct" => m.structs.push(struct_def(&mut l)?),
                "enum" => m.enums.push(enum_def(&mut l)?),
                "extern" => m.externs.push(extern_fn(&mut l)?),
                "fn" => {
                    if !profile {
                        return l.err("missing `profile` header");
                    }
                    func = Some(fn_header(&mut l)?);
                    need_regs = true;
                }
                _ => return l.err(format!("expected a header, `struct`, `enum`, `extern` or `fn`, found `{w}`")),
            }
            continue;
        };

        if need_regs {
            l.keyword("regs")?;
            while !l.done() {
                f.regs.push(l.ty()?);
            }
            if !f.regs.starts_with(&f.params) {
                return l.err("register types must start with the parameter types");
            }
            need_regs = false;
            continue;
        }
        match &l.toks[..] {
            [Tok::Punct("}")] => {
                if open_block {
                    return l.err(format!("block bb{} has no terminator", f.blocks.len() - 1));
                }
                if f.blocks.is_empty() {
                    return l.err("a function needs blocks");
                }
                m.funcs.push(func.take().unwrap());
            }
            [Tok::Word(w), Tok::Punct(":")] if w.starts_with("bb") => {
                if open_block {
                    return l.err("the block before has no terminator");
                }
                let id = l.block()?;
                if id.0 as usize != f.blocks.len() {
                    return l.err("blocks must be numbered in order");
                }
                f.blocks.push(Block { insts: vec![], term: Terminator::Unreachable });
                open_block = true;
            }
            _ => {
                if !open_block {
                    return l.err("an instruction outside of a block");
                }
                let block = f.blocks.last_mut().unwrap();
                if let Some(&Tok::Reg(r)) = l.peek() {
                    l.i += 1;
                    l.punct("=")?;
                    block.insts.push(assign(&mut l, Reg(r))?);
                } else {
                    match statement(&mut l)? {
                        Stmt::Inst(inst) => block.insts.push(inst),
                        Stmt::Term(t) => {
                            block.term = t;
                            open_block = false;
                        }
                    }
                }
                l.end()?;
            }
        }
    }
    let fail = |msg: String| Err(ParseError { line: last, msg });
    if let Some(f) = func {
        return fail(format!("function @{} has no closing `}}`", f.name));
    }
    if !version {
        return fail("missing `jir N` header".into());
    }
    if !profile {
        return fail("missing `profile` header".into());
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> String {
        parse(text).unwrap_err().to_string()
    }

    #[test]
    fn reads_what_it_prints() {
        let text = format!(
            "; jihoo IR\njir {JIR_VERSION}\nprofile hosted\ntarget x86_64\n\n\
             struct $P {{ x: i64, s: str, e: $\"Opt(i64)\" }}\n\
             enum $\"Opt(i64)\" {{ None, Some(i64) }} size 16 align 8\n\n\
             fn @main() -> i64 {{\n  regs $P i64 str $\"Opt(i64)\" bool f64 chan i64 cell [2 x i64] unit fn(i64) -> i64 i64\n\
             bb0:\n  %1 = const -5\n  %2 = str \"a\\\"b\\x01\\n\"\n  %3 = variant 1(%1)\n  %0 = struct $P(%1, %2, %3)\n\
             \x20 %5 = fconst -0.0\n  %5 = sqrt %5\n  %5 = copysign %5, %5\n  %4 = lt %1, %1\n  %6 = chan %1\n\
             \x20 %1 = select [recv %6 -> %1 | send %6, %1 | default]\n  %7 = cell %1\n  %1 = cellget %7, ()\n\
             \x20 cellset %7, (elem %1), %1\n  %8 = unit\n  %9 = funcref @main.0\n  spawn %9(%1)\n  print %1\n  br %4, bb1, bb2\n\
             bb1:\n  ret %1\nbb2:\n  unreachable\n}}\n"
        );
        let m = parse(&text).unwrap();
        assert_eq!(m.enums[0].name, "Opt(i64)");
        assert_eq!(m.structs[0].fields[2].1, Type::Enum("Opt(i64)".into()));
        // Printing it again gives the same text, but for the comment and the
        // struct layouts, which the printer works out.
        let again = m.to_string();
        assert_eq!(parse(&again).unwrap().to_string(), again);
        assert!(again.contains("%2 = str \"a\\\"b\\x01\\n\""), "{again}");
        assert!(again.contains("%1 = select [recv %6 -> %1 | send %6, %1 | default]"), "{again}");
        assert!(again.contains("cellset %7, (elem %1), %1"), "{again}");
    }

    #[test]
    fn refuses_what_is_not_jir() {
        assert_eq!(err("profile hosted"), "line 1: expected the `jir N` header first");
        assert!(err("jir 0\nprofile hosted").contains("this is JIR version 0, but this jihoo reads version"));
        let head = format!("jir {JIR_VERSION}\nprofile hosted\n");
        assert!(err(&format!("{head}fn @f() -> unit {{\n  regs\nbb0:\n  %0 = wat %1\n}}")).contains("line 6: unknown opcode `wat`"));
        assert!(err(&format!("{head}fn @f() -> unit {{\n  regs\nbb0:\n}}")).contains("block bb0 has no terminator"));
        assert!(err(&format!("{head}fn @f() -> unit {{\n  regs unit\nbb1:\n")).contains("numbered in order"));
        assert!(err(&format!("{head}fn @f() -> unit {{\n  regs unit\nbb0:\n  ret %0\n")).contains("has no closing `}`"));
        assert!(err(&format!("{head}struct $S {{ x: u7 }}")).contains("unknown type `u7`"));
        assert!(err(&format!("{head}fn @f(i64) -> unit {{\n  regs bool\n")).contains("must start with the parameter types"));
    }
}
