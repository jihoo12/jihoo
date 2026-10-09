// JIR -> LLVM IR.
//
// Every JIR register becomes an alloca of its type in the entry block; mem2reg
// (part of the optimization pipeline) turns them back into SSA values. `addr %r`
// is simply the address of that alloca, which mem2reg then leaves in memory.
//
// Type mapping: iN/uN -> iN, bool -> i1, *T -> ptr, unit -> {} (empty struct),
// $S -> a named LLVM struct. `str` is a GC type and never reaches this backend.

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

// Module-wide type information.
class Types {
 public:
  Types(LLVMContext &ctx, const jir::Module &m) : ctx_(ctx) {
    // Create all structs first so fields can refer to any of them through pointers.
    for (const auto &s : m.structs) {
      if (structs_.count(s.name)) throw CodegenError("duplicate struct $" + s.name);
      structs_[s.name] = {StructType::create(ctx, "jihoo." + s.name), &s};
    }
    for (const auto &s : m.structs) {
      std::vector<Type *> fields;
      for (const auto &f : s.fields) fields.push_back(lower(f.second));
      structs_[s.name].ty->setBody(fields);
    }
  }

  Type *lower(const jir::Type &t) const {
    switch (t.kind) {
      case jir::Type::Unit: return StructType::get(ctx_);
      case jir::Type::Bool: return Type::getInt1Ty(ctx_);
      case jir::Type::Int: return Type::getIntNTy(ctx_, t.bits);
      case jir::Type::Ptr: return PointerType::get(ctx_, 0);
      case jir::Type::Struct: return info(t.name).ty;
      case jir::Type::Str: break;
    }
    throw CodegenError("type `str` is garbage collected and cannot be compiled natively");
  }

  FunctionType *signature(const jir::Function &f) const {
    std::vector<Type *> params;
    for (const auto &t : f.params) params.push_back(lower(t));
    return FunctionType::get(lower(f.ret), params, false);
  }

  // JIR type of field `index` of struct type `t`.
  const jir::Type &field(const jir::Type &t, int64_t index) const {
    if (t.kind != jir::Type::Struct) throw CodegenError(t.str() + " is not a struct");
    const auto &fields = info(t.name).def->fields;
    if (index < 0 || size_t(index) >= fields.size())
      throw CodegenError(t.str() + " has no field " + std::to_string(index));
    return fields[index].second;
  }

  StructType *struct_type(const std::string &name) const { return info(name).ty; }

 private:
  struct Info {
    StructType *ty;
    const jir::StructDef *def;
  };
  LLVMContext &ctx_;
  std::unordered_map<std::string, Info> structs_;

  const Info &info(const std::string &name) const {
    auto it = structs_.find(name);
    if (it == structs_.end()) throw CodegenError("unknown struct $" + name);
    return it->second;
  }
};

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
  FnGen(const jir::Function &f, Function *llfn, Module &mod, const Types &types,
        const std::unordered_map<std::string, Function *> &fns, const Triple &triple)
      : f_(f), fn_(llfn), mod_(mod), types_(types), fns_(fns), triple_(triple),
        ctx_(mod.getContext()), b_(ctx_), i64_(Type::getInt64Ty(ctx_)) {}

  void run() {
    auto *entry = BasicBlock::Create(ctx_, "entry", fn_);
    for (size_t i = 0; i < f_.blocks.size(); i++)
      blocks_.push_back(BasicBlock::Create(ctx_, "bb" + std::to_string(i), fn_));

    b_.SetInsertPoint(entry);
    for (size_t r = 0; r < f_.regs.size(); r++)
      slots_.push_back(b_.CreateAlloca(types_.lower(f_.regs[r]), nullptr, "r" + std::to_string(r)));
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
  const Types &types_;
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
  const jir::Type &type(uint32_t r) {
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

  // The type `*T` points to, as an LLVM type.
  Type *pointee(uint32_t r) {
    const jir::Type &t = type(r);
    if (t.kind != jir::Type::Ptr) fail(f_.name, "%" + std::to_string(r) + " is not a pointer");
    return types_.lower(*t.pointee);
  }

  // Integers keep their signedness in JIR; LLVM wants it on each operation.
  bool is_signed(uint32_t r) {
    const jir::Type &t = type(r);
    return t.kind == jir::Type::Int && t.is_signed;
  }

  Value *compare(jir::Op op, Value *a, Value *b, bool is_signed) {
    using P = CmpInst::Predicate;
    using jir::Op;
    P pred;
    switch (op) {
      case Op::Eq: pred = P::ICMP_EQ; break;
      case Op::Ne: pred = P::ICMP_NE; break;
      case Op::Lt: pred = is_signed ? P::ICMP_SLT : P::ICMP_ULT; break;
      case Op::Le: pred = is_signed ? P::ICMP_SLE : P::ICMP_ULE; break;
      case Op::Gt: pred = is_signed ? P::ICMP_SGT : P::ICMP_UGT; break;
      default: pred = is_signed ? P::ICMP_SGE : P::ICMP_UGE; break;
    }
    return b_.CreateICmp(pred, a, b);
  }

  Value *cast(uint32_t src, const jir::Type &to) {
    const jir::Type &from = type(src);
    Value *v = load(src);
    Type *ty = types_.lower(to);
    if (from == to) return v;
    if (to.kind == jir::Type::Int) {
      if (from.kind == jir::Type::Ptr) return b_.CreatePtrToInt(v, ty);
      if (from.kind == jir::Type::Int || from.kind == jir::Type::Bool)
        return b_.CreateIntCast(v, ty, from.kind == jir::Type::Int && from.is_signed);
    }
    if (to.kind == jir::Type::Ptr) {
      if (from.kind == jir::Type::Ptr) return v;  // pointers are untyped in LLVM
      if (from.kind == jir::Type::Int) return b_.CreateIntToPtr(v, ty);
    }
    fail(f_.name, "cannot cast " + from.str() + " to " + to.str());
  }

  void emit(const jir::Inst &inst) {
    using jir::Op;
    auto arg = [&](size_t i) { return load(inst.args.at(i)); };
    auto arg_reg = [&](size_t i) { return inst.args.at(i); };
    switch (inst.op) {
      case Op::Const: {
        Type *ty = slot(inst.dst)->getAllocatedType();
        if (!ty->isIntegerTy()) fail(f_.name, "`const` needs an integer or bool register");
        store(inst.dst, ConstantInt::get(ty, uint64_t(inst.imm), /*isSigned=*/true));
        return;
      }
      case Op::Unit: store(inst.dst, unit()); return;
      case Op::Str:
        // Freestanding strings are addresses of NUL-terminated constant bytes.
        store(inst.dst, b_.CreateGlobalString(inst.text, ".str", 0, &mod_));
        return;
      case Op::Copy: store(inst.dst, arg(0)); return;
      case Op::Neg: store(inst.dst, b_.CreateNeg(arg(0))); return;
      case Op::Not: store(inst.dst, b_.CreateNot(arg(0))); return;
      case Op::Add:
        if (type(arg_reg(0)).kind == jir::Type::Ptr)
          store(inst.dst, b_.CreateGEP(pointee(arg_reg(0)), arg(0), {arg(1)}));
        else
          store(inst.dst, b_.CreateAdd(arg(0), arg(1)));
        return;
      case Op::Sub:
        if (type(arg_reg(0)).kind == jir::Type::Ptr)
          store(inst.dst, b_.CreateGEP(pointee(arg_reg(0)), arg(0), {b_.CreateNeg(arg(1))}));
        else
          store(inst.dst, b_.CreateSub(arg(0), arg(1)));
        return;
      case Op::Mul: store(inst.dst, b_.CreateMul(arg(0), arg(1))); return;
      // TODO: division by zero is undefined behaviour here; the VM traps instead.
      case Op::Div:
        store(inst.dst, is_signed(arg_reg(0)) ? b_.CreateSDiv(arg(0), arg(1)) : b_.CreateUDiv(arg(0), arg(1)));
        return;
      case Op::Rem:
        store(inst.dst, is_signed(arg_reg(0)) ? b_.CreateSRem(arg(0), arg(1)) : b_.CreateURem(arg(0), arg(1)));
        return;
      case Op::Eq: case Op::Ne: case Op::Lt: case Op::Le: case Op::Gt: case Op::Ge:
        store(inst.dst, compare(inst.op, arg(0), arg(1), is_signed(arg_reg(0))));
        return;
      case Op::Cast: store(inst.dst, cast(arg_reg(0), type(inst.dst))); return;
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
      case Op::Struct: {
        Value *v = PoisonValue::get(types_.struct_type(inst.text));
        for (unsigned i = 0; i < inst.args.size(); i++) v = b_.CreateInsertValue(v, arg(i), {i});
        store(inst.dst, v);
        return;
      }
      case Op::Field:
        types_.field(type(arg_reg(0)), inst.imm);  // bounds check
        store(inst.dst, b_.CreateExtractValue(arg(0), {unsigned(inst.imm)}));
        return;
      case Op::SetField:
        types_.field(type(arg_reg(0)), inst.imm);
        store(inst.dst, b_.CreateInsertValue(arg(0), arg(1), {unsigned(inst.imm)}));
        return;
      case Op::Load: store(inst.dst, b_.CreateLoad(pointee(arg_reg(0)), arg(0))); return;
      case Op::Store: {
        Value *v = arg(1);
        if (v->getType() != pointee(arg_reg(0))) fail(f_.name, "stored value has the wrong type");
        b_.CreateStore(v, arg(0));
        return;
      }
      case Op::Addr: store(inst.dst, slot(arg_reg(0))); return;
      case Op::FieldPtr: {
        const jir::Type &t = type(arg_reg(0));
        if (t.kind != jir::Type::Ptr) fail(f_.name, "`fieldptr` needs a pointer");
        types_.field(*t.pointee, inst.imm);
        store(inst.dst, b_.CreateStructGEP(types_.lower(*t.pointee), arg(0), unsigned(inst.imm)));
        return;
      }
      case Op::Syscall: {
        if (inst.args.empty() || inst.args.size() > 7) fail(f_.name, "syscall takes 1 to 7 arguments");
        std::vector<Value *> args;
        for (size_t i = 0; i < inst.args.size(); i++) {
          const jir::Type &t = type(arg_reg(i));
          Value *v = arg(i);
          if (t.kind == jir::Type::Ptr) v = b_.CreatePtrToInt(v, i64_);
          else if (t.kind == jir::Type::Int) v = b_.CreateIntCast(v, i64_, t.is_signed);
          else fail(f_.name, "syscall arguments must be integers or pointers");
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
        if (type(t.reg).kind != jir::Type::Bool) fail(f_.name, "branch condition must be bool");
        b_.CreateCondBr(load(t.reg), block(t.target), block(t.els));
        return;
      case jir::TermKind::Unreachable: b_.CreateUnreachable(); return;
      case jir::TermKind::Ret:
        if (type(t.reg) != f_.ret) fail(f_.name, "returned value has the wrong type");
        if (f_.name == "_start") {
          // There is nothing to return to: returning from `_start` means exit(value).
          Value *code = f_.ret.kind == jir::Type::Int ? load(t.reg) : b_.getInt64(0);
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
  Types types(ctx, m);

  // Declare everything first so calls can refer to any function.
  std::unordered_map<std::string, Function *> fns;
  for (const auto &f : m.funcs) {
    if (fns.count(f.name)) throw CodegenError("duplicate function @" + f.name);
    bool is_entry = f.name == "_start";
    auto *fn = Function::Create(types.signature(f),
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

  for (const auto &f : m.funcs) FnGen(f, fns[f.name], *mod, types, fns, triple).run();

  std::string err;
  raw_string_ostream os(err);
  if (verifyModule(*mod, &os)) throw CodegenError("LLVM verifier failed:\n" + os.str());
  return mod;
}

}  // namespace jihoo
