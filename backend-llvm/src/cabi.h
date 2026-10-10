// The C calling convention for structs passed and returned by value, on the
// targets jihoo compiles for (docs/internals/llvm-backend.md#the-c-abi).
//
// jihoo's own functions pass aggregates by pointer (codegen.cpp). C does not:
// each ABI passes a small struct in registers, split by what its bytes hold,
// and a large one in memory. LLVM leaves that to the frontend, as clang does,
// so this decides it for `extern fn` calls and `export fn` definitions.
#pragma once

#include <vector>

#include <llvm/IR/DataLayout.h>
#include <llvm/IR/DerivedTypes.h>
#include <llvm/TargetParser/Triple.h>

namespace jihoo::cabi {

// How one parameter, or the result, crosses a C call.
struct Arg {
  enum Kind {
    // As its own LLVM type: a scalar, or `void` for a `unit` result.
    Direct,
    // A struct in registers: its bytes read as `mem` (a literal struct of the
    // parts, or the one part) and passed as `parts`, one LLVM parameter each.
    Expand,
    // A struct in memory: a pointer to a copy, which is `byval` (LLVM puts the
    // copy on the stack) on x86_64; a result goes through an `sret` pointer.
    Indirect,
  } kind = Direct;
  llvm::Type *mem = nullptr;
  std::vector<llvm::Type *> parts;
  bool byval = false;
  // Expand: the alignment it gets on the stack, if not its own (aarch64 puts
  // a struct of floats there at a multiple of 8).
  unsigned stack_align = 0;
};

struct Signature {
  llvm::FunctionType *type = nullptr;
  Arg ret;
  std::vector<Arg> params;
  // The LLVM parameter each parameter starts at (after an `sret` pointer).
  std::vector<unsigned> first;
};

// The C signature of a function taking `params` and returning `ret` (LLVM
// types of jihoo values: a struct type for a struct, nullptr for `unit`).
// With `variadic`, more scalar arguments may follow.
Signature lower(const llvm::Triple &triple, const llvm::DataLayout &dl, llvm::LLVMContext &ctx,
                const std::vector<llvm::Type *> &params, llvm::Type *ret, bool variadic);

}  // namespace jihoo::cabi
