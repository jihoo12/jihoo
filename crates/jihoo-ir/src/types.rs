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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Unit,
    Bool,
    Int(IntTy),
    /// GC-managed string. Hosted only.
    Str,
    /// Raw pointer to a `T`. Freestanding only.
    Ptr(Box<Type>),
    /// A struct, by name. Structs are values: copying one copies its fields.
    Struct(String),
    /// `[T; N]`, a fixed-size array. Also a value type.
    Array(Box<Type>, u64),
    /// A piece of code (an expression), inside macros. Only exists while
    /// compiling: the VM represents it as the expression's source text.
    Expr,
}

impl Type {
    pub const I64: Type = Type::Int(IntTy::I64);
    pub const U8: Type = Type::Int(IntTy::U8);

    pub fn ptr(to: Type) -> Type {
        Type::Ptr(Box::new(to))
    }

    pub fn from_name(name: &str) -> Option<Type> {
        Some(match name {
            "unit" => Type::Unit,
            "bool" => Type::Bool,
            "str" => Type::Str,
            _ => Type::Int(*IntTy::ALL.iter().find(|t| t.name() == name)?),
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

    /// GC types need the hosted runtime; raw pointers are only for freestanding code.
    /// Struct fields are checked where the struct is defined.
    pub fn available_in(&self, profile: Profile) -> bool {
        match self {
            Type::Str => profile == Profile::Hosted,
            Type::Ptr(inner) => profile == Profile::Freestanding && inner.available_in(profile),
            Type::Array(elem, _) => elem.available_in(profile),
            Type::Expr => false,
            Type::Unit | Type::Bool | Type::Int(_) | Type::Struct(_) => true,
        }
    }

    pub fn is_printable(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Bool | Type::Str)
    }

    pub fn is_syscall_arg(&self) -> bool {
        matches!(self, Type::Int(_) | Type::Ptr(_))
    }

    /// How this type is spelled in JIR text (`*u8`, `$Point`, `$"Pair(i64)"`).
    pub fn jir(&self) -> String {
        match self {
            Type::Ptr(t) => format!("*{}", t.jir()),
            Type::Struct(name) => format!("${}", struct_name_jir(name)),
            Type::Array(elem, n) => format!("[{n} x {}]", elem.jir()),
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
            Type::Str => f.write_str("str"),
            Type::Ptr(t) => write!(f, "*{t}"),
            Type::Struct(name) => f.write_str(name),
            Type::Array(elem, n) => write!(f, "[{elem}; {n}]"),
            Type::Expr => f.write_str("expr"),
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

/// Type of a string literal in the given profile.
pub fn str_literal(profile: Profile) -> Type {
    match profile {
        Profile::Hosted => Type::Str,
        Profile::Freestanding => Type::ptr(Type::U8),
    }
}

pub fn unary(op: UnOp, t: &Type) -> Option<Type> {
    match (op, t) {
        (UnOp::Neg, Type::Int(i)) if i.signed() => Some(t.clone()),
        (UnOp::Not, Type::Bool) => Some(Type::Bool),
        _ => None,
    }
}

pub fn binary(op: BinOp, l: &Type, r: &Type) -> Option<Type> {
    use BinOp::*;
    use Type::*;
    Some(match (op, l, r) {
        (Add | Sub | Mul | Div | Rem, Int(a), Int(b)) if a == b => l.clone(),
        (Add, Str, Str) => Str,
        // Pointer arithmetic counts in elements, like C.
        (Add | Sub, Ptr(_), Int(IntTy::I64)) => l.clone(),
        (Eq | Ne, a, b) if a == b && matches!(a, Bool | Int(_) | Str | Ptr(_)) => Bool,
        (Lt | Le | Gt | Ge, Int(a), Int(b)) if a == b => Bool,
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
