// In-memory form of JIR, mirroring crates/jihoo-ir/src/lib.rs.
// The text format is specified in docs/jir.md.
#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <utility>
#include <vector>

namespace jir {

// The one JIR version this backend reads, from the `jir N` header. It must
// equal JIR_VERSION in crates/jihoo-ir/src/lib.rs (a Rust test checks this).
constexpr int64_t kVersion = 3;

enum class Profile { Hosted, Native, Freestanding };

struct Type {
  enum Kind { Unit, Bool, Int, Float, Str, Ptr, Struct, Array, Fn } kind = Unit;
  unsigned bits = 0;               // Int, Float (32 or 64)
  bool is_signed = false;          // Int
  std::shared_ptr<Type> pointee;   // Ptr: the pointee; Array: the element type; Fn: the result
  uint64_t count = 0;              // Array
  std::string name;                // Struct: a struct or enum name
  std::vector<Type> params;        // Fn

  static Type unit() { return {}; }
  static Type integer(unsigned bits, bool is_signed) {
    Type t;
    t.kind = Int;
    t.bits = bits;
    t.is_signed = is_signed;
    return t;
  }
  static Type floating(unsigned bits) {
    Type t;
    t.kind = Float;
    t.bits = bits;
    return t;
  }
  static Type pointer(Type to) {
    Type t;
    t.kind = Ptr;
    t.pointee = std::make_shared<Type>(std::move(to));
    return t;
  }
  static Type function(std::vector<Type> params, Type ret) {
    Type t;
    t.kind = Fn;
    t.params = std::move(params);
    t.pointee = std::make_shared<Type>(std::move(ret));
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
      case Float: return bits == o.bits;
      case Ptr: return *pointee == *o.pointee;
      case Array: return count == o.count && *pointee == *o.pointee;
      case Struct: return name == o.name;
      case Fn: return params == o.params && *pointee == *o.pointee;
      default: return true;
    }
  }
  bool operator!=(const Type &o) const { return !(*this == o); }

  std::string str() const {
    switch (kind) {
      case Unit: return "unit";
      case Bool: return "bool";
      case Int: return (is_signed ? "i" : "u") + std::to_string(bits);
      case Float: return "f" + std::to_string(bits);
      case Str: return "str";
      case Ptr: return "*" + pointee->str();
      case Struct: return "$" + name;
      case Array: return "[" + std::to_string(count) + " x " + pointee->str() + "]";
      case Fn: {
        std::string s = "fn(";
        for (size_t i = 0; i < params.size(); i++) s += (i ? ", " : "") + params[i].str();
        return s + ") -> " + pointee->str();
      }
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

// An enum: a u32 tag, then the payload of the active variant. Variants are
// addressed by index.
struct EnumDef {
  std::string name;
  std::vector<std::pair<std::string, std::vector<Type>>> variants;
  bool has_layout = false;
  uint64_t size = 0, align = 0;
};

// A C function the program calls (native only). Its types are integers, bools,
// pointers, functions, and `unit` for a void result.
struct ExternFn {
  std::string name;  // the C symbol
  std::vector<Type> params;
  Type ret;
  bool variadic = false;  // takes more arguments after `params`, like printf
};

enum class Op {
  Const, FConst, Unit, Str, Copy, Neg, Not,
  Add, Sub, Mul, Div, Rem, Eq, Ne, Lt, Le, Gt, Ge, And, Or, Xor, Shl, Shr,
  Cast, Call, FuncRef, CallIndirect, Struct, Field, SetField, Load, Store, Addr, FieldPtr,
  Array, Splat, Elem, SetElem, ElemPtr, Variant, Tag, Payload, GetPath, SetPath,
  Syscall, Print, Asm,
};

// One step of a `getpath`/`setpath` path.
struct PathStep {
  bool elem;       // an array element (else a struct field)
  uint32_t value;  // the index register for an element, the field number for a field
};

struct Inst {
  Op op;
  uint32_t dst = 0;            // unused for Store and Print
  std::vector<uint32_t> args;  // operand registers
  int64_t imm = 0;             // Const value, field index, or variant index
  double fimm = 0;             // FConst value
  int64_t imm2 = 0;            // Payload: index of the value in the variant
  std::string text;            // Str bytes, Call/FuncRef function, Struct name, or Asm template
  std::string constraints;     // Asm only: LLVM constraint string
  std::vector<PathStep> path;  // GetPath / SetPath: the steps, outside in
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
  // `x86_64` or `aarch64` from the `target` header; empty without one, which
  // means the machine jihoo-llc runs on.
  std::string target;
  std::vector<StructDef> structs;
  std::vector<EnumDef> enums;
  std::vector<ExternFn> externs;
  std::vector<Function> funcs;
};

}  // namespace jir
