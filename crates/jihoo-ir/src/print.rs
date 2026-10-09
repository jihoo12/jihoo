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
        for s in &self.structs {
            writeln!(f)?;
            let fields: Vec<String> = s.fields.iter().map(|(n, t)| format!("{n}: {}", t.jir())).collect();
            write!(f, "struct ${} {{ {} }}", types::struct_name_jir(&s.name), fields.join(", "))?;
            // The backend checks this against LLVM's data layout.
            let fields = |n: &str| self.struct_def(n).map(|d| d.fields.iter().map(|(_, t)| t.clone()).collect());
            if let Some(l) = layout::of(&Type::Struct(s.name.clone()), &fields) {
                write!(f, " size {} align {}", l.size, l.align)?;
            }
            writeln!(f)?;
        }
        for func in &self.funcs {
            writeln!(f)?;
            write!(f, "{func}")?;
        }
        Ok(())
    }
}

impl Display for Function {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let params = self.params.iter().map(|t| t.jir()).collect::<Vec<_>>().join(", ");
        writeln!(f, "fn @{}({params}) -> {} {{", self.name, self.ret.jir())?;
        write!(f, "  regs")?;
        for t in &self.regs {
            write!(f, " {}", t.jir())?;
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
            Inst::Cast { dst, src } => write!(f, "{dst} = cast {src}"),
            Inst::Call { dst, func, args } => write!(f, "{dst} = call @{func}({})", list(args)),
            Inst::FuncRef { dst, func } => write!(f, "{dst} = funcref @{func}"),
            Inst::CallIndirect { dst, callee, args } => write!(f, "{dst} = call {callee}({})", list(args)),
            Inst::Struct { dst, name, fields } => {
                write!(f, "{dst} = struct ${}({})", types::struct_name_jir(name), list(fields))
            }
            Inst::Field { dst, src, index } => write!(f, "{dst} = field {src}, {index}"),
            Inst::SetField { dst, src, index, value } => {
                write!(f, "{dst} = setfield {src}, {index}, {value}")
            }
            Inst::Load { dst, ptr } => write!(f, "{dst} = load {ptr}"),
            Inst::Store { ptr, value } => write!(f, "store {ptr}, {value}"),
            Inst::Addr { dst, src } => write!(f, "{dst} = addr {src}"),
            Inst::FieldPtr { dst, ptr, index } => write!(f, "{dst} = fieldptr {ptr}, {index}"),
            Inst::Array { dst, items } => write!(f, "{dst} = array({})", list(items)),
            Inst::Splat { dst, value } => write!(f, "{dst} = splat {value}"),
            Inst::Elem { dst, src, index } => write!(f, "{dst} = elem {src}, {index}"),
            Inst::SetElem { dst, src, index, value } => write!(f, "{dst} = setelem {src}, {index}, {value}"),
            Inst::ElemPtr { dst, ptr, index } => write!(f, "{dst} = elemptr {ptr}, {index}"),
            Inst::Syscall { dst, args } => write!(f, "{dst} = syscall({})", list(args)),
            Inst::Print { src } => write!(f, "print {src}"),
            Inst::ToStr { dst, src } => write!(f, "{dst} = to_str {src}"),
            Inst::Unique { dst, prefix } => write!(f, "{dst} = unique {prefix}"),
            Inst::Quote { dst, pieces, holes, kinds } => {
                let pieces: Vec<String> = pieces.iter().map(|p| quote(p)).collect();
                let holes: Vec<String> = holes.iter().zip(kinds).map(|(h, k)| format!("{k:?} {h}").to_lowercase()).collect();
                write!(f, "{dst} = quote [{}]({})", pieces.join(", "), holes.join(", "))
            }
            Inst::Asm { dst, template, constraints, args } => {
                write!(f, "{dst} = asm {}, {}({})", quote(template), quote(constraints), list(args))
            }
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
