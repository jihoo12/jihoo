// JIR -> LLVM IR.
//
// Every JIR register becomes an alloca of its type in the entry block; mem2reg
// (part of the optimization pipeline) turns them back into SSA values.
//
// Type mapping: i64 -> i64, bool -> i1, ptr -> ptr, unit -> {} (empty struct).
// `str` is a GC type and never reaches this backend.

#include "codegen.h"

#include <unordered_map>

#include <llvm/IR/Constants.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/InlineAsm.h>
#include <llvm/IR/Verifier.h>
#include <llvm/Support/raw_ostream.h>

using namespace llvm;

namespace jihoo {
namespace {

[[noreturn]] void fail(const std::string &fn, const std::string &msg) {
  throw CodegenError("@" + fn + ": " + msg);
}

Type *lower_type(LLVMContext &ctx, jir::Type t) {
  switch (t) {
    case jir::Type::Unit: return StructType::get(ctx);
    case jir::Type::I64: return Type::getInt64Ty(ctx);
    case jir::Type::Bool: return Type::getInt1Ty(ctx);
    case jir::Type::Ptr: return PointerType::get(ctx, 0);
    case jir::Type::Str: break;
  }
  throw CodegenError("type `str` is garbage collected and cannot be compiled natively");
}

FunctionType *lower_sig(LLVMContext &ctx, const jir::Function &f) {
  std::vector<Type *> params;
  for (auto t : f.params) params.push_back(lower_type(ctx, t));
  return FunctionType::get(lower_type(ctx, f.ret), params, false);
}

// Builds the inline asm for a raw Linux syscall. args[0] is the syscall number;
// all operands are i64.
InlineAsm *syscall_asm(LLVMContext &ctx, const Triple &triple, size_t nargs) {
  const char *insn;
  std::string cons;
  std::vector<std::string> arg_regs;
  std::string clobbers;
  switch (triple.getArch()) {
    case Triple::x86_64:
      insn = "syscall";
      cons = "={rax},{rax}";
      arg_regs = {"rdi", "rsi", "rdx", "r10", "r8", "r9"};
      clobbers = ",~{rcx},~{r11},~{memory}";
      break;
    case Triple::aarch64:
      insn = "svc #0";
      cons = "={x0},{x8}";
      arg_regs = {"x0", "x1", "x2", "x3", "x4", "x5"};
      clobbers = ",~{memory}";
      break;
    default:
      throw CodegenError("syscall is not supported on " + triple.str());
  }
  for (size_t i = 1; i < nargs; i++) cons += ",{" + arg_regs[i - 1] + "}";
  cons += clobbers;

  Type *i64 = Type::getInt64Ty(ctx);
  auto *ty = FunctionType::get(i64, std::vector<Type *>(nargs, i64), false);
  return InlineAsm::get(ty, insn, cons, /*hasSideEffects=*/true);
}

class FnGen {
 public:
  FnGen(const jir::Function &f, Function *llfn, Module &mod,
        const std::unordered_map<std::string, Function *> &fns, const Triple &triple)
      : f_(f), fn_(llfn), mod_(mod), fns_(fns), triple_(triple),
        ctx_(mod.getContext()), b_(ctx_), i64_(Type::getInt64Ty(ctx_)) {}

  void run() {
    auto *entry = BasicBlock::Create(ctx_, "entry", fn_);
    for (size_t i = 0; i < f_.blocks.size(); i++)
      blocks_.push_back(BasicBlock::Create(ctx_, "bb" + std::to_string(i), fn_));

    b_.SetInsertPoint(entry);
    for (size_t r = 0; r < f_.regs.size(); r++)
      slots_.push_back(b_.CreateAlloca(lower_type(ctx_, f_.regs[r]), nullptr, "r" + std::to_string(r)));
    for (size_t p = 0; p < f_.params.size(); p++) b_.CreateStore(fn_->getArg(p), slots_[p]);
    b_.CreateBr(blocks_[0]);

    for (size_t i = 0; i < f_.blocks.size(); i++) {
      b_.SetInsertPoint(blocks_[i]);
      for (const auto &inst : f_.blocks[i].insts) emit(inst);
      term(f_.blocks[i].term);
    }
  }

 private:
  const jir::Function &f_;
  Function *fn_;
  Module &mod_;
  const std::unordered_map<std::string, Function *> &fns_;
  const Triple &triple_;
  LLVMContext &ctx_;
  IRBuilder<> b_;
  Type *i64_;
  std::vector<AllocaInst *> slots_;
  std::vector<BasicBlock *> blocks_;

  AllocaInst *slot(uint32_t r) {
    if (r >= slots_.size()) fail(f_.name, "register %" + std::to_string(r) + " out of range");
    return slots_[r];
  }
  jir::Type type(uint32_t r) {
    slot(r);
    return f_.regs[r];
  }
  Value *load(uint32_t r) { return b_.CreateLoad(slot(r)->getAllocatedType(), slot(r)); }
  void store(uint32_t r, Value *v) {
    if (v->getType() != slot(r)->getAllocatedType())
      fail(f_.name, "type mismatch when writing %" + std::to_string(r));
    b_.CreateStore(v, slot(r));
  }
  BasicBlock *block(uint32_t id) {
    if (id >= blocks_.size()) fail(f_.name, "jump to missing block bb" + std::to_string(id));
    return blocks_[id];
  }
  Value *unit() { return ConstantStruct::get(StructType::get(ctx_), {}); }

  // Comparisons are signed for i64 and unsigned for pointers.
  Value *compare(jir::Op op, Value *a, Value *b, bool is_ptr) {
    using P = CmpInst::Predicate;
    using jir::Op;
    P pred;
    switch (op) {
      case Op::Eq: pred = P::ICMP_EQ; break;
      case Op::Ne: pred = P::ICMP_NE; break;
      case Op::Lt: pred = is_ptr ? P::ICMP_ULT : P::ICMP_SLT; break;
      case Op::Le: pred = is_ptr ? P::ICMP_ULE : P::ICMP_SLE; break;
      case Op::Gt: pred = is_ptr ? P::ICMP_UGT : P::ICMP_SGT; break;
      default: pred = is_ptr ? P::ICMP_UGE : P::ICMP_SGE; break;
    }
    return b_.CreateICmp(pred, a, b);
  }

  void emit(const jir::Inst &inst) {
    using jir::Op;
    auto arg = [&](size_t i) { return load(inst.args.at(i)); };
    auto arg_is_ptr = [&](size_t i) { return type(inst.args.at(i)) == jir::Type::Ptr; };
    switch (inst.op) {
      case Op::Const:
        if (type(inst.dst) == jir::Type::Bool)
          store(inst.dst, b_.getInt1(inst.imm != 0));
        else
          store(inst.dst, b_.getInt64(inst.imm));
        return;
      case Op::Unit: store(inst.dst, unit()); return;
      case Op::Str:
        // Freestanding strings are addresses of NUL-terminated constant bytes.
        store(inst.dst, b_.CreateGlobalString(inst.text, ".str", 0, &mod_));
        return;
      case Op::Copy: store(inst.dst, arg(0)); return;
      case Op::Neg: store(inst.dst, b_.CreateNeg(arg(0))); return;
      case Op::Not: store(inst.dst, b_.CreateNot(arg(0))); return;
      case Op::Add:
        store(inst.dst, arg_is_ptr(0) ? b_.CreatePtrAdd(arg(0), arg(1)) : b_.CreateAdd(arg(0), arg(1)));
        return;
      case Op::Sub:
        store(inst.dst, arg_is_ptr(0) ? b_.CreatePtrAdd(arg(0), b_.CreateNeg(arg(1)))
                                      : b_.CreateSub(arg(0), arg(1)));
        return;
      case Op::Mul: store(inst.dst, b_.CreateMul(arg(0), arg(1))); return;
      // TODO: division by zero is undefined behaviour here; the VM traps instead.
      case Op::Div: store(inst.dst, b_.CreateSDiv(arg(0), arg(1))); return;
      case Op::Rem: store(inst.dst, b_.CreateSRem(arg(0), arg(1))); return;
      case Op::Eq: case Op::Ne: case Op::Lt: case Op::Le: case Op::Gt: case Op::Ge:
        store(inst.dst, compare(inst.op, arg(0), arg(1), arg_is_ptr(0)));
        return;
      case Op::Call: {
        auto it = fns_.find(inst.text);
        if (it == fns_.end()) fail(f_.name, "call to unknown function @" + inst.text);
        Function *callee = it->second;
        if (callee->arg_size() != inst.args.size())
          fail(f_.name, "wrong number of arguments to @" + inst.text);
        std::vector<Value *> args;
        for (size_t i = 0; i < inst.args.size(); i++) {
          args.push_back(arg(i));
          if (args.back()->getType() != callee->getArg(i)->getType())
            fail(f_.name, "argument " + std::to_string(i + 1) + " to @" + inst.text + " has the wrong type");
        }
        store(inst.dst, b_.CreateCall(callee, args));
        return;
      }
      case Op::Syscall: {
        if (inst.args.empty() || inst.args.size() > 7) fail(f_.name, "syscall takes 1 to 7 arguments");
        std::vector<Value *> args;
        for (size_t i = 0; i < inst.args.size(); i++) {
          Value *v = arg(i);
          if (v->getType()->isPointerTy()) v = b_.CreatePtrToInt(v, i64_);
          if (v->getType() != i64_) fail(f_.name, "syscall arguments must be i64 or ptr");
          args.push_back(v);
        }
        store(inst.dst, b_.CreateCall(syscall_asm(ctx_, triple_, args.size()), args));
        return;
      }
      case Op::Print: fail(f_.name, "`print` is not available in freestanding mode");
    }
  }

  void term(const jir::Terminator &t) {
    switch (t.kind) {
      case jir::TermKind::Jump: b_.CreateBr(block(t.target)); return;
      case jir::TermKind::Branch:
        if (type(t.reg) != jir::Type::Bool) fail(f_.name, "branch condition must be bool");
        b_.CreateCondBr(load(t.reg), block(t.target), block(t.els));
        return;
      case jir::TermKind::Unreachable: b_.CreateUnreachable(); return;
      case jir::TermKind::Ret:
        if (type(t.reg) != f_.ret) fail(f_.name, "returned value has the wrong type");
        if (f_.name == "_start") {
          // There is nothing to return to: returning from `_start` means exit(value).
          Value *code = f_.ret == jir::Type::I64 ? load(t.reg) : b_.getInt64(0);
          int64_t exit_nr = triple_.getArch() == Triple::aarch64 ? 93 : 60;
          b_.CreateCall(syscall_asm(ctx_, triple_, 2), {b_.getInt64(exit_nr), code});
          b_.CreateUnreachable();
        } else {
          b_.CreateRet(load(t.reg));
        }
        return;
    }
  }
};

}  // namespace

std::unique_ptr<llvm::Module> codegen(const jir::Module &m, LLVMContext &ctx, const Triple &triple) {
  if (m.profile != jir::Profile::Freestanding)
    throw CodegenError("only freestanding modules can be compiled natively (for now)");

  auto mod = std::make_unique<Module>("jihoo", ctx);

  // Declare everything first so calls can refer to any function.
  std::unordered_map<std::string, Function *> fns;
  for (const auto &f : m.funcs) {
    if (fns.count(f.name)) throw CodegenError("duplicate function @" + f.name);
    bool is_entry = f.name == "_start";
    auto *fn = Function::Create(lower_sig(ctx, f),
                                is_entry ? GlobalValue::ExternalLinkage : GlobalValue::InternalLinkage,
                                f.name, mod.get());
    // No libc to fall back on: never turn loops into memcpy/memset calls.
    fn->addFnAttr("no-builtins");
    if (is_entry) {
      fn->addFnAttr(Attribute::NoReturn);
      // The kernel enters `_start` with a 16-byte aligned stack, not the
      // "just called" alignment the ABI promises to normal functions.
      if (triple.getArch() == Triple::x86_64) fn->addFnAttr("stackrealign");
    }
    fns[f.name] = fn;
  }
  auto entry = fns.find("_start");
  if (entry == fns.end()) throw CodegenError("freestanding module needs @_start");
  if (entry->second->arg_size() != 0) throw CodegenError("@_start must take no parameters");

  for (const auto &f : m.funcs) FnGen(f, fns[f.name], *mod, fns, triple).run();

  std::string err;
  raw_string_ostream os(err);
  if (verifyModule(*mod, &os)) throw CodegenError("LLVM verifier failed:\n" + os.str());
  return mod;
}

}  // namespace jihoo
