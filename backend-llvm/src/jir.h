// In-memory form of JIR, mirroring crates/jihoo-ir/src/lib.rs.
// The text format is specified in docs/jir.md.
#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <utility>
#include <vector>

namespace jir {

enum class Profile { Hosted, Freestanding };

struct Type {
  enum Kind { Unit, Bool, Int, Str, Ptr, Struct, Array } kind = Unit;
  unsigned bits = 0;               // Int
  bool is_signed = false;          // Int
  std::shared_ptr<Type> pointee;   // Ptr: the pointee; Array: the element type
  uint64_t count = 0;              // Array
  std::string name;                // Struct

  static Type unit() { return {}; }
  static Type integer(unsigned bits, bool is_signed) {
    Type t;
    t.kind = Int;
    t.bits = bits;
    t.is_signed = is_signed;
    return t;
  }
  static Type pointer(Type to) {
    Type t;
    t.kind = Ptr;
    t.pointee = std::make_shared<Type>(std::move(to));
    return t;
  }
  static Type array(Type elem, uint64_t count) {
    Type t;
    t.kind = Array;
    t.pointee = std::make_shared<Type>(std::move(elem));
    t.count = count;
    return t;
  }

  bool operator==(const Type &o) const {
    if (kind != o.kind) return false;
    switch (kind) {
      case Int: return bits == o.bits && is_signed == o.is_signed;
      case Ptr: return *pointee == *o.pointee;
      case Array: return count == o.count && *pointee == *o.pointee;
      case Struct: return name == o.name;
      default: return true;
    }
  }
  bool operator!=(const Type &o) const { return !(*this == o); }

  std::string str() const {
    switch (kind) {
      case Unit: return "unit";
      case Bool: return "bool";
      case Int: return (is_signed ? "i" : "u") + std::to_string(bits);
      case Str: return "str";
      case Ptr: return "*" + pointee->str();
      case Struct: return "$" + name;
      case Array: return "[" + std::to_string(count) + " x " + pointee->str() + "]";
    }
    return "?";
  }
};

struct StructDef {
  std::string name;
  std::vector<std::pair<std::string, Type>> fields;
  // Layout computed by the frontend; checked against LLVM's data layout.
  bool has_layout = false;
  uint64_t size = 0, align = 0;
};

enum class Op {
  Const, Unit, Str, Copy, Neg, Not,
  Add, Sub, Mul, Div, Rem, Eq, Ne, Lt, Le, Gt, Ge,
  Cast, Call, Struct, Field, SetField, Load, Store, Addr, FieldPtr,
  Array, Splat, Elem, SetElem, ElemPtr,
  Syscall, Print, Asm,
};

struct Inst {
  Op op;
  uint32_t dst = 0;            // unused for Store and Print
  std::vector<uint32_t> args;  // operand registers
  int64_t imm = 0;             // Const value, or field index
  std::string text;            // Str bytes, Call callee, Struct name, or Asm template
  std::string constraints;     // Asm only: LLVM constraint string
};

enum class TermKind { Jump, Branch, Ret, Unreachable };

struct Terminator {
  TermKind kind;
  uint32_t reg = 0;  // Branch condition / Ret value
  uint32_t target = 0, els = 0;
};

struct Block {
  std::vector<Inst> insts;
  Terminator term;
};

struct Function {
  std::string name;
  std::vector<Type> params;
  Type ret;
  std::vector<Type> regs;  // starts with the parameter types
  std::vector<Block> blocks;
};

struct Module {
  Profile profile = Profile::Hosted;
  std::vector<StructDef> structs;
  std::vector<Function> funcs;
};

}  // namespace jir
