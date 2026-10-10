//! Patterns and `match`, as a statement and as an expression.
//!
//! A pattern is first checked against the type it matches, which resolves its
//! names: a name is a variant of the matched enum if it has one by that name,
//! else a new variable; a name starting with an uppercase letter must be a
//! variant, so a misspelled variant is an error rather than a binding.
//!
//! The arms are then checked with the usefulness algorithm of Maranget
//! ("Warnings for pattern matching", 2007): an arm that matches no value the
//! arms above it miss is an error, and so is a `match` that misses a value,
//! which the error spells out as a pattern (`Some(Rect(_, _))`). Arms with a
//! guard do not count towards covering values.
//!
//! Lowering tests the arms in order. Each arm's tests run on the matched value
//! and branch to the next arm on failure; payloads and fields are only read
//! once the tag they belong to has been checked. Since every value is covered,
//! falling off the last arm is unreachable.

use std::collections::HashMap;

use jihoo_ir::{BinOp, BlockId, Inst, IntTy, Reg, Terminator, Type};
use jihoo_syntax::ast::*;
use jihoo_syntax::{Error, Pos};

use crate::FnCx;

/// A pattern checked against its type.
#[derive(Debug, Clone)]
pub(crate) enum Pat {
    /// Matches anything: `_`, or a name bound to the value.
    Any(Option<String>),
    /// A constructor, and patterns for its parts.
    Ctor(Ctor, Vec<Pat>),
    /// Any of the alternatives, which bind the same names.
    Or(Vec<Pat>),
}

/// A way to build a value; the parts are what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctor {
    /// Variant `i` of an enum; the parts are its payload.
    Variant(u32),
    /// A struct; the parts are its fields, in declaration order.
    Struct,
    Bool(bool),
    /// An integer literal, in canonical form; no parts. Integers have too many
    /// values to list, so only `_` covers them.
    Int(i64),
    /// A `ref`, looked through; the one part is the value it refers to.
    Ref,
}

const ANY: Pat = Pat::Any(None);

/// An arm, for [`FnCx::lower_match`]: where it is, its pattern and its guard.
struct Arm<'a> {
    pos: Pos,
    pattern: &'a Pattern,
    guard: Option<&'a Expr>,
}

impl FnCx<'_> {
    pub(crate) fn match_stmt(&mut self, pos: Pos, value: &Expr, arms: &[MatchArm]) -> Result<(), Error> {
        let heads: Vec<Arm> =
            arms.iter().map(|a| Arm { pos: a.pos, pattern: &a.pattern, guard: a.guard.as_ref() }).collect();
        self.lower_match(pos, value, &heads, &mut |cx, i| cx.block(&arms[i].body))
    }

    /// A `match` expression: every arm gives a value of the same type.
    pub(crate) fn match_expr(
        &mut self,
        pos: Pos,
        value: &Expr,
        arms: &[MatchExprArm],
        expected: Option<&Type>,
    ) -> Result<Reg, Error> {
        if arms.is_empty() {
            return Err(Error::new(pos, "a `match` expression needs at least one arm"));
        }
        let heads: Vec<Arm> =
            arms.iter().map(|a| Arm { pos: a.pos, pattern: &a.pattern, guard: a.guard.as_ref() }).collect();
        let mut result: Option<Reg> = None;
        self.lower_match(pos, value, &heads, &mut |cx, i| {
            let arm = &arms[i];
            // The first arm decides the type, unless the context does.
            let want = result.map(|r| cx.ty(r).clone()).or_else(|| expected.cloned());
            let v = cx.expr(&arm.value, want.as_ref())?;
            let dst = match result {
                Some(r) => {
                    let t = cx.ty(r).clone();
                    cx.expect(arm.value.pos, v, &t, "the value of this arm (like the arms before it)")?;
                    r
                }
                None => {
                    let r = cx.new_reg(cx.ty(v).clone());
                    result = Some(r);
                    r
                }
            };
            cx.emit(Inst::Copy { dst, src: v });
            Ok(())
        })?;
        Ok(result.unwrap())
    }

    /// Checks the arms, then tests them in order against `value`; `body` lowers
    /// arm `i` once its pattern and guard have matched, with its names bound.
    fn lower_match(
        &mut self,
        pos: Pos,
        value: &Expr,
        arms: &[Arm],
        body: &mut dyn FnMut(&mut Self, usize) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let v = self.expr(value, None)?;
        let ty = self.ty(v).clone();
        if !matches!(ty, Type::Enum(_) | Type::Struct(_) | Type::Int(_) | Type::Bool) {
            let msg = format!("cannot `match` on {ty}; `match` works on enums, structs, integers and bools");
            return Err(Error::new(value.pos, msg));
        }

        // Check every arm, and that it can match something the arms above miss.
        let mut pats = Vec::with_capacity(arms.len());
        let mut covered: Vec<Vec<Pat>> = Vec::new();
        for arm in arms {
            let p = self.check_pattern(arm.pattern, &ty, &mut Vec::new())?;
            if self.useful(&covered, std::slice::from_ref(&p), std::slice::from_ref(&ty)).is_none() {
                let msg = if covered.iter().any(|r| matches!(r[0], Pat::Any(_))) {
                    "unreachable arm: an arm above matches every value (`_` or a name)"
                } else {
                    "unreachable arm: the arms above already match every value this one does"
                };
                return Err(Error::new(arm.pos, msg));
            }
            // So must each alternative of `p | q`, given the ones before it.
            if let (Pat::Or(alts), PatternKind::Or(written)) = (&p, &arm.pattern.kind) {
                let mut seen = covered.clone();
                for (alt, w) in alts.iter().zip(written) {
                    if self.useful(&seen, std::slice::from_ref(alt), std::slice::from_ref(&ty)).is_none() {
                        let msg = format!("unreachable alternative: `{}` is already matched above", self.show(alt, &ty));
                        return Err(Error::new(w.pos, msg));
                    }
                    seen.push(vec![alt.clone()]);
                }
            }
            if arm.guard.is_none() {
                covered.push(vec![p.clone()]);
            }
            pats.push(p);
        }
        let missing = self.missing(&covered, &ty);
        if !missing.is_empty() {
            let msg = if matches!(ty, Type::Int(_)) {
                "`match` does not cover every other integer; add a `_ => ...` arm".to_string()
            } else {
                let shown: Vec<String> = missing.iter().map(|p| format!("`{}`", self.show(p, &ty))).collect();
                format!("`match` does not cover {}; add arms for them, or a `_ => ...` arm", shown.join(", "))
            };
            return Err(Error::new(pos, msg));
        }

        let end = self.new_block();
        for (i, arm) in arms.iter().enumerate() {
            let next = self.new_block();
            let mut binds = Vec::new();
            self.test(&pats[i], &ty, v, next, &mut binds);
            self.scopes.push(binds.into_iter().collect::<HashMap<_, _>>());
            let r = (|| {
                if let Some(g) = arm.guard {
                    let c = self.expr(g, Some(&Type::Bool))?;
                    self.expect(g.pos, c, &Type::Bool, "a guard")?;
                    self.branch(c, next);
                }
                body(self, i)
            })();
            self.scopes.pop();
            r?;
            self.terminate(Terminator::Jump(end));
            self.switch_to(next);
        }
        self.terminate(Terminator::Unreachable);
        self.switch_to(end);
        Ok(())
    }

    /// Goes on in a new block if `cond` holds, else to `fail`.
    fn branch(&mut self, cond: Reg, fail: BlockId) {
        let ok = self.new_block();
        self.terminate(Terminator::Branch { cond, then: ok, els: fail });
        self.switch_to(ok);
    }

    // ---- checking ----

    /// Checks `p` against type `t`. `names` are the names bound so far in the
    /// whole pattern, which must differ, with their types.
    fn check_pattern(&self, p: &Pattern, t: &Type, names: &mut Vec<(String, Type)>) -> Result<Pat, Error> {
        let err = |msg: String| Err(Error::new(p.pos, msg));
        // A pattern for what a ref refers to reads through the ref; `_` and a
        // name that binds take the ref itself.
        if let Type::Ref(inner) = t {
            let through = match &p.kind {
                PatternKind::Wild => false,
                PatternKind::Name(n) => {
                    self.variant_index(inner, n)?.is_some() || n.starts_with(|c: char| c.is_ascii_uppercase())
                }
                _ => true,
            };
            if through {
                return Ok(Pat::Ctor(Ctor::Ref, vec![self.check_pattern(p, inner, names)?]));
            }
        }
        Ok(match &p.kind {
            PatternKind::Wild => ANY,
            PatternKind::Name(n) => {
                if let Some(i) = self.variant_index(t, n)? {
                    let payload = self.env.payload_types(t, i);
                    if !payload.is_empty() {
                        let ignore = vec!["_"; payload.len()].join(", ");
                        return err(format!("`{n}` holds {} values; write `{n}({ignore})` to ignore them", payload.len()));
                    }
                    return Ok(Pat::Ctor(Ctor::Variant(i), vec![]));
                }
                if n.starts_with(|c: char| c.is_ascii_uppercase()) {
                    return match t {
                        Type::Enum(_) => err(self.no_variant(t, n)?),
                        _ => err(format!("`{n}` looks like a variant, but the value is {t}, not an enum")),
                    };
                }
                if names.iter().any(|(m, _)| m == n) {
                    return err(format!("`{n}` is bound twice in this pattern"));
                }
                names.push((n.clone(), t.clone()));
                Pat::Any(Some(n.clone()))
            }
            PatternKind::Or(alts) => {
                // Every alternative must bind the same names, with the same types.
                let before = names.len();
                let mut bound: Option<Vec<(String, Type)>> = None;
                let mut pats = Vec::with_capacity(alts.len());
                for a in alts {
                    let mut these = names.clone();
                    pats.push(self.check_pattern(a, t, &mut these)?);
                    let mut new = these.split_off(before);
                    new.sort_by(|x, y| x.0.cmp(&y.0));
                    match &bound {
                        None => bound = Some(new),
                        Some(first) if *first != new => {
                            let only = first.iter().chain(&new).find(|(n, _)| {
                                first.iter().all(|(m, _)| m != n) || new.iter().all(|(m, _)| m != n)
                            });
                            let msg = match only {
                                Some((n, _)) => format!("`{n}` is bound in some alternatives of this `|` pattern but not in others"),
                                None => {
                                    let ((n, a), (_, b)) = first.iter().zip(&new).find(|(x, y)| x.1 != y.1).unwrap();
                                    format!("`{n}` is {a} in one alternative of this `|` pattern and {b} in another")
                                }
                            };
                            return Err(Error::new(a.pos, msg));
                        }
                        Some(_) => {}
                    }
                }
                names.extend(bound.unwrap_or_default());
                Pat::Or(pats)
            }
            PatternKind::Variant(n, args) => {
                if !matches!(t, Type::Enum(_)) {
                    return err(format!("`{n}(...)` is a variant pattern, but the value is {t}, not an enum"));
                }
                let Some(i) = self.variant_index(t, n)? else { return err(self.no_variant(t, n)?) };
                let payload = self.env.payload_types(t, i);
                if payload.is_empty() {
                    return err(format!("`{n}` holds no values; drop the parentheses"));
                }
                if payload.len() != args.len() {
                    return err(format!("`{n}` holds {} values, {} given", payload.len(), args.len()));
                }
                let parts = args.iter().zip(&payload).map(|(a, pt)| self.check_pattern(a, pt, names));
                Pat::Ctor(Ctor::Variant(i), parts.collect::<Result<_, _>>()?)
            }
            PatternKind::Struct(n, fields, rest) => {
                let Type::Struct(s) = t else {
                    return err(format!("`{n} {{ ... }}` is a struct pattern, but the value is {t}"));
                };
                // `Pair` matches any instance `Pair(...)`; `geo.Point` and `Point` match `geo.Point`.
                let base = s.split('(').next().unwrap_or(s);
                let short = |x: &str| x.rsplit('.').next().unwrap_or(x).to_string();
                let written = crate::hygiene::qualified(n).map_or(n.as_str(), |(_, name)| name);
                if short(base) != short(written) {
                    return err(format!("this pattern matches `{n}`, but the value is {t}"));
                }
                let decl = self.env.struct_fields(p.pos, s)?;
                let mut parts: Vec<Option<Pat>> = vec![None; decl.len()];
                for (fpos, f, fp) in fields {
                    let Some(i) = decl.iter().position(|(name, _)| name == f) else {
                        return Err(Error::new(*fpos, format!("struct `{s}` has no field `{f}`")));
                    };
                    if parts[i].is_some() {
                        return Err(Error::new(*fpos, format!("field `{f}` appears twice in this pattern")));
                    }
                    parts[i] = Some(self.check_pattern(fp, &decl[i].1, names)?);
                }
                let missing: Vec<&str> =
                    decl.iter().zip(&parts).filter(|(_, p)| p.is_none()).map(|((n, _), _)| n.as_str()).collect();
                if !rest && !missing.is_empty() {
                    return err(format!("missing fields in this pattern: {} (add `..` to ignore them)", missing.join(", ")));
                }
                Pat::Ctor(Ctor::Struct, parts.into_iter().map(|p| p.unwrap_or(ANY)).collect())
            }
            PatternKind::Int(n) => {
                let Type::Int(it) = t else { return err(format!("an integer pattern does not match values of type {t}")) };
                if *n < it.min() || *n > it.max() {
                    return err(format!("{n} does not fit in {t}"));
                }
                Pat::Ctor(Ctor::Int(it.wrap(*n as i64)), vec![])
            }
            PatternKind::Bool(b) => {
                if *t != Type::Bool {
                    return err(format!("a bool pattern does not match values of type {t}"));
                }
                Pat::Ctor(Ctor::Bool(*b), vec![])
            }
        })
    }

    /// The index of variant `name` of `t`, if `t` is an enum that has one.
    fn variant_index(&self, t: &Type, name: &str) -> Result<Option<u32>, Error> {
        let Type::Enum(e) = t else { return Ok(None) };
        let variants = self.env.enum_variants(Pos::default(), e)?;
        Ok(variants.iter().position(|(n, _)| n == name).map(|i| i as u32))
    }

    fn no_variant(&self, t: &Type, name: &str) -> Result<String, Error> {
        let Type::Enum(e) = t else { unreachable!() };
        let variants = self.env.enum_variants(Pos::default(), e)?;
        let names: Vec<&str> = variants.iter().map(|(n, _)| n.as_str()).collect();
        Ok(format!("enum `{t}` has no variant `{name}` (it has {})", names.join(", ")))
    }

    // ---- coverage ----

    /// Every constructor of `t`, if there are few enough to list.
    fn ctors(&self, t: &Type) -> Option<Vec<Ctor>> {
        match t {
            Type::Enum(e) => {
                let n = self.env.enum_variants(Pos::default(), e).ok()?.len();
                Some((0..n as u32).map(Ctor::Variant).collect())
            }
            Type::Struct(_) => Some(vec![Ctor::Struct]),
            Type::Ref(_) => Some(vec![Ctor::Ref]),
            Type::Bool => Some(vec![Ctor::Bool(false), Ctor::Bool(true)]),
            _ => None,
        }
    }

    /// The types of the parts of a value of type `t` built with `c`.
    fn parts(&self, t: &Type, c: Ctor) -> Vec<Type> {
        match (c, t) {
            (Ctor::Variant(i), _) => self.env.payload_types(t, i),
            (Ctor::Struct, Type::Struct(s)) => {
                self.env.struct_fields(Pos::default(), s).map(|f| f.iter().map(|(_, t)| t.clone()).collect()).unwrap_or_default()
            }
            (Ctor::Ref, Type::Ref(inner)) => vec![(**inner).clone()],
            _ => vec![],
        }
    }

    /// Whether a value matching all of `q` (one pattern per column, of types
    /// `tys`) can fail to match every row. If so, returns such a value, as
    /// patterns.
    fn useful(&self, rows: &[Vec<Pat>], q: &[Pat], tys: &[Type]) -> Option<Vec<Pat>> {
        let Some(head) = q.first() else {
            return rows.is_empty().then(Vec::new);
        };
        // `p | q` is useful if one of its alternatives is.
        if let Pat::Or(alts) = head {
            return alts.iter().find_map(|a| {
                let q: Vec<Pat> = std::iter::once(a.clone()).chain(q[1..].iter().cloned()).collect();
                self.useful(rows, &q, tys)
            });
        }
        let rows = &expand(rows);
        if let Pat::Ctor(c, _) = head {
            return self.useful_ctor(rows, q, tys, *c);
        }
        let mut used: Vec<Ctor> = Vec::new();
        for r in rows {
            if let Pat::Ctor(c, _) = &r[0] {
                if !used.contains(c) {
                    used.push(*c);
                }
            }
        }
        match self.ctors(&tys[0]) {
            // Every constructor appears: a value is missed if one with some
            // constructor is.
            Some(all) if all.iter().all(|c| used.contains(c)) => {
                all.into_iter().find_map(|c| self.useful_ctor(rows, q, tys, c))
            }
            // Some constructor never appears (or there are too many to list):
            // a value built with it is only matched by rows that match anything here.
            all => {
                let rest: Vec<Vec<Pat>> =
                    rows.iter().filter(|r| matches!(r[0], Pat::Any(_))).map(|r| r[1..].to_vec()).collect();
                let w = self.useful(&rest, &q[1..], &tys[1..])?;
                let head = match all.and_then(|all| all.into_iter().find(|c| !used.contains(c))) {
                    Some(c) => Pat::Ctor(c, vec![ANY; self.parts(&tys[0], c).len()]),
                    // An integer no arm lists, so the message names a real value.
                    None => match &tys[0] {
                        Type::Int(it) if !used.is_empty() => {
                            let n = (0i128..)
                                .flat_map(|n| [n, -n])
                                .find(|n| *n >= it.min() && *n <= it.max() && !used.contains(&Ctor::Int(*n as i64)))
                                .unwrap();
                            Pat::Ctor(Ctor::Int(n as i64), vec![])
                        }
                        _ => ANY,
                    },
                };
                Some(std::iter::once(head).chain(w).collect())
            }
        }
    }

    /// [`Self::useful`] for values built with constructor `c` in the first column.
    fn useful_ctor(&self, rows: &[Vec<Pat>], q: &[Pat], tys: &[Type], c: Ctor) -> Option<Vec<Pat>> {
        let parts = self.parts(&tys[0], c);
        let n = parts.len();
        let rows: Vec<Vec<Pat>> = expand(rows).iter().filter_map(|r| specialize(r, c, n)).collect();
        let q = specialize(q, c, n)?;
        let tys: Vec<Type> = parts.into_iter().chain(tys[1..].iter().cloned()).collect();
        let mut w = self.useful(&rows, &q, &tys)?;
        let rest = w.split_off(n);
        Some(std::iter::once(Pat::Ctor(c, w)).chain(rest).collect())
    }

    /// Values of type `t` that no row matches: one for each constructor that has
    /// some, so the error can list them.
    fn missing(&self, rows: &[Vec<Pat>], t: &Type) -> Vec<Pat> {
        let tys = std::slice::from_ref(t);
        match self.ctors(t) {
            Some(all) => all.into_iter().filter_map(|c| self.useful_ctor(rows, &[ANY], tys, c)).map(|mut w| w.remove(0)).collect(),
            None => self.useful(rows, &[ANY], tys).map(|mut w| w.remove(0)).into_iter().collect(),
        }
    }

    /// `p` as it would be written, for messages.
    fn show(&self, p: &Pat, t: &Type) -> String {
        match p {
            Pat::Any(_) => "_".into(),
            Pat::Or(alts) => alts.iter().map(|a| self.show(a, t)).collect::<Vec<_>>().join(" | "),
            Pat::Ctor(Ctor::Bool(b), _) => b.to_string(),
            Pat::Ctor(Ctor::Int(n), _) => n.to_string(),
            Pat::Ctor(Ctor::Ref, args) => {
                let Type::Ref(inner) = t else { unreachable!() };
                self.show(&args[0], inner)
            }
            Pat::Ctor(c @ Ctor::Variant(i), args) => {
                let Type::Enum(e) = t else { unreachable!() };
                let name = self.env.enum_variants(Pos::default(), e).map(|v| v[*i as usize].0.clone()).unwrap_or_default();
                if args.is_empty() {
                    return name;
                }
                let tys = self.parts(t, *c);
                let args: Vec<String> = args.iter().zip(&tys).map(|(a, t)| self.show(a, t)).collect();
                format!("{name}({})", args.join(", "))
            }
            // A struct has one constructor: with no field narrowed, it is any value.
            Pat::Ctor(Ctor::Struct, args) if args.iter().all(|a| matches!(a, Pat::Any(_))) => "_".into(),
            Pat::Ctor(Ctor::Struct, args) => {
                let Type::Struct(s) = t else { unreachable!() };
                let fields = self.env.struct_fields(Pos::default(), s).unwrap_or_default();
                let shown: Vec<String> = fields.iter().zip(args).map(|((n, ft), a)| format!("{n}: {}", self.show(a, ft))).collect();
                format!("{s} {{ {} }}", shown.join(", "))
            }
        }
    }

    // ---- lowering ----

    /// Emits the tests of `p` against value `v` of type `t`: control goes on in
    /// the current block if they pass, and to `fail` if not. Names it binds are
    /// added to `binds`, each to a copy of its value.
    fn test(&mut self, p: &Pat, t: &Type, v: Reg, fail: BlockId, binds: &mut Vec<(String, Reg)>) {
        match p {
            Pat::Any(None) => {}
            Pat::Any(Some(n)) => {
                let r = self.emit_to(t.clone(), |dst| Inst::Copy { dst, src: v });
                binds.push((n.clone(), r));
            }
            // Try each alternative in turn; the names they bind go to registers
            // they share, made for the first one.
            Pat::Or(alts) => {
                let join = self.new_block();
                let mut shared: Vec<(String, Reg)> = Vec::new();
                for (k, alt) in alts.iter().enumerate() {
                    let last = k + 1 == alts.len();
                    let next = if last { fail } else { self.new_block() };
                    let mut these = Vec::new();
                    self.test(alt, t, v, next, &mut these);
                    if k == 0 {
                        shared = these.iter().map(|(n, r)| (n.clone(), self.new_reg(self.ty(*r).clone()))).collect();
                    }
                    for (n, r) in these {
                        let dst = shared.iter().find(|(m, _)| *m == n).unwrap().1;
                        self.emit(Inst::Copy { dst, src: r });
                    }
                    self.terminate(Terminator::Jump(join));
                    if !last {
                        self.switch_to(next);
                    }
                }
                self.switch_to(join);
                binds.extend(shared);
            }
            Pat::Ctor(c, args) => {
                let k = match c {
                    Ctor::Bool(b) => Some(self.konst(Type::Bool, *b as i64)),
                    Ctor::Int(n) => Some(self.konst(t.clone(), *n)),
                    Ctor::Variant(i) => Some(self.konst(Type::Int(IntTy::U32), *i as i64)),
                    Ctor::Struct | Ctor::Ref => None,
                };
                if let Some(k) = k {
                    let subject = match c {
                        Ctor::Variant(_) => self.emit_to(Type::Int(IntTy::U32), |dst| Inst::Tag { dst, src: v }),
                        _ => v,
                    };
                    let cond = self.emit_to(Type::Bool, |dst| Inst::Binary { dst, op: BinOp::Eq, lhs: subject, rhs: k });
                    self.branch(cond, fail);
                }
                let tys = self.parts(t, *c);
                for (index, (a, at)) in args.iter().zip(&tys).enumerate() {
                    if matches!(a, Pat::Any(None)) {
                        continue;
                    }
                    let index = index as u32;
                    let part = match c {
                        Ctor::Variant(variant) => {
                            self.emit_to(at.clone(), |dst| Inst::Payload { dst, src: v, variant: *variant, index })
                        }
                        Ctor::Ref => self.emit_to(at.clone(), |dst| Inst::Deref { dst, src: v }),
                        _ => self.emit_to(at.clone(), |dst| Inst::Field { dst, src: v, index }),
                    };
                    self.test(a, at, part, fail, binds);
                }
            }
        }
    }
}

/// The rows, with every row whose first pattern is `p | q | ...` replaced by a
/// row for each alternative.
fn expand(rows: &[Vec<Pat>]) -> Vec<Vec<Pat>> {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        match &r[0] {
            Pat::Or(alts) => {
                let alt_rows: Vec<Vec<Pat>> =
                    alts.iter().map(|a| std::iter::once(a.clone()).chain(r[1..].iter().cloned()).collect()).collect();
                out.extend(expand(&alt_rows));
            }
            _ => out.push(r.clone()),
        }
    }
    out
}

/// Row `row` for values built with `c` (which has `n` parts) in the first
/// column: the parts replace it. `None` if the row cannot match such a value.
/// The row must not start with `|` (see [`expand`]).
fn specialize(row: &[Pat], c: Ctor, n: usize) -> Option<Vec<Pat>> {
    let parts = match &row[0] {
        Pat::Ctor(d, parts) if *d == c => parts.clone(),
        Pat::Ctor(..) => return None,
        Pat::Any(_) => vec![ANY; n],
        Pat::Or(_) => unreachable!("rows are expanded first"),
    };
    Some(parts.into_iter().chain(row[1..].iter().cloned()).collect())
}
