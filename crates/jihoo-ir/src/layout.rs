//! Memory layout of types, for `size_of` / `align_of`.
//!
//! jihoo uses the C layout rules of 64-bit (LP64) targets: every field is placed
//! at the next offset aligned for its type, and a struct's size is rounded up to its
//! largest field alignment. This matches LLVM's layout of non-packed structs on
//! x86_64 and aarch64; the LLVM backend double-checks every struct against the
//! target's data layout (see the `size`/`align` suffix of struct lines in JIR).
//!
//! An enum is laid out like the C `struct { uint32_t tag; union { ... } }`: a
//! `u32` tag, then the payload of the largest variant, where each variant's
//! payload is laid out like a struct of its values.

use crate::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
}

/// Looks up what a named type holds: for a struct, one list with its field
/// types; for an enum, one list per variant with its payload types.
pub type Members<'a> = dyn Fn(&Type) -> Option<Vec<Vec<Type>>> + 'a;

/// Size of the tag of an enum, which comes first.
pub const TAG: Layout = Layout { size: 4, align: 4 };

/// Layout of `t`, or `None` for types without a fixed layout (`str`, which is a GC
/// reference, and anything containing it). The struct and enum types reachable
/// from `t` must not contain themselves.
pub fn of(t: &Type, members: &Members<'_>) -> Option<Layout> {
    Some(match t {
        Type::Unit => Layout { size: 0, align: 1 },
        Type::Bool => Layout { size: 1, align: 1 },
        Type::Int(i) => {
            let bytes = u64::from(i.bits() / 8);
            Layout { size: bytes, align: bytes }
        }
        Type::Ptr(_) | Type::Fn(..) => Layout { size: 8, align: 8 },
        Type::Str | Type::Ref(_) | Type::Chan(_) | Type::Cell(_) | Type::Expr | Type::Stmts | Type::Items => return None,
        Type::Array(elem, n) => {
            let e = of(elem, members)?;
            Layout { size: e.size.checked_mul(*n)?, align: e.align }
        }
        Type::Struct(_) => record(&members(t)?.into_iter().next()?, members)?,
        Type::Enum(_) => {
            let payload = payload(&members(t)?, members)?;
            let align = TAG.align.max(payload.align);
            let size = TAG.size.next_multiple_of(payload.align) + payload.size;
            Layout { size: size.next_multiple_of(align), align }
        }
    })
}

/// Layout of a C struct with fields of the given types.
pub fn record(fields: &[Type], members: &Members<'_>) -> Option<Layout> {
    let mut size = 0u64;
    let mut align = 1u64;
    for ft in fields {
        let f = of(ft, members)?;
        size = size.next_multiple_of(f.align) + f.size;
        align = align.max(f.align);
    }
    Some(Layout { size: size.next_multiple_of(align), align })
}

/// Layout of the payload area of an enum with these variants: large and aligned
/// enough for every variant's payload.
pub fn payload(variants: &[Vec<Type>], members: &Members<'_>) -> Option<Layout> {
    let mut l = Layout { size: 0, align: 1 };
    for v in variants {
        let r = record(v, members)?;
        l = Layout { size: l.size.max(r.size), align: l.align.max(r.align) };
    }
    Some(Layout { size: l.size.next_multiple_of(l.align), align: l.align })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntTy;

    #[test]
    fn c_layout() {
        let defs: Vec<(String, Vec<(String, Type)>)> = vec![
            ("A".into(), vec![("a".into(), Type::U8), ("b".into(), Type::I64), ("c".into(), Type::Bool)]),
            ("B".into(), vec![("x".into(), Type::Int(IntTy::U16)), ("a".into(), Type::Struct("A".into()))]),
            ("E".into(), vec![]),
        ];
        let fields = |t: &Type| match t {
            Type::Struct(n) => {
                defs.iter().find(|(d, _)| d == n).map(|(_, f)| vec![f.iter().map(|(_, t)| t.clone()).collect()])
            }
            // enum Shape { Dot, Line(u8, u8), Rect(i64, u8) }
            Type::Enum(n) if n == "Shape" => {
                Some(vec![vec![], vec![Type::U8, Type::U8], vec![Type::I64, Type::U8]])
            }
            // enum Flag { Off, On(u8) }
            Type::Enum(_) => Some(vec![vec![], vec![Type::U8]]),
            _ => None,
        };
        let l = |t: Type| of(&t, &fields).unwrap();

        assert_eq!(l(Type::Struct("A".into())), Layout { size: 24, align: 8 });
        assert_eq!(l(Type::Struct("B".into())), Layout { size: 32, align: 8 });
        assert_eq!(l(Type::Struct("E".into())), Layout { size: 0, align: 1 });
        assert_eq!(l(Type::array(Type::Int(IntTy::U16), 3)), Layout { size: 6, align: 2 });
        assert_eq!(l(Type::array(Type::Struct("A".into()), 2)), Layout { size: 48, align: 8 });
        assert_eq!(of(&Type::Str, &fields), None);
        // Tag at 0, payload at 8 (aligned for i64), 16 bytes of payload.
        assert_eq!(l(Type::Enum("Shape".into())), Layout { size: 24, align: 8 });
        // Tag at 0, payload at 4.
        assert_eq!(l(Type::Enum("Flag".into())), Layout { size: 8, align: 4 });
    }
}
