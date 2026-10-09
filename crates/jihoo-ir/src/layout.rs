//! Memory layout of types, for `size_of` / `align_of`.
//!
//! jihoo uses the C layout rules of 64-bit (LP64) targets: every field is placed
//! at the next offset aligned for its type, and a struct's size is rounded up to its
//! largest field alignment. This matches LLVM's layout of non-packed structs on
//! x86_64 and aarch64; the LLVM backend double-checks every struct against the
//! target's data layout (see the `size`/`align` suffix of struct lines in JIR).

use crate::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
}

/// Looks up the field types of a struct by name.
pub type Fields<'a> = dyn Fn(&str) -> Option<Vec<Type>> + 'a;

/// Layout of `t`, or `None` for types without a fixed layout (`str`, which is a GC
/// reference, and anything containing it). The struct types reachable from `t`
/// must not contain themselves.
pub fn of(t: &Type, fields: &Fields<'_>) -> Option<Layout> {
    Some(match t {
        Type::Unit => Layout { size: 0, align: 1 },
        Type::Bool => Layout { size: 1, align: 1 },
        Type::Int(i) => {
            let bytes = u64::from(i.bits() / 8);
            Layout { size: bytes, align: bytes }
        }
        Type::Ptr(_) | Type::Fn(..) => Layout { size: 8, align: 8 },
        Type::Str | Type::Expr | Type::Stmts | Type::Items => return None,
        Type::Array(elem, n) => {
            let e = of(elem, fields)?;
            Layout { size: e.size.checked_mul(*n)?, align: e.align }
        }
        Type::Struct(name) => {
            let mut size = 0u64;
            let mut align = 1u64;
            for ft in fields(name)? {
                let f = of(&ft, fields)?;
                size = size.next_multiple_of(f.align) + f.size;
                align = align.max(f.align);
            }
            Layout { size: size.next_multiple_of(align), align }
        }
    })
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
        let fields = |n: &str| {
            defs.iter().find(|(d, _)| d == n).map(|(_, f)| f.iter().map(|(_, t)| t.clone()).collect())
        };
        let l = |t: Type| of(&t, &fields).unwrap();

        assert_eq!(l(Type::Struct("A".into())), Layout { size: 24, align: 8 });
        assert_eq!(l(Type::Struct("B".into())), Layout { size: 32, align: 8 });
        assert_eq!(l(Type::Struct("E".into())), Layout { size: 0, align: 1 });
        assert_eq!(l(Type::array(Type::Int(IntTy::U16), 3)), Layout { size: 6, align: 2 });
        assert_eq!(l(Type::array(Type::Struct("A".into()), 2)), Layout { size: 48, align: 8 });
        assert_eq!(of(&Type::Str, &fields), None);
    }
}
