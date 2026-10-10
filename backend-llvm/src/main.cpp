// jihoo-llc: compiles a native or freestanding JIR module (.jir) to an object file.

#include <fstream>
#include <iostream>
#include <optional>
#include <sstream>

#include <llvm/IR/LegacyPassManager.h>
#include <llvm/MC/TargetRegistry.h>
#include <llvm/Passes/PassBuilder.h>
#include <llvm/Support/FileSystem.h>
#include <llvm/Support/TargetSelect.h>
#include <llvm/Support/raw_ostream.h>
#include <llvm/Target/TargetMachine.h>
#include <llvm/Target/TargetOptions.h>
#include <llvm/TargetParser/Host.h>

#include "codegen.h"
#include "jir_parser.h"

namespace {

const char *kUsage =
    "usage: jihoo-llc <input.jir> -o <output> [options]\n"
    "\n"
    "options:\n"
    "  --emit-llvm        write textual LLVM IR instead of an object file\n"
    "  -O0 | -O1 | -O2 | -O3   optimization level (default -O2)\n"
    "  --target <triple>  target triple (default: host)\n";

struct Options {
  std::string input, output;
  bool emit_llvm = false;
  int opt_level = 2;
  std::string triple;
};

std::optional<Options> parse_args(int argc, char **argv) {
  Options o;
  for (int i = 1; i < argc; i++) {
    std::string a = argv[i];
    if (a == "-o" && i + 1 < argc) {
      o.output = argv[++i];
    } else if (a == "--target" && i + 1 < argc) {
      o.triple = argv[++i];
    } else if (a == "--emit-llvm") {
      o.emit_llvm = true;
    } else if (a.size() == 3 && a.rfind("-O", 0) == 0 && a[2] >= '0' && a[2] <= '3') {
      o.opt_level = a[2] - '0';
    } else if (a == "-h" || a == "--help") {
      return std::nullopt;
    } else if (!a.empty() && a[0] != '-' && o.input.empty()) {
      o.input = a;
    } else {
      std::cerr << "jihoo-llc: unknown argument `" << a << "`\n";
      return std::nullopt;
    }
  }
  if (o.input.empty() || o.output.empty()) return std::nullopt;
  return o;
}

void optimize(llvm::Module &m, llvm::TargetMachine &tm, int level) {
  using llvm::OptimizationLevel;
  llvm::LoopAnalysisManager lam;
  llvm::FunctionAnalysisManager fam;
  llvm::CGSCCAnalysisManager cgam;
  llvm::ModuleAnalysisManager mam;
  llvm::PassBuilder pb(&tm);
  pb.registerModuleAnalyses(mam);
  pb.registerCGSCCAnalyses(cgam);
  pb.registerFunctionAnalyses(fam);
  pb.registerLoopAnalyses(lam);
  pb.crossRegisterProxies(lam, fam, cgam, mam);

  llvm::ModulePassManager mpm;
  switch (level) {
    case 0: mpm = pb.buildO0DefaultPipeline(OptimizationLevel::O0); break;
    case 1: mpm = pb.buildPerModuleDefaultPipeline(OptimizationLevel::O1); break;
    case 2: mpm = pb.buildPerModuleDefaultPipeline(OptimizationLevel::O2); break;
    default: mpm = pb.buildPerModuleDefaultPipeline(OptimizationLevel::O3); break;
  }
  mpm.run(m, mam);
}

int run(const Options &o) {
  std::ifstream in(o.input);
  if (!in) {
    std::cerr << "jihoo-llc: cannot open " << o.input << "\n";
    return 1;
  }
  std::stringstream text;
  text << in.rdbuf();
  jir::Module jm = jir::parse(text.str());

  llvm::InitializeAllTargetInfos();
  llvm::InitializeAllTargets();
  llvm::InitializeAllTargetMCs();
  llvm::InitializeAllAsmParsers();  // needed for the inline asm in `syscall`
  llvm::InitializeAllAsmPrinters();

  llvm::Triple triple(o.triple.empty() ? llvm::sys::getDefaultTargetTriple() : o.triple);
  std::string err;
  const llvm::Target *target = llvm::TargetRegistry::lookupTarget(triple, err);
  if (!target) {
    std::cerr << "jihoo-llc: " << err << "\n";
    return 1;
  }
  // Native programs are linked by the C compiler, usually into a position
  // independent executable; freestanding ones are linked statically.
  auto reloc = jm.profile == jir::Profile::Native ? llvm::Reloc::PIC_ : llvm::Reloc::Static;
  std::unique_ptr<llvm::TargetMachine> tm(target->createTargetMachine(
      triple, "generic", "", llvm::TargetOptions(), reloc));

  llvm::LLVMContext ctx;
  auto mod = jihoo::codegen(jm, ctx, triple, tm->createDataLayout());
  mod->setTargetTriple(triple);
  optimize(*mod, *tm, o.opt_level);

  std::error_code ec;
  llvm::raw_fd_ostream out(o.output, ec, llvm::sys::fs::OF_None);
  if (ec) {
    std::cerr << "jihoo-llc: cannot write " << o.output << ": " << ec.message() << "\n";
    return 1;
  }
  if (o.emit_llvm) {
    mod->print(out, nullptr);
    return 0;
  }
  llvm::legacy::PassManager pm;
  if (tm->addPassesToEmitFile(pm, out, nullptr, llvm::CodeGenFileType::ObjectFile)) {
    std::cerr << "jihoo-llc: target cannot emit object files\n";
    return 1;
  }
  pm.run(*mod);
  return 0;
}

}  // namespace

int main(int argc, char **argv) {
  auto opts = parse_args(argc, argv);
  if (!opts) {
    std::cerr << kUsage;
    return 2;
  }
  try {
    return run(*opts);
  } catch (const jir::ParseError &e) {
    std::cerr << opts->input << ": " << e.what() << "\n";
  } catch (const jihoo::CodegenError &e) {
    std::cerr << opts->input << ": " << e.what() << "\n";
  }
  return 1;
}
