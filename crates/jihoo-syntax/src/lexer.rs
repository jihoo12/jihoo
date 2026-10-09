use crate::{Error, Pos};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Str(String),
    /// `#![name]`
    InnerAttr(String),

    Fn,
    Let,
    Return,
    If,
    Else,
    While,
    True,
    False,
    Struct,
    Enum,
    Match,
    Ref,
    Go,
    Chan,
    As,
    Const,
    Comptime,
    Asm,
    Macro,
    Quote,
    Import,
    Pub,

    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Dot,
    Comma,
    Colon,
    Semi,
    Arrow,
    /// `=>`
    FatArrow,
    Assign,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Pipe,
    Caret,
    Shl,
    Shr,
    Bang,
    Amp,
    Dollar,
    AndAnd,
    OrOr,

    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub pos: Pos,
    /// Whether a newline preceded this token. Used to end statements without `;`.
    pub newline_before: bool,
    /// Byte range in the source, used to recover the text of macro arguments.
    pub start: usize,
    pub end: usize,
}

struct Lexer<'a> {
    src: &'a [u8],
    i: usize,
    line: u32,
    col: u32,
    file: u16,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> u8 {
        self.src.get(self.i).copied().unwrap_or(0)
    }

    fn peek2(&self) -> u8 {
        self.src.get(self.i + 1).copied().unwrap_or(0)
    }

    fn bump(&mut self) -> u8 {
        let c = self.peek();
        self.i += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        c
    }

    fn pos(&self) -> Pos {
        Pos { line: self.line, col: self.col, file: self.file }
    }

    /// Skips whitespace and comments; returns whether a newline was crossed.
    fn skip_trivia(&mut self) -> bool {
        let mut newline = false;
        loop {
            match self.peek() {
                b'\n' => {
                    newline = true;
                    self.bump();
                }
                b' ' | b'\t' | b'\r' => {
                    self.bump();
                }
                b'/' if self.peek2() == b'/' => {
                    while self.peek() != b'\n' && self.i < self.src.len() {
                        self.bump();
                    }
                }
                _ => return newline,
            }
        }
    }

    fn string(&mut self, start: Pos) -> Result<String, Error> {
        let mut bytes = Vec::new();
        loop {
            if self.i >= self.src.len() {
                return Err(Error::new(start, "unterminated string literal"));
            }
            match self.bump() {
                b'"' => break,
                b'\\' => {
                    let esc_pos = self.pos();
                    let b = match self.bump() {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        b'0' => 0,
                        b'\\' => b'\\',
                        b'"' => b'"',
                        c => {
                            return Err(Error::new(
                                esc_pos,
                                format!("unknown escape `\\{}`", c as char),
                            ))
                        }
                    };
                    bytes.push(b);
                }
                c => bytes.push(c),
            }
        }
        String::from_utf8(bytes).map_err(|_| Error::new(start, "string literal is not valid UTF-8"))
    }

    fn next(&mut self) -> Result<Token, Error> {
        let newline_before = self.skip_trivia();
        let pos = self.pos();
        let start = self.i;

        if self.i >= self.src.len() {
            return Ok(Token { tok: Tok::Eof, pos, newline_before, start, end: start });
        }

        let c = self.bump();
        let tok = match c {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b'[' => Tok::LBracket,
            b']' => Tok::RBracket,
            b'.' => Tok::Dot,
            b',' => Tok::Comma,
            b':' => Tok::Colon,
            b';' => Tok::Semi,
            b'$' => Tok::Dollar,
            b'+' => Tok::Plus,
            b'*' => Tok::Star,
            b'/' => Tok::Slash,
            b'%' => Tok::Percent,
            b'^' => Tok::Caret,
            b'-' if self.peek() == b'>' => {
                self.bump();
                Tok::Arrow
            }
            b'-' => Tok::Minus,
            b'=' if self.peek() == b'=' => {
                self.bump();
                Tok::EqEq
            }
            b'=' if self.peek() == b'>' => {
                self.bump();
                Tok::FatArrow
            }
            b'=' => Tok::Assign,
            b'!' if self.peek() == b'=' => {
                self.bump();
                Tok::NotEq
            }
            b'!' => Tok::Bang,
            b'<' if self.peek() == b'<' => {
                self.bump();
                Tok::Shl
            }
            b'>' if self.peek() == b'>' => {
                self.bump();
                Tok::Shr
            }
            b'<' if self.peek() == b'=' => {
                self.bump();
                Tok::Le
            }
            b'<' => Tok::Lt,
            b'>' if self.peek() == b'=' => {
                self.bump();
                Tok::Ge
            }
            b'>' => Tok::Gt,
            b'&' if self.peek() == b'&' => {
                self.bump();
                Tok::AndAnd
            }
            b'&' => Tok::Amp,
            b'|' if self.peek() == b'|' => {
                self.bump();
                Tok::OrOr
            }
            b'|' => Tok::Pipe,
            b'"' => Tok::Str(self.string(pos)?),
            b'#' if self.peek() == b'!' && self.peek2() == b'[' => {
                self.bump();
                self.bump();
                let start = self.i;
                while self.peek() != b']' {
                    if self.i >= self.src.len() || self.peek() == b'\n' {
                        return Err(Error::new(pos, "unterminated `#![...]` attribute"));
                    }
                    self.bump();
                }
                let name = String::from_utf8_lossy(&self.src[start..self.i]).trim().to_string();
                self.bump();
                Tok::InnerAttr(name)
            }
            c if c.is_ascii_digit() => {
                let start = self.i - 1;
                // `0x` / `0b` prefixes; digits may be separated by `_`.
                let radix = match (c, self.peek()) {
                    (b'0', b'x' | b'X') => 16,
                    (b'0', b'b' | b'B') => 2,
                    _ => 10,
                };
                if radix != 10 {
                    self.bump();
                }
                let digits_start = self.i;
                while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                    self.bump();
                }
                let raw = std::str::from_utf8(&self.src[start..self.i]).unwrap();
                let from = if radix == 10 { start } else { digits_start };
                let digits: String =
                    std::str::from_utf8(&self.src[from..self.i]).unwrap().chars().filter(|&c| c != '_').collect();
                let n = i64::from_str_radix(&digits, radix).map_err(|e| {
                    let why = match e.kind() {
                        std::num::IntErrorKind::PosOverflow => "is too large",
                        _ => "is not a valid number",
                    };
                    Error::new(pos, format!("integer literal `{raw}` {why}"))
                })?;
                Tok::Int(n)
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = self.i - 1;
                while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                    self.bump();
                }
                let word = std::str::from_utf8(&self.src[start..self.i]).unwrap();
                match word {
                    "fn" => Tok::Fn,
                    "let" => Tok::Let,
                    "return" => Tok::Return,
                    "if" => Tok::If,
                    "else" => Tok::Else,
                    "while" => Tok::While,
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "struct" => Tok::Struct,
                    "enum" => Tok::Enum,
                    "match" => Tok::Match,
                    "ref" => Tok::Ref,
                    "go" => Tok::Go,
                    "chan" => Tok::Chan,
                    "as" => Tok::As,
                    "const" => Tok::Const,
                    "comptime" => Tok::Comptime,
                    "asm" => Tok::Asm,
                    "macro" => Tok::Macro,
                    "quote" => Tok::Quote,
                    "import" => Tok::Import,
                    "pub" => Tok::Pub,
                    _ => Tok::Ident(word.to_string()),
                }
            }
            c => {
                return Err(Error::new(
                    pos,
                    format!("unexpected character `{}`", c as char),
                ))
            }
        };
        Ok(Token { tok, pos, newline_before, start, end: self.i })
    }
}

pub fn lex(src: &str, file: u16) -> Result<Vec<Token>, Error> {
    let mut lx = Lexer { src: src.as_bytes(), i: 0, line: 1, col: 1, file };
    let mut out = Vec::new();
    loop {
        let t = lx.next()?;
        let eof = t.tok == Tok::Eof;
        out.push(t);
        if eof {
            return Ok(out);
        }
    }
}
