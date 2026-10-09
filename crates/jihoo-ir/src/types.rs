//! JIR types and the typing rules for operators.
//!
//! The rules live here so that the frontend's type checker and the IR verifier
//! can never disagree.

use std::fmt;

use crate::{BinOp, Profile, UnOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    Unit,
    I64,
    Bool,
    /// GC-managed string. Hosted only.
    Str,
    /// Raw, untyped pointer. Freestanding only.
    Ptr,
}

impl Type {
    pub fn name(self) -> &'static str {
        match self {
            Type::Unit => "unit",
            Type::I64 => "i64",
            Type::Bool => "bool",
            Type::Str => "str",
            Type::Ptr => "ptr",
        }
    }

    pub fn from_name(name: &str) -> Option<Type> {
        Some(match name {
            "unit" => Type::Unit,
            "i64" => Type::I64,
            "bool" => Type::Bool,
            "str" => Type::Str,
            "ptr" => Type::Ptr,
            _ => return None,
        })
    }

    /// GC types need the hosted runtime; raw pointers are only for freestanding code.
    pub fn available_in(self, profile: Profile) -> bool {
        match self {
            Type::Str => profile == Profile::Hosted,
            Type::Ptr => profile == Profile::Freestanding,
            Type::Unit | Type::I64 | Type::Bool => true,
        }
    }

    pub fn is_printable(self) -> bool {
        matches!(self, Type::I64 | Type::Bool | Type::Str)
    }

    pub fn is_syscall_arg(self) -> bool {
        matches!(self, Type::I64 | Type::Ptr)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Type of a string literal in the given profile.
pub fn str_literal(profile: Profile) -> Type {
    match profile {
        Profile::Hosted => Type::Str,
        Profile::Freestanding => Type::Ptr,
    }
}

pub fn unary(op: UnOp, t: Type) -> Option<Type> {
    match (op, t) {
        (UnOp::Neg, Type::I64) => Some(Type::I64),
        (UnOp::Not, Type::Bool) => Some(Type::Bool),
        _ => None,
    }
}

pub fn binary(op: BinOp, l: Type, r: Type) -> Option<Type> {
    use BinOp::*;
    use Type::*;
    Some(match (op, l, r) {
        (Add | Sub | Mul | Div | Rem, I64, I64) => I64,
        (Add, Str, Str) => Str,
        // Pointer arithmetic is in bytes.
        (Add | Sub, Ptr, I64) => Ptr,
        (Eq | Ne, a, b) if a == b && a != Unit => Bool,
        (Lt | Le | Gt | Ge, I64, I64) | (Lt | Le | Gt | Ge, Ptr, Ptr) => Bool,
        _ => return None,
    })
}
