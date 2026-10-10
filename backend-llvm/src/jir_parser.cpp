// Line-oriented parser for the JIR text format (docs/jir.md).

#include "jir_parser.h"

#include <algorithm>
#include <cctype>
#include <sstream>
#include <unordered_map>

namespace jir {
namespace {

enum class TokKind { Word, Reg, Global, StructName, Int, Str, Punct };

struct Tok {
  TokKind kind;
  std::string text;  // word / global name / string bytes / punct char
  int64_t num = 0;   // Reg index or Int value
};

[[noreturn]] void fail(int line, const std::string &msg) {
  throw ParseError("line " + std::to_string(line) + ": " + msg);
}

int hex(int line, char c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'a' && c <= 'f') return c - 'a' + 10;
  if (c >= 'A' && c <= 'F') return c - 'A' + 10;
  fail(line, "bad hex digit in string escape");
}

int64_t number(int line, const std::string &s) {
  try {
    size_t used = 0;
    long long v = std::stoll(s, &used, 10);
    if (used != s.size()) fail(line, "bad number `" + s + "`");
    return v;
  } catch (const std::logic_error &) {
    fail(line, "bad number `" + s + "`");
  }
}

std::vector<Tok> tokenize(int line, const std::string &src) {
  std::vector<Tok> out;
  size_t i = 0;
  auto ident_char = [](char c) { return std::isalnum((unsigned char)c) || c == '_'; };
  while (i < src.size()) {
    char c = src[i];
    if (std::isspace((unsigned char)c)) {
      i++;
    } else if (c == ';') {
      break;  // comment
    } else if (c == '"') {
      std::string s;
      i++;
      while (true) {
        if (i >= src.size()) fail(line, "unterminated string");
        char d = src[i++];
        if (d == '"') break;
        if (d != '\\') {
          s += d;
          continue;
        }
        if (i >= src.size()) fail(line, "unterminated string");
        char e = src[i++];
        switch (e) {
          case 'n': s += '\n'; break;
          case 't': s += '\t'; break;
          case '\\': s += '\\'; break;
          case '"': s += '"'; break;
          case 'x':
            if (i + 2 > src.size()) fail(line, "short \\x escape");
            s += char(hex(line, src[i]) * 16 + hex(line, src[i + 1]));
            i += 2;
            break;
          default: fail(line, std::string("unknown escape \\") + e);
        }
      }
      out.push_back({TokKind::Str, s});
    } else if (c == '$' && i + 1 < src.size() && src[i + 1] == '"') {
      // A quoted struct name, such as `$"Pair(i64)"`.
      std::string name;
      i += 2;
      while (true) {
        if (i >= src.size()) fail(line, "unterminated struct name");
        char d = src[i++];
        if (d == '"') break;
        if (d == '\\' && i < src.size()) d = src[i++];
        name += d;
      }
      out.push_back({TokKind::StructName, name});
    } else if (c == '%' || c == '@' || c == '$') {
      size_t j = ++i;
      // Instance names of generic functions contain a dot: `@max.0`.
      while (i < src.size() && (ident_char(src[i]) || src[i] == '.')) i++;
      std::string name = src.substr(j, i - j);
      if (name.empty()) fail(line, std::string("expected a name after `") + c + "`");
      if (c == '%') {
        out.push_back({TokKind::Reg, name, number(line, name)});
      } else {
        out.push_back({c == '@' ? TokKind::Global : TokKind::StructName, name});
      }
    } else if (src.compare(i, 3, "...") == 0) {
      out.push_back({TokKind::Punct, "..."});
      i += 3;
    } else if (c == '-' && i + 1 < src.size() && src[i + 1] == '>') {
      out.push_back({TokKind::Punct, "->"});
      i += 2;
    } else if (std::isdigit((unsigned char)c) || c == '-') {
      size_t j = i++;
      while (i < src.size() && std::isdigit((unsigned char)src[i])) i++;
      out.push_back({TokKind::Int, "", number(line, src.substr(j, i - j))});
    } else if (ident_char(c)) {
      size_t j = i;
      while (i < src.size() && ident_char(src[i])) i++;
      out.push_back({TokKind::Word, src.substr(j, i - j)});
    } else if (std::string("=,(){}:*[]").find(c) != std::string::npos) {
      out.push_back({TokKind::Punct, std::string(1, c)});
      i++;
    } else {
      fail(line, std::string("unexpected character `") + c + "`");
    }
  }
  return out;
}

// Cursor over one line's tokens.
struct Line {
  int no;
  std::vector<Tok> toks;
  size_t i = 0;

  bool done() const { return i >= toks.size(); }
  const Tok &next(const char *what) {
    if (done()) fail(no, std::string("expected ") + what + ", found end of line");
    return toks[i++];
  }
  void end() {
    if (!done()) fail(no, "unexpected trailing tokens");
  }
  void punct(const char *p) {
    const Tok &t = next(p);
    if (t.kind != TokKind::Punct || t.text != p) fail(no, std::string("expected `") + p + "`");
  }
  void word(const char *w) {
    const Tok &t = next(w);
    if (t.kind != TokKind::Word || t.text != w) fail(no, std::string("expected `") + w + "`");
  }
  uint32_t reg() {
    const Tok &t = next("a register");
    if (t.kind != TokKind::Reg) fail(no, "expected a register like %0");
    return uint32_t(t.num);
  }
  int64_t integer() {
    const Tok &t = next("an integer");
    if (t.kind != TokKind::Int) fail(no, "expected an integer");
    return t.num;
  }
  uint32_t block() {
    const Tok &t = next("a block label");
    if (t.kind != TokKind::Word || t.text.rfind("bb", 0) != 0) fail(no, "expected a block like bb0");
    return uint32_t(number(no, t.text.substr(2)));
  }
  // unit | bool | str | i8..i64 | u8..u64 | *T | $Name | [N x T] | fn(T, ...) -> R
  Type type() {
    const Tok &t = next("a type");
    if (t.kind == TokKind::Word && t.text == "fn") {
      std::vector<Type> params;
      punct("(");
      if (!peek_punct(")")) {
        params.push_back(type());
        while (peek_punct(",")) {
          i++;
          params.push_back(type());
        }
      }
      punct(")");
      punct("->");
      Type ret = type();
      return Type::function(std::move(params), std::move(ret));
    }
    if (t.kind == TokKind::Punct && t.text == "*") return Type::pointer(type());
    if (t.kind == TokKind::Punct && t.text == "[") {
      int64_t n = integer();
      if (n < 0) fail(no, "array length must not be negative");
      word("x");
      Type elem = type();
      punct("]");
      return Type::array(std::move(elem), uint64_t(n));
    }
    if (t.kind == TokKind::StructName) {
      Type s;
      s.kind = Type::Struct;
      s.name = t.text;
      return s;
    }
    if (t.kind == TokKind::Word) {
      const std::string &w = t.text;
      if (w == "unit") return Type::unit();
      if (w == "bool") {
        Type b;
        b.kind = Type::Bool;
        return b;
      }
      if (w == "str") {
        Type s;
        s.kind = Type::Str;
        return s;
      }
      if (w.size() >= 2 && (w[0] == 'i' || w[0] == 'u')) {
        std::string digits = w.substr(1);
        if (digits == "8" || digits == "16" || digits == "32" || digits == "64")
          return Type::integer(unsigned(std::stoi(digits)), w[0] == 'i');
      }
    }
    fail(no, "expected a type");
  }
  uint32_t index() {
    int64_t n = integer();
    if (n < 0) fail(no, "field index must not be negative");
    return uint32_t(n);
  }
  bool peek_punct(const char *p) const {
    return !done() && toks[i].kind == TokKind::Punct && toks[i].text == p;
  }
  // `(` [reg {`,` reg}] `)`
  std::vector<uint32_t> reg_list() {
    std::vector<uint32_t> regs;
    punct("(");
    if (!peek_punct(")")) {
      regs.push_back(reg());
      while (peek_punct(",")) {
        i++;
        regs.push_back(reg());
      }
    }
    punct(")");
    return regs;
  }
};

const std::unordered_map<std::string, Op> kBinOps = {
    {"add", Op::Add}, {"sub", Op::Sub}, {"mul", Op::Mul}, {"div", Op::Div},
    {"rem", Op::Rem}, {"eq", Op::Eq},   {"ne", Op::Ne},   {"lt", Op::Lt},
    {"le", Op::Le},   {"gt", Op::Gt},   {"ge", Op::Ge},   {"and", Op::And},
    {"or", Op::Or},   {"xor", Op::Xor}, {"shl", Op::Shl}, {"shr", Op::Shr},
};

// `%d = <op> ...`
Inst parse_assign(Line &l) {
  Inst inst{};
  inst.dst = l.reg();
  l.punct("=");
  const Tok &op = l.next("an opcode");
  if (op.kind != TokKind::Word) fail(l.no, "expected an opcode");
  const std::string &w = op.text;

  if (w == "const") {
    inst.op = Op::Const;
    inst.imm = l.integer();
  } else if (w == "unit") {
    inst.op = Op::Unit;
  } else if (w == "str") {
    const Tok &s = l.next("a string");
    if (s.kind != TokKind::Str) fail(l.no, "expected a string literal");
    inst.op = Op::Str;
    inst.text = s.text;
  } else if (w == "copy" || w == "neg" || w == "not") {
    inst.op = w == "copy" ? Op::Copy : w == "neg" ? Op::Neg : Op::Not;
    inst.args = {l.reg()};
  } else if (auto it = kBinOps.find(w); it != kBinOps.end()) {
    inst.op = it->second;
    uint32_t a = l.reg();
    l.punct(",");
    inst.args = {a, l.reg()};
  } else if (w == "call") {
    // `call @f(...)` calls a function by name, `call %r(...)` a function value.
    const Tok &f = l.next("a function");
    if (f.kind == TokKind::Global) {
      inst.op = Op::Call;
      inst.text = f.text;
      inst.args = l.reg_list();
    } else if (f.kind == TokKind::Reg) {
      inst.op = Op::CallIndirect;
      inst.args = l.reg_list();
      inst.args.insert(inst.args.begin(), uint32_t(f.num));
    } else {
      fail(l.no, "expected a function like @name or %N");
    }
  } else if (w == "funcref") {
    const Tok &f = l.next("a function");
    if (f.kind != TokKind::Global) fail(l.no, "expected a function like @name");
    inst.op = Op::FuncRef;
    inst.text = f.text;
  } else if (w == "syscall") {
    inst.op = Op::Syscall;
    inst.args = l.reg_list();
  } else if (w == "cast" || w == "load" || w == "addr") {
    inst.op = w == "cast" ? Op::Cast : w == "load" ? Op::Load : Op::Addr;
    inst.args = {l.reg()};
  } else if (w == "struct") {
    const Tok &s = l.next("a struct");
    if (s.kind != TokKind::StructName) fail(l.no, "expected a struct like $Name");
    inst.op = Op::Struct;
    inst.text = s.text;
    inst.args = l.reg_list();
  } else if (w == "field" || w == "fieldptr") {
    inst.op = w == "field" ? Op::Field : Op::FieldPtr;
    inst.args = {l.reg()};
    l.punct(",");
    inst.imm = l.index();
  } else if (w == "asm") {
    const Tok &t = l.next("an asm template");
    if (t.kind != TokKind::Str) fail(l.no, "expected an asm template string");
    inst.text = t.text;
    l.punct(",");
    const Tok &c = l.next("asm constraints");
    if (c.kind != TokKind::Str) fail(l.no, "expected an asm constraint string");
    inst.constraints = c.text;
    inst.op = Op::Asm;
    inst.args = l.reg_list();
  } else if (w == "array") {
    inst.op = Op::Array;
    inst.args = l.reg_list();
  } else if (w == "splat") {
    inst.op = Op::Splat;
    inst.args = {l.reg()};
  } else if (w == "elem" || w == "elemptr") {
    inst.op = w == "elem" ? Op::Elem : Op::ElemPtr;
    uint32_t a = l.reg();
    l.punct(",");
    inst.args = {a, l.reg()};
  } else if (w == "setelem") {
    inst.op = Op::SetElem;
    uint32_t a = l.reg();
    l.punct(",");
    uint32_t i = l.reg();
    l.punct(",");
    inst.args = {a, i, l.reg()};
  } else if (w == "getpath" || w == "setpath") {
    // `getpath %s, (field N, elem %i, ...)` / `setpath %s, (...), %v`
    inst.op = w == "getpath" ? Op::GetPath : Op::SetPath;
    inst.args = {l.reg()};
    l.punct(",");
    l.punct("(");
    while (true) {
      const Tok &kind = l.next("`field` or `elem`");
      if (kind.kind == TokKind::Word && kind.text == "field") {
        inst.path.push_back({false, l.index()});
      } else if (kind.kind == TokKind::Word && kind.text == "elem") {
        inst.path.push_back({true, l.reg()});
      } else {
        fail(l.no, "expected `field N` or `elem %r`");
      }
      if (!l.peek_punct(",")) break;
      l.i++;
    }
    l.punct(")");
    if (inst.path.empty()) fail(l.no, "empty path");
    if (inst.op == Op::SetPath) {
      l.punct(",");
      inst.args.push_back(l.reg());
    }
  } else if (w == "variant") {
    inst.op = Op::Variant;
    inst.imm = l.index();
    inst.args = l.reg_list();
  } else if (w == "tag") {
    inst.op = Op::Tag;
    inst.args = {l.reg()};
  } else if (w == "payload") {
    inst.op = Op::Payload;
    inst.args = {l.reg()};
    l.punct(",");
    inst.imm = l.index();
    l.punct(",");
    inst.imm2 = l.index();
  } else if (w == "setfield") {
    inst.op = Op::SetField;
    uint32_t src = l.reg();
    l.punct(",");
    inst.imm = l.index();
    l.punct(",");
    inst.args = {src, l.reg()};
  } else {
    fail(l.no, "unknown opcode `" + w + "`");
  }
  l.end();
  return inst;
}

}  // namespace

Module parse(const std::string &text) {
  Module m;
  Function *fn = nullptr;
  Block *block = nullptr;  // current block, null after a terminator
  bool need_regs = false;  // the `regs` line must follow the function header
  bool saw_version = false, saw_profile = false;

  std::istringstream in(text);
  std::string raw;
  int no = 0;
  while (std::getline(in, raw)) {
    no++;
    Line l{no, tokenize(no, raw)};
    if (l.done()) continue;
    const Tok &first = l.toks[0];

    if (!fn) {
      // Module header and function headers.
      l.i++;
      if (first.kind == TokKind::Word && first.text == "jir") {
        if (l.integer() != 0) fail(no, "unsupported JIR version");
        saw_version = true;
      } else if (first.kind == TokKind::Word && first.text == "profile") {
        const Tok &p = l.next("a profile");
        if (p.text == "hosted") m.profile = Profile::Hosted;
        else if (p.text == "native") m.profile = Profile::Native;
        else if (p.text == "freestanding") m.profile = Profile::Freestanding;
        else fail(no, "unknown profile `" + p.text + "`");
        saw_profile = true;
      } else if (first.kind == TokKind::Word && first.text == "struct") {
        const Tok &name = l.next("a struct name");
        if (name.kind != TokKind::StructName) fail(no, "expected a struct like $Name");
        StructDef def{name.text, {}};
        l.punct("{");
        while (!l.peek_punct("}")) {
          const Tok &field = l.next("a field name");
          if (field.kind != TokKind::Word) fail(no, "expected a field name");
          l.punct(":");
          def.fields.emplace_back(field.text, l.type());
          if (!l.peek_punct("}")) l.punct(",");
        }
        l.punct("}");
        if (!l.done()) {
          l.word("size");
          def.size = uint64_t(l.integer());
          l.word("align");
          def.align = uint64_t(l.integer());
          def.has_layout = true;
        }
        m.structs.push_back(std::move(def));
      } else if (first.kind == TokKind::Word && first.text == "enum") {
        // enum $Name { A, B(T, U), ... } [size S align A]
        const Tok &name = l.next("an enum name");
        if (name.kind != TokKind::StructName) fail(no, "expected an enum like $Name");
        EnumDef def{name.text, {}};
        l.punct("{");
        while (!l.peek_punct("}")) {
          const Tok &variant = l.next("a variant name");
          if (variant.kind != TokKind::Word) fail(no, "expected a variant name");
          std::vector<Type> payload;
          if (l.peek_punct("(")) {
            l.i++;
            if (!l.peek_punct(")")) {
              payload.push_back(l.type());
              while (l.peek_punct(",")) {
                l.i++;
                payload.push_back(l.type());
              }
            }
            l.punct(")");
          }
          def.variants.emplace_back(variant.text, std::move(payload));
          if (!l.peek_punct("}")) l.punct(",");
        }
        l.punct("}");
        if (!l.done()) {
          l.word("size");
          def.size = uint64_t(l.integer());
          l.word("align");
          def.align = uint64_t(l.integer());
          def.has_layout = true;
        }
        m.enums.push_back(std::move(def));
      } else if (first.kind == TokKind::Word && first.text == "extern") {
        // extern @name(T, U[, ...]) -> R
        const Tok &name = l.next("a function name");
        if (name.kind != TokKind::Global) fail(no, "expected a function like @name");
        ExternFn e;
        e.name = name.text;
        l.punct("(");
        while (!l.peek_punct(")")) {
          if (l.peek_punct("...")) {
            l.i++;
            e.variadic = true;
            break;
          }
          e.params.push_back(l.type());
          if (!l.peek_punct(")")) l.punct(",");
        }
        l.punct(")");
        l.punct("->");
        e.ret = l.type();
        m.externs.push_back(std::move(e));
      } else if (first.kind == TokKind::Word && first.text == "fn") {
        if (!saw_version || !saw_profile) fail(no, "missing `jir 0` / `profile` header");
        const Tok &name = l.next("a function name");
        if (name.kind != TokKind::Global) fail(no, "expected a function like @name");
        m.funcs.push_back({});
        fn = &m.funcs.back();
        fn->name = name.text;
        l.punct("(");
        if (!l.peek_punct(")")) {
          fn->params.push_back(l.type());
          while (l.peek_punct(",")) {
            l.i++;
            fn->params.push_back(l.type());
          }
        }
        l.punct(")");
        l.punct("->");
        fn->ret = l.type();
        l.punct("{");
        need_regs = true;
      } else {
        fail(no, "expected `jir`, `profile`, `struct`, `enum`, `extern`, or `fn`");
      }
      l.end();
      continue;
    }

    // Inside a function body.
    if (need_regs) {
      l.word("regs");
      while (!l.done()) fn->regs.push_back(l.type());
      if (fn->regs.size() < fn->params.size() ||
          !std::equal(fn->params.begin(), fn->params.end(), fn->regs.begin()))
        fail(no, "register types must start with the parameter types");
      need_regs = false;
    } else if (first.kind == TokKind::Punct && first.text == "}") {
      if (block) fail(no, "block bb" + std::to_string(fn->blocks.size() - 1) + " has no terminator");
      if (fn->blocks.empty()) fail(no, "function has no blocks");
      fn = nullptr;
      l.i++;
      l.end();
    } else if (first.kind == TokKind::Word && first.text.rfind("bb", 0) == 0 &&
               l.toks.size() == 2 && l.toks[1].text == ":") {
      if (block) fail(no, "previous block has no terminator");
      uint32_t id = l.block();
      if (id != fn->blocks.size()) fail(no, "blocks must be numbered in order");
      fn->blocks.push_back({});
      block = &fn->blocks.back();
    } else {
      if (!block) fail(no, "instruction outside of a block");
      if (first.kind == TokKind::Reg) {
        block->insts.push_back(parse_assign(l));
        continue;
      }
      const std::string &w = l.next("an instruction").text;
      Terminator &t = block->term;
      if (w == "print" || w == "store") {
        Inst inst{};
        inst.op = w == "print" ? Op::Print : Op::Store;
        inst.args = {l.reg()};
        if (inst.op == Op::Store) {
          l.punct(",");
          inst.args.push_back(l.reg());
        }
        block->insts.push_back(inst);
        l.end();
        continue;
      } else if (w == "jmp") {
        t.kind = TermKind::Jump;
        t.target = l.block();
      } else if (w == "br") {
        t.kind = TermKind::Branch;
        t.reg = l.reg();
        l.punct(",");
        t.target = l.block();
        l.punct(",");
        t.els = l.block();
      } else if (w == "ret") {
        t.kind = TermKind::Ret;
        t.reg = l.reg();
      } else if (w == "unreachable") {
        t.kind = TermKind::Unreachable;
      } else {
        fail(no, "unknown instruction `" + w + "`");
      }
      l.end();
      block = nullptr;
    }
  }
  if (fn) fail(no, "unterminated function @" + fn->name);
  if (!saw_version) fail(no, "missing `jir 0` header");
  return m;
}

}  // namespace jir
