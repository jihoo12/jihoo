//! JIR types and the typing rules for operators and casts.
//!
//! The rules live here so that the frontend's type checker and the IR verifier
//! can never disagree.

use std::fmt;

use crate::{BinOp, Profile, UnOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl IntTy {
    pub const ALL: [IntTy; 8] =
        [IntTy::I8, IntTy::I16, IntTy::I32, IntTy::I64, IntTy::U8, IntTy::U16, IntTy::U32, IntTy::U64];

    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 => 64,
        }
    }

    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64)
    }

    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
        }
    }

    pub fn min(self) -> i128 {
        if self.signed() {
            -(1i128 << (self.bits() - 1))
        } else {
            0
        }
    }

    pub fn max(self) -> i128 {
        if self.signed() {
            (1i128 << (self.bits() - 1)) - 1
        } else {
            (1i128 << self.bits()) - 1
        }
    }

    /// Wraps an arbitrary 64-bit pattern to this type's canonical i64 form:
    /// sign-extended for signed types, zero-extended for unsigned ones.
    pub fn wrap(self, v: i64) -> i64 {
        let shift = 64 - self.bits();
        if self.signed() {
            (v << shift) >> shift
        } else {
            ((v as u64) << shift >> shift) as i64
        }
    }
}

/// An IEEE 754 binary floating-point type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F32,
    F64,
}

impl FloatTy {
    pub const ALL: [FloatTy; 2] = [FloatTy::F32, FloatTy::F64];

    pub fn bits(self) -> u32 {
        match self {
            FloatTy::F32 => 32,
            FloatTy::F64 => 64,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "f32",
            FloatTy::F64 => "f64",
        }
    }

    /// Rounds `v` to the nearest value of this type. Values are carried as
    /// `f64`s; an `f32` is an `f64` that is exactly representable as `f32`.
    /// Rounding the exact `f64` result of `+ - * /` on two `f32`s gives the
    /// correctly rounded `f32` result, so the VM can compute in `f64`.
    pub fn round(self, v: f64) -> f64 {
        match self {
            FloatTy::F32 => v as f32 as f64,
            FloatTy::F64 => v,
        }
    }
}

/// How JIR text spells a float constant: the shortest decimal that reads back
/// as the same `f64` (so also exact for an `f32`), or `inf`, `-inf`, `nan`.
pub fn float_jir(v: f64) -> String {
    if v.is_nan() {
        "nan".into()
    } else if v.is_infinite() {
        if v > 0.0 { "inf" } else { "-inf" }.into()
    } else {
        // `{:?}` always has a `.` or an exponent, so it never reads as an integer.
        format!("{v:?}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Unit,
    Bool,
    Int(IntTy),
    Float(FloatTy),
    /// GC-managed string. Hosted only.
    Str,
    /// Raw pointer to a `T`. Native and freestanding only.
    Ptr(Box<Type>),
    /// A struct, by name. Structs are values: copying one copies its fields.
    Struct(String),
    /// An enum (sum type), by name. Also a value type.
    Enum(String),
    /// `[T; N]`, a fixed-size array. Also a value type.
    Array(Box<Type>, u64),
    /// `fn(A, B) -> R`: a function value. Natively a code pointer.
    Fn(Vec<Type>, Box<Type>),
    /// `ref T`: an immutable reference to a `T` on the GC heap. Hosted only.
    Ref(Box<Type>),
    /// `chan T`: a channel carrying `T` values between tasks. Hosted only.
    Chan(Box<Type>),
    /// `cell T`: a mutable `T` on the GC heap, shared by everything that holds
    /// the cell. Hosted only.
    Cell(Box<Type>),
    /// Pieces of code inside macros: an expression, statements, or items. They
    /// only exist while compiling; the VM represents them as source text.
    Expr,
    Stmts,
    Items,
}

impl Type {
    pub const I64: Type = Type::Int(IntTy::I64);
    pub const U8: Type = Type::Int(IntTy::U8);
    pub const F64: Type = Type::Float(FloatTy::F64);

    pub fn ptr(to: Type) -> Type {
        Type::Ptr(Box::new(to))
    }

    pub fn from_name(name: &str) -> Option<Type> {
        Some(match name {
            "unit" => Type::Unit,
            "bool" => Type::Bool,
            "str" => Type::Str,
            _ => match IntTy::ALL.iter().find(|t| t.name() == name) {
                Some(t) => Type::Int(*t),
                None => Type::Float(*FloatTy::ALL.iter().find(|t| t.name() == name)?),
            },
        })
    }

    pub fn as_int(&self) -> Option<IntTy> {
        match self {
            Type::Int(t) => Some(*t),
            _ => None,
        }
    }

    pub fn array(elem: Type, len: u64) -> Type {
        Type::Array(Box::new(elem), len)
    }

    pub fn pointee(&self) -> Option<&Type> {
        match self {
            Type::Ptr(t) => Some(t),
            _ => None,
        }
    }

    /// GC types need the hosted runtime; raw pointers are only for compiled code.
    /// Struct fields are checked where the struct is defined.
    pub fn available_in(&self, profile: Profile) -> bool {
        match self {
            Type::Str => profile == Profile::Hosted,
            Type::Ptr(inner) => profile.is_compiled() && inner.available_in(profile),
            Type::Array(elem, _) => elem.available_in(profile),
            Type::Fn(params, ret) => params.iter().chain([&**ret]).all(|t| t.available_in(profile)),
            Type::Ref(inner) | Type::Chan(inner) | Type::Cell(inner) => {
                profile == Profile::Hosted && inner.available_in(profile)
            }
            Type::Expr | Type::Stmts | Type::Items => false,
            Type::Unit | Type::Bool | Type::Int(_) | Type::Float(_) | Type::Struct(_) | Type::Enum(_) => true,
        }
    }

    /// `expr`, `stmts` or `items`.
    pub fn is_code(&self) -> bool {
        matches!(self, Type::Expr | Type::Stmts | Type::Items)
    }

    pub fn is_printable(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Float(_) | Type::Bool | Type::Str)
    }

    pub fn is_syscall_arg(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Ptr(_))
    }

    /// How this type is spelled in JIR text (`*u8`, `$Point`, `$"Pair(i64)"`).
    pub fn jir(&self) -> String {
        match self {
            Type::Ptr(t) => format!("*{}", t.jir()),
            Type::Struct(name) | Type::Enum(name) => format!("${}", struct_name_jir(name)),
            Type::Array(elem, n) => format!("[{n} x {}]", elem.jir()),
            Type::Fn(params, ret) => {
                let params: Vec<String> = params.iter().map(Type::jir).collect();
                format!("fn({}) -> {}", params.join(", "), ret.jir())
            }
            Type::Ref(t) => format!("ref {}", t.jir()),
            Type::Chan(t) => format!("chan {}", t.jir()),
            Type::Cell(t) => format!("cell {}", t.jir()),
            other => other.to_string(),
        }
    }
}

/// Surface spelling, used in error messages (`*u8`, `Point`).
impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Unit => f.write_str("unit"),
            Type::Bool => f.write_str("bool"),
            Type::Int(t) => f.write_str(t.name()),
            Type::Float(t) => f.write_str(t.name()),
            Type::Str => f.write_str("str"),
            Type::Ptr(t) => write!(f, "*{t}"),
            Type::Struct(name) | Type::Enum(name) => f.write_str(name),
            Type::Array(elem, n) => write!(f, "[{elem}; {n}]"),
            Type::Ref(t) => write!(f, "ref {t}"),
            Type::Chan(t) => write!(f, "chan {t}"),
            Type::Cell(t) => write!(f, "cell {t}"),
            Type::Fn(params, ret) => {
                let params: Vec<String> = params.iter().map(Type::to_string).collect();
                write!(f, "fn({})", params.join(", "))?;
                if **ret != Type::Unit {
                    write!(f, " -> {ret}")?;
                }
                Ok(())
            }
            Type::Expr => f.write_str("expr"),
            Type::Stmts => f.write_str("stmts"),
            Type::Items => f.write_str("items"),
        }
    }
}

/// A struct name as JIR spells it: bare if it is an identifier, else quoted (the
/// instances of generic structs are named like `Pair(i64)`).
pub fn struct_name_jir(name: &str) -> String {
    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        return name.to_string();
    }
    let mut out = String::from("\"");
    for c in name.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Whether values of type `t` can be passed to or from C, as a parameter or
/// (with `is_ret`) the result of an `extern fn`: integers, bools, pointers, and
/// functions taking and returning those. Structs, enums and arrays are passed
/// differently by every C ABI, so they cross by pointer.
pub fn c_compatible(t: &Type, is_ret: bool) -> bool {
    match t {
        Type::Unit => is_ret,
        Type::Bool | Type::Int(_) | Type::Float(_) | Type::Ptr(_) => true,
        Type::Fn(params, ret) => params.iter().all(|p| c_compatible(p, false)) && c_compatible(ret, true),
        _ => false,
    }
}

/// Whether `params` are those of C's `main(int argc, char **argv)`, which a
/// native `main` may take: `(i32, **u8)`.
pub fn is_main_args(params: &[Type]) -> bool {
    params == [Type::Int(IntTy::I32), Type::ptr(Type::ptr(Type::U8))]
}

/// Type of a string literal in the given profile.
pub fn str_literal(profile: Profile) -> Type {
    match profile {
        Profile::Hosted => Type::Str,
        Profile::Native | Profile::Freestanding => Type::ptr(Type::U8),
    }
}

pub fn unary(op: UnOp, t: &Type) -> Option<Type> {
    match (op, t) {
        (UnOp::Neg, Type::Int(i)) if i.signed() => Some(t.clone()),
        (UnOp::Neg, Type::Float(_)) => Some(t.clone()),
        (UnOp::Not, Type::Bool) => Some(Type::Bool),
        // On integers, `!` flips every bit.
        (UnOp::Not, Type::Int(_)) => Some(t.clone()),
        _ => None,
    }
}

pub fn binary(op: BinOp, l: &Type, r: &Type) -> Option<Type> {
    use BinOp::*;
    use Type::*;
    Some(match (op, l, r) {
        (Add | Sub | Mul | Div | Rem | And | Or | Xor | Shl | Shr, Int(a), Int(b)) if a == b => l.clone(),
        // IEEE 754: `/` by zero is an infinity or NaN, `%` is C's `fmod`.
        (Add | Sub | Mul | Div | Rem, Float(a), Float(b)) if a == b => l.clone(),
        // `&`, `|` and `^` on bools evaluate both sides, unlike `&&` and `||`.
        (And | Or | Xor, Bool, Bool) => Bool,
        (Add, Str, Str) => Str,
        // Pointer arithmetic counts in elements, like C.
        (Add | Sub, Ptr(_), Int(IntTy::I64)) => l.clone(),
        // Floats compare as IEEE 754 says: NaN is unequal to everything, itself too.
        (Eq | Ne, a, b) if a == b && matches!(a, Bool | Int(_) | Float(_) | Str | Ptr(_)) => Bool,
        (Lt | Le | Gt | Ge, Int(a), Int(b)) if a == b => Bool,
        (Lt | Le | Gt | Ge, Float(a), Float(b)) if a == b => Bool,
        (Lt | Le | Gt | Ge, Ptr(a), Ptr(b)) if a == b => Bool,
        _ => return None,
    })
}

/// Explicit `as` conversions.
pub fn can_cast(from: &Type, to: &Type) -> bool {
    use Type::*;
    match (from, to) {
        _ if from == to => true,
        (Int(_) | Bool, Int(_)) => true,
        // Integers and floats round to the nearest float; floats become integers by
        // dropping the fraction, saturating at the limits, and NaN becomes 0.
        (Int(_) | Float(_), Float(_)) | (Float(_), Int(_)) => true,
        (Ptr(_), Ptr(_)) => true,
        (Ptr(_), Int(IntTy::I64 | IntTy::U64)) | (Int(IntTy::I64 | IntTy::U64), Ptr(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap() {
        assert_eq!(IntTy::U8.wrap(256 + 7), 7);
        assert_eq!(IntTy::U8.wrap(-1), 255);
        assert_eq!(IntTy::I8.wrap(128), -128);
        assert_eq!(IntTy::I16.wrap(-1), -1);
        assert_eq!(IntTy::U64.wrap(-1), -1); // same bit pattern
    }

    #[test]
    fn ranges() {
        assert_eq!((IntTy::I8.min(), IntTy::I8.max()), (-128, 127));
        assert_eq!(IntTy::U64.max(), u64::MAX as i128);
    }
}
