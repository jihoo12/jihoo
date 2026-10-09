//! JIR text format printer. The format is specified in `docs/jir.md`.

use std::fmt::{self, Display, Formatter, Write};

use crate::*;

impl Display for Reg {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

impl Display for BlockId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

impl Display for Module {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        writeln!(f, "; jihoo IR")?;
        writeln!(f, "jir 0")?;
        writeln!(f, "profile {}", self.profile.as_str())?;
        for func in &self.funcs {
            writeln!(f)?;
            write!(f, "{func}")?;
        }
        Ok(())
    }
}

impl Display for Function {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let params = self.params.iter().map(|t| t.name()).collect::<Vec<_>>().join(", ");
        writeln!(f, "fn @{}({params}) -> {} {{", self.name, self.ret)?;
        write!(f, "  regs")?;
        for t in &self.regs {
            write!(f, " {t}")?;
        }
        writeln!(f)?;
        for (i, b) in self.blocks.iter().enumerate() {
            writeln!(f, "{}:", BlockId(i as u32))?;
            for inst in &b.insts {
                writeln!(f, "  {inst}")?;
            }
            writeln!(f, "  {}", b.term)?;
        }
        writeln!(f, "}}")
    }
}

impl Display for Inst {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Inst::Const { dst, value } => write!(f, "{dst} = const {value}"),
            Inst::Unit { dst } => write!(f, "{dst} = unit"),
            Inst::Str { dst, value } => write!(f, "{dst} = str {}", quote(value)),
            Inst::Copy { dst, src } => write!(f, "{dst} = copy {src}"),
            Inst::Unary { dst, op, src } => write!(f, "{dst} = {} {src}", op.mnemonic()),
            Inst::Binary { dst, op, lhs, rhs } => {
                write!(f, "{dst} = {} {lhs}, {rhs}", op.mnemonic())
            }
            Inst::Call { dst, func, args } => write!(f, "{dst} = call @{func}({})", list(args)),
            Inst::Syscall { dst, args } => write!(f, "{dst} = syscall({})", list(args)),
            Inst::Print { src } => write!(f, "print {src}"),
        }
    }
}

impl Display for Terminator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Terminator::Jump(b) => write!(f, "jmp {b}"),
            Terminator::Branch { cond, then, els } => write!(f, "br {cond}, {then}, {els}"),
            Terminator::Ret(r) => write!(f, "ret {r}"),
            Terminator::Unreachable => write!(f, "unreachable"),
        }
    }
}

fn list(regs: &[Reg]) -> String {
    regs.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
}

/// Printable ASCII is kept as is; every other byte is written as `\xHH`.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(b as char),
            _ => write!(out, "\\x{b:02x}").unwrap(),
        }
    }
    out.push('"');
    out
}
