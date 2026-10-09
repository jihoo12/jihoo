//! jihoo surface language: lexer, parser, AST.

pub mod ast;
mod lexer;
mod parser;

use std::fmt;

pub use parser::{parse, parse_expr};

/// Source position (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// Error shared by the frontend (parsing and lowering).
#[derive(Debug, Clone)]
pub struct Error {
    pub pos: Pos,
    pub msg: String,
}

impl Error {
    pub fn new(pos: Pos, msg: impl Into<String>) -> Self {
        Error { pos, msg: msg.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.pos, self.msg)
    }
}

impl std::error::Error for Error {}
