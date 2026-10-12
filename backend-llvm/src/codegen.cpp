// JIR -> LLVM IR.
//
// Every JIR register becomes an alloca of its type in the entry block; mem2reg
// (part of the optimization pipeline) turns them back into SSA values. `addr %r`
// is simply the address of that alloca, which mem2reg then leaves in memory.
//
// Type mapping: iN/uN -> iN, f32/f64 -> float/double, bool -> i1, *T and
// fn(...) -> R -> ptr, unit -> {} (empty struct), $S -> a named LLVM struct,
// [N x T] -> [N x T]. `str` is a GC type and never reaches this backend.
//
// Aggregates (structs and arrays) are never loaded or stored as SSA values: LLVM
// handles large first-class aggregates very poorly (a 64 KiB array copy takes
// minutes to compile). Instead they are copied with `llvm.memcpy`, accessed with
// GEPs, passed to functions by pointer (the callee copies), and returned through
// a hidden `sret` pointer. Freestanding modules get weak `memcpy`/`memmove`/
// `memset` definitions, since there is no libc to provide them.
//
// The program's functions are named `jihoo.<name>` so they cannot clash with C
// symbols (but a freestanding `_start`). Native modules link with libc:
// `extern` functions are declared with the C calling convention, structs
// passed as the target's C ABI says (cabi.cpp), C's `main` calls `jihoo.main`,
// and `print` calls the C library. An exported function gets a C function of
// its own name that calls it.
//
// Array element access is bounds-checked and integer division is checked for a
// zero divisor; both trap (`llvm.trap`) when the check fails. Raw pointer
// arithmetic is not checked.

#include "codegen.h"
#include "cabi.h"

#include <algorithm>
#include <functional>
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
      case jir::Type::Float: return t.bits == 32 ? Type::getFloatTy(ctx_) : Type::getDoubleTy(ctx_);
      case jir::Type::Ptr:
      case jir::Type::Fn: return PointerType::get(ctx_, 0);
      case jir::Type::Struct: return info(t.name).ty;
      case jir::Type::Array: return ArrayType::get(lower(*t.pointee), t.count);
      // `{ T *data, i64 len, i64 cap }`; see `Glue`. A `str` is the same
      // header over bytes.
      case jir::Type::Str:
      case jir::Type::List: {
        Type *i64 = Type::getInt64Ty(ctx_);
        return StructType::get(ctx_, {PointerType::get(ctx_, 0), i64, i64});
      }
    }
    throw CodegenError("unknown type " + t.str());
  }

  // Structs, enums, arrays and list headers.
  static bool is_agg(const jir::Type &t) {
    return t.kind == jir::Type::Struct || t.kind == jir::Type::Array || t.kind == jir::Type::List ||
           t.kind == jir::Type::Str;
  }

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

// Native lists and the memory they own.
//
// A list `[T]` is a header `{ T *data, i64 len, i64 cap }` held by value like
// any aggregate; the elements live in memory from `malloc`. A type *owns*
// memory if it holds a list by value, directly or in a field, element or
// variant. Copying a header copies no elements, so who frees what is decided
// before the backend runs: the frontend's ownership pass leaves a `copy` of an
// owning value only where the original is not used again (a move), and puts a
// `clone` (a deep copy) where it is, and a `drop` where a value dies. Reading
// an owning part out of a value (`field`, `elem`, `getpath`, `payload`) gives
// a view that shares the elements, which the pass clones where it must; the
// backend drops the old part where it writes one.
//
// A `str` is a list of bytes that never changes. One whose `cap` is 0 holds
// the bytes of a literal, in the program's constant data: freeing it does
// nothing and copying it copies the header, so literals cost no memory.
//
// The glue for a type is a function per type, `jihoo.drop.T(ptr)` and
// `jihoo.clone.T(ptr dst, ptr src)`, made on first use.
class Glue {
 public:
  Glue(Module &m, const Types &types, const jir::Module &jm) : m_(m), types_(types), ctx_(m.getContext()) {
    for (const auto &s : jm.structs) structs_[s.name] = &s;
    for (const auto &e : jm.enums) enums_[e.name] = &e;
  }

  // Whether a value of type `t` owns memory that a drop frees.
  bool owns(const jir::Type &t) {
    switch (t.kind) {
      case jir::Type::List: case jir::Type::Str: return true;
      case jir::Type::Array: return owns(*t.pointee);
      case jir::Type::Struct: {
        auto memo = owns_.find(t.name);
        if (memo != owns_.end()) return memo->second;
        owns_[t.name] = false;  // a type holds itself only through a list, which owns anyway
        bool result = false;
        if (auto s = structs_.find(t.name); s != structs_.end()) {
          for (const auto &f : s->second->fields) result = result || owns(f.second);
        } else if (auto e = enums_.find(t.name); e != enums_.end()) {
          for (const auto &v : e->second->variants)
            for (const auto &p : v.second) result = result || owns(p);
        }
        owns_[t.name] = result;
        return result;
      }
      default: return false;
    }
  }

  uint64_t size(const jir::Type &t) const { return m_.getDataLayout().getTypeAllocSize(types_.lower(t)); }

  StructType *header() const {
    Type *ptr = PointerType::get(ctx_, 0);
    Type *i64 = Type::getInt64Ty(ctx_);
    return StructType::get(ctx_, {ptr, i64, i64});
  }

  FunctionCallee malloc_fn() {
    return m_.getOrInsertFunction("malloc", FunctionType::get(ptr(), {Type::getInt64Ty(ctx_)}, false));
  }
  FunctionCallee realloc_fn() {
    return m_.getOrInsertFunction("realloc", FunctionType::get(ptr(), {ptr(), Type::getInt64Ty(ctx_)}, false));
  }
  FunctionCallee free_fn() {
    return m_.getOrInsertFunction("free", FunctionType::get(Type::getVoidTy(ctx_), {ptr()}, false));
  }

  // `void jihoo.drop.T(T *v)`: frees what the value at `v` owns.
  Function *drop(const jir::Type &t) {
    return get(drops_, "jihoo.drop." + t.str(), {ptr()}, [&](IRBuilder<> &b, Function *fn) {
      drop_body(b, fn, t, fn->getArg(0));
    });
  }

  // `void jihoo.clone.T(T *dst, T *src)`: a deep copy of the value at `src`
  // into `dst`, which may be the same place.
  Function *clone(const jir::Type &t) {
    return get(clones_, "jihoo.clone." + t.str(), {ptr(), ptr()}, [&](IRBuilder<> &b, Function *fn) {
      clone_body(b, fn, t, fn->getArg(0), fn->getArg(1));
    });
  }

  // Calls `body(i)` for `i` from 0 to `n` (an i64) at the builder's position.
  static void loop(IRBuilder<> &b, Value *n, const std::function<void(Value *)> &body) {
    LLVMContext &ctx = b.getContext();
    Function *fn = b.GetInsertBlock()->getParent();
    BasicBlock *pre = b.GetInsertBlock();
    auto *head = BasicBlock::Create(ctx, "each", fn);
    auto *step = BasicBlock::Create(ctx, "each.body", fn);
    auto *done = BasicBlock::Create(ctx, "each.done", fn);
    b.CreateBr(head);
    b.SetInsertPoint(head);
    PHINode *i = b.CreatePHI(b.getInt64Ty(), 2);
    i->addIncoming(b.getInt64(0), pre);
    b.CreateCondBr(b.CreateICmpULT(i, n), step, done);
    b.SetInsertPoint(step);
    body(i);
    i->addIncoming(b.CreateAdd(i, b.getInt64(1)), b.GetInsertBlock());
    b.CreateBr(head);
    b.SetInsertPoint(done);
  }

  // `n * size_of(elem)`, checking that it does not overflow.
  Value *byte_count(IRBuilder<> &b, const jir::Type &elem, Value *n, const std::function<void(Value *)> &check) {
    Value *mul = b.CreateBinaryIntrinsic(Intrinsic::umul_with_overflow, n, b.getInt64(size(elem)));
    check(b.CreateNot(b.CreateExtractValue(mul, 1)));
    return b.CreateExtractValue(mul, 0);
  }

 private:
  Module &m_;
  const Types &types_;
  LLVMContext &ctx_;
  std::unordered_map<std::string, const jir::StructDef *> structs_;
  std::unordered_map<std::string, const jir::EnumDef *> enums_;
  std::unordered_map<std::string, bool> owns_;
  std::unordered_map<std::string, Function *> drops_, clones_;

  Type *ptr() const { return PointerType::get(ctx_, 0); }

  Function *get(std::unordered_map<std::string, Function *> &memo, const std::string &name,
                std::vector<Type *> params, const std::function<void(IRBuilder<> &, Function *)> &body) {
    if (auto it = memo.find(name); it != memo.end()) return it->second;
    auto *fn = Function::Create(FunctionType::get(Type::getVoidTy(ctx_), params, false),
                                GlobalValue::InternalLinkage, name, m_);
    memo[name] = fn;  // before the body, which may need the function itself
    IRBuilder<> b(BasicBlock::Create(ctx_, "entry", fn));
    body(b, fn);
    b.CreateRetVoid();
    return fn;
  }

  // Traps unless `ok`, inside glue.
  static void check(IRBuilder<> &b, Value *ok) {
    LLVMContext &ctx = b.getContext();
    Function *fn = b.GetInsertBlock()->getParent();
    auto *next = BasicBlock::Create(ctx, "ok", fn);
    auto *trap = BasicBlock::Create(ctx, "trap", fn);
    b.CreateCondBr(ok, next, trap);
    IRBuilder<> tb(trap);
    tb.CreateCall(Intrinsic::getOrInsertDeclaration(trap->getModule(), Intrinsic::trap));
    tb.CreateUnreachable();
    b.SetInsertPoint(next);
  }

  // Calls `each(dst_part, src_part, type)` for every owning part of a `t` at
  // `dst` (and the same part at `src`, if given) that is not inside a list.
  void parts(IRBuilder<> &b, const jir::Type &t, Value *dst, Value *src,
             const std::function<void(Value *, Value *, const jir::Type &)> &each) {
    switch (t.kind) {
      case jir::Type::Array: {
        const jir::Type &elem = *t.pointee;
        if (!owns(elem)) return;
        Type *ty = types_.lower(t);
        loop(b, b.getInt64(t.count), [&](Value *i) {
          Value *d = b.CreateGEP(ty, dst, {b.getInt64(0), i});
          Value *s = src ? b.CreateGEP(ty, src, {b.getInt64(0), i}) : nullptr;
          each(d, s, elem);
        });
        return;
      }
      case jir::Type::Struct: {
        Type *ty = types_.lower(t);
        if (auto it = structs_.find(t.name); it != structs_.end()) {
          const auto &fields = it->second->fields;
          for (unsigned k = 0; k < fields.size(); k++) {
            if (!owns(fields[k].second)) continue;
            Value *d = b.CreateStructGEP(ty, dst, k);
            Value *s = src ? b.CreateStructGEP(ty, src, k) : nullptr;
            each(d, s, fields[k].second);
          }
          return;
        }
        // An enum: only the active variant's payload.
        const jir::EnumDef *e = enums_.at(t.name);
        Function *fn = b.GetInsertBlock()->getParent();
        auto *done = BasicBlock::Create(ctx_, "variant.done", fn);
        Value *tag = b.CreateLoad(b.getInt32Ty(), b.CreateStructGEP(ty, dst, 0));
        SwitchInst *sw = b.CreateSwitch(tag, done);
        for (unsigned v = 0; v < e->variants.size(); v++) {
          const auto &payload = e->variants[v].second;
          if (std::none_of(payload.begin(), payload.end(), [&](const jir::Type &p) { return owns(p); })) continue;
          auto *bb = BasicBlock::Create(ctx_, "variant", fn);
          sw->addCase(b.getInt32(v), bb);
          b.SetInsertPoint(bb);
          StructType *pty = types_.variant(t, v);
          Value *dp = b.CreateStructGEP(ty, dst, 1);
          Value *sp = src ? b.CreateStructGEP(ty, src, 1) : nullptr;
          for (unsigned k = 0; k < payload.size(); k++) {
            if (!owns(payload[k])) continue;
            each(b.CreateStructGEP(pty, dp, k), sp ? b.CreateStructGEP(pty, sp, k) : nullptr, payload[k]);
          }
          b.CreateBr(done);
        }
        b.SetInsertPoint(done);
        return;
      }
      default: return;
    }
  }

  void drop_body(IRBuilder<> &b, Function *fn, const jir::Type &t, Value *v) {
    if (t.kind == jir::Type::Str) {
      // A literal's bytes are not ours to free.
      auto *heap = BasicBlock::Create(ctx_, "heap", fn);
      auto *done = BasicBlock::Create(ctx_, "done", fn);
      Value *cap = b.CreateLoad(b.getInt64Ty(), b.CreateStructGEP(header(), v, 2));
      b.CreateCondBr(b.CreateICmpNE(cap, b.getInt64(0)), heap, done);
      b.SetInsertPoint(heap);
      b.CreateCall(free_fn(), {b.CreateLoad(ptr(), b.CreateStructGEP(header(), v, 0))});
      b.CreateBr(done);
      b.SetInsertPoint(done);
      return;
    }
    if (t.kind != jir::Type::List) {
      parts(b, t, v, nullptr, [&](Value *d, Value *, const jir::Type &pt) { b.CreateCall(drop(pt), {d}); });
      return;
    }
    const jir::Type &elem = *t.pointee;
    Value *data = b.CreateLoad(ptr(), b.CreateStructGEP(header(), v, 0));
    if (owns(elem)) {
      Value *len = b.CreateLoad(b.getInt64Ty(), b.CreateStructGEP(header(), v, 1));
      Type *ety = types_.lower(elem);
      loop(b, len, [&](Value *i) { b.CreateCall(drop(elem), {b.CreateGEP(ety, data, {i})}); });
    }
    b.CreateCall(free_fn(), {data});
  }

  void clone_body(IRBuilder<> &b, Function *fn, const jir::Type &t, Value *dst, Value *src) {
    const DataLayout &dl = m_.getDataLayout();
    if (t.kind == jir::Type::Str) {
      // A literal stays a literal; other bytes are copied.
      Value *data = b.CreateLoad(ptr(), b.CreateStructGEP(header(), src, 0));
      Value *len = b.CreateLoad(b.getInt64Ty(), b.CreateStructGEP(header(), src, 1));
      Value *cap = b.CreateLoad(b.getInt64Ty(), b.CreateStructGEP(header(), src, 2));
      auto *heap = BasicBlock::Create(ctx_, "heap", fn);
      auto *done = BasicBlock::Create(ctx_, "done", fn);
      BasicBlock *from = b.GetInsertBlock();
      b.CreateCondBr(b.CreateICmpNE(cap, b.getInt64(0)), heap, done);
      b.SetInsertPoint(heap);
      Value *copy = b.CreateCall(malloc_fn(), {len});
      check(b, b.CreateOr(b.CreateIsNotNull(copy), b.CreateICmpEQ(len, b.getInt64(0))));
      b.CreateMemCpy(copy, Align(1), data, Align(1), len);
      BasicBlock *copied = b.GetInsertBlock();
      b.CreateBr(done);
      b.SetInsertPoint(done);
      PHINode *p = b.CreatePHI(ptr(), 2);
      p->addIncoming(data, from);
      p->addIncoming(copy, copied);
      b.CreateStore(p, b.CreateStructGEP(header(), dst, 0));
      b.CreateStore(len, b.CreateStructGEP(header(), dst, 1));
      b.CreateStore(cap, b.CreateStructGEP(header(), dst, 2));
      return;
    }
    if (t.kind != jir::Type::List) {
      // Copy the bytes, then replace each owning part with a copy of its own.
      Type *ty = types_.lower(t);
      Align a = dl.getABITypeAlign(ty);
      b.CreateMemMove(dst, a, src, a, dl.getTypeAllocSize(ty));
      parts(b, t, dst, src, [&](Value *d, Value *s, const jir::Type &pt) { b.CreateCall(clone(pt), {d, s}); });
      return;
    }
    // Read the source header before writing `dst`, which may be the same place.
    const jir::Type &elem = *t.pointee;
    Value *data = b.CreateLoad(ptr(), b.CreateStructGEP(header(), src, 0));
    Value *len = b.CreateLoad(b.getInt64Ty(), b.CreateStructGEP(header(), src, 1));
    auto chk = [&](Value *ok) { check(b, ok); };
    Value *bytes = byte_count(b, elem, len, chk);
    Value *copy = b.CreateCall(malloc_fn(), {bytes});
    check(b, b.CreateOr(b.CreateIsNotNull(copy), b.CreateICmpEQ(bytes, b.getInt64(0))));
    Type *ety = types_.lower(elem);
    if (owns(elem)) {
      loop(b, len, [&](Value *i) {
        b.CreateCall(clone(elem), {b.CreateGEP(ety, copy, {i}), b.CreateGEP(ety, data, {i})});
      });
    } else {
      Align a = dl.getABITypeAlign(ety);
      b.CreateMemCpy(copy, a, data, a, bytes);
    }
    b.CreateStore(copy, b.CreateStructGEP(header(), dst, 0));
    b.CreateStore(len, b.CreateStructGEP(header(), dst, 1));
    b.CreateStore(len, b.CreateStructGEP(header(), dst, 2));
  }
};

// `print` in native modules (defined below).
FunctionCallee libc_fn(Module &m, const char *name);
Function *print_float_fn(Module &m);

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

// A C function the module calls, and how its C signature passes its values.
struct CFunc {
  const jir::ExternFn *def;
  cabi::Signature sig;
};

class FnGen {
 public:
  // `fns` holds every function and extern function by JIR name; `externs` says
  // which are extern. With `exit_on_return`, returning exits the process (for
  // a freestanding `_start`). `native` modules link with libc.
  FnGen(const jir::Function &f, Function *llfn, Module &mod, const Types &types,
        const std::unordered_map<std::string, Function *> &fns,
        const std::unordered_map<std::string, CFunc> &externs, const Triple &triple, bool exit_on_return,
        bool native, Glue &glue)
      : f_(f), fn_(llfn), mod_(mod), types_(types), glue_(glue), fns_(fns), externs_(externs), triple_(triple),
        exit_on_return_(exit_on_return), native_(native), ctx_(mod.getContext()), b_(ctx_), i64_(Type::getInt64Ty(ctx_)) {}

  void run() {
    auto *entry = BasicBlock::Create(ctx_, "entry", fn_);
    for (size_t i = 0; i < f_.blocks.size(); i++)
      blocks_.push_back(BasicBlock::Create(ctx_, "bb" + std::to_string(i), fn_));

    b_.SetInsertPoint(entry);
    for (size_t r = 0; r < f_.regs.size(); r++)
      slots_.push_back(b_.CreateAlloca(types_.lower(f_.regs[r]), nullptr, "r" + std::to_string(r)));
    // A register that owns memory starts out owning none, so freeing it
    // before anything was written to it frees nothing.
    for (size_t r = f_.params.size(); r < f_.regs.size(); r++)
      if (glue_.owns(f_.regs[r]))
        b_.CreateMemSet(slots_[r], b_.getInt8(0), glue_.size(f_.regs[r]), slots_[r]->getAlign());
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
  Glue &glue_;
  const std::unordered_map<std::string, Function *> &fns_;
  const std::unordered_map<std::string, CFunc> &externs_;
  const Triple &triple_;
  bool exit_on_return_;
  bool native_;
  LLVMContext &ctx_;
  IRBuilder<> b_;
  Type *i64_;
  std::vector<AllocaInst *> slots_;
  std::vector<BasicBlock *> blocks_;
  BasicBlock *trap_ = nullptr;  // shared target of failed bounds and division checks
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

  // Calls C function `x` (declared as `fn`) with the operands of `inst`, as
  // its C signature says. Arguments past the fixed parameters get C's default
  // promotions: bools and integers narrower than `int` widen to 32 bits, and
  // `f32` to `f64`.
  void call_extern(const CFunc &x, Function *fn, const jir::Inst &inst) {
    const jir::ExternFn &e = *x.def;
    const cabi::Signature &s = x.sig;
    size_t n = inst.args.size();
    if (n < e.params.size() || (!e.variadic && n != e.params.size()))
      fail(f_.name, "wrong number of arguments to @" + e.name);
    std::vector<Value *> args;
    // A struct result in memory: C writes it straight into the register.
    if (s.ret.kind == cabi::Arg::Indirect) args.push_back(slot(inst.dst));
    for (size_t i = 0; i < n; i++) {
      uint32_t r = inst.args[i];
      const jir::Type &t = type(r);
      if (i < e.params.size()) {
        if (t != e.params[i]) fail(f_.name, "argument " + std::to_string(i + 1) + " to @" + e.name + " has the wrong type");
        c_arg(s.params[i], r, args);
        continue;
      }
      Value *v = load(r);
      if (t.kind == jir::Type::Bool) v = b_.CreateZExt(v, b_.getInt32Ty());
      else if (t.kind == jir::Type::Int && t.bits < 32) v = b_.CreateIntCast(v, b_.getInt32Ty(), t.is_signed);
      else if (t.kind == jir::Type::Float && t.bits == 32) v = b_.CreateFPExt(v, b_.getDoubleTy());
      else if (t.kind != jir::Type::Int && t.kind != jir::Type::Float && t.kind != jir::Type::Ptr && t.kind != jir::Type::Fn)
        fail(f_.name, t.str() + " cannot be passed to C");
      args.push_back(v);
    }
    CallInst *c = b_.CreateCall(fn->getFunctionType(), fn, args);
    c->setAttributes(fn->getAttributes());  // extensions, `sret` and `byval`, as C wants
    switch (s.ret.kind) {
      case cabi::Arg::Direct: store(inst.dst, e.ret.kind == jir::Type::Unit ? unit() : c); return;
      case cabi::Arg::Expand: {
        // The struct comes back in registers, as `mem`.
        Value *tmp = temp(s.ret.mem, type(inst.dst));
        b_.CreateStore(c, tmp);
        memcopy(slot(inst.dst), tmp, type(inst.dst));
        return;
      }
      case cabi::Arg::Indirect: return;
    }
  }

  // Adds the C arguments that pass register `r` as `a` says to `args`.
  void c_arg(const cabi::Arg &a, uint32_t r, std::vector<Value *> &args) {
    switch (a.kind) {
      case cabi::Arg::Direct: args.push_back(load(r)); return;
      case cabi::Arg::Expand: {
        // The struct's bytes, read as the parts the registers take.
        Value *tmp = temp(a.mem, type(r));
        memcopy(tmp, slot(r), type(r));
        if (a.parts.size() == 1) {
          args.push_back(b_.CreateLoad(a.mem, tmp));
        } else {
          for (unsigned j = 0; j < a.parts.size(); j++)
            args.push_back(b_.CreateLoad(a.parts[j], b_.CreateStructGEP(a.mem, tmp, j)));
        }
        return;
      }
      case cabi::Arg::Indirect:
        if (a.byval) {
          args.push_back(slot(r));  // LLVM copies it onto the stack
        } else {
          // The callee may change the copy, so it is not the register itself.
          Value *copy = temp(types_.lower(type(r)), type(r));
          memcopy(copy, slot(r), type(r));
          args.push_back(copy);
        }
        return;
    }
  }

  // A temporary of LLVM type `ty` that also holds a `t`, in the entry block (so
  // a loop does not grow the stack).
  AllocaInst *temp(Type *ty, const jir::Type &t) {
    BasicBlock &entry = fn_->getEntryBlock();
    IRBuilder<> eb(&entry, entry.getFirstInsertionPt());
    AllocaInst *a = eb.CreateAlloca(ty);
    const DataLayout &dl = mod_.getDataLayout();
    a->setAlignment(std::max({a->getAlign(), dl.getABITypeAlign(types_.lower(t)), Align(8)}));
    return a;
  }

  // `print %r` in a native module, through the C library's stdout: integers
  // with `printf`, bools and C strings (`*u8`) with `puts`, floats with
  // `jihoo.print_float`.
  void print(uint32_t r) {
    if (!native_) fail(f_.name, "`print` needs the C library; it is only available in hosted and native modules");
    const jir::Type &t = type(r);
    auto str = [&](const char *s) { return b_.CreateGlobalString(s, ".str.print", 0, &mod_); };
    if (t.kind == jir::Type::Str) {
      Value *n = b_.CreateTrunc(list_field(slot(r), 1), b_.getInt32Ty());
      b_.CreateCall(libc_fn(mod_, "printf"), {str("%.*s\n"), n, list_field(slot(r), 0)});
      return;
    }
    Value *v = load(r);
    switch (t.kind) {
      case jir::Type::Int:
        b_.CreateCall(libc_fn(mod_, "printf"), {str(t.is_signed ? "%lld\n" : "%llu\n"), b_.CreateIntCast(v, i64_, t.is_signed)});
        return;
      case jir::Type::Bool:
        b_.CreateCall(libc_fn(mod_, "puts"), {b_.CreateSelect(v, str("true"), str("false"))});
        return;
      case jir::Type::Ptr:
        if (*t.pointee != jir::Type::integer(8, false)) break;
        b_.CreateCall(libc_fn(mod_, "puts"), {v});
        return;
      case jir::Type::Float:
        if (t.bits == 32) v = b_.CreateFPExt(v, b_.getDoubleTy());
        b_.CreateCall(print_float_fn(mod_), {v, b_.getInt1(t.bits == 32), ConstantPointerNull::get(PointerType::get(ctx_, 0))});
        return;
      default: break;
    }
    fail(f_.name, "cannot print " + t.str());
  }

  // `spawn %f(%a, ...)` in a native module: a detached thread of the C
  // library's that calls `f(a, ...)`. The function value and the arguments
  // are copied into memory from `malloc`, which the thread frees after the
  // call; failing to get the memory or the thread traps.
  void spawn(const jir::Inst &inst) {
    if (!native_) fail(f_.name, "`spawn` starts a thread of the C library's; it is only available in native modules");
    const jir::Type &ft = type(inst.args.at(0));
    if (ft.kind != jir::Type::Fn) fail(f_.name, "`spawn` needs a function value");
    if (ft.params.size() != inst.args.size() - 1) fail(f_.name, "wrong number of arguments to `spawn`");
    Type *ptr = PointerType::get(ctx_, 0);
    std::vector<Type *> fields{ptr};
    for (size_t i = 1; i < inst.args.size(); i++) {
      if (type(inst.args[i]) != ft.params[i - 1])
        fail(f_.name, "argument " + std::to_string(i) + " to `spawn` has the wrong type");
      fields.push_back(types_.lower(ft.params[i - 1]));
    }
    StructType *env_ty = StructType::get(ctx_, fields);
    uint64_t size = mod_.getDataLayout().getTypeAllocSize(env_ty);

    FunctionCallee malloc_fn = mod_.getOrInsertFunction("malloc", FunctionType::get(ptr, {i64_}, false));
    Value *env = b_.CreateCall(malloc_fn, {b_.getInt64(size)});
    check(b_.CreateIsNotNull(env), "spawn.env");
    b_.CreateStore(load(inst.args[0]), b_.CreateStructGEP(env_ty, env, 0));
    for (size_t i = 1; i < inst.args.size(); i++)
      write_mem(b_.CreateStructGEP(env_ty, env, unsigned(i)), inst.args[i]);

    // pthread_t is an unsigned long on Linux, on x86_64 and aarch64 alike.
    Type *i32 = b_.getInt32Ty();
    FunctionCallee create = mod_.getOrInsertFunction("pthread_create", FunctionType::get(i32, {ptr, ptr, ptr, ptr}, false));
    FunctionCallee detach = mod_.getOrInsertFunction("pthread_detach", FunctionType::get(i32, {i64_}, false));
    AllocaInst *id = b_.CreateAlloca(i64_, nullptr, "thread");
    Value *err = b_.CreateCall(create, {id, ConstantPointerNull::get(PointerType::get(ctx_, 0)),
                                        spawn_trampoline(ft, env_ty), env});
    check(b_.CreateICmpEQ(err, ConstantInt::get(i32, 0)), "spawn.started");
    b_.CreateCall(detach, {b_.CreateLoad(i64_, id)});
  }

  // A C thread start function, `void *(void *env)`, that calls the function
  // value in `env` (of type `ft`) with the arguments after it, frees `env`,
  // and returns null. Aggregates are passed by pointer, as everywhere: here
  // pointers into `env`, which the callee copies before `env` is freed.
  Function *spawn_trampoline(const jir::Type &ft, StructType *env_ty) {
    Type *ptr = PointerType::get(ctx_, 0);
    auto *fn = Function::Create(FunctionType::get(ptr, {ptr}, false), GlobalValue::InternalLinkage, "jihoo.go", mod_);
    IRBuilder<> b(BasicBlock::Create(ctx_, "entry", fn));
    Value *env = fn->getArg(0);
    std::vector<Value *> args;
    const jir::Type &ret = *ft.pointee;
    if (Types::is_agg(ret)) args.push_back(b.CreateAlloca(types_.lower(ret)));  // the result, dropped
    Value *callee = b.CreateLoad(ptr, b.CreateStructGEP(env_ty, env, 0));
    for (size_t i = 0; i < ft.params.size(); i++) {
      Value *field = b.CreateStructGEP(env_ty, env, unsigned(i + 1));
      args.push_back(Types::is_agg(ft.params[i]) ? field : b.CreateLoad(types_.lower(ft.params[i]), field));
    }
    b.CreateCall(types_.signature(ft.params, ret), callee, args);
    // The thread owns its arguments (they were moved into `env`); the callee
    // only borrowed them.
    if (Types::is_agg(ret) && glue_.owns(ret)) b.CreateCall(glue_.drop(ret), {args[0]});
    for (size_t i = 0; i < ft.params.size(); i++)
      if (glue_.owns(ft.params[i]))
        b.CreateCall(glue_.drop(ft.params[i]), {b.CreateStructGEP(env_ty, env, unsigned(i + 1))});
    b.CreateCall(mod_.getOrInsertFunction("free", FunctionType::get(Type::getVoidTy(ctx_), {ptr}, false)), {env});
    b.CreateRet(ConstantPointerNull::get(PointerType::get(ctx_, 0)));
    return fn;
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

  // Continues in a new block if `ok` holds, traps otherwise.
  void check(Value *ok, const char *name) {
    if (!trap_) {
      trap_ = BasicBlock::Create(ctx_, "trap", fn_);
      IRBuilder<> tb(trap_);
      tb.CreateCall(Intrinsic::getOrInsertDeclaration(&mod_, Intrinsic::trap));
      tb.CreateUnreachable();
    }
    auto *next = BasicBlock::Create(ctx_, name, fn_);
    b_.CreateCondBr(ok, next, trap_);
    b_.SetInsertPoint(next);
  }

  // Continues in a new block if `index < len`, traps otherwise.
  void bounds_check(Value *index, uint64_t len) {
    // Unsigned, so negative indices are out of range too.
    check(b_.CreateICmpULT(index, b_.getInt64(len)), "in_bounds");
  }

  // Integer `div` or `rem` with the VM's semantics: dividing by zero traps
  // (LLVM leaves it undefined), and the one signed overflow, MIN / -1, wraps
  // to MIN with remainder 0 (LLVM leaves that undefined too).
  Value *divide(bool rem, bool is_signed, Value *a, Value *b) {
    check(b_.CreateICmpNE(b, ConstantInt::get(b->getType(), 0)), "nonzero");
    if (!is_signed) return rem ? b_.CreateURem(a, b) : b_.CreateUDiv(a, b);
    // Dividing by 1 instead of -1 is always defined; the result is then fixed up.
    Value *minus_one = b_.CreateICmpEQ(b, ConstantInt::getSigned(b->getType(), -1));
    Value *safe = b_.CreateSelect(minus_one, ConstantInt::get(b->getType(), 1), b);
    if (rem) return b_.CreateSelect(minus_one, ConstantInt::get(a->getType(), 0), b_.CreateSRem(a, safe));
    return b_.CreateSelect(minus_one, b_.CreateNeg(a), b_.CreateSDiv(a, safe));
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

  // Address and JIR type of the part at `path` inside the aggregate of type `t`
  // at `base`. Elements are bounds-checked.
  std::pair<Value *, const jir::Type *> path_addr(const jir::Type &t, Value *base,
                                                  const std::vector<jir::PathStep> &path) {
    const jir::Type *cur = &t;
    Value *addr = base;
    for (const auto &step : path) {
      if (step.elem) {
        if (cur->kind != jir::Type::Array && cur->kind != jir::Type::List)
          fail(f_.name, cur->str() + " is not an array or list");
        if (type(step.value) != jir::Type::integer(64, true)) fail(f_.name, "path index must be i64");
        addr = cur->kind == jir::Type::List ? list_elem_addr(*cur, addr, load(step.value))
                                             : elem_addr(*cur, addr, load(step.value));
        cur = cur->pointee.get();
      } else {
        const jir::Type &field = types_.field(*cur, step.value);
        addr = b_.CreateStructGEP(types_.lower(*cur), addr, step.value);
        cur = &field;
      }
    }
    return {addr, cur};
  }

  // Address of element `index` of the list whose header is at `header`, after
  // a bounds check against its length.
  Value *list_elem_addr(const jir::Type &list, Value *header, Value *index) {
    StructType *h = glue_.header();
    Value *len = b_.CreateLoad(i64_, b_.CreateStructGEP(h, header, 1));
    check(b_.CreateICmpULT(index, len), "in_bounds");
    Value *data = b_.CreateLoad(PointerType::get(ctx_, 0), b_.CreateStructGEP(h, header, 0));
    return b_.CreateGEP(types_.lower(*list.pointee), data, {index});
  }

  // Reads the part at `addr` into `dst`. A part that owns memory is read as a
  // view (the header only); the ownership pass puts a `clone` after the read
  // where the part must become a value of its own.
  void read_part(uint32_t dst, Value *addr) { read_mem(dst, addr); }

  // Replaces the part at `addr` by the value of `src`, which moves there,
  // freeing what the old part owned.
  void write_part(Value *addr, uint32_t src) {
    if (glue_.owns(type(src))) b_.CreateCall(glue_.drop(type(src)), {addr});
    write_mem(addr, src);
  }

  // Makes the list at `header` a new list of `n` elements of `elem`, not yet
  // written; gives the address of the first.
  Value *new_list(const jir::Type &elem, Value *n, Value *header) {
    auto chk = [&](Value *ok) { check(ok, "list.size"); };
    Value *bytes = glue_.byte_count(b_, elem, n, chk);
    Value *data = b_.CreateCall(glue_.malloc_fn(), {bytes});
    check(b_.CreateOr(b_.CreateIsNotNull(data), b_.CreateICmpEQ(bytes, b_.getInt64(0))), "list.alloc");
    StructType *h = glue_.header();
    b_.CreateStore(data, b_.CreateStructGEP(h, header, 0));
    b_.CreateStore(n, b_.CreateStructGEP(h, header, 1));
    b_.CreateStore(n, b_.CreateStructGEP(h, header, 2));
    return data;
  }

  // Copies `n` elements of `elem` from `src` to `dst` (fresh memory): deep
  // copies if they own memory.
  void copy_elems(const jir::Type &elem, Value *dst, Value *src, Value *n) {
    Type *ety = types_.lower(elem);
    if (glue_.owns(elem)) {
      Glue::loop(b_, n, [&](Value *i) {
        b_.CreateCall(glue_.clone(elem), {b_.CreateGEP(ety, dst, {i}), b_.CreateGEP(ety, src, {i})});
      });
    } else {
      Align a = mod_.getDataLayout().getABITypeAlign(ety);
      b_.CreateMemCpy(dst, a, src, a, b_.CreateMul(n, b_.getInt64(glue_.size(elem))));
    }
  }

  const jir::Type &list_of(uint32_t r) {
    if (type(r).kind != jir::Type::List) fail(f_.name, "%" + std::to_string(r) + " is not a list");
    return type(r);
  }

  bool is_str(uint32_t r) { return type(r).kind == jir::Type::Str; }

  // Makes the string at `header` new memory for `n` bytes, not yet written;
  // gives its address.
  Value *new_str(Value *n, Value *header) {
    Value *data = b_.CreateCall(glue_.malloc_fn(), {n});
    check(b_.CreateOr(b_.CreateIsNotNull(data), b_.CreateICmpEQ(n, b_.getInt64(0))), "str.alloc");
    StructType *h = glue_.header();
    b_.CreateStore(data, b_.CreateStructGEP(h, header, 0));
    b_.CreateStore(n, b_.CreateStructGEP(h, header, 1));
    b_.CreateStore(b_.getInt64(1), b_.CreateStructGEP(h, header, 2));  // not a literal
    return data;
  }

  // Sets the string at `header` to a copy of the NUL-terminated bytes at `s`.
  void str_from_c(Value *s, Value *header) {
    Value *n = b_.CreateCall(mod_.getOrInsertFunction("strlen", FunctionType::get(i64_, {PointerType::get(ctx_, 0)}, false)), {s});
    Value *data = new_str(n, header);
    b_.CreateMemCpy(data, Align(1), s, Align(1), n);
  }

  // `a op b` on strings: byte by byte, then by length, as the VM orders them.
  Value *str_compare(jir::Op op, uint32_t a, uint32_t b) {
    Value *la = list_field(slot(a), 1), *lb = list_field(slot(b), 1);
    Value *n = b_.CreateSelect(b_.CreateICmpULT(la, lb), la, lb);
    FunctionCallee memcmp = mod_.getOrInsertFunction(
        "memcmp", FunctionType::get(b_.getInt32Ty(), {PointerType::get(ctx_, 0), PointerType::get(ctx_, 0), i64_}, false));
    Value *c = b_.CreateCall(memcmp, {list_field(slot(a), 0), list_field(slot(b), 0), n});
    // -1, 0 or 1 from the bytes, else from the lengths.
    Value *by_len = b_.CreateSelect(b_.CreateICmpULT(la, lb), b_.getInt32(-1),
                                    b_.CreateSelect(b_.CreateICmpUGT(la, lb), b_.getInt32(1), b_.getInt32(0)));
    Value *order = b_.CreateSelect(b_.CreateICmpEQ(c, b_.getInt32(0)), by_len, c);
    return compare(op, order, b_.getInt32(0), true);
  }

  Value *list_field(Value *header, unsigned k) {
    Type *ty = k == 0 ? static_cast<Type *>(PointerType::get(ctx_, 0)) : i64_;
    return b_.CreateLoad(ty, b_.CreateStructGEP(glue_.header(), header, k));
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
    Value *at = b_.CreateGEP(arr_ty, slot(dst), {b_.getInt64(0), i});
    if (glue_.owns(type(value)))
      b_.CreateCall(glue_.clone(type(value)), {at, slot(value)});
    else
      write_mem(at, value);
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

  // What the pointer `r` of an atomic points to: an integer, or a pointer when
  // `pointers`.
  Type *atomic_pointee(uint32_t r, bool pointers) {
    Type *ty = pointee(r);
    const jir::Type &t = *type(r).pointee;
    if (t.kind != jir::Type::Int && !(pointers && t.kind == jir::Type::Ptr))
      fail(f_.name, "atomics cannot work on " + t.str());
    return ty;
  }

  // Integers keep their signedness in JIR; LLVM wants it on each operation.
  bool is_signed(uint32_t r) {
    const jir::Type &t = type(r);
    return t.kind == jir::Type::Int && t.is_signed;
  }

  bool is_float(uint32_t r) { return type(r).kind == jir::Type::Float; }

  // Ordered comparisons, false when either side is NaN; `!=` is unordered, so
  // NaN != NaN, as on the VM.
  Value *fcompare(jir::Op op, Value *a, Value *b) {
    using P = CmpInst::Predicate;
    using jir::Op;
    P pred;
    switch (op) {
      case Op::Eq: pred = P::FCMP_OEQ; break;
      case Op::Ne: pred = P::FCMP_UNE; break;
      case Op::Lt: pred = P::FCMP_OLT; break;
      case Op::Le: pred = P::FCMP_OLE; break;
      case Op::Gt: pred = P::FCMP_OGT; break;
      default: pred = P::FCMP_OGE; break;
    }
    return b_.CreateFCmp(pred, a, b);
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
    if (to.kind == jir::Type::Float) {
      if (from.kind == jir::Type::Float)
        return to.bits > from.bits ? b_.CreateFPExt(v, ty) : b_.CreateFPTrunc(v, ty);
      if (from.kind == jir::Type::Int) return from.is_signed ? b_.CreateSIToFP(v, ty) : b_.CreateUIToFP(v, ty);
    }
    if (to.kind == jir::Type::Int && from.kind == jir::Type::Float) {
      // Saturating, NaN to 0, like the VM (plain fptosi would be poison).
      Intrinsic::ID id = to.is_signed ? Intrinsic::fptosi_sat : Intrinsic::fptoui_sat;
      return b_.CreateIntrinsic(id, {ty, v->getType()}, {v});
    }
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
    // IEEE 754 arithmetic, with no fast-math flags, so results match the VM bit
    // for bit. `frem` is C's fmod, like Rust's `%` on the VM.
    if (inst.op >= Op::Add && inst.op <= Op::Rem && is_float(arg_reg(0))) {
      static const Instruction::BinaryOps ops[] = {Instruction::FAdd, Instruction::FSub, Instruction::FMul,
                                                   Instruction::FDiv, Instruction::FRem};
      store(inst.dst, b_.CreateBinOp(ops[int(inst.op) - int(Op::Add)], arg(0), arg(1)));
      return;
    }
    switch (inst.op) {
      case Op::Const: {
        Type *ty = slot(inst.dst)->getAllocatedType();
        if (!ty->isIntegerTy()) fail(f_.name, "`const` needs an integer or bool register");
        store(inst.dst, ConstantInt::get(ty, uint64_t(inst.imm), /*isSigned=*/true));
        return;
      }
      case Op::FConst: {
        Type *ty = slot(inst.dst)->getAllocatedType();
        if (!ty->isFloatingPointTy()) fail(f_.name, "`fconst` needs a float register");
        store(inst.dst, ConstantFP::get(ty, inst.fimm));  // exact: JIR f32 constants fit in f32
        return;
      }
      case Op::Unit: store(inst.dst, unit()); return;
      case Op::Str:
        if (is_str(inst.dst)) {
          StructType *h = glue_.header();
          b_.CreateStore(b_.CreateGlobalString(inst.text, ".str", 0, &mod_), b_.CreateStructGEP(h, slot(inst.dst), 0));
          b_.CreateStore(b_.getInt64(inst.text.size()), b_.CreateStructGEP(h, slot(inst.dst), 1));
          b_.CreateStore(b_.getInt64(0), b_.CreateStructGEP(h, slot(inst.dst), 2));  // a literal
          return;
        }
        // Compiled strings are addresses of NUL-terminated constant bytes.
        store(inst.dst, b_.CreateGlobalString(inst.text, ".str", 0, &mod_));
        return;
      case Op::Copy: copy_reg(inst.dst, arg_reg(0)); return;
      case Op::Neg: store(inst.dst, is_float(arg_reg(0)) ? b_.CreateFNeg(arg(0)) : b_.CreateNeg(arg(0))); return;
      case Op::Not: store(inst.dst, b_.CreateNot(arg(0))); return;
      case Op::Add:
        if (is_str(arg_reg(0))) {
          Value *a = slot(arg_reg(0)), *b = slot(arg_reg(1));
          Value *la = list_field(a, 1), *lb = list_field(b, 1);
          Value *data = new_str(b_.CreateAdd(la, lb), slot(inst.dst));
          b_.CreateMemCpy(data, Align(1), list_field(a, 0), Align(1), la);
          b_.CreateMemCpy(b_.CreateGEP(b_.getInt8Ty(), data, {la}), Align(1), list_field(b, 0), Align(1), lb);
        } else if (type(arg_reg(0)).kind == jir::Type::List) {
          const jir::Type &elem = *list_of(inst.dst).pointee;
          Value *a = slot(arg_reg(0)), *b = slot(arg_reg(1));
          Value *la = list_field(a, 1), *lb = list_field(b, 1);
          Value *data = new_list(elem, b_.CreateAdd(la, lb), slot(inst.dst));
          copy_elems(elem, data, list_field(a, 0), la);
          copy_elems(elem, b_.CreateGEP(types_.lower(elem), data, {la}), list_field(b, 0), lb);
        } else if (type(arg_reg(0)).kind == jir::Type::Ptr)
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
      case Op::Div: case Op::Rem:
        store(inst.dst, divide(inst.op == Op::Rem, is_signed(arg_reg(0)), arg(0), arg(1)));
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
        if (is_str(arg_reg(0))) {
          store(inst.dst, str_compare(inst.op, arg_reg(0), arg_reg(1)));
          return;
        }
        store(inst.dst, is_float(arg_reg(0)) ? fcompare(inst.op, arg(0), arg(1))
                                             : compare(inst.op, arg(0), arg(1), is_signed(arg_reg(0))));
        return;
      case Op::Cast:
        if (type(arg_reg(0)) == type(inst.dst))
          copy_reg(inst.dst, arg_reg(0));
        else
          store(inst.dst, cast(arg_reg(0), type(inst.dst)));
        return;
      // IEEE 754 defines these exactly, so LLVM's intrinsics (instructions, or
      // libm's functions) give what the VM gives.
      case Op::Sqrt: case Op::Floor: case Op::Ceil: case Op::Trunc: case Op::Round: case Op::Abs: {
        if (!is_float(arg_reg(0))) fail(f_.name, "float operation on a non-float");
        static const Intrinsic::ID ids[] = {Intrinsic::sqrt, Intrinsic::floor, Intrinsic::ceil,
                                            Intrinsic::trunc, Intrinsic::round, Intrinsic::fabs};
        store(inst.dst, b_.CreateUnaryIntrinsic(ids[int(inst.op) - int(Op::Sqrt)], arg(0)));
        return;
      }
      case Op::Copysign: {
        if (!is_float(arg_reg(0))) fail(f_.name, "`copysign` needs floats");
        // A NaN has no sign in JIR (machines disagree about it): the result is
        // then the magnitude.
        Value *x = arg(0), *y = arg(1);
        Value *nan = b_.CreateFCmpUNO(y, y);
        Value *r = b_.CreateBinaryIntrinsic(Intrinsic::copysign, x, y);
        store(inst.dst, b_.CreateSelect(nan, b_.CreateUnaryIntrinsic(Intrinsic::fabs, x), r));
        return;
      }
      case Op::Bitcast: {
        Type *to = slot(inst.dst)->getAllocatedType();
        Value *v = arg(0);
        if (v->getType()->getPrimitiveSizeInBits() != to->getPrimitiveSizeInBits() ||
            v->getType()->isFloatingPointTy() == to->isFloatingPointTy())
          fail(f_.name, "`bitcast` needs a float and an integer of the same width");
        Value *bits = b_.CreateBitCast(v, to);
        if (v->getType()->isFloatingPointTy()) {
          // Every NaN reads as the quiet NaN with a clear sign, as on the VM:
          // which NaN an operation makes differs between machines.
          uint64_t nan = to->getIntegerBitWidth() == 32 ? 0x7fc00000u : 0x7ff8000000000000u;
          bits = b_.CreateSelect(b_.CreateFCmpUNO(v, v), ConstantInt::get(to, nan), bits);
        }
        store(inst.dst, bits);
        return;
      }
      case Op::Call: {
        auto it = fns_.find(inst.text);
        if (it == fns_.end()) fail(f_.name, "call to unknown function @" + inst.text);
        if (auto e = externs_.find(inst.text); e != externs_.end())
          call_extern(e->second, it->second, inst);
        else
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
        read_part(inst.dst, b_.CreateStructGEP(payload_ty, payload, unsigned(inst.imm2)));
        return;
      }
      case Op::Field: {
        const jir::Type &st = type(arg_reg(0));
        types_.field(st, inst.imm);  // bounds check
        read_part(inst.dst, b_.CreateStructGEP(types_.lower(st), slot(arg_reg(0)), unsigned(inst.imm)));
        return;
      }
      case Op::SetField: {
        const jir::Type &st = type(arg_reg(0));
        types_.field(st, inst.imm);
        copy_reg(inst.dst, arg_reg(0));
        write_part(b_.CreateStructGEP(types_.lower(st), slot(inst.dst), unsigned(inst.imm)), arg_reg(1));
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
      // Atomics are sequentially consistent and naturally aligned, on integers
      // and pointers (the arithmetic on integers only).
      case Op::AtomicLoad: {
        Type *ty = atomic_pointee(arg_reg(0), true);
        if (types_.lower(type(inst.dst)) != ty) fail(f_.name, "loaded value has the wrong type");
        LoadInst *l = b_.CreateAlignedLoad(ty, arg(0), mod_.getDataLayout().getABITypeAlign(ty));
        l->setAtomic(AtomicOrdering::SequentiallyConsistent);
        store(inst.dst, l);
        return;
      }
      case Op::AtomicStore: {
        Type *ty = atomic_pointee(arg_reg(0), true);
        if (types_.lower(type(arg_reg(1))) != ty) fail(f_.name, "stored value has the wrong type");
        StoreInst *s = b_.CreateAlignedStore(arg(1), arg(0), mod_.getDataLayout().getABITypeAlign(ty));
        s->setAtomic(AtomicOrdering::SequentiallyConsistent);
        return;
      }
      case Op::AtomicSwap: case Op::AtomicAdd: case Op::AtomicSub:
      case Op::AtomicAnd: case Op::AtomicOr: case Op::AtomicXor: {
        static const AtomicRMWInst::BinOp ops[] = {AtomicRMWInst::Xchg, AtomicRMWInst::Add, AtomicRMWInst::Sub,
                                                   AtomicRMWInst::And,  AtomicRMWInst::Or,  AtomicRMWInst::Xor};
        Type *ty = atomic_pointee(arg_reg(0), inst.op == Op::AtomicSwap);
        if (types_.lower(type(arg_reg(1))) != ty || types_.lower(type(inst.dst)) != ty)
          fail(f_.name, "atomic operand has the wrong type");
        Value *old = b_.CreateAtomicRMW(ops[int(inst.op) - int(Op::AtomicSwap)], arg(0), arg(1),
                                        mod_.getDataLayout().getABITypeAlign(ty),
                                        AtomicOrdering::SequentiallyConsistent);
        store(inst.dst, old);
        return;
      }
      case Op::AtomicCas: {
        Type *ty = atomic_pointee(arg_reg(0), true);
        if (types_.lower(type(arg_reg(1))) != ty || types_.lower(type(arg_reg(2))) != ty)
          fail(f_.name, "atomic operand has the wrong type");
        Value *pair = b_.CreateAtomicCmpXchg(arg(0), arg(1), arg(2), mod_.getDataLayout().getABITypeAlign(ty),
                                             AtomicOrdering::SequentiallyConsistent,
                                             AtomicOrdering::SequentiallyConsistent);
        store(inst.dst, b_.CreateExtractValue(pair, 1));
        return;
      }
      case Op::Fence: b_.CreateFence(AtomicOrdering::SequentiallyConsistent); return;
      case Op::Spawn: spawn(inst); return;
      case Op::Addr: store(inst.dst, slot(arg_reg(0))); return;
      case Op::FieldPtr: {
        const jir::Type &t = type(arg_reg(0));
        if (t.kind != jir::Type::Ptr) fail(f_.name, "`fieldptr` needs a pointer");
        types_.field(*t.pointee, inst.imm);
        store(inst.dst, b_.CreateStructGEP(types_.lower(*t.pointee), arg(0), unsigned(inst.imm)));
        return;
      }
      case Op::Array: {
        if (type(inst.dst).kind == jir::Type::List) {
          const jir::Type &elem = *type(inst.dst).pointee;
          Value *data = new_list(elem, b_.getInt64(inst.args.size()), slot(inst.dst));
          for (unsigned i = 0; i < inst.args.size(); i++)
            write_mem(b_.CreateConstGEP1_64(types_.lower(elem), data, i), arg_reg(i));
          return;
        }
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
        if (type(arg_reg(0)).kind == jir::Type::List) {
          read_part(inst.dst, list_elem_addr(type(arg_reg(0)), slot(arg_reg(0)), arg(1)));
          return;
        }
        const jir::Type &arr = array_of(arg_reg(0), false);
        read_part(inst.dst, elem_addr(arr, slot(arg_reg(0)), arg(1)));
        return;
      }
      case Op::SetElem: {
        const jir::Type &arr = type(arg_reg(0));
        if (arr.kind != jir::Type::List) array_of(arg_reg(0), false);
        if (type(inst.dst) != arr) fail(f_.name, "`setelem` result has the wrong type");
        Value *index = arg(1);
        copy_reg(inst.dst, arg_reg(0));
        Value *at = arr.kind == jir::Type::List ? list_elem_addr(arr, slot(inst.dst), index)
                                                 : elem_addr(arr, slot(inst.dst), index);
        write_part(at, arg_reg(2));
        return;
      }
      case Op::GetPath: {
        auto [addr, t] = path_addr(type(arg_reg(0)), slot(arg_reg(0)), inst.path);
        if (*t != type(inst.dst)) fail(f_.name, "`getpath` result has the wrong type");
        read_part(inst.dst, addr);
        return;
      }
      case Op::SetPath: {
        if (type(inst.dst) != type(arg_reg(0))) fail(f_.name, "`setpath` result has the wrong type");
        copy_reg(inst.dst, arg_reg(0));
        auto [addr, t] = path_addr(type(inst.dst), slot(inst.dst), inst.path);
        if (*t != type(arg_reg(1))) fail(f_.name, "`setpath` value has the wrong type");
        write_part(addr, arg_reg(1));
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
      case Op::Print: print(arg_reg(0)); return;
      case Op::Len:
        if (!is_str(arg_reg(0))) list_of(arg_reg(0));
        store(inst.dst, list_field(slot(arg_reg(0)), 1));
        return;
      case Op::StrByte: {
        if (!is_str(arg_reg(0))) fail(f_.name, "`strbyte` needs a str");
        Value *h = slot(arg_reg(0)), *i = arg(1);
        check(b_.CreateICmpULT(i, list_field(h, 1)), "in_bounds");
        store(inst.dst, b_.CreateLoad(b_.getInt8Ty(), b_.CreateGEP(b_.getInt8Ty(), list_field(h, 0), {i})));
        return;
      }
      case Op::FromBytes: {
        if (list_of(arg_reg(0)) != jir::Type::list(jir::Type::integer(8, false))) fail(f_.name, "`frombytes` needs a [u8]");
        Value *src = slot(arg_reg(0));
        Value *n = list_field(src, 1);
        Value *data = new_str(n, slot(inst.dst));
        b_.CreateMemCpy(data, Align(1), list_field(src, 0), Align(1), n);
        return;
      }
      case Op::ToStr: {
        const jir::Type &t = type(arg_reg(0));
        Value *v = arg(0);
        AllocaInst *buf = temp(ArrayType::get(b_.getInt8Ty(), 64), jir::Type::integer(64, true));
        auto str = [&](const char *s) { return b_.CreateGlobalString(s, ".str.to_str", 0, &mod_); };
        if (t.kind == jir::Type::Int) {
          b_.CreateCall(libc_fn(mod_, "snprintf"), {buf, b_.getInt64(64), str(t.is_signed ? "%lld" : "%llu"),
                                                    b_.CreateIntCast(v, i64_, t.is_signed)});
        } else if (t.kind == jir::Type::Bool) {
          b_.CreateCall(libc_fn(mod_, "snprintf"), {buf, b_.getInt64(64), str("%s"), b_.CreateSelect(v, str("true"), str("false"))});
        } else if (t.kind == jir::Type::Float) {
          if (t.bits == 32) v = b_.CreateFPExt(v, b_.getDoubleTy());
          b_.CreateCall(print_float_fn(mod_), {v, b_.getInt1(t.bits == 32), buf});
        } else {
          fail(f_.name, "`to_str` takes a number or a bool");
        }
        str_from_c(buf, slot(inst.dst));
        return;
      }
      case Op::ReadLine: {
        // getline(&line, &size, stdin): the line with its newline, or -1 at the end.
        Type *ptr = PointerType::get(ctx_, 0);
        auto *in = mod_.getOrInsertGlobal("stdin", ptr);
        AllocaInst *line = temp(ptr, jir::Type::pointer(jir::Type::integer(8, false)));
        AllocaInst *size = temp(i64_, jir::Type::integer(64, true));
        b_.CreateStore(ConstantPointerNull::get(PointerType::get(ctx_, 0)), line);
        b_.CreateStore(b_.getInt64(0), size);
        FunctionCallee getline = mod_.getOrInsertFunction("getline", FunctionType::get(i64_, {ptr, ptr, ptr}, false));
        Value *n = b_.CreateCall(getline, {line, size, b_.CreateLoad(ptr, in)});
        Value *got = b_.CreateICmpSGT(n, b_.getInt64(0));
        StructType *h = glue_.header();
        // The buffer becomes the string's memory, so nothing is copied.
        b_.CreateStore(b_.CreateLoad(ptr, line), b_.CreateStructGEP(h, slot(inst.dst), 0));
        b_.CreateStore(b_.CreateSelect(got, n, b_.getInt64(0)), b_.CreateStructGEP(h, slot(inst.dst), 1));
        b_.CreateStore(b_.getInt64(1), b_.CreateStructGEP(h, slot(inst.dst), 2));
        return;
      }
      case Op::Clone:
        if (type(inst.dst) != type(arg_reg(0))) fail(f_.name, "`clone` result has the wrong type");
        if (glue_.owns(type(inst.dst)))
          b_.CreateCall(glue_.clone(type(inst.dst)), {slot(inst.dst), slot(arg_reg(0))});
        else
          copy_reg(inst.dst, arg_reg(0));
        return;
      case Op::Drop:
        if (glue_.owns(type(arg_reg(0)))) b_.CreateCall(glue_.drop(type(arg_reg(0))), {slot(arg_reg(0))});
        return;
      case Op::Repeat: {
        const jir::Type &elem = *list_of(inst.dst).pointee;
        Value *n = arg(1);
        check(b_.CreateICmpSGE(n, b_.getInt64(0)), "repeat.count");
        Value *data = new_list(elem, n, slot(inst.dst));
        Type *ety = types_.lower(elem);
        uint32_t v = arg_reg(0);
        Glue::loop(b_, n, [&](Value *i) {
          Value *at = b_.CreateGEP(ety, data, {i});
          if (glue_.owns(elem))
            b_.CreateCall(glue_.clone(elem), {at, slot(v)});
          else
            write_mem(at, v);
        });
        return;
      }
      case Op::Slice: {
        if (is_str(arg_reg(0))) {
          Value *src = slot(arg_reg(0));
          Value *lo = arg(1), *hi = arg(2);
          check(b_.CreateAnd(b_.CreateICmpULE(lo, hi), b_.CreateICmpULE(hi, list_field(src, 1))), "slice.bounds");
          Value *n = b_.CreateSub(hi, lo);
          Value *data = new_str(n, slot(inst.dst));
          b_.CreateMemCpy(data, Align(1), b_.CreateGEP(b_.getInt8Ty(), list_field(src, 0), {lo}), Align(1), n);
          return;
        }
        const jir::Type &elem = *list_of(arg_reg(0)).pointee;
        Value *src = slot(arg_reg(0));
        Value *lo = arg(1), *hi = arg(2);
        // Unsigned: a negative `lo` is above any `hi` that passes the second test.
        check(b_.CreateAnd(b_.CreateICmpULE(lo, hi), b_.CreateICmpULE(hi, list_field(src, 1))), "slice.bounds");
        Value *n = b_.CreateSub(hi, lo);
        Value *data = new_list(elem, n, slot(inst.dst));
        copy_elems(elem, data, b_.CreateGEP(types_.lower(elem), list_field(src, 0), {lo}), n);
        return;
      }
      case Op::Push: {
        auto [header, t] = path_addr(type(arg_reg(0)), slot(arg_reg(0)), inst.path);
        if (t->kind != jir::Type::List) fail(f_.name, "`push` needs a list");
        const jir::Type &elem = *t->pointee;
        if (elem != type(arg_reg(1))) fail(f_.name, "pushed value has the wrong type");
        StructType *h = glue_.header();
        Value *len = list_field(header, 1), *cap = list_field(header, 2);
        auto *grow = BasicBlock::Create(ctx_, "push.grow", fn_);
        auto *put = BasicBlock::Create(ctx_, "push.put", fn_);
        b_.CreateCondBr(b_.CreateICmpEQ(len, cap), grow, put);
        b_.SetInsertPoint(grow);
        Value *more = b_.CreateSelect(b_.CreateICmpEQ(cap, b_.getInt64(0)), b_.getInt64(4), b_.CreateShl(cap, 1));
        auto chk = [&](Value *ok) { check(ok, "push.size"); };
        Value *bytes = glue_.byte_count(b_, elem, more, chk);
        Value *data = b_.CreateCall(glue_.realloc_fn(), {list_field(header, 0), bytes});
        check(b_.CreateIsNotNull(data), "push.alloc");
        b_.CreateStore(data, b_.CreateStructGEP(h, header, 0));
        b_.CreateStore(more, b_.CreateStructGEP(h, header, 2));
        b_.CreateBr(put);
        b_.SetInsertPoint(put);
        write_mem(b_.CreateGEP(types_.lower(elem), list_field(header, 0), {len}), arg_reg(1));
        b_.CreateStore(b_.CreateAdd(len, b_.getInt64(1)), b_.CreateStructGEP(h, header, 1));
        return;
      }
      case Op::Pop: {
        auto [header, t] = path_addr(type(arg_reg(0)), slot(arg_reg(0)), inst.path);
        if (t->kind != jir::Type::List) fail(f_.name, "`pop` needs a list");
        if (*t->pointee != type(inst.dst)) fail(f_.name, "`pop` result has the wrong type");
        Value *len = list_field(header, 1);
        check(b_.CreateICmpNE(len, b_.getInt64(0)), "pop.nonempty");
        Value *last = b_.CreateSub(len, b_.getInt64(1));
        // The element moves out: nothing is copied or freed.
        read_mem(inst.dst, b_.CreateGEP(types_.lower(*t->pointee), list_field(header, 0), {last}));
        b_.CreateStore(last, b_.CreateStructGEP(glue_.header(), header, 1));
        return;
      }
      // Goes to the shared trap block; what follows in the JIR block is dead.
      case Op::Trap: {
        check(ConstantInt::getFalse(ctx_), "after.trap");
        return;
      }
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
        if (exit_on_return_) {
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

// Defines `fmod` or `fmodf` (for `ft`), which `frem` becomes, weak like the
// memory functions. The result is exact, as C requires: with t = |y| * 2^k the
// largest such value <= |x|, subtracting t whenever the remainder r is at
// least t, then halving t down to |y|, every subtraction has t <= r < 2t and
// so is exact (Sterbenz), and so is every doubling and halving of t.
void define_fmod(Module &m, Type *ft, const std::string &name) {
  LLVMContext &ctx = m.getContext();
  auto *f = Function::Create(FunctionType::get(ft, {ft, ft}, false), GlobalValue::WeakAnyLinkage, name, m);
  f->addFnAttr("no-builtins");
  f->addFnAttr(Attribute::NoUnwind);
  Value *x = f->getArg(0), *y = f->getArg(1);
  auto block = [&](const char *n) { return BasicBlock::Create(ctx, n, f); };
  BasicBlock *entry = block("entry"), *nan = block("nan"), *check = block("check"), *small = block("small"),
             *up = block("up"), *down = block("down"), *sub = block("sub"), *next = block("next"),
             *halve = block("halve"), *done = block("done");

  IRBuilder<> b(entry);
  Value *ax = b.CreateUnaryIntrinsic(Intrinsic::fabs, x);
  Value *ay = b.CreateUnaryIntrinsic(Intrinsic::fabs, y);
  // NaN if either is NaN, x is infinite, or y is zero.
  Value *bad = b.CreateOr(b.CreateOr(b.CreateFCmpUNO(x, y), b.CreateFCmpOEQ(ax, ConstantFP::getInfinity(ft))),
                          b.CreateFCmpOEQ(ay, ConstantFP::get(ft, 0.0)));
  b.CreateCondBr(bad, nan, check);

  b.SetInsertPoint(nan);
  b.CreateRet(ConstantFP::getNaN(ft));

  b.SetInsertPoint(check);  // |x| < |y| (which includes an infinite y): x itself
  b.CreateCondBr(b.CreateFCmpOLT(ax, ay), small, up);
  b.SetInsertPoint(small);
  b.CreateRet(x);

  b.SetInsertPoint(up);  // t = the largest |y| * 2^k <= |x|
  PHINode *t_up = b.CreatePHI(ft, 2);
  t_up->addIncoming(ay, check);
  Value *twice = b.CreateFMul(t_up, ConstantFP::get(ft, 2.0));
  t_up->addIncoming(twice, up);
  b.CreateCondBr(b.CreateFCmpOLE(twice, ax), up, down);

  b.SetInsertPoint(down);  // r -= t if r >= t; stop at t == |y|, else halve t
  PHINode *t = b.CreatePHI(ft, 2), *r = b.CreatePHI(ft, 2);
  t->addIncoming(t_up, up);
  r->addIncoming(ax, up);
  b.CreateCondBr(b.CreateFCmpOGE(r, t), sub, next);
  b.SetInsertPoint(sub);
  Value *less = b.CreateFSub(r, t);
  b.CreateBr(next);
  b.SetInsertPoint(next);
  PHINode *r2 = b.CreatePHI(ft, 2);
  r2->addIncoming(r, down);
  r2->addIncoming(less, sub);
  b.CreateCondBr(b.CreateFCmpOEQ(t, ay), done, halve);
  b.SetInsertPoint(halve);
  t->addIncoming(b.CreateFMul(t, ConstantFP::get(ft, 0.5)), halve);
  r->addIncoming(r2, halve);
  b.CreateBr(down);

  b.SetInsertPoint(done);  // the sign of x, also for a zero result
  b.CreateRet(b.CreateBinaryIntrinsic(Intrinsic::copysign, r2, x));
}

// Defines `floor`, `ceil`, `trunc` or `round` (`kind`), or their `f` versions
// for `ft`, weak like `fmod`: LLVM calls them where the target has no
// instruction for its rounding intrinsics (x86_64 before SSE4.1). A float of
// magnitude 2^(mantissa bits) or more, an infinity or a NaN is its own result;
// any other one converts to an integer exactly, and back, rounded toward zero.
// That is corrected by one toward -inf (floor), +inf (ceil) or away from zero
// (round, when the dropped fraction, which is exact, is at least a half). The
// sign of the argument stays, also on a zero result (`floor(-0.0)` is `-0.0`,
// `round(-0.4)` too).
void define_rounding(Module &m, Type *ft, const std::string &kind) {
  LLVMContext &ctx = m.getContext();
  bool f32 = ft->isFloatTy();
  std::string name = f32 ? kind + "f" : kind;
  auto *f = Function::Create(FunctionType::get(ft, {ft}, false), GlobalValue::WeakAnyLinkage, name, m);
  // Unoptimized, so the conversions below are never recognized as the very
  // intrinsic that calls this function.
  f->addFnAttr("no-builtins");
  f->addFnAttr(Attribute::NoUnwind);
  f->addFnAttr(Attribute::NoInline);
  f->addFnAttr(Attribute::OptimizeNone);
  Value *x = f->getArg(0);
  auto *entry = BasicBlock::Create(ctx, "entry", f);
  auto *same = BasicBlock::Create(ctx, "same", f);
  auto *round = BasicBlock::Create(ctx, "round", f);
  IRBuilder<> b(entry);
  Value *ax = b.CreateUnaryIntrinsic(Intrinsic::fabs, x);
  // Not `<`, so NaN goes to `same` too.
  Value *small = b.CreateFCmpOLT(ax, ConstantFP::get(ft, f32 ? 8388608.0 : 4503599627370496.0));
  b.CreateCondBr(small, round, same);
  b.SetInsertPoint(same);
  b.CreateRet(x);

  b.SetInsertPoint(round);
  Type *it = f32 ? b.getInt32Ty() : b.getInt64Ty();
  Value *t = b.CreateBinaryIntrinsic(Intrinsic::copysign, b.CreateSIToFP(b.CreateFPToSI(x, it), ft), x);
  Value *one = ConstantFP::get(ft, 1.0);
  Value *r = t;
  if (kind == "floor") r = b.CreateSelect(b.CreateFCmpOGT(t, x), b.CreateFSub(t, one), t);
  if (kind == "ceil") r = b.CreateSelect(b.CreateFCmpOLT(t, x), b.CreateFAdd(t, one), t);
  if (kind == "round") {
    Value *frac = b.CreateUnaryIntrinsic(Intrinsic::fabs, b.CreateFSub(x, t));
    Value *away = b.CreateFAdd(t, b.CreateBinaryIntrinsic(Intrinsic::copysign, one, x));
    r = b.CreateSelect(b.CreateFCmpOGE(frac, ConstantFP::get(ft, 0.5)), away, t);
  }
  // A zero result keeps the sign of `x`: `floor(-0.0)`, `ceil(-0.5)`.
  b.CreateRet(b.CreateBinaryIntrinsic(Intrinsic::copysign, r, x));
}

// The C library functions `print` uses, declared once (or as the program
// declared them: the types are C's).
FunctionCallee libc_fn(Module &m, const char *name) {
  LLVMContext &ctx = m.getContext();
  Type *i32 = Type::getInt32Ty(ctx), *ptr = PointerType::get(ctx, 0), *dbl = Type::getDoubleTy(ctx);
  std::string n = name;
  FunctionType *ty = n == "printf"                    ? FunctionType::get(i32, {ptr}, true)
                     : n == "snprintf"              ? FunctionType::get(i32, {ptr, Type::getInt64Ty(ctx), ptr}, true)
                     : n == "strtod"                ? FunctionType::get(dbl, {ptr, ptr}, false)
                     : n == "strtof"                ? FunctionType::get(Type::getFloatTy(ctx), {ptr, ptr}, false)
                                                    : FunctionType::get(i32, {ptr}, false);  // puts, atoi
  return m.getOrInsertFunction(name, ty);
}

// Defines `void jihoo.print_float(double x, i1 is_f32, ptr out)`, which prints `x` (an
// `f32` if `is_f32`) and a newline the way the VM does: the shortest decimal
// that reads back as `x` in its type, with a `.` or an exponent (`1.0`, `0.1`,
// `1e100`, `1.5e-7`; exponents for magnitudes below 1e-4 and from 1e16 on),
// or `inf`, `-inf`, `NaN`. Given an `out` buffer of 64 bytes, it writes the
// text there instead, without the newline (for `to_str`).
//
// The search reads the first 18 digits of x exactly, from `%.40e` or, if
// that may have rounded them, `%.767e` (a double has at most 767 significant
// digits, and the C library prints them exactly). For L = 1, 2, ... digits:
// the L digit decimal c nearest to x, with ties rounded up as the VM does
// (`printf` would round them to even), is the first L digits as an integer
// M, plus one if the next digit is 5 or more. If c reads back as x, it is the
// shortest such decimal and the nearest. If not, the decimal on the other
// side of x (M or M + 1) may still read back (when x is a power of two, whose
// neighbor below is closer than the one above), and then it is the one.
Function *print_float_fn(Module &m) {
  if (Function *f = m.getFunction("jihoo.print_float")) return f;
  LLVMContext &ctx = m.getContext();
  Type *dbl = Type::getDoubleTy(ctx), *i64 = Type::getInt64Ty(ctx), *i32 = Type::getInt32Ty(ctx);
  Type *i8 = Type::getInt8Ty(ctx);
  Type *ptr = PointerType::get(ctx, 0);
  auto *fn = Function::Create(FunctionType::get(Type::getVoidTy(ctx), {dbl, Type::getInt1Ty(ctx), ptr}, false),
                              GlobalValue::InternalLinkage, "jihoo.print_float", m);
  fn->addFnAttr(Attribute::NoInline);
  Value *x = fn->getArg(0), *is_f32 = fn->getArg(1), *out = fn->getArg(2);
  auto block = [&](const char *n) { return BasicBlock::Create(ctx, n, fn); };
  IRBuilder<> b(block("entry"));
  auto str = [&](const char *s) { return b.CreateGlobalString(s, ".str.print", 0, &m); };
  auto call = [&](const char *name, std::vector<Value *> args) { return b.CreateCall(libc_fn(m, name), args); };
  // Prints `fmt` (which ends in a newline) with `args` and returns; with an
  // `out` buffer (64 bytes), writes the text there instead, without the newline.
  auto finish = [&](std::string fmt, std::vector<Value *> args) {
    BasicBlock *to_stdout = block("stdout"), *to_buf = block("buffer");
    b.CreateCondBr(b.CreateIsNull(out), to_stdout, to_buf);
    b.SetInsertPoint(to_stdout);
    std::vector<Value *> pargs{str(fmt.c_str())};
    pargs.insert(pargs.end(), args.begin(), args.end());
    call("printf", pargs);
    b.CreateRetVoid();
    b.SetInsertPoint(to_buf);
    std::vector<Value *> sargs{out, b.getInt64(64), str(fmt.substr(0, fmt.size() - 1).c_str())};
    sargs.insert(sargs.end(), args.begin(), args.end());
    call("snprintf", sargs);
    b.CreateRetVoid();
  };
  auto puts_and_return = [&](Value *s) { finish("%s\n", {s}); };

  // `d.<767 digits>e-XXX` and a NUL.
  const uint64_t kDigits = 767, kExact = kDigits + 8;
  Value *exact = b.CreateAlloca(ArrayType::get(i8, kExact));
  Value *num = b.CreateAlloca(ArrayType::get(i8, 40));
  // 10^0 .. 10^17.
  std::vector<Constant *> pows;
  for (uint64_t k = 0, v = 1; k <= 17; k++, v *= 10) pows.push_back(ConstantInt::get(i64, v));
  auto *pow_ty = ArrayType::get(i64, pows.size());
  auto *pow_table = new GlobalVariable(m, pow_ty, true, GlobalValue::PrivateLinkage, ConstantArray::get(pow_ty, pows),
                                       "jihoo.pow10");
  auto pow10 = [&](Value *k) { return b.CreateLoad(i64, b.CreateGEP(pow_ty, pow_table, {b.getInt64(0), k})); };
  // Digit `i` of x as a number: `exact` holds `d.ddd...`, so the digits after
  // the first are one byte further on.
  auto digit = [&](Value *i) {
    Value *at = b.CreateSelect(b.CreateICmpEQ(i, b.getInt64(0)), i, b.CreateAdd(i, b.getInt64(1)));
    Value *c = b.CreateLoad(i8, b.CreateGEP(i8, exact, at));
    return b.CreateSub(b.CreateZExt(c, i64), b.getInt64('0'));
  };

  // NaN, infinities and zeros.
  BasicBlock *nan = block("nan"), *not_nan = block("not_nan"), *inf = block("inf"), *finite = block("finite"),
             *zero = block("zero"), *nonzero = block("nonzero");
  b.CreateCondBr(b.CreateFCmpUNO(x, x), nan, not_nan);
  b.SetInsertPoint(nan);
  puts_and_return(str("NaN"));
  b.SetInsertPoint(not_nan);
  Value *ax = b.CreateUnaryIntrinsic(Intrinsic::fabs, x);
  Value *negative = b.CreateICmpSLT(b.CreateBitCast(x, i64), b.getInt64(0));
  b.CreateCondBr(b.CreateFCmpOEQ(ax, ConstantFP::getInfinity(dbl)), inf, finite);
  b.SetInsertPoint(inf);
  puts_and_return(b.CreateSelect(negative, str("-inf"), str("inf")));
  b.SetInsertPoint(finite);
  b.CreateCondBr(b.CreateFCmpOEQ(ax, ConstantFP::get(dbl, 0.0)), zero, nonzero);
  b.SetInsertPoint(zero);
  puts_and_return(b.CreateSelect(negative, str("-0.0"), str("0.0")));

  b.SetInsertPoint(nonzero);
  Value *sign = b.CreateSelect(negative, str("-"), str(""));
  Value *ax32 = b.CreateFPTrunc(ax, b.getFloatTy());
  // The first 18 digits are all the search reads. 41 digits, rounded, have
  // them unless rounding carried into them, which leaves digits 19 to 41 all
  // zero; only then are all the digits needed.
  call("snprintf", {exact, b.getInt64(kExact), str("%.40e"), ax});
  BasicBlock *check = block("carry"), *all = block("all_digits"), *have = block("have_digits");
  b.CreateBr(check);
  b.SetInsertPoint(check);
  PHINode *k = b.CreatePHI(i64, 2);
  k->addIncoming(b.getInt64(18), nonzero);
  Value *zero_digit = b.CreateICmpEQ(digit(k), b.getInt64(0));
  Value *k1 = b.CreateAdd(k, b.getInt64(1));
  k->addIncoming(k1, check);
  BasicBlock *carry_done = block("carry_done");
  b.CreateCondBr(b.CreateAnd(zero_digit, b.CreateICmpULT(k1, b.getInt64(41))), check, carry_done);
  // No rounding at all happened if x = m * 2^q has 41 digits or fewer: an
  // integer below 1e41, or q >= -34 (m * 5^-q has at most 16 + 25 digits).
  b.SetInsertPoint(carry_done);
  Value *bits = b.CreateBitCast(ax, i64);
  Value *field = b.CreateLShr(bits, b.getInt64(52));
  Value *frac = b.CreateAnd(bits, b.getInt64((1ull << 52) - 1));
  Value *subnormal = b.CreateICmpEQ(field, b.getInt64(0));
  Value *m_int = b.CreateSelect(subnormal, frac, b.CreateOr(frac, b.getInt64(1ull << 52)));
  Value *q = b.CreateSub(b.CreateSelect(subnormal, b.getInt64(1), field), b.getInt64(1075));
  q = b.CreateAdd(q, b.CreateBinaryIntrinsic(Intrinsic::cttz, m_int, b.getTrue()));
  Value *short_int = b.CreateAnd(b.CreateICmpSGE(q, b.getInt64(0)), b.CreateFCmpOLT(ax, ConstantFP::get(dbl, 1e41)));
  Value *short_frac = b.CreateAnd(b.CreateICmpSLT(q, b.getInt64(0)), b.CreateICmpSGE(q, b.getInt64(-34)));
  Value *exact41 = b.CreateOr(short_int, short_frac);
  b.CreateCondBr(b.CreateAnd(zero_digit, b.CreateNot(exact41)), all, have);
  b.SetInsertPoint(all);
  call("snprintf", {exact, b.getInt64(kExact), str("%.767e"), ax});
  b.CreateBr(have);
  b.SetInsertPoint(have);
  PHINode *precision = b.CreatePHI(i64, 2);
  precision->addIncoming(b.getInt64(40), carry_done);
  precision->addIncoming(b.getInt64(kDigits), all);
  // The exponent of the first digit, after `d.<digits>e`.
  Value *e = call("atoi", {b.CreateGEP(i8, exact, b.CreateAdd(precision, b.getInt64(3)))});
  BasicBlock *loop = block("digits"), *found = block("found");
  b.CreateBr(loop);

  // Whether mant * 10^shift reads back as x, in the type of x.
  auto reads_back = [&](Value *mant, Value *shift) {
    call("snprintf", {num, b.getInt64(40), str("%llue%d"), mant, shift});
    BasicBlock *as32 = block("as_f32"), *as64 = block("as_f64"), *done = block("read_back");
    b.CreateCondBr(is_f32, as32, as64);
    b.SetInsertPoint(as32);
    Value *same32 = b.CreateFCmpOEQ(call("strtof", {num, ConstantPointerNull::get(b.getPtrTy())}), ax32);
    b.CreateBr(done);
    b.SetInsertPoint(as64);
    Value *same64 = b.CreateFCmpOEQ(call("strtod", {num, ConstantPointerNull::get(b.getPtrTy())}), ax);
    b.CreateBr(done);
    b.SetInsertPoint(done);
    PHINode *same = b.CreatePHI(b.getInt1Ty(), 2);
    same->addIncoming(same32, as32);
    same->addIncoming(same64, as64);
    return same;
  };
  // An L digit decimal M (which may have grown to 10^L) as (digits, exponent
  // of the first one).
  auto normalize = [&](Value *mant, Value *len) {
    Value *over = b.CreateICmpEQ(mant, pow10(len));
    Value *len1 = b.CreateSub(len, b.getInt64(1));
    return std::make_pair(b.CreateSelect(over, pow10(len1), mant),
                          b.CreateSelect(over, b.CreateAdd(e, b.getInt32(1)), e));
  };
  auto shift_of = [&](Value *e_first, Value *len) {
    return b.CreateSub(e_first, b.CreateTrunc(b.CreateSub(len, b.getInt64(1)), i32));
  };

  b.SetInsertPoint(loop);
  PHINode *len = b.CreatePHI(i64, 2), *prefix = b.CreatePHI(i64, 2);
  len->addIncoming(b.getInt64(1), have);
  prefix->addIncoming(b.getInt64(0), have);
  Value *mant = b.CreateAdd(b.CreateMul(prefix, b.getInt64(10)), digit(b.CreateSub(len, b.getInt64(1))));
  Value *up = b.CreateICmpUGE(digit(len), b.getInt64(5));
  auto [near_m, near_e] = normalize(b.CreateAdd(mant, b.CreateZExt(up, i64)), len);
  BasicBlock *near_ok = block("nearest"), *try_other = block("other");
  // 17 digits always read back.
  Value *last = b.CreateICmpEQ(len, b.getInt64(17));
  b.CreateCondBr(b.CreateOr(last, reads_back(near_m, shift_of(near_e, len))), near_ok, try_other);
  b.SetInsertPoint(near_ok);
  b.CreateBr(found);

  b.SetInsertPoint(try_other);
  auto [other_m, other_e] = normalize(b.CreateAdd(mant, b.CreateZExt(b.CreateNot(up), i64)), len);
  BasicBlock *other_ok = block("other_ok"), *next = block("next");
  b.CreateCondBr(reads_back(other_m, shift_of(other_e, len)), other_ok, next);
  b.SetInsertPoint(other_ok);
  b.CreateBr(found);
  b.SetInsertPoint(next);
  len->addIncoming(b.CreateAdd(len, b.getInt64(1)), next);
  prefix->addIncoming(mant, next);
  b.CreateBr(loop);

  // The digits, without trailing zeros, and the exponent of the first one.
  b.SetInsertPoint(found);
  PHINode *fm = b.CreatePHI(i64, 2), *fe = b.CreatePHI(i32, 2);
  fm->addIncoming(near_m, near_ok);
  fe->addIncoming(near_e, near_ok);
  fm->addIncoming(other_m, other_ok);
  fe->addIncoming(other_e, other_ok);
  BasicBlock *strip = block("strip"), *stripped = block("stripped");
  b.CreateBr(strip);
  b.SetInsertPoint(strip);
  PHINode *m_digits = b.CreatePHI(i64, 2);
  m_digits->addIncoming(fm, found);
  Value *tens = b.CreateURem(m_digits, b.getInt64(10));
  Value *fewer = b.CreateUDiv(m_digits, b.getInt64(10));
  m_digits->addIncoming(fewer, strip);
  b.CreateCondBr(b.CreateICmpEQ(tens, b.getInt64(0)), strip, stripped);
  b.SetInsertPoint(stripped);
  Value *n = call("snprintf", {num, b.getInt64(40), str("%llu"), m_digits});
  Value *zeros = str("0000000000000000");

  // Exponents outside [-4, 16): `1.5e-7`, `1e16` (the limits of the type of x).
  BasicBlock *sci = block("sci"), *dec = block("dec"), *dec_int = block("dec_int"), *dec_frac = block("dec_frac"),
             *dec_small = block("dec_small"), *big = block("big");
  Value *lo = b.CreateSelect(is_f32, ConstantFP::get(dbl, double(1e-4f)), ConstantFP::get(dbl, 1e-4));
  Value *hi = b.CreateSelect(is_f32, ConstantFP::get(dbl, double(1e16f)), ConstantFP::get(dbl, 1e16));
  b.CreateCondBr(b.CreateOr(b.CreateFCmpOLT(ax, lo), b.CreateFCmpOGE(ax, hi)), sci, dec);
  b.SetInsertPoint(sci);
  Value *point = b.CreateSelect(b.CreateICmpSGT(n, b.getInt32(1)), str("."), str(""));
  finish("%s%.1s%s%se%d\n", {sign, num, point, b.CreateGEP(b.getInt8Ty(), num, b.getInt64(1)), fe});

  b.SetInsertPoint(dec);
  b.CreateCondBr(b.CreateICmpSLT(fe, b.getInt32(0)), dec_small, big);
  b.SetInsertPoint(dec_small);  // 0.000ddd
  finish("%s0.%.*s%s\n", {sign, b.CreateSub(b.getInt32(-1), fe), zeros, num});
  b.SetInsertPoint(big);
  Value *int_digits = b.CreateAdd(fe, b.getInt32(1));
  b.CreateCondBr(b.CreateICmpSLE(n, int_digits), dec_int, dec_frac);
  b.SetInsertPoint(dec_int);  // ddd000.0
  finish("%s%s%.*s.0\n", {sign, num, b.CreateSub(int_digits, n), zeros});
  b.SetInsertPoint(dec_frac);  // ddd.ddd
  Value *rest = b.CreateGEP(b.getInt8Ty(), num, b.CreateSExt(int_digits, i64));
  finish("%s%.*s.%s\n", {sign, int_digits, num, rest});
  return fn;
}

// The extension the C ABI wants for a narrow integer or a bool.
Attribute::AttrKind c_extension(const jir::Type &t) {
  if (t.kind == jir::Type::Bool) return Attribute::ZExt;
  if (t.kind == jir::Type::Int && t.bits < 32) return t.is_signed ? Attribute::SExt : Attribute::ZExt;
  return Attribute::None;
}

// The C signature of a function with jihoo `params` and `ret`. Only scalars,
// structs and a `unit` result reach here (the verifier checks).
cabi::Signature c_signature(const Module &m, const Types &types, const Triple &triple, const std::vector<jir::Type> &params,
                            const jir::Type &ret, bool variadic, const std::string &what) {
  auto lower = [&](const jir::Type &t) -> Type * {
    if (t.kind == jir::Type::Array || t.kind == jir::Type::Str || t.kind == jir::Type::Unit)
      throw CodegenError(what + ": " + t.str() + " cannot be passed to or from C");
    if (t.kind == jir::Type::Struct) types.struct_type(t.name);  // not an enum
    return types.lower(t);
  };
  std::vector<Type *> lowered;
  for (const auto &t : params) lowered.push_back(lower(t));
  return cabi::lower(triple, m.getDataLayout(), m.getContext(), lowered,
                     ret.kind == jir::Type::Unit ? nullptr : lower(ret), variadic);
}

// The attributes C signature `s` puts on `fn`: extensions of narrow scalars,
// `sret` for a result in memory and `byval` for structs copied onto the stack.
void c_attributes(Function *fn, const cabi::Signature &s, const Types &types, const std::vector<jir::Type> &params,
                  const jir::Type &ret) {
  LLVMContext &ctx = fn->getContext();
  if (s.ret.kind == cabi::Arg::Indirect)
    fn->addParamAttr(0, Attribute::getWithStructRetType(ctx, types.lower(ret)));
  else if (c_extension(ret) != Attribute::None)
    fn->addRetAttr(c_extension(ret));
  for (unsigned i = 0; i < params.size(); i++) {
    const cabi::Arg &a = s.params[i];
    if (a.kind == cabi::Arg::Indirect && a.byval) {
      fn->addParamAttr(s.first[i], Attribute::getWithByValType(ctx, types.lower(params[i])));
      fn->addParamAttr(s.first[i], Attribute::getWithAlignment(ctx, Align(8)));
    } else if (a.kind == cabi::Arg::Direct && c_extension(params[i]) != Attribute::None) {
      fn->addParamAttr(s.first[i], c_extension(params[i]));
    } else if (a.kind == cabi::Arg::Expand && a.stack_align) {
      for (unsigned j = 0; j < a.parts.size(); j++)
        fn->addParamAttr(s.first[i] + j, Attribute::getWithStackAlignment(ctx, Align(a.stack_align)));
    }
  }
}

// Declares C function `e` with its C signature.
CFunc declare_extern(Module &m, const Types &types, const Triple &triple, const jir::ExternFn &e, Function *&fn) {
  cabi::Signature s = c_signature(m, types, triple, e.params, e.ret, e.variadic, "extern @" + e.name);
  fn = Function::Create(s.type, GlobalValue::ExternalLinkage, e.name, m);
  if (fn->getName() != e.name) throw CodegenError("extern @" + e.name + " clashes with another symbol");
  c_attributes(fn, s, types, e.params, e.ret);
  return {&e, s};
}

// Defines C function `f.export_name`, which takes its arguments the C way and
// calls `jihoo_fn`, the program's function `f`, with them.
void define_export(Module &m, const Types &types, const Triple &triple, const jir::Function &f, Function *jihoo_fn) {
  const std::string &sym = f.export_name;
  cabi::Signature s = c_signature(m, types, triple, f.params, f.ret, false, "exported @" + f.name);
  if (m.getFunction(sym)) throw CodegenError("exported @" + f.name + ": the symbol `" + sym + "` is taken");
  auto *fn = Function::Create(s.type, GlobalValue::ExternalLinkage, sym, m);
  c_attributes(fn, s, types, f.params, f.ret);

  LLVMContext &ctx = m.getContext();
  IRBuilder<> b(BasicBlock::Create(ctx, "entry", fn));
  const DataLayout &dl = m.getDataLayout();
  // Room for a struct and the registers it came in, which may be a little larger.
  auto temp = [&](Type *ty, const jir::Type &t) {
    AllocaInst *a = b.CreateAlloca(ty);
    a->setAlignment(std::max({a->getAlign(), dl.getABITypeAlign(types.lower(t)), Align(8)}));
    return a;
  };
  std::vector<Value *> args;
  Value *ret_tmp = nullptr;
  if (Types::is_agg(f.ret)) {
    // The program's function writes a struct result through a pointer: C's
    // own, or a temporary to return the registers from.
    if (s.ret.kind == cabi::Arg::Indirect) {
      args.push_back(fn->getArg(0));
    } else {
      ret_tmp = temp(s.ret.mem, f.ret);
      args.push_back(ret_tmp);
    }
  }
  for (unsigned i = 0; i < f.params.size(); i++) {
    const cabi::Arg &a = s.params[i];
    Value *first = fn->getArg(s.first[i]);
    switch (a.kind) {
      case cabi::Arg::Direct: args.push_back(first); break;
      case cabi::Arg::Indirect: args.push_back(first); break;  // the callee copies it
      case cabi::Arg::Expand: {
        // The registers, stored back as the struct's bytes.
        Value *tmp = temp(a.mem, f.params[i]);
        if (a.parts.size() == 1) {
          b.CreateStore(first, tmp);
        } else {
          for (unsigned j = 0; j < a.parts.size(); j++)
            b.CreateStore(fn->getArg(s.first[i] + j), b.CreateStructGEP(a.mem, tmp, j));
        }
        args.push_back(tmp);
        break;
      }
    }
  }
  Value *result = b.CreateCall(jihoo_fn, args);
  if (s.ret.kind == cabi::Arg::Expand)
    b.CreateRet(b.CreateLoad(s.ret.mem, ret_tmp));
  else if (s.ret.kind == cabi::Arg::Indirect || f.ret.kind == jir::Type::Unit)
    b.CreateRetVoid();
  else
    b.CreateRet(result);
}

// Defines C's `int main(int argc, char **argv)`, which calls the program's
// `main` (with argc and argv if it takes them) and returns its result.
void define_c_main(Module &m, Function *jihoo_main, const jir::Function &f) {
  LLVMContext &ctx = m.getContext();
  Type *i32 = Type::getInt32Ty(ctx);
  auto *ty = FunctionType::get(i32, {i32, PointerType::get(ctx, 0)}, false);
  auto *fn = Function::Create(ty, GlobalValue::ExternalLinkage, "main", m);
  IRBuilder<> b(BasicBlock::Create(ctx, "entry", fn));
  std::vector<Value *> args;
  if (f.params.size() == 2) args = {fn->getArg(0), fn->getArg(1)};
  Value *result = b.CreateCall(jihoo_main, args);
  b.CreateRet(f.ret.kind == jir::Type::Int ? b.CreateTrunc(result, i32) : b.getInt32(0));
}

}  // namespace

std::unique_ptr<llvm::Module> codegen(const jir::Module &m, LLVMContext &ctx, const Triple &triple,
                                      const DataLayout &dl) {
  if (m.profile == jir::Profile::Hosted)
    throw CodegenError("hosted modules run on the VM and cannot be compiled natively");
  bool native = m.profile == jir::Profile::Native;
  if (!native && !m.externs.empty()) throw CodegenError("extern functions need a native module");

  auto mod = std::make_unique<Module>("jihoo", ctx);
  mod->setDataLayout(dl);
  if (native) {
    // Linked by the system C compiler, which usually makes a position
    // independent executable.
    mod->setPICLevel(PICLevel::BigPIC);
    mod->setPIELevel(PIELevel::Large);
  }
  Types types(ctx, m, dl);
  Glue glue(*mod, types, m);

  // Declare everything first so calls can refer to any function.
  std::unordered_map<std::string, Function *> fns;
  std::unordered_map<std::string, CFunc> externs;
  for (const auto &e : m.externs) {
    if (fns.count(e.name)) throw CodegenError("duplicate extern @" + e.name);
    externs.emplace(e.name, declare_extern(*mod, types, triple, e, fns[e.name]));
  }
  const std::string entry_name = native ? "main" : "_start";
  for (const auto &f : m.funcs) {
    if (fns.count(f.name)) throw CodegenError("duplicate function @" + f.name);
    bool is_start = !native && f.name == "_start";
    // The program's own functions get a prefix so they cannot clash with C
    // symbols: C's `main` (defined below, calling `jihoo.main`), the C
    // functions exported functions are defined as, and the C library's.
    auto *fn = Function::Create(types.signature(f),
                                is_start ? GlobalValue::ExternalLinkage : GlobalValue::InternalLinkage,
                                is_start ? f.name : "jihoo." + f.name, mod.get());
    if (!native) {
      // No libc to fall back on: never turn loops into memcpy/memset calls.
      fn->addFnAttr("no-builtins");
    }
    if (is_start) {
      fn->addFnAttr(Attribute::NoReturn);
      // The kernel enters `_start` with a 16-byte aligned stack, not the
      // "just called" alignment the ABI promises to normal functions.
      if (triple.getArch() == Triple::x86_64) fn->addFnAttr("stackrealign");
    }
    fns[f.name] = fn;
  }
  auto entry = std::find_if(m.funcs.begin(), m.funcs.end(), [&](const jir::Function &f) { return f.name == entry_name; });
  if (entry == m.funcs.end()) throw CodegenError(std::string(native ? "native" : "freestanding") + " module needs @" + entry_name);
  if (!entry->params.empty() && !native) throw CodegenError("@_start must take no parameters");

  for (const auto &f : m.funcs)
    FnGen(f, fns[f.name], *mod, types, fns, externs, triple, !native && f.name == "_start", native, glue).run();
  for (const auto &f : m.funcs)
    if (!f.export_name.empty()) define_export(*mod, types, triple, f, fns[f.name]);
  if (native) {
    define_c_main(*mod, fns["main"], *entry);
  } else {
    // What the C library would define, unless the program exports its own.
    auto missing = [&](const std::string &name) { return !mod->getFunction(name); };
    for (const char *kind : {"memcpy", "memmove", "memset"})
      if (missing(kind)) define_mem_function(*mod, kind);
    if (missing("fmod")) define_fmod(*mod, Type::getDoubleTy(ctx), "fmod");
    if (missing("fmodf")) define_fmod(*mod, Type::getFloatTy(ctx), "fmodf");
    for (std::string kind : {"floor", "ceil", "trunc", "round"}) {
      if (missing(kind)) define_rounding(*mod, Type::getDoubleTy(ctx), kind);
      if (missing(kind + "f")) define_rounding(*mod, Type::getFloatTy(ctx), kind);
    }
  }

  std::string err;
  raw_string_ostream os(err);
  if (verifyModule(*mod, &os)) throw CodegenError("LLVM verifier failed:\n" + os.str());
  return mod;
}

}  // namespace jihoo
