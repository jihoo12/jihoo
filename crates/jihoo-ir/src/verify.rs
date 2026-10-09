//! IR verifier. Checks types and profile rules (what is allowed in which mode).
//!
//! The frontend should never produce IR that fails here; this exists to catch
//! frontend bugs and to define what the backends may assume.

use std::collections::HashMap;

use crate::types::{self, Type};
use crate::*;

struct Cx<'m> {
    profile: Profile,
    funcs: HashMap<&'m str, &'m Function>,
    structs: HashMap<&'m str, &'m StructDef>,
    enums: HashMap<&'m str, &'m EnumDef>,
}

pub fn verify(m: &Module) -> Result<(), String> {
    let cx = Cx {
        profile: m.profile,
        funcs: m.funcs.iter().map(|f| (f.name.as_str(), f)).collect(),
        structs: m.structs.iter().map(|s| (s.name.as_str(), s)).collect(),
        enums: m.enums.iter().map(|e| (e.name.as_str(), e)).collect(),
    };
    if cx.funcs.len() != m.funcs.len() {
        return Err("duplicate function names".into());
    }
    if cx.structs.len() != m.structs.len() || cx.enums.len() != m.enums.len() {
        return Err("duplicate struct or enum names".into());
    }
    if let Some(e) = m.enums.iter().find(|e| cx.structs.contains_key(e.name.as_str())) {
        return Err(format!("${} is both a struct and an enum", e.name));
    }

    for s in &m.structs {
        for (name, t) in &s.fields {
            cx.check_type(t).map_err(|e| format!("in ${}.{name}: {e}", s.name))?;
        }
        cx.check_acyclic(&s.name, &mut Vec::new())?;
    }
    for e in &m.enums {
        for (name, ts) in &e.variants {
            for t in ts {
                cx.check_type(t).map_err(|err| format!("in ${}.{name}: {err}", e.name))?;
            }
        }
        cx.check_acyclic(&e.name, &mut Vec::new())?;
    }

    let entry = m.profile.entry();
    match cx.funcs.get(entry) {
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
        cx.verify_fn(f).map_err(|e| format!("in @{}: {e}", f.name))?;
    }
    Ok(())
}

impl Cx<'_> {
    fn check_type(&self, t: &Type) -> Result<(), String> {
        if !t.available_in(self.profile) {
            return Err(format!("type `{}` is not available in {} mode", t.jir(), self.profile.as_str()));
        }
        match t {
            Type::Struct(name) if !self.structs.contains_key(name.as_str()) => {
                Err(format!("unknown struct `${name}`"))
            }
            Type::Enum(name) if !self.enums.contains_key(name.as_str()) => Err(format!("unknown enum `${name}`")),
            Type::Ptr(inner) | Type::Array(inner, _) | Type::Ref(inner) | Type::Chan(inner) => self.check_type(inner),
            Type::Fn(params, ret) => params.iter().chain([&**ret]).try_for_each(|t| self.check_type(t)),
            _ => Ok(()),
        }
    }

    /// A struct or enum may not contain itself by value (it would be infinitely
    /// large).
    fn check_acyclic(&self, name: &str, stack: &mut Vec<String>) -> Result<(), String> {
        if stack.iter().any(|s| s == name) {
            return Err(format!("${name} contains itself"));
        }
        stack.push(name.to_string());
        let members: Vec<&Type> = match self.structs.get(name) {
            Some(s) => s.fields.iter().map(|(_, t)| t).collect(),
            None => self.enums[name].variants.iter().flat_map(|(_, ts)| ts).collect(),
        };
        for t in members {
            // Arrays hold their elements by value; pointers break the cycle.
            let mut t = t;
            while let Type::Array(elem, _) = t {
                t = elem;
            }
            if let Type::Struct(inner) | Type::Enum(inner) = t {
                self.check_acyclic(inner, stack)?;
            }
        }
        stack.pop();
        Ok(())
    }

    fn variant<'a>(&'a self, t: &Type, index: u32) -> Result<&'a [Type], String> {
        let Type::Enum(name) = t else { return Err(format!("{} is not an enum", t.jir())) };
        let def = self.enums.get(name.as_str()).ok_or_else(|| format!("unknown enum `${name}`"))?;
        def.variants
            .get(index as usize)
            .map(|(_, ts)| ts.as_slice())
            .ok_or_else(|| format!("${name} has no variant {index}"))
    }

    fn field<'a>(&'a self, t: &Type, index: u32) -> Result<&'a Type, String> {
        let Type::Struct(name) = t else { return Err(format!("{} is not a struct", t.jir())) };
        let def = self.structs.get(name.as_str()).ok_or_else(|| format!("unknown struct `${name}`"))?;
        def.fields
            .get(index as usize)
            .map(|(_, t)| t)
            .ok_or_else(|| format!("${name} has no field {index}"))
    }

    fn verify_fn(&self, f: &Function) -> Result<(), String> {
        let profile = self.profile;
        if f.blocks.is_empty() {
            return Err("function has no blocks".into());
        }
        if f.regs.len() < f.params.len() || f.regs[..f.params.len()] != f.params[..] {
            return Err("register types must start with the parameter types".into());
        }
        for t in f.regs.iter().chain([&f.ret]) {
            self.check_type(t)?;
        }

        let ty = |r: &Reg| {
            f.regs
                .get(r.0 as usize)
                .ok_or_else(|| format!("register {r} out of range ({} registers)", f.regs.len()))
        };
        let expect = |r: &Reg, want: &Type| -> Result<(), String> {
            let got = ty(r)?;
            if got == want {
                Ok(())
            } else {
                Err(format!("{r} has type {}, expected {}", got.jir(), want.jir()))
            }
        };
        let freestanding = |what: &str| {
            if profile == Profile::Freestanding {
                Ok(())
            } else {
                Err(format!("`{what}` is only available in freestanding mode"))
            }
        };

        for (i, b) in f.blocks.iter().enumerate() {
            let at = |e: String| format!("bb{i}: {e}");
            for inst in &b.insts {
                match inst {
                    Inst::Const { dst, value } => match ty(dst)? {
                        Type::Bool if matches!(value, 0 | 1) => Ok(()),
                        Type::Int(t) if t.wrap(*value) == *value => Ok(()),
                        t => Err(format!("`const {value}` is not a valid {}", t.jir())),
                    },
                    Inst::Unit { dst } => expect(dst, &Type::Unit),
                    Inst::Str { dst, .. } => expect(dst, &types::str_literal(profile)),
                    Inst::Copy { dst, src } => expect(dst, ty(src)?),
                    Inst::Unary { dst, op, src } => match types::unary(*op, ty(src)?) {
                        Some(t) => expect(dst, &t),
                        None => Err(format!("`{}` cannot take {}", op.mnemonic(), ty(src)?.jir())),
                    },
                    Inst::Binary { dst, op, lhs, rhs } => match types::binary(*op, ty(lhs)?, ty(rhs)?) {
                        Some(t) => expect(dst, &t),
                        None => Err(format!(
                            "`{}` cannot take {} and {}",
                            op.mnemonic(),
                            ty(lhs)?.jir(),
                            ty(rhs)?.jir()
                        )),
                    },
                    Inst::Cast { dst, src } => {
                        if types::can_cast(ty(src)?, ty(dst)?) {
                            Ok(())
                        } else {
                            Err(format!("cannot cast {} to {}", ty(src)?.jir(), ty(dst)?.jir()))
                        }
                    }
                    Inst::Call { dst, func, args } => {
                        let callee = self
                            .funcs
                            .get(func.as_str())
                            .ok_or_else(|| format!("call to unknown function @{func}"))?;
                        if callee.params.len() != args.len() {
                            Err(format!("@{func} takes {} arguments, {} given", callee.params.len(), args.len()))
                        } else {
                            args.iter().zip(&callee.params).try_for_each(|(a, t)| expect(a, t))?;
                            expect(dst, &callee.ret)
                        }
                    }
                    Inst::FuncRef { dst, func } => {
                        let callee = self
                            .funcs
                            .get(func.as_str())
                            .ok_or_else(|| format!("funcref to unknown function @{func}"))?;
                        expect(dst, &Type::Fn(callee.params.clone(), Box::new(callee.ret.clone())))
                    }
                    Inst::Closure { dst, func, captures } => {
                        if profile != Profile::Hosted {
                            return Err(at("closures that capture values need the GC; hosted only".into()));
                        }
                        let callee = self
                            .funcs
                            .get(func.as_str())
                            .ok_or_else(|| format!("closure of unknown function @{func}"))?;
                        if callee.params.len() < captures.len() {
                            Err(format!("@{func} takes {} arguments, {} captured", callee.params.len(), captures.len()))
                        } else {
                            let (caps, rest) = callee.params.split_at(captures.len());
                            captures.iter().zip(caps).try_for_each(|(r, t)| expect(r, t))?;
                            expect(dst, &Type::Fn(rest.to_vec(), Box::new(callee.ret.clone())))
                        }
                    }
                    Inst::CallIndirect { dst, callee, args } => {
                        let Type::Fn(params, ret) = ty(callee)? else {
                            return Err(at(format!("{callee} is not a function")));
                        };
                        if params.len() != args.len() {
                            Err(format!("{callee} takes {} arguments, {} given", params.len(), args.len()))
                        } else {
                            args.iter().zip(params).try_for_each(|(a, t)| expect(a, t))?;
                            expect(dst, ret)
                        }
                    }
                    Inst::Struct { dst, name, fields } => {
                        let def = self.structs.get(name.as_str()).ok_or_else(|| format!("unknown struct `${name}`"))?;
                        if def.fields.len() != fields.len() {
                            Err(format!("${name} has {} fields, {} given", def.fields.len(), fields.len()))
                        } else {
                            fields.iter().zip(&def.fields).try_for_each(|(r, (_, t))| expect(r, t))?;
                            expect(dst, &Type::Struct(name.clone()))
                        }
                    }
                    Inst::Field { dst, src, index } => expect(dst, self.field(ty(src)?, *index)?),
                    Inst::Variant { dst, index, fields } => {
                        let payload = self.variant(ty(dst)?, *index)?;
                        if payload.len() != fields.len() {
                            Err(format!("variant {index} holds {} values, {} given", payload.len(), fields.len()))
                        } else {
                            fields.iter().zip(payload).try_for_each(|(r, t)| expect(r, t))
                        }
                    }
                    Inst::Ref { dst, src } => {
                        if profile != Profile::Hosted {
                            Err("`ref` needs the GC and is only available in hosted mode".into())
                        } else {
                            expect(dst, &Type::Ref(Box::new(ty(src)?.clone())))
                        }
                    }
                    // Channel types are hosted only, so checking types checks the profile.
                    Inst::NewChan { dst, cap } => match ty(dst)? {
                        Type::Chan(_) => expect(cap, &Type::I64),
                        t => Err(format!("`chan` cannot produce {}", t.jir())),
                    },
                    Inst::Send { chan, value } => match ty(chan)? {
                        Type::Chan(t) => expect(value, t),
                        t => Err(format!("`send` needs a channel, found {}", t.jir())),
                    },
                    Inst::Recv { dst, chan } => match ty(chan)? {
                        Type::Chan(t) => expect(dst, t),
                        t => Err(format!("`recv` needs a channel, found {}", t.jir())),
                    },
                    Inst::Spawn { callee, args } => {
                        if profile != Profile::Hosted {
                            return Err(at("`spawn` needs the VM's scheduler; hosted only".into()));
                        }
                        let Type::Fn(params, _) = ty(callee)? else {
                            return Err(at(format!("{callee} is not a function")));
                        };
                        if params.len() != args.len() {
                            Err(format!("{callee} takes {} arguments, {} given", params.len(), args.len()))
                        } else {
                            args.iter().zip(params).try_for_each(|(a, t)| expect(a, t))
                        }
                    }
                    Inst::Deref { dst, src } => match ty(src)? {
                        Type::Ref(inner) => expect(dst, inner),
                        t => Err(format!("`deref` needs a ref, found {}", t.jir())),
                    },
                    Inst::Tag { dst, src } => match ty(src)? {
                        Type::Enum(_) => expect(dst, &Type::Int(IntTy::U32)),
                        t => Err(format!("`tag` needs an enum, found {}", t.jir())),
                    },
                    Inst::Payload { dst, src, variant, index } => {
                        let payload = self.variant(ty(src)?, *variant)?;
                        let t = payload
                            .get(*index as usize)
                            .ok_or_else(|| format!("variant {variant} has no value {index}"))?;
                        expect(dst, t)
                    }
                    Inst::SetField { dst, src, index, value } => {
                        expect(value, self.field(ty(src)?, *index)?)?;
                        expect(dst, ty(src)?)
                    }
                    Inst::Load { dst, ptr } => {
                        freestanding("load")?;
                        let pointee = ty(ptr)?.pointee().ok_or_else(|| format!("{ptr} is not a pointer"))?;
                        expect(dst, pointee)
                    }
                    Inst::Store { ptr, value } => {
                        freestanding("store")?;
                        let pointee = ty(ptr)?.pointee().ok_or_else(|| format!("{ptr} is not a pointer"))?;
                        expect(value, pointee)
                    }
                    Inst::Addr { dst, src } => {
                        freestanding("addr")?;
                        expect(dst, &Type::ptr(ty(src)?.clone()))
                    }
                    Inst::FieldPtr { dst, ptr, index } => {
                        freestanding("fieldptr")?;
                        let pointee = ty(ptr)?.pointee().ok_or_else(|| format!("{ptr} is not a pointer"))?;
                        expect(dst, &Type::ptr(self.field(pointee, *index)?.clone()))
                    }
                    Inst::Array { dst, items } => {
                        let Type::Array(elem, n) = ty(dst)? else {
                            return Err(at(format!("{dst} is not an array")));
                        };
                        if *n != items.len() as u64 {
                            Err(format!("{} needs {n} elements, {} given", ty(dst)?.jir(), items.len()))
                        } else {
                            items.iter().try_for_each(|r| expect(r, elem))
                        }
                    }
                    Inst::Splat { dst, value } => match ty(dst)? {
                        Type::Array(elem, _) => expect(value, elem),
                        t => Err(format!("`splat` cannot produce {}", t.jir())),
                    },
                    Inst::Elem { dst, src, index } => match ty(src)? {
                        Type::Array(elem, _) => expect(index, &Type::I64).and(expect(dst, elem)),
                        t => Err(format!("{} is not an array", t.jir())),
                    },
                    Inst::SetElem { dst, src, index, value } => match ty(src)? {
                        Type::Array(elem, _) => {
                            expect(index, &Type::I64)?;
                            expect(value, elem)?;
                            expect(dst, ty(src)?)
                        }
                        t => Err(format!("{} is not an array", t.jir())),
                    },
                    Inst::ElemPtr { dst, ptr, index } => {
                        freestanding("elemptr")?;
                        match ty(ptr)?.pointee() {
                            Some(Type::Array(elem, _)) => {
                                expect(index, &Type::I64)?;
                                expect(dst, &Type::ptr((**elem).clone()))
                            }
                            _ => Err(format!("{ptr} is not a pointer to an array")),
                        }
                    }
                    Inst::Syscall { dst, args } => {
                        freestanding("syscall")?;
                        if args.is_empty() || args.len() > 7 {
                            Err("`syscall` takes 1 to 7 arguments".into())
                        } else {
                            for a in args {
                                if !ty(a)?.is_syscall_arg() {
                                    return Err(at(format!("syscall argument {a} has type {}", ty(a)?.jir())));
                                }
                            }
                            expect(dst, &Type::I64)
                        }
                    }
                    Inst::Asm { dst, constraints, args, .. } => {
                        freestanding("asm")?;
                        // Inputs are the entries that are neither outputs nor clobbers.
                        let inputs = constraints
                            .split(',')
                            .filter(|c| !c.is_empty() && !c.starts_with('=') && !c.starts_with('~'))
                            .count();
                        if inputs != args.len() {
                            return Err(at(format!("asm constraints name {inputs} inputs, {} given", args.len())));
                        }
                        for a in args {
                            if !matches!(ty(a)?, Type::Int(_) | Type::Ptr(_) | Type::Bool) {
                                return Err(at(format!("asm operand {a} has type {}", ty(a)?.jir())));
                            }
                        }
                        let has_out = constraints.split(',').any(|c| c.starts_with('='));
                        match ty(dst)? {
                            Type::Unit if !has_out => Ok(()),
                            Type::Int(_) | Type::Ptr(_) if has_out => Ok(()),
                            t => Err(format!("asm result {dst} has type {}", t.jir())),
                        }
                    }
                    // Macros run on the VM while compiling and never end up in a module.
                    Inst::Quote { .. } => Err("`quote` only exists at compile time".into()),
                    Inst::ToStr { dst, src } => {
                        if !matches!(ty(src)?, Type::Int(_) | Type::Bool) {
                            Err(format!("`to_str` cannot take {}", ty(src)?.jir()))
                        } else {
                            expect(dst, &Type::Str)
                        }
                    }
                    Inst::Unique { .. } => Err("`unique` only exists at compile time".into()),
                    Inst::Print { src } => {
                        if profile != Profile::Hosted {
                            Err("`print` needs std and is not available in freestanding mode".into())
                        } else if !ty(src)?.is_printable() {
                            Err(format!("cannot print {}", ty(src)?.jir()))
                        } else {
                            Ok(())
                        }
                    }
                }
                .map_err(at)?;
            }

            match &b.term {
                Terminator::Branch { cond, .. } => expect(cond, &Type::Bool).map_err(at)?,
                Terminator::Ret(r) => expect(r, &f.ret).map_err(at)?,
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
}
