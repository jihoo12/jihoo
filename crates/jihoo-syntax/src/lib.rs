//! jihoo surface language: lexer, parser, AST.

pub mod ast;
mod lexer;
pub mod loader;
mod parser;

use std::fmt;

pub use parser::{parse, parse_expr, parse_file};

/// Source position: 1-based line and column, in file number `file` (an index
/// into the loader's list of files; 0 is the root file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
    pub file: u16,
}

impl Pos {
    pub fn new(line: u32, col: u32) -> Self {
        Pos { line, col, file: 0 }
    }
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
