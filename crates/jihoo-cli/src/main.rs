//! `jihoo` command line driver.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use jihoo_ir::{Module, Profile, Target};

const USAGE: &str = "\
usage:
  jihoo run <file.jh> [link inputs] [-- args]
                                      compile and run a program (a hosted
                                      one on the VM), passing it args
  jihoo emit-ir <file.jh> [-o out]    print JIR (text format)
  jihoo build <file.jh> [-o out] [link inputs]
                                      compile a native or freestanding program

Programs are native unless their root file says `#![hosted]` (run on the VM
with a GC) or `#![freestanding]` (no libc).

Each command also takes a JIR file (<file.jir>), such as `emit-ir` writes,
in place of a source file.

options:
  -I <dir>    also look for imported modules in <dir> (may be repeated)
  --target <t>  compile for x86_64 or aarch64 Linux (emit-ir, build;
              default: this machine)
  -O0 ... -O3 how much the LLVM backend optimizes (run, build; default -O2)

link inputs (native programs only, passed on to the C compiler; `run` and
`build`):
  -l <lib>    link with lib<lib> (also -l<lib>)
  -L <dir>    look for libraries in <dir> (also -L<dir>)
  <file>      a .c, .o, .a or .so file to compile or link in

environment:
  JIHOO_PATH  directories to look for imported modules in, separated by `:`
  JIHOO_LLC   path to the LLVM backend (default: jihoo-llc in PATH)
  JIHOO_LD    linker for freestanding programs (default: ld.lld in PATH)
  JIHOO_CC    C compiler that links native programs (default: cc in PATH)

`import a.b` loads `a/b.jh` from the importing file's directory, the -I
directories, JIHOO_PATH, and finally the standard library (`lib/`); in each,
`a/b.<target>.jh` (such as `a/b.x86_64.jh`) comes first.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = jihoo_syntax::on_compiler_stack(|| dispatch(&args));
    match result {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

struct Opts {
    input: PathBuf,
    output: Option<PathBuf>,
    include: Vec<PathBuf>,
    target: Target,
    /// `--target` was given, rather than the default.
    target_given: bool,
    /// Arguments for the C compiler that links a native program: `-l`, `-L`
    /// and extra input files.
    link: Vec<String>,
    /// Arguments after `--`, for the program `jihoo run` runs.
    args: Vec<String>,
    /// `-O0` ... `-O3`, for the LLVM backend.
    opt: Option<String>,
}

/// Files `jihoo build` passes on to the C compiler.
const LINK_INPUTS: [&str; 4] = ["c", "o", "a", "so"];

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut input = None;
    let mut output = None;
    let mut include = Vec::new();
    let mut link = Vec::new();
    let mut target = Target::host();
    let mut target_given = false;
    let mut program_args = Vec::new();
    let mut opt = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => {
                program_args = it.by_ref().cloned().collect();
                break;
            }
            "-o" => output = Some(PathBuf::from(it.next().ok_or("`-o` needs a path")?)),
            "-I" => include.push(PathBuf::from(it.next().ok_or("`-I` needs a directory")?)),
            "--target" => {
                let name = it.next().ok_or("`--target` needs a target: x86_64 or aarch64")?;
                target = Target::parse(name)
                    .ok_or_else(|| format!("unknown target `{name}`; the targets are x86_64 and aarch64"))?;
                target_given = true;
            }
            "-O0" | "-O1" | "-O2" | "-O3" => opt = Some(a.clone()),
            "-l" | "-L" => link.push(format!("{a}{}", it.next().ok_or_else(|| format!("`{a}` needs an argument"))?)),
            s if (s.starts_with("-l") || s.starts_with("-L")) && s.len() > 2 => link.push(s.to_string()),
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`\n\n{USAGE}")),
            s if Path::new(s).extension().is_some_and(|e| LINK_INPUTS.iter().any(|x| e == *x)) => {
                link.push(s.to_string())
            }
            s if input.is_none() => input = Some(PathBuf::from(s)),
            s => return Err(format!("unexpected argument `{s}`")),
        }
    }
    let input = input.ok_or_else(|| format!("missing input file\n\n{USAGE}"))?;
    Ok(Opts { input, output, include, target, target_given, link, args: program_args, opt })
}

/// `parse_opts` for commands that do not link or run.
fn parse_opts_no_link(args: &[String]) -> Result<Opts, String> {
    let opts = parse_opts(args)?;
    if let Some(a) = opts.link.first() {
        return Err(format!("`{a}` is a link input; only `jihoo build` and `jihoo run` link"));
    }
    if !opts.args.is_empty() {
        return Err("arguments after `--` are for `jihoo run`".into());
    }
    if let Some(o) = &opts.opt {
        return Err(format!("`{o}` is for the LLVM backend; only `jihoo build` and `jihoo run` use it"));
    }
    Ok(opts)
}

fn dispatch(args: &[String]) -> Result<ExitCode, String> {
    let Some((cmd, rest)) = args.split_first() else {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };
    match cmd.as_str() {
        "run" => {
            let opts = parse_opts(rest)?;
            if opts.output.is_some() {
                return Err("`jihoo run` writes no output file; use `jihoo build -o`".into());
            }
            if opts.target != Target::host() {
                return Err(format!("`jihoo run` runs programs on this machine, which is not {}", opts.target.as_str()));
            }
            let m = compile(&opts)?;
            if m.profile == Profile::Hosted {
                if let Some(a) = opts.link.first() {
                    return Err(format!("{}: hosted programs link nothing, so `{a}` cannot be linked in", opts.input.display()));
                }
                if !opts.args.is_empty() {
                    return Err(format!("{}: hosted programs take no arguments", opts.input.display()));
                }
                let code = jihoo_vm::run(&m, &mut std::io::stdout()).map_err(|e| e.to_string())?;
                return Ok(ExitCode::from(code as u8));
            }
            run_compiled(&m, &opts)
        }
        "emit-ir" => {
            let opts = parse_opts_no_link(rest)?;
            let m = compile(&opts)?;
            match opts.output {
                Some(p) => write(&p, &m.to_string())?,
                None => print!("{m}"),
            }
            Ok(ExitCode::SUCCESS)
        }
        "build" => {
            let opts = parse_opts(rest)?;
            build(&opts)?;
            Ok(ExitCode::SUCCESS)
        }
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

/// Where `import` looks, after the importing file's own directory.
fn search_path(include: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = include.to_vec();
    if let Some(p) = std::env::var_os("JIHOO_PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    // The standard library of a source checkout. Installed builds point
    // JIHOO_PATH at their copy instead.
    if let Ok(lib) = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib").canonicalize() {
        dirs.push(lib);
    }
    dirs
}

/// `file:line:col: message`, with the file the error is in.
fn render(files: &[PathBuf], e: &jihoo_syntax::Error) -> String {
    let file = files.get(e.pos.file as usize).map(|p| p.display().to_string()).unwrap_or_default();
    if e.pos.line == 0 {
        format!("{file}: {}", e.msg)
    } else {
        format!("{file}:{e}")
    }
}

/// The module `opts.input` holds: a program compiled from source, or a JIR
/// file read as it is (its `target` line says what it was made for).
fn compile(opts: &Opts) -> Result<Module, String> {
    let path = &opts.input;
    if path.extension().is_some_and(|e| e == "jir") {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let m = jihoo_ir::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if opts.target_given && m.target != opts.target {
            return Err(format!(
                "{}: the module is for {}, not {}; JIR is made for one target",
                path.display(),
                m.target.as_str(),
                opts.target.as_str()
            ));
        }
        jihoo_ir::verify(&m).map_err(|e| format!("{}: invalid IR: {e}", path.display()))?;
        return Ok(m);
    }
    let loaded = jihoo_syntax::loader::load(path, &search_path(&opts.include), opts.target.as_str())
        .map_err(|(e, files)| render(&files, &e))?;
    let mut m = jihoo_sema::analyze_modules(&loaded.modules).map_err(|errs| {
        errs.iter().map(|e| render(&loaded.files, e)).collect::<Vec<_>>().join("\nerror: ")
    })?;
    m.target = opts.target;
    jihoo_ir::verify(&m).map_err(|e| format!("{}: invalid IR: {e}", path.display()))?;
    Ok(m)
}

/// source -> .jir -> (jihoo-llc) -> .o, then
///   native:       (cc) -> a program linked with libc and the link inputs
///   freestanding: (ld.lld) -> a static binary with nothing else in it
fn build(opts: &Opts) -> Result<(), String> {
    if !opts.args.is_empty() {
        return Err("arguments after `--` are for `jihoo run`".into());
    }
    let m = compile(opts)?;
    if m.profile == Profile::Hosted {
        return Err(format!(
            "{}: `jihoo build` needs a native or freestanding program; hosted programs \
             (`#![hosted]`) run on the VM with `jihoo run`",
            opts.input.display()
        ));
    }
    let out = opts.output.clone().unwrap_or_else(|| opts.input.with_extension(""));
    // Without an extension, the default output is the source file itself.
    if same_file(&out, &opts.input) {
        return Err(format!(
            "{}: the program would overwrite its own source; name the output with `-o`",
            opts.input.display()
        ));
    }
    link(&m, opts, &out)
}

/// `jihoo run` for a native or freestanding program: builds it in a fresh
/// temporary directory, runs it with `opts.args` and exits as it did.
fn run_compiled(m: &Module, opts: &Opts) -> Result<ExitCode, String> {
    use std::os::unix::process::ExitStatusExt;

    let dir = std::env::temp_dir().join(format!("jihoo-run-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let stem = opts.input.file_stem().map_or("program".into(), |s| s.to_string_lossy().into_owned());
    let bin = dir.join(stem);
    let status = link(m, opts, &bin).and_then(|_| {
        Command::new(&bin).args(&opts.args).status().map_err(|e| format!("cannot run {}: {e}", bin.display()))
    });
    let _ = std::fs::remove_dir_all(&dir);
    let status = status?;
    if let Some(code) = status.code() {
        return Ok(ExitCode::from(code as u8));
    }
    // Killed by a signal: say so, and exit as a shell would report it.
    let sig = status.signal().unwrap_or(0);
    let what = match sig {
        4 | 5 => " (a trap: `trap()`, a failed bounds or overflow check, or `unreachable`)",
        6 => " (aborted)",
        11 => " (segmentation fault)",
        _ => "",
    };
    eprintln!("error: {} was killed by signal {sig}{what}", opts.input.display());
    Ok(ExitCode::from((128 + sig) as u8))
}

/// Compiles `m`, a native or freestanding program, to the executable `out`.
fn link(m: &Module, opts: &Opts, out: &Path) -> Result<(), String> {
    match m.profile {
        Profile::Hosted => unreachable!("hosted programs run on the VM"),
        Profile::Freestanding if !opts.link.is_empty() => {
            return Err(format!(
                "{}: freestanding programs link nothing but themselves, so `{}` cannot be linked in; \
                 a native program (without `#![freestanding]`) links with C",
                opts.input.display(),
                opts.link[0]
            ))
        }
        _ => {}
    }

    // Not `out.o`: that may be one of the link inputs.
    let jir = out.with_extension("jihoo.jir");
    let obj = out.with_extension("jihoo.o");
    write(&jir, &m.to_string())?;

    let llc = std::env::var("JIHOO_LLC").unwrap_or_else(|_| "jihoo-llc".into());
    let result = run_tool(Command::new(&llc).arg(&jir).arg("-o").arg(&obj).args(&opts.opt)).and_then(|_| {
        if m.profile == Profile::Native {
            let cc = std::env::var("JIHOO_CC").unwrap_or_else(|_| "cc".into());
            // libm is part of the C standard library, and float `%` calls its `fmod`.
            run_tool(Command::new(&cc).arg("-o").arg(out).arg(&obj).args(&opts.link).arg("-lm"))
        } else {
            let ld = std::env::var("JIHOO_LD").unwrap_or_else(|_| "ld.lld".into());
            run_tool(
                Command::new(&ld)
                    .args(["-static", "--gc-sections", "-e", "_start", "-o"])
                    .arg(out)
                    .arg(&obj),
            )
        }
    });

    let _ = std::fs::remove_file(&obj);
    let _ = std::fs::remove_file(&jir);
    result
}

/// Whether `a` and `b` name the same file. `b` exists; `a` may not.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

fn run_tool(cmd: &mut Command) -> Result<(), String> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    let status = cmd.status().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            // Most likely the first program someone compiles without the toolchain.
            format!(
                "cannot run `{name}`: not found. Native and freestanding programs need the LLVM \
                 backend (`jihoo-llc`, or JIHOO_LLC) and a linker (`cc`/JIHOO_CC, `ld.lld`/JIHOO_LD); \
                 `nix develop` provides them. Without them, a program with `#![hosted]` runs on the VM"
            )
        } else {
            format!("cannot run `{name}`: {e}")
        }
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{name}` failed ({status})"))
    }
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}
