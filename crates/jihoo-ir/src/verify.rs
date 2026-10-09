//! IR verifier. Checks types and profile rules (what is allowed in which mode).
//!
//! The frontend should never produce IR that fails here; this exists to catch
//! frontend bugs and to define what the backends may assume.

use std::collections::HashMap;

use crate::types::{self, Type};
use crate::*;

pub fn verify(m: &Module) -> Result<(), String> {
    let sigs: HashMap<&str, &Function> = m.funcs.iter().map(|f| (f.name.as_str(), f)).collect();
    if sigs.len() != m.funcs.len() {
        return Err("duplicate function names".into());
    }

    let entry = m.profile.entry();
    match sigs.get(entry) {
        Some(f) if f.params.is_empty() && matches!(f.ret, Type::Unit | Type::I64) => {}
        Some(_) => return Err(format!("entry function `{entry}` must be `fn {entry}()` returning unit or i64")),
        None => {
            return Err(format!(
                "{} program needs an entry function `fn {entry}()`",
                m.profile.as_str()
            ))
        }
    }

    for f in &m.funcs {
        verify_fn(m.profile, &sigs, f).map_err(|e| format!("in @{}: {e}", f.name))?;
    }
    Ok(())
}

fn verify_fn(profile: Profile, sigs: &HashMap<&str, &Function>, f: &Function) -> Result<(), String> {
    if f.blocks.is_empty() {
        return Err("function has no blocks".into());
    }
    if f.regs.len() < f.params.len() || f.regs[..f.params.len()] != f.params[..] {
        return Err("register types must start with the parameter types".into());
    }
    for t in f.regs.iter().chain([&f.ret]) {
        if !t.available_in(profile) {
            return Err(format!("type `{t}` is not available in {} mode", profile.as_str()));
        }
    }

    let ty = |r: &Reg| {
        f.regs
            .get(r.0 as usize)
            .copied()
            .ok_or_else(|| format!("register {r} out of range ({} registers)", f.regs.len()))
    };
    let expect = |r: &Reg, want: Type| -> Result<(), String> {
        let got = ty(r)?;
        if got == want {
            Ok(())
        } else {
            Err(format!("{r} has type {got}, expected {want}"))
        }
    };

    for (i, b) in f.blocks.iter().enumerate() {
        let at = |e: String| format!("bb{i}: {e}");
        for inst in &b.insts {
            match inst {
                Inst::Const { dst, .. } => match ty(dst)? {
                    Type::I64 | Type::Bool => Ok(()),
                    t => Err(format!("`const` cannot produce {t}")),
                },
                Inst::Unit { dst } => expect(dst, Type::Unit),
                Inst::Str { dst, .. } => expect(dst, types::str_literal(profile)),
                Inst::Copy { dst, src } => expect(dst, ty(src)?),
                Inst::Unary { dst, op, src } => match types::unary(*op, ty(src)?) {
                    Some(t) => expect(dst, t),
                    None => Err(format!("`{}` cannot take {}", op.mnemonic(), ty(src)?)),
                },
                Inst::Binary { dst, op, lhs, rhs } => {
                    match types::binary(*op, ty(lhs)?, ty(rhs)?) {
                        Some(t) => expect(dst, t),
                        None => Err(format!(
                            "`{}` cannot take {} and {}",
                            op.mnemonic(),
                            ty(lhs)?,
                            ty(rhs)?
                        )),
                    }
                }
                Inst::Call { dst, func, args } => {
                    let callee =
                        sigs.get(func.as_str()).ok_or_else(|| format!("call to unknown function @{func}"))?;
                    if callee.params.len() != args.len() {
                        Err(format!("@{func} takes {} arguments, {} given", callee.params.len(), args.len()))
                    } else {
                        args.iter().zip(&callee.params).try_for_each(|(a, &t)| expect(a, t))?;
                        expect(dst, callee.ret)
                    }
                }
                Inst::Syscall { dst, args } => {
                    if profile != Profile::Freestanding {
                        Err("`syscall` is only available in freestanding mode".into())
                    } else if args.is_empty() || args.len() > 7 {
                        Err("`syscall` takes 1 to 7 arguments".into())
                    } else {
                        for a in args {
                            if !ty(a)?.is_syscall_arg() {
                                return Err(at(format!("syscall argument {a} has type {}", ty(a)?)));
                            }
                        }
                        expect(dst, Type::I64)
                    }
                }
                Inst::Print { src } => {
                    if profile != Profile::Hosted {
                        Err("`print` needs std and is not available in freestanding mode".into())
                    } else if !ty(src)?.is_printable() {
                        Err(format!("cannot print {}", ty(src)?))
                    } else {
                        Ok(())
                    }
                }
            }
            .map_err(at)?;
        }

        match &b.term {
            Terminator::Branch { cond, .. } => expect(cond, Type::Bool).map_err(at)?,
            Terminator::Ret(r) => expect(r, f.ret).map_err(at)?,
            Terminator::Jump(_) | Terminator::Unreachable => {}
        }
        for s in b.term.successors() {
            if s.0 as usize >= f.blocks.len() {
                return Err(at(format!("jump to missing block {s}")));
            }
        }
    }
    Ok(())
}
