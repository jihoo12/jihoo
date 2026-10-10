// Classifies structs for the C calling convention of each target, the way
// clang does for C:
//
// x86_64 (System V): a struct of at most 16 bytes is split into eightbytes;
// one holding only floats goes in an SSE register (as `double`, `float` or
// `<2 x float>`), any other in a general one (as `i64`). If the registers the
// whole struct needs are not all free, it goes on the stack instead (`byval`),
// never half and half. Larger structs go on the stack; a larger result is
// written through a hidden pointer (`sret`) the caller passes in `rdi`.
//
// aarch64 (AAPCS64): a struct of one to four floats of one type (a
// homogeneous floating-point aggregate) goes in that many SIMD registers (as
// `[N x float]` or `[N x double]`); any other struct of at most 16 bytes in one
// or two general registers (as `i64` or `[2 x i64]`); on the stack, the floats
// start at a multiple of 8 (`alignstack(8)`). LLVM puts such an array
// in consecutive registers or wholly on the stack. A larger struct is passed
// as a pointer to a copy the caller makes; a larger result through `sret`
// (`x8`).

#include "cabi.h"

#include <llvm/IR/LLVMContext.h>

#include "codegen.h"

using namespace llvm;

namespace jihoo::cabi {
namespace {

// A scalar inside a struct, at `offset` bytes.
struct Leaf {
  uint64_t offset;
  Type *type;
};

void leaves(const DataLayout &dl, Type *t, uint64_t base, std::vector<Leaf> &out) {
  if (auto *st = dyn_cast<StructType>(t)) {
    const StructLayout *layout = dl.getStructLayout(st);
    for (unsigned i = 0; i < st->getNumElements(); i++)
      leaves(dl, st->getElementType(i), base + layout->getElementOffset(i), out);
  } else if (auto *at = dyn_cast<ArrayType>(t)) {
    uint64_t size = dl.getTypeAllocSize(at->getElementType());
    for (uint64_t i = 0; i < at->getNumElements(); i++) leaves(dl, at->getElementType(), base + i * size, out);
  } else {
    out.push_back({base, t});
  }
}

bool is_struct(Type *t) { return t && t->isStructTy(); }

// x86_64: the eightbytes of a struct of at most 16 bytes, and how many
// general (`ints`) and SSE (`sses`) registers they take.
struct Eightbytes {
  std::vector<Type *> parts;
  unsigned ints = 0, sses = 0;
};

Eightbytes x86_eightbytes(const DataLayout &dl, LLVMContext &ctx, Type *st) {
  std::vector<Leaf> ls;
  leaves(dl, st, 0, ls);
  Eightbytes e;
  uint64_t n = (dl.getTypeAllocSize(st) + 7) / 8;
  for (uint64_t i = 0; i < n; i++) {
    bool integer = false, dbl = false, high_float = false;
    for (const Leaf &l : ls) {
      if (l.offset / 8 != i) continue;
      if (!l.type->isFloatingPointTy()) integer = true;
      else if (l.type->isDoubleTy()) dbl = true;
      else if (l.offset % 8 == 4) high_float = true;
    }
    if (integer) {
      e.parts.push_back(Type::getInt64Ty(ctx));
      e.ints++;
    } else {
      e.parts.push_back(dbl ? Type::getDoubleTy(ctx)
                        : high_float ? static_cast<Type *>(FixedVectorType::get(Type::getFloatTy(ctx), 2))
                                     : Type::getFloatTy(ctx));
      e.sses++;
    }
  }
  return e;
}

Arg expand(LLVMContext &ctx, std::vector<Type *> parts) {
  Arg a;
  a.kind = Arg::Expand;
  a.mem = parts.size() == 1 ? parts[0] : StructType::get(ctx, parts);
  a.parts = std::move(parts);
  return a;
}

Arg indirect(bool byval) {
  Arg a;
  a.kind = Arg::Indirect;
  a.byval = byval;
  return a;
}

// aarch64: a struct of one to four floats of one type, as `[N x T]`.
Type *aarch64_hfa(const DataLayout &dl, Type *st) {
  std::vector<Leaf> ls;
  leaves(dl, st, 0, ls);
  if (ls.empty() || ls.size() > 4 || !ls[0].type->isFloatingPointTy()) return nullptr;
  for (const Leaf &l : ls)
    if (l.type != ls[0].type) return nullptr;
  return ArrayType::get(ls[0].type, ls.size());
}

// aarch64: how a struct goes, as a parameter or a result (the same).
Arg aarch64_struct(const DataLayout &dl, LLVMContext &ctx, Type *st) {
  if (Type *hfa = aarch64_hfa(dl, st)) {
    Arg a = expand(ctx, {hfa});
    a.stack_align = 8;
    return a;
  }
  uint64_t size = dl.getTypeAllocSize(st);
  if (size > 16) return indirect(false);
  Type *i64 = Type::getInt64Ty(ctx);
  return expand(ctx, {size <= 8 ? i64 : static_cast<Type *>(ArrayType::get(i64, 2))});
}

}  // namespace

Signature lower(const Triple &triple, const DataLayout &dl, LLVMContext &ctx, const std::vector<Type *> &params,
                Type *ret, bool variadic) {
  bool x86 = triple.getArch() == Triple::x86_64;
  if (!x86 && triple.getArch() != Triple::aarch64)
    throw CodegenError("the C calling convention is not known for " + triple.str());
  Signature s;
  Type *ptr = PointerType::get(ctx, 0);
  std::vector<Type *> lowered;

  // The result.
  unsigned ints = 6, sses = 8;  // x86_64's argument registers
  if (!ret) {
    s.ret.mem = Type::getVoidTy(ctx);
  } else if (!is_struct(ret)) {
    s.ret.mem = ret;
  } else if (x86 && dl.getTypeAllocSize(ret) <= 16) {
    s.ret = expand(ctx, x86_eightbytes(dl, ctx, ret).parts);
  } else if (x86) {
    s.ret = indirect(false);
  } else {
    s.ret = aarch64_struct(dl, ctx, ret);
  }
  if (s.ret.kind == Arg::Indirect) {
    lowered.push_back(ptr);
    ints--;
  }

  for (Type *t : params) {
    s.first.push_back(lowered.size());
    Arg a;
    if (!is_struct(t)) {
      a.mem = t;
      if (t->isFloatingPointTy()) {
        if (sses) sses--;
      } else if (ints) {
        ints--;
      }
    } else if (x86) {
      Eightbytes e = dl.getTypeAllocSize(t) <= 16 ? x86_eightbytes(dl, ctx, t) : Eightbytes{};
      if (!e.parts.empty() && e.ints <= ints && e.sses <= sses) {
        ints -= e.ints;
        sses -= e.sses;
        a = expand(ctx, e.parts);
      } else {
        a = indirect(true);
      }
    } else {
      a = aarch64_struct(dl, ctx, t);
    }
    if (a.kind == Arg::Expand)
      lowered.insert(lowered.end(), a.parts.begin(), a.parts.end());
    else
      lowered.push_back(a.kind == Arg::Indirect ? ptr : t);
    s.params.push_back(a);
  }

  Type *result = s.ret.kind == Arg::Indirect ? Type::getVoidTy(ctx) : s.ret.mem;
  s.type = FunctionType::get(result, lowered, variadic);
  return s;
}

}  // namespace jihoo::cabi
