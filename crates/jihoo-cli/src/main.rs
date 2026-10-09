//! `jihoo` command line driver.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use jihoo_ir::{Module, Profile};

const USAGE: &str = "\
usage:
  jihoo run <file.jh>                 run a hosted program on the VM
  jihoo emit-ir <file.jh> [-o out]    print JIR (text format)
  jihoo build <file.jh> [-o out]      compile a freestanding program to a native binary

options:
  -I <dir>    also look for imported modules in <dir> (may be repeated)

environment:
  JIHOO_PATH  directories to look for imported modules in, separated by `:`
  JIHOO_LLC   path to the LLVM backend   (default: jihoo-llc in PATH)
  JIHOO_LD    path to the linker         (default: ld.lld in PATH)

`import a.b` loads `a/b.jh` from the importing file's directory, the -I
directories, JIHOO_PATH, and finally the standard library (`lib/`).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
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
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut input = None;
    let mut output = None;
    let mut include = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => output = Some(PathBuf::from(it.next().ok_or("`-o` needs a path")?)),
            "-I" => include.push(PathBuf::from(it.next().ok_or("`-I` needs a directory")?)),
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`\n\n{USAGE}")),
            s if input.is_none() => input = Some(PathBuf::from(s)),
            s => return Err(format!("unexpected argument `{s}`")),
        }
    }
    let input = input.ok_or_else(|| format!("missing input file\n\n{USAGE}"))?;
    Ok(Opts { input, output, include })
}

fn dispatch(args: &[String]) -> Result<ExitCode, String> {
    let Some((cmd, rest)) = args.split_first() else {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };
    match cmd.as_str() {
        "run" => {
            let opts = parse_opts(rest)?;
            let m = compile(&opts)?;
            if m.profile == Profile::Freestanding {
                return Err(format!(
                    "{}: freestanding programs do not run on the VM; compile them with `jihoo build`",
                    opts.input.display()
                ));
            }
            let code = jihoo_vm::run(&m, &mut std::io::stdout()).map_err(|e| e.to_string())?;
            Ok(ExitCode::from(code as u8))
        }
        "emit-ir" => {
            let opts = parse_opts(rest)?;
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

fn compile(opts: &Opts) -> Result<Module, String> {
    let path = &opts.input;
    let loaded = jihoo_syntax::loader::load(path, &search_path(&opts.include))
        .map_err(|(e, files)| render(&files, &e))?;
    let m = jihoo_sema::analyze_modules(&loaded.modules).map_err(|errs| {
        errs.iter().map(|e| render(&loaded.files, e)).collect::<Vec<_>>().join("\nerror: ")
    })?;
    jihoo_ir::verify(&m).map_err(|e| format!("{}: invalid IR: {e}", path.display()))?;
    Ok(m)
}

/// source -> .jir -> (jihoo-llc) -> .o -> (ld.lld) -> static binary
fn build(opts: &Opts) -> Result<(), String> {
    let m = compile(opts)?;
    if m.profile != Profile::Freestanding {
        return Err(format!(
            "{}: `jihoo build` needs a freestanding program (add `#![freestanding]`); \
             hosted programs run with `jihoo run`",
            opts.input.display()
        ));
    }

    let out = opts.output.clone().unwrap_or_else(|| opts.input.with_extension(""));
    let jir = out.with_extension("jir");
    let obj = out.with_extension("o");
    write(&jir, &m.to_string())?;

    let llc = std::env::var("JIHOO_LLC").unwrap_or_else(|_| "jihoo-llc".into());
    run_tool(Command::new(&llc).arg(&jir).arg("-o").arg(&obj))?;

    let ld = std::env::var("JIHOO_LD").unwrap_or_else(|_| "ld.lld".into());
    run_tool(
        Command::new(&ld)
            .args(["-static", "--gc-sections", "-e", "_start", "-o"])
            .arg(&out)
            .arg(&obj),
    )?;

    let _ = std::fs::remove_file(&obj);
    let _ = std::fs::remove_file(&jir);
    Ok(())
}

fn run_tool(cmd: &mut Command) -> Result<(), String> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    let status = cmd.status().map_err(|e| format!("cannot run `{name}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{name}` failed ({status})"))
    }
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}
