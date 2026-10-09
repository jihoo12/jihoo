// Line-oriented parser for the JIR text format (docs/jir.md).

#include "jir_parser.h"

#include <algorithm>
#include <cctype>
#include <sstream>
#include <unordered_map>

namespace jir {
namespace {

enum class TokKind { Word, Reg, Global, Int, Str, Punct };

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
    } else if (c == '%' || c == '@') {
      size_t j = ++i;
      while (i < src.size() && ident_char(src[i])) i++;
      std::string name = src.substr(j, i - j);
      if (name.empty()) fail(line, std::string("expected a name after `") + c + "`");
      if (c == '%') {
        out.push_back({TokKind::Reg, name, number(line, name)});
      } else {
        out.push_back({TokKind::Global, name});
      }
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
    } else if (std::string("=,(){}:").find(c) != std::string::npos) {
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
  Type type() {
    const Tok &t = next("a type");
    if (t.kind == TokKind::Word) {
      if (t.text == "unit") return Type::Unit;
      if (t.text == "i64") return Type::I64;
      if (t.text == "bool") return Type::Bool;
      if (t.text == "str") return Type::Str;
      if (t.text == "ptr") return Type::Ptr;
    }
    fail(no, "expected a type (unit, i64, bool, str, ptr)");
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
    {"le", Op::Le},   {"gt", Op::Gt},   {"ge", Op::Ge},
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
    const Tok &f = l.next("a function");
    if (f.kind != TokKind::Global) fail(l.no, "expected a function like @name");
    inst.op = Op::Call;
    inst.text = f.text;
    inst.args = l.reg_list();
  } else if (w == "syscall") {
    inst.op = Op::Syscall;
    inst.args = l.reg_list();
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
        else if (p.text == "freestanding") m.profile = Profile::Freestanding;
        else fail(no, "unknown profile `" + p.text + "`");
        saw_profile = true;
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
        fail(no, "expected `jir`, `profile`, or `fn`");
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
      if (w == "print") {
        Inst inst{};
        inst.op = Op::Print;
        inst.args = {l.reg()};
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
