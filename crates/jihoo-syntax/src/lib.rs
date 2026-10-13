//! jihoo surface language: lexer, parser, AST.

pub mod ast;
mod lexer;
pub mod loader;
mod parser;

use std::fmt;

pub use lexer::{mark_names, number_marks};
pub use parser::{parse, parse_expr, parse_file, parse_items, parse_stmts};

/// Stack for the compiler, which walks the syntax tree recursively: the
/// parser, the checker and macro expansion. The parser bounds how deep the tree
/// nests (`MAX_NESTING`); this is enough for that bound even in a debug build,
/// whose frames are many times larger. It is only reserved: the pages used are
/// the ones the program touches.
pub const COMPILER_STACK: usize = 256 << 20;

/// Runs `f` on a thread with [`COMPILER_STACK`] of stack, as everything that
/// compiles should: the command line, and tests that call the compiler
/// directly instead of a test thread's 2 MB. A panic in `f` goes on here.
pub fn on_compiler_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(COMPILER_STACK)
            .spawn_scoped(s, f)
            .expect("cannot start a thread for the compiler")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

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
