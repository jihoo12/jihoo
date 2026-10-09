//! IR verifier. Also enforces profile rules (what is allowed in which mode).

use std::collections::HashMap;

use crate::*;

pub fn verify(m: &Module) -> Result<(), String> {
    let arity: HashMap<&str, u32> = m.funcs.iter().map(|f| (f.name.as_str(), f.params)).collect();
    if arity.len() != m.funcs.len() {
        return Err("duplicate function names".into());
    }

    let entry = match m.profile {
        Profile::Hosted => "main",
        Profile::Freestanding => "_start",
    };
    match arity.get(entry) {
        Some(0) => {}
        Some(_) => return Err(format!("entry function `{entry}` must take no parameters")),
        None => {
            return Err(format!(
                "{} program needs an entry function `fn {entry}()`",
                m.profile.as_str()
            ))
        }
    }

    for f in &m.funcs {
        verify_fn(m.profile, &arity, f).map_err(|e| format!("in @{}: {e}", f.name))?;
    }
    Ok(())
}

fn verify_fn(profile: Profile, arity: &HashMap<&str, u32>, f: &Function) -> Result<(), String> {
    if f.blocks.is_empty() {
        return Err("function has no blocks".into());
    }
    if f.params > f.num_regs {
        return Err("more params than registers".into());
    }
    let reg = |r: &Reg| {
        if r.0 < f.num_regs {
            Ok(())
        } else {
            Err(format!("register {r} out of range (regs {})", f.num_regs))
        }
    };

    for (i, b) in f.blocks.iter().enumerate() {
        let at = |e: String| format!("bb{i}: {e}");
        for inst in &b.insts {
            match inst {
                Inst::Const { dst, .. } | Inst::Str { dst, .. } => reg(dst),
                Inst::Copy { dst, src } | Inst::Unary { dst, src, .. } => reg(dst).and(reg(src)),
                Inst::Binary { dst, lhs, rhs, .. } => reg(dst).and(reg(lhs)).and(reg(rhs)),
                Inst::Call { dst, func, args } => {
                    reg(dst)?;
                    args.iter().try_for_each(reg)?;
                    match arity.get(func.as_str()) {
                        None => Err(format!("call to unknown function @{func}")),
                        Some(&n) if n as usize != args.len() => Err(format!(
                            "@{func} takes {n} arguments, {} given",
                            args.len()
                        )),
                        _ => Ok(()),
                    }
                }
                Inst::Syscall { dst, args } => {
                    reg(dst)?;
                    args.iter().try_for_each(reg)?;
                    if profile != Profile::Freestanding {
                        Err("`syscall` is only available in freestanding mode".into())
                    } else if args.is_empty() || args.len() > 7 {
                        Err("`syscall` takes 1 to 7 arguments".into())
                    } else {
                        Ok(())
                    }
                }
                Inst::Print { src } => {
                    reg(src)?;
                    if profile != Profile::Hosted {
                        Err("`print` needs std and is not available in freestanding mode".into())
                    } else {
                        Ok(())
                    }
                }
            }
            .map_err(at)?;
        }

        match &b.term {
            Terminator::Branch { cond, .. } | Terminator::Ret(cond) => reg(cond).map_err(at)?,
            Terminator::Jump(_) => {}
        }
        for s in b.term.successors() {
            if s.0 as usize >= f.blocks.len() {
                return Err(at(format!("jump to missing block {s}")));
            }
        }
    }
    Ok(())
}
