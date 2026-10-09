//! `jihoo` command line driver.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use jihoo_ir::{Module, Profile};

const USAGE: &str = "\
usage:
  jihoo run <file.jh>                 run a hosted program on the VM
  jihoo emit-ir <file.jh> [-o out]    print JIR (text format)
  jihoo build <file.jh> [-o out]      compile a freestanding program to a native binary

environment:
  JIHOO_LLC   path to the LLVM backend   (default: jihoo-llc in PATH)
  JIHOO_LD    path to the linker         (default: ld.lld in PATH)";

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
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut input = None;
    let mut output = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => output = Some(PathBuf::from(it.next().ok_or("`-o` needs a path")?)),
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`\n\n{USAGE}")),
            s if input.is_none() => input = Some(PathBuf::from(s)),
            s => return Err(format!("unexpected argument `{s}`")),
        }
    }
    let input = input.ok_or_else(|| format!("missing input file\n\n{USAGE}"))?;
    Ok(Opts { input, output })
}

fn dispatch(args: &[String]) -> Result<ExitCode, String> {
    let Some((cmd, rest)) = args.split_first() else {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };
    match cmd.as_str() {
        "run" => {
            let opts = parse_opts(rest)?;
            let m = compile(&opts.input)?;
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
            let m = compile(&opts.input)?;
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

fn compile(path: &Path) -> Result<Module, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let at = |e: &jihoo_syntax::Error| format!("{}:{e}", path.display());
    let ast = jihoo_syntax::parse(&src).map_err(|e| at(&e))?;
    let m = jihoo_sema::analyze(&ast)
        .map_err(|errs| errs.iter().map(at).collect::<Vec<_>>().join("\nerror: "))?;
    jihoo_ir::verify(&m).map_err(|e| format!("{}: invalid IR: {e}", path.display()))?;
    Ok(m)
}

/// source -> .jir -> (jihoo-llc) -> .o -> (ld.lld) -> static binary
fn build(opts: &Opts) -> Result<(), String> {
    let m = compile(&opts.input)?;
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
