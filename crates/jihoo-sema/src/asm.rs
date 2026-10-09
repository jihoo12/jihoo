//! Inline assembly (freestanding only).
//!
//! `asm("bswap {out}", out(reg) u64, in(reg) x)` becomes one JIR `asm`
//! instruction with an LLVM template and constraint string:
//!
//! - `{out}` is the output and `{0}`, `{1}`, ... are the inputs; they become
//!   LLVM's `$0`, `$1`, ... (the output comes first). `{{` and `}}` are literal
//!   braces, and a literal `$` is escaped for LLVM.
//! - `out("rax")` / `in("rdi")` pin an operand to a register, `reg` lets LLVM pick
//!   one, and `in(out) x` puts `x` in the output's register (for instructions that
//!   update a register in place). `clobber("rcx", "memory")` lists what the code
//!   overwrites; the flags (`"cc"`) are always treated as clobbered.
//!
//! On x86_64 the template uses Intel syntax.

use jihoo_ir::{Inst, Profile, Reg, Type};
use jihoo_syntax::ast::{AsmExpr, AsmReg};
use jihoo_syntax::{Error, Pos};

use crate::FnCx;

fn constraint(reg: &AsmReg) -> String {
    match reg {
        AsmReg::Named(r) => format!("{{{r}}}"),
        AsmReg::Any => "r".to_string(),
        // Tied to operand 0, which is always the output.
        AsmReg::Out => "0".to_string(),
    }
}

/// Rewrites `{out}` / `{N}` to LLVM operand references.
fn translate(pos: Pos, template: &str, inputs: usize, has_out: bool) -> Result<String, Error> {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err(Error::new(pos, "unterminated `{` in asm template (write `{{` for a brace)")),
                    }
                }
                let n = match (name.as_str(), name.parse::<usize>()) {
                    ("out", _) if has_out => 0,
                    ("out", _) => return Err(Error::new(pos, "asm template uses `{out}` but there is no `out(...)`")),
                    (_, Ok(i)) if i < inputs => i + has_out as usize,
                    (_, Ok(i)) => {
                        return Err(Error::new(pos, format!("asm template uses `{{{i}}}` but there are only {inputs} inputs")))
                    }
                    _ => return Err(Error::new(pos, format!("unknown asm operand `{{{name}}}`; use `{{out}}` or `{{0}}`, `{{1}}`, ..."))),
                };
                out.push_str(&format!("${{{n}}}"));
            }
            '}' => return Err(Error::new(pos, "unmatched `}` in asm template (write `}}` for a brace)")),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    Ok(out)
}

impl FnCx<'_> {
    pub(crate) fn inline_asm(&mut self, pos: Pos, a: &AsmExpr) -> Result<Reg, Error> {
        if self.profile() != Profile::Freestanding {
            return Err(Error::new(pos, "inline asm is only available in freestanding mode"));
        }

        let mut constraints = Vec::new();
        let out_ty = match &a.output {
            Some((reg, t)) => {
                let ty = self.env.resolve(t, &self.bindings)?;
                if !matches!(ty, Type::Int(_) | Type::Ptr(_)) {
                    return Err(Error::new(t.pos, format!("asm output must be an integer or a pointer, found {ty}")));
                }
                constraints.push(format!("={}", constraint(reg)));
                ty
            }
            None => Type::Unit,
        };

        let mut args = Vec::new();
        for (reg, e) in &a.inputs {
            if *reg == AsmReg::Out && a.output.is_none() {
                return Err(Error::new(e.pos, "`in(out)` needs an `out(...)` operand"));
            }
            let r = self.expr(e, None)?;
            if !matches!(self.ty(r), Type::Int(_) | Type::Ptr(_) | Type::Bool) {
                return Err(Error::new(
                    e.pos,
                    format!("asm inputs must be integers, bools or pointers, found {}", self.ty(r)),
                ));
            }
            constraints.push(constraint(reg));
            args.push(r);
        }
        for c in &a.clobbers {
            if c != "cc" {
                constraints.push(format!("~{{{c}}}"));
            }
        }

        let template = translate(pos, &a.template, args.len(), a.output.is_some())?;
        let constraints = constraints.join(",");
        Ok(self.emit_to(out_ty, |dst| Inst::Asm { dst, template, constraints, args }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(t: &str, inputs: usize, out: bool) -> Result<String, String> {
        translate(Pos::new(1, 1), t, inputs, out).map_err(|e| e.msg)
    }

    #[test]
    fn template_translation() {
        assert_eq!(tr("mov {out}, {0}", 1, true).unwrap(), "mov ${0}, ${1}");
        assert_eq!(tr("add {0}, {1}", 2, false).unwrap(), "add ${0}, ${1}");
        assert_eq!(tr("{{x}} $5", 0, false).unwrap(), "{x} $$5");
        assert!(tr("mov {out}", 0, false).unwrap_err().contains("no `out(...)`"));
        assert!(tr("{2}", 2, false).unwrap_err().contains("only 2 inputs"));
        assert!(tr("{x}", 0, false).unwrap_err().contains("unknown asm operand"));
        assert!(tr("{0", 1, false).unwrap_err().contains("unterminated"));
        assert!(tr("}", 0, false).unwrap_err().contains("unmatched"));
    }
}
