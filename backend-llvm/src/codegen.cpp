// JIR -> LLVM IR.
//
// Every JIR register becomes an alloca of its type in the entry block; mem2reg
// (part of the optimization pipeline) turns them back into SSA values. `addr %r`
// is simply the address of that alloca, which mem2reg then leaves in memory.
//
// Type mapping: iN/uN -> iN, bool -> i1, *T and fn(...) -> R -> ptr, unit -> {}
// (empty struct), $S -> a named LLVM struct, [N x T] -> [N x T]. `str` is a GC
// type and never reaches this backend.
//
// Aggregates (structs and arrays) are never loaded or stored as SSA values: LLVM
// handles large first-class aggregates very poorly (a 64 KiB array copy takes
// minutes to compile). Instead they are copied with `llvm.memcpy`, accessed with
// GEPs, passed to functions by pointer (the callee copies), and returned through
// a hidden `sret` pointer. Weak `memcpy`/`memmove`/`memset` definitions are added
// to the module, since there is no libc to provide them.
//
// Array element access is bounds-checked and traps (`llvm.trap`) when out of
// range; raw pointer arithmetic is not checked.

#include "codegen.h"

#include <unordered_map>

#include <llvm/IR/Constants.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/InlineAsm.h>
#include <llvm/IR/Intrinsics.h>
#include <llvm/IR/Verifier.h>
#include <llvm/Support/Error.h>
#include <llvm/Support/raw_ostream.h>

using namespace llvm;

namespace jihoo {
namespace {

[[noreturn]] void fail(const std::string &fn, const std::string &msg) {
  throw CodegenError("@" + fn + ": " + msg);
}

// Module-wide type information.
//
// An enum is `{ i32 tag, [N x iA] payload }`, where A is the largest alignment of
// a variant's payload and N * A bytes hold the largest payload. Each variant's
// payload is accessed through a literal struct of its value types, at the
// payload's address. This is the C layout of `struct { u32 tag; union {...} }`.
class Types {
 public:
  Types(LLVMContext &ctx, const jir::Module &m, const DataLayout &dl) : ctx_(ctx), dl_(dl) {
    // Create every named type first, so members can refer to any of them through pointers.
    for (const auto &s : m.structs) add(s.name, &s, nullptr);
    for (const auto &e : m.enums) add(e.name, nullptr, &e);
    // A body needs the sizes of the types held by value, so those come first.
    for (const auto &s : m.structs) define(s.name);
    for (const auto &e : m.enums) define(e.name);
    // `size_of`/`align_of` were folded to constants by the frontend; make sure
    // they describe the same layout LLVM uses for this target.
    for (const auto &s : m.structs)
      if (s.has_layout) check_layout(s.name, s.size, s.align);
    for (const auto &e : m.enums)
      if (e.has_layout) check_layout(e.name, e.size, e.align);
  }

  Type *lower(const jir::Type &t) const {
    switch (t.kind) {
      case jir::Type::Unit: return StructType::get(ctx_);
      case jir::Type::Bool: return Type::getInt1Ty(ctx_);
      case jir::Type::Int: return Type::getIntNTy(ctx_, t.bits);
      case jir::Type::Ptr:
      case jir::Type::Fn: return PointerType::get(ctx_, 0);
      case jir::Type::Struct: return info(t.name).ty;
      case jir::Type::Array: return ArrayType::get(lower(*t.pointee), t.count);
      case jir::Type::Str: break;
    }
    throw CodegenError("type `str` is garbage collected and cannot be compiled natively");
  }

  // Structs, enums and arrays.
  static bool is_agg(const jir::Type &t) { return t.kind == jir::Type::Struct || t.kind == jir::Type::Array; }

  // Aggregate parameters become pointers; an aggregate result becomes a leading
  // `sret` pointer parameter and a void return.
  FunctionType *signature(const std::vector<jir::Type> &params, const jir::Type &ret) const {
    std::vector<Type *> lowered;
    Type *ptr = PointerType::get(ctx_, 0);
    if (is_agg(ret)) lowered.push_back(ptr);
    for (const auto &t : params) lowered.push_back(is_agg(t) ? ptr : lower(t));
    Type *r = is_agg(ret) ? Type::getVoidTy(ctx_) : lower(ret);
    return FunctionType::get(r, lowered, false);
  }
  FunctionType *signature(const jir::Function &f) const { return signature(f.params, f.ret); }

  // JIR type of field `index` of struct type `t`.
  const jir::Type &field(const jir::Type &t, int64_t index) const {
    if (t.kind != jir::Type::Struct || !info(t.name).def) throw CodegenError(t.str() + " is not a struct");
    const auto &fields = info(t.name).def->fields;
    if (index < 0 || size_t(index) >= fields.size())
      throw CodegenError(t.str() + " has no field " + std::to_string(index));
    return fields[index].second;
  }

  StructType *struct_type(const std::string &name) const {
    if (!info(name).def) throw CodegenError("$" + name + " is not a struct");
    return info(name).ty;
  }

  // The payload of variant `index` of enum type `t`, as a literal struct.
  StructType *variant(const jir::Type &t, int64_t index) const {
    if (t.kind != jir::Type::Struct || !info(t.name).enum_def) throw CodegenError(t.str() + " is not an enum");
    const auto &variants = info(t.name).variants;
    if (index < 0 || size_t(index) >= variants.size())
      throw CodegenError(t.str() + " has no variant " + std::to_string(index));
    return variants[index];
  }

 private:
  struct Info {
    StructType *ty;
    const jir::StructDef *def;     // for a struct
    const jir::EnumDef *enum_def;  // for an enum
    std::vector<StructType *> variants;
    bool defining = false;
  };
  LLVMContext &ctx_;
  const DataLayout &dl_;
  std::unordered_map<std::string, Info> named_;

  void add(const std::string &name, const jir::StructDef *s, const jir::EnumDef *e) {
    if (named_.count(name)) throw CodegenError("duplicate struct or enum $" + name);
    named_[name] = {StructType::create(ctx_, "jihoo." + name), s, e, {}};
  }

  // Defines the types `t` holds by value (arrays hold their elements by value).
  void define_members(const jir::Type &t) {
    if (t.kind == jir::Type::Struct) define(t.name);
    if (t.kind == jir::Type::Array) define_members(*t.pointee);
  }

  void define(const std::string &name) {
    Info &i = named_.at(name);
    if (!i.ty->isOpaque()) return;
    if (i.defining) throw CodegenError("$" + name + " contains itself");
    i.defining = true;
    if (i.def) {
      std::vector<Type *> fields;
      for (const auto &f : i.def->fields) {
        define_members(f.second);
        fields.push_back(lower(f.second));
      }
      i.ty->setBody(fields);
    } else {
      uint64_t size = 0, align = 1;
      for (const auto &v : i.enum_def->variants) {
        std::vector<Type *> values;
        for (const auto &t : v.second) {
          define_members(t);
          values.push_back(lower(t));
        }
        StructType *payload = StructType::get(ctx_, values);
        i.variants.push_back(payload);
        size = std::max<uint64_t>(size, dl_.getTypeAllocSize(payload));
        align = std::max<uint64_t>(align, dl_.getABITypeAlign(payload).value());
      }
      uint64_t words = (size + align - 1) / align;
      Type *word = Type::getIntNTy(ctx_, unsigned(align * 8));
      i.ty->setBody({Type::getInt32Ty(ctx_), ArrayType::get(word, words)});
    }
    i.defining = false;
  }

  void check_layout(const std::string &name, uint64_t want_size, uint64_t want_align) const {
    StructType *ty = named_.at(name).ty;
    uint64_t size = dl_.getTypeAllocSize(ty);
    uint64_t align = dl_.getABITypeAlign(ty).value();
    if (size != want_size || align != want_align)
      throw CodegenError("layout mismatch for $" + name + ": JIR says size " + std::to_string(want_size) +
                         " align " + std::to_string(want_align) + ", the target says size " +
                         std::to_string(size) + " align " + std::to_string(align));
  }

  const Info &info(const std::string &name) const {
    auto it = named_.find(name);
    if (it == named_.end()) throw CodegenError("unknown struct or enum $" + name);
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
    unsigned first = 0;
    if (Types::is_agg(f_.ret)) sret_ = fn_->getArg(first++);
    for (size_t p = 0; p < f_.params.size(); p++) {
      Argument *a = fn_->getArg(first + p);
      if (Types::is_agg(f_.params[p]))
        memcopy(slots_[p], a, f_.params[p]);  // the callee owns its copy
      else
        b_.CreateStore(a, slots_[p]);
    }
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
  BasicBlock *trap_ = nullptr;  // shared target of failed bounds checks
  Value *sret_ = nullptr;       // where an aggregate result goes

  bool agg(uint32_t r) { return Types::is_agg(type(r)); }

  // Calls `callee` of type `ty` with the operands of `inst` from index `first`
  // on, and stores the result in `inst.dst`. `what` names the callee in errors.
  void call(FunctionType *ty, Value *callee, const jir::Inst &inst, size_t first, const std::string &what) {
    bool sret = agg(inst.dst);
    size_t n = inst.args.size() - first;
    if (ty->getNumParams() != n + sret) fail(f_.name, "wrong number of arguments to " + what);
    std::vector<Value *> args;
    if (sret) args.push_back(slot(inst.dst));
    for (size_t i = first; i < inst.args.size(); i++) {
      // Aggregates are passed by pointer; the callee copies them.
      uint32_t r = inst.args[i];
      args.push_back(agg(r) ? slot(r) : load(r));
      if (args.back()->getType() != ty->getParamType(args.size() - 1))
        fail(f_.name, "argument " + std::to_string(i - first + 1) + " to " + what + " has the wrong type");
    }
    Value *result = b_.CreateCall(ty, callee, args);
    if (!sret) store(inst.dst, result);
  }

  void memcopy(Value *dst, Value *src, const jir::Type &t) {
    const DataLayout &dl = mod_.getDataLayout();
    Type *ty = types_.lower(t);
    Align a = dl.getABITypeAlign(ty);
    b_.CreateMemCpy(dst, a, src, a, dl.getTypeAllocSize(ty));
  }

  // Writes the value of register `src` to memory at `dst`.
  void write_mem(Value *dst, uint32_t src) {
    if (agg(src))
      memcopy(dst, slot(src), type(src));
    else
      b_.CreateStore(load(src), dst);
  }

  // Reads a value of register `dst`'s type from memory at `src` into `dst`.
  void read_mem(uint32_t dst, Value *src) {
    if (agg(dst))
      memcopy(slot(dst), src, type(dst));
    else
      store(dst, b_.CreateLoad(slot(dst)->getAllocatedType(), src));
  }

  void copy_reg(uint32_t dst, uint32_t src) {
    if (dst != src) read_mem(dst, slot(src));
  }

  // Continues in a new block if `index < len`, traps otherwise.
  void bounds_check(Value *index, uint64_t len) {
    if (!trap_) {
      trap_ = BasicBlock::Create(ctx_, "out_of_bounds", fn_);
      IRBuilder<> tb(trap_);
      tb.CreateCall(Intrinsic::getOrInsertDeclaration(&mod_, Intrinsic::trap));
      tb.CreateUnreachable();
    }
    auto *ok = BasicBlock::Create(ctx_, "in_bounds", fn_);
    // Unsigned, so negative indices are out of range too.
    b_.CreateCondBr(b_.CreateICmpULT(index, b_.getInt64(len)), ok, trap_);
    b_.SetInsertPoint(ok);
  }

  // Array type of register `r`, which must hold an array (or point to one if `ptr`).
  const jir::Type &array_of(uint32_t r, bool ptr) {
    const jir::Type *t = &type(r);
    if (ptr) {
      if (t->kind != jir::Type::Ptr) fail(f_.name, "%" + std::to_string(r) + " is not a pointer");
      t = t->pointee.get();
    }
    if (t->kind != jir::Type::Array) fail(f_.name, "%" + std::to_string(r) + " is not an array");
    return *t;
  }

  // Address of element `index` of the array at `base`, after a bounds check.
  Value *elem_addr(const jir::Type &arr, Value *base, Value *index) {
    bounds_check(index, arr.count);
    return b_.CreateGEP(types_.lower(arr), base, {b_.getInt64(0), index});
  }

  void splat(uint32_t dst, uint32_t value) {
    const jir::Type &arr = array_of(dst, false);
    if (arr.count == 0) return;
    Type *arr_ty = types_.lower(arr);
    BasicBlock *pre = b_.GetInsertBlock();
    auto *loop = BasicBlock::Create(ctx_, "splat", fn_);
    auto *done = BasicBlock::Create(ctx_, "splat.done", fn_);
    b_.CreateBr(loop);
    b_.SetInsertPoint(loop);
    PHINode *i = b_.CreatePHI(i64_, 2);
    i->addIncoming(b_.getInt64(0), pre);
    write_mem(b_.CreateGEP(arr_ty, slot(dst), {b_.getInt64(0), i}), value);
    Value *next = b_.CreateAdd(i, b_.getInt64(1));
    i->addIncoming(next, loop);
    b_.CreateCondBr(b_.CreateICmpULT(next, b_.getInt64(arr.count)), loop, done);
    b_.SetInsertPoint(done);
  }

  AllocaInst *slot(uint32_t r) {
    if (r >= slots_.size()) fail(f_.name, "register %" + std::to_string(r) + " out of range");
    return slots_[r];
  }
  const jir::Type &type(uint32_t r) {
    slot(r);
    return f_.regs[r];
  }
  Value *load(uint32_t r) {
    if (agg(r)) fail(f_.name, "internal error: aggregate %" + std::to_string(r) + " loaded as a value");
    return b_.CreateLoad(slot(r)->getAllocatedType(), slot(r));
  }
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
    if (from == to) return v;  // aggregates are handled by the caller
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
      case Op::Copy: copy_reg(inst.dst, arg_reg(0)); return;
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
      case Op::And: store(inst.dst, b_.CreateAnd(arg(0), arg(1))); return;
      case Op::Or: store(inst.dst, b_.CreateOr(arg(0), arg(1))); return;
      case Op::Xor: store(inst.dst, b_.CreateXor(arg(0), arg(1))); return;
      case Op::Shl: case Op::Shr: {
        // Shifting by the bit width or more is poison in LLVM; JIR takes the
        // amount modulo the width instead, like the VM.
        Value *v = arg(0);
        unsigned bits = v->getType()->getIntegerBitWidth();
        Value *amount = b_.CreateAnd(arg(1), ConstantInt::get(v->getType(), bits - 1));
        Value *r = inst.op == Op::Shl ? b_.CreateShl(v, amount)
                   : is_signed(arg_reg(0)) ? b_.CreateAShr(v, amount)
                                           : b_.CreateLShr(v, amount);
        store(inst.dst, r);
        return;
      }
      case Op::Eq: case Op::Ne: case Op::Lt: case Op::Le: case Op::Gt: case Op::Ge:
        store(inst.dst, compare(inst.op, arg(0), arg(1), is_signed(arg_reg(0))));
        return;
      case Op::Cast:
        if (type(arg_reg(0)) == type(inst.dst))
          copy_reg(inst.dst, arg_reg(0));
        else
          store(inst.dst, cast(arg_reg(0), type(inst.dst)));
        return;
      case Op::Call: {
        auto it = fns_.find(inst.text);
        if (it == fns_.end()) fail(f_.name, "call to unknown function @" + inst.text);
        call(it->second->getFunctionType(), it->second, inst, 0, "@" + inst.text);
        return;
      }
      case Op::FuncRef: {
        auto it = fns_.find(inst.text);
        if (it == fns_.end()) fail(f_.name, "funcref to unknown function @" + inst.text);
        if (type(inst.dst).kind != jir::Type::Fn) fail(f_.name, "`funcref` result is not a function type");
        store(inst.dst, it->second);
        return;
      }
      case Op::CallIndirect: {
        const jir::Type &t = type(arg_reg(0));
        if (t.kind != jir::Type::Fn) fail(f_.name, "%" + std::to_string(arg_reg(0)) + " is not a function");
        call(types_.signature(t.params, *t.pointee), arg(0), inst, 1, "%" + std::to_string(arg_reg(0)));
        return;
      }
      case Op::Struct: {
        StructType *ty = types_.struct_type(inst.text);
        if (ty != slot(inst.dst)->getAllocatedType()) fail(f_.name, "`struct` result has the wrong type");
        if (ty->getNumElements() != inst.args.size()) fail(f_.name, "wrong number of struct fields");
        for (unsigned i = 0; i < inst.args.size(); i++)
          write_mem(b_.CreateStructGEP(ty, slot(inst.dst), i), arg_reg(i));
        return;
      }
      case Op::Variant: {
        const jir::Type &t = type(inst.dst);
        StructType *payload_ty = types_.variant(t, inst.imm);
        if (payload_ty->getNumElements() != inst.args.size()) fail(f_.name, "wrong number of variant values");
        Type *ty = types_.lower(t);
        b_.CreateStore(b_.getInt32(uint32_t(inst.imm)), b_.CreateStructGEP(ty, slot(inst.dst), 0));
        Value *payload = b_.CreateStructGEP(ty, slot(inst.dst), 1);
        for (unsigned i = 0; i < inst.args.size(); i++)
          write_mem(b_.CreateStructGEP(payload_ty, payload, i), arg_reg(i));
        return;
      }
      case Op::Tag: {
        const jir::Type &t = type(arg_reg(0));
        types_.variant(t, 0);  // checks that it is an enum
        if (slot(inst.dst)->getAllocatedType() != b_.getInt32Ty()) fail(f_.name, "`tag` result must be u32");
        store(inst.dst, b_.CreateLoad(b_.getInt32Ty(), b_.CreateStructGEP(types_.lower(t), slot(arg_reg(0)), 0)));
        return;
      }
      case Op::Payload: {
        const jir::Type &t = type(arg_reg(0));
        StructType *payload_ty = types_.variant(t, inst.imm);
        if (inst.imm2 < 0 || uint64_t(inst.imm2) >= payload_ty->getNumElements())
          fail(f_.name, "variant " + std::to_string(inst.imm) + " has no value " + std::to_string(inst.imm2));
        Value *payload = b_.CreateStructGEP(types_.lower(t), slot(arg_reg(0)), 1);
        read_mem(inst.dst, b_.CreateStructGEP(payload_ty, payload, unsigned(inst.imm2)));
        return;
      }
      case Op::Field: {
        const jir::Type &st = type(arg_reg(0));
        types_.field(st, inst.imm);  // bounds check
        read_mem(inst.dst, b_.CreateStructGEP(types_.lower(st), slot(arg_reg(0)), unsigned(inst.imm)));
        return;
      }
      case Op::SetField: {
        const jir::Type &st = type(arg_reg(0));
        types_.field(st, inst.imm);
        copy_reg(inst.dst, arg_reg(0));
        write_mem(b_.CreateStructGEP(types_.lower(st), slot(inst.dst), unsigned(inst.imm)), arg_reg(1));
        return;
      }
      case Op::Load:
        if (types_.lower(type(inst.dst)) != pointee(arg_reg(0))) fail(f_.name, "loaded value has the wrong type");
        read_mem(inst.dst, arg(0));
        return;
      case Op::Store:
        if (types_.lower(type(arg_reg(1))) != pointee(arg_reg(0))) fail(f_.name, "stored value has the wrong type");
        write_mem(arg(0), arg_reg(1));
        return;
      case Op::Addr: store(inst.dst, slot(arg_reg(0))); return;
      case Op::FieldPtr: {
        const jir::Type &t = type(arg_reg(0));
        if (t.kind != jir::Type::Ptr) fail(f_.name, "`fieldptr` needs a pointer");
        types_.field(*t.pointee, inst.imm);
        store(inst.dst, b_.CreateStructGEP(types_.lower(*t.pointee), arg(0), unsigned(inst.imm)));
        return;
      }
      case Op::Array: {
        const jir::Type &arr = array_of(inst.dst, false);
        if (arr.count != inst.args.size()) fail(f_.name, "wrong number of array elements");
        Type *ty = types_.lower(arr);
        for (unsigned i = 0; i < inst.args.size(); i++)
          write_mem(b_.CreateConstGEP2_64(ty, slot(inst.dst), 0, i), arg_reg(i));
        return;
      }
      case Op::Splat: splat(inst.dst, arg_reg(0)); return;
      case Op::Elem: {
        // Registers live in allocas, so index the alloca instead of the loaded value.
        const jir::Type &arr = array_of(arg_reg(0), false);
        read_mem(inst.dst, elem_addr(arr, slot(arg_reg(0)), arg(1)));
        return;
      }
      case Op::SetElem: {
        const jir::Type &arr = array_of(arg_reg(0), false);
        if (type(inst.dst) != arr) fail(f_.name, "`setelem` result has the wrong type");
        Value *index = arg(1);
        copy_reg(inst.dst, arg_reg(0));
        write_mem(elem_addr(arr, slot(inst.dst), index), arg_reg(2));
        return;
      }
      case Op::ElemPtr: {
        const jir::Type &arr = array_of(arg_reg(0), true);
        store(inst.dst, elem_addr(arr, arg(0), arg(1)));
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
      case Op::Asm: {
        std::vector<Value *> args;
        std::vector<Type *> arg_types;
        for (size_t i = 0; i < inst.args.size(); i++) {
          Value *v = arg(i);
          // There are no 1-bit registers: pass bools as bytes.
          if (type(arg_reg(i)).kind == jir::Type::Bool) v = b_.CreateZExt(v, b_.getInt8Ty());
          args.push_back(v);
          arg_types.push_back(v->getType());
        }
        bool has_out = type(inst.dst).kind != jir::Type::Unit;
        Type *ret = has_out ? types_.lower(type(inst.dst)) : b_.getVoidTy();
        std::string cons = inst.constraints;
        bool x86 = triple_.getArch() == Triple::x86_64;
        // Like clang, assume x86 asm may change the flags and direction state.
        if (x86) cons += std::string(cons.empty() ? "" : ",") + "~{dirflag},~{fpsr},~{flags}";
        auto *fty = FunctionType::get(ret, arg_types, false);
        if (Error e = InlineAsm::verify(fty, cons)) {
          std::string msg = toString(std::move(e));
          fail(f_.name, "invalid asm constraints \"" + inst.constraints + "\": " + msg);
        }
        auto *ia = InlineAsm::get(fty, inst.text, cons, /*hasSideEffects=*/true, /*isAlignStack=*/false,
                                  x86 ? InlineAsm::AD_Intel : InlineAsm::AD_ATT);
        Value *result = b_.CreateCall(ia, args);
        store(inst.dst, has_out ? result : unit());
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
        } else if (sret_) {
          write_mem(sret_, t.reg);
          b_.CreateRetVoid();
        } else {
          b_.CreateRet(load(t.reg));
        }
        return;
    }
  }
};

// Defines `kind(dst, src_or_byte, n)` as a byte loop, where `kind` is "memcpy",
// "memmove" or "memset". The definitions are weak so a real libc can take over.
void define_mem_function(Module &m, const std::string &kind) {
  LLVMContext &ctx = m.getContext();
  Type *ptr = PointerType::get(ctx, 0);
  Type *i64 = Type::getInt64Ty(ctx);
  Type *i8 = Type::getInt8Ty(ctx);
  bool is_set = kind == "memset";
  bool is_move = kind == "memmove";
  auto *ty = FunctionType::get(ptr, {ptr, is_set ? Type::getInt32Ty(ctx) : ptr, i64}, false);
  Function *f = Function::Create(ty, GlobalValue::WeakAnyLinkage, kind, m);
  // The loop below must not be recognized as a call to itself.
  f->addFnAttr("no-builtins");
  f->addFnAttr(Attribute::NoUnwind);

  Value *dst = f->getArg(0), *src = f->getArg(1), *n = f->getArg(2);
  auto *entry = BasicBlock::Create(ctx, "entry", f);
  auto *pick = is_move ? BasicBlock::Create(ctx, "pick", f) : nullptr;
  auto *fwd = BasicBlock::Create(ctx, "forward", f);
  auto *bwd = is_move ? BasicBlock::Create(ctx, "backward", f) : nullptr;
  auto *done = BasicBlock::Create(ctx, "done", f);

  IRBuilder<> b(entry);
  b.CreateCondBr(b.CreateICmpEQ(n, b.getInt64(0)), done, is_move ? pick : fwd);
  Value *last = nullptr;
  if (is_move) {
    // Copy backwards when the destination starts after the source, so an
    // overlapping tail is read before it is overwritten.
    b.SetInsertPoint(pick);
    Value *after = b.CreateICmpUGT(b.CreatePtrToInt(dst, i64), b.CreatePtrToInt(src, i64));
    last = b.CreateSub(n, b.getInt64(1));
    b.CreateCondBr(after, bwd, fwd);
  }

  // One loop over i = start, start +/- 1, ...: dst[i] = src[i] (or the byte).
  auto loop = [&](BasicBlock *bb, BasicBlock *pred, Value *start, bool backward) {
    b.SetInsertPoint(bb);
    PHINode *i = b.CreatePHI(i64, 2);
    i->addIncoming(start, pred);
    Value *byte = is_set ? b.CreateTrunc(src, i8) : b.CreateLoad(i8, b.CreateGEP(i8, src, i));
    b.CreateStore(byte, b.CreateGEP(i8, dst, i));
    Value *next = backward ? b.CreateSub(i, b.getInt64(1)) : b.CreateAdd(i, b.getInt64(1));
    i->addIncoming(next, bb);
    Value *more = backward ? b.CreateICmpNE(i, b.getInt64(0)) : b.CreateICmpULT(next, n);
    b.CreateCondBr(more, bb, done);
  };
  loop(fwd, is_move ? pick : entry, b.getInt64(0), false);
  if (is_move) loop(bwd, pick, last, true);

  b.SetInsertPoint(done);
  b.CreateRet(dst);
}

}  // namespace

std::unique_ptr<llvm::Module> codegen(const jir::Module &m, LLVMContext &ctx, const Triple &triple,
                                      const DataLayout &dl) {
  if (m.profile != jir::Profile::Freestanding)
    throw CodegenError("only freestanding modules can be compiled natively (for now)");

  auto mod = std::make_unique<Module>("jihoo", ctx);
  mod->setDataLayout(dl);
  Types types(ctx, m, dl);

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
  for (const char *kind : {"memcpy", "memmove", "memset"}) define_mem_function(*mod, kind);

  std::string err;
  raw_string_ostream os(err);
  if (verifyModule(*mod, &os)) throw CodegenError("LLVM verifier failed:\n" + os.str());
  return mod;
}

}  // namespace jihoo
