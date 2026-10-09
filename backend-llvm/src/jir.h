// In-memory form of JIR, mirroring crates/jihoo-ir/src/lib.rs.
// The text format is specified in docs/jir.md.
#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace jir {

enum class Profile { Hosted, Freestanding };

enum class Type { Unit, I64, Bool, Str, Ptr };

enum class Op {
  // instructions
  Const, Unit, Str, Copy, Neg, Not,
  Add, Sub, Mul, Div, Rem, Eq, Ne, Lt, Le, Gt, Ge,
  Call, Syscall, Print,
};

struct Inst {
  Op op;
  uint32_t dst = 0;            // unused for Print
  std::vector<uint32_t> args;  // operand registers
  int64_t imm = 0;             // Const
  std::string text;            // Str bytes, or Call callee name
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
  Type ret = Type::Unit;
  std::vector<Type> regs;  // starts with the parameter types
  std::vector<Block> blocks;
};

struct Module {
  Profile profile = Profile::Hosted;
  std::vector<Function> funcs;
};

}  // namespace jir
