#pragma once

#include <memory>
#include <stdexcept>

#include <llvm/IR/LLVMContext.h>
#include <llvm/IR/Module.h>
#include <llvm/TargetParser/Triple.h>

#include "jir.h"

namespace jihoo {

struct CodegenError : std::runtime_error {
  using std::runtime_error::runtime_error;
};

// Lowers a freestanding JIR module to LLVM IR for the given target.
std::unique_ptr<llvm::Module> codegen(const jir::Module &m, llvm::LLVMContext &ctx,
                                      const llvm::Triple &triple);

}  // namespace jihoo
