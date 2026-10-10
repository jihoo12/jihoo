//! Shared by the differential tests and the fuzzer: a core-only program (no
//! profile attribute, no `print`, no `syscall`) that defines
//! `fn entry() -> i64` and may call `out(x: i64)`, wrapped and run as a hosted,
//! a native and a freestanding program. `out` prints `x` and a newline in each:
//! with `print` on the VM, `printf` natively and `io.print_int` freestanding.
//! So the programs must not define `out`, `main`, `_start`, `io` or `libc`.

#![allow(dead_code)] // each test file uses part of it

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const JIHOO: &str = env!("CARGO_BIN_EXE_jihoo");

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Whether the compiled profiles can be checked: they need `jihoo-llc`.
pub fn compiled_enabled() -> bool {
    std::env::var_os("JIHOO_LLC").is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Hosted,
    Native,
    Freestanding,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Hosted => "hosted",
            Profile::Native => "native",
            Profile::Freestanding => "freestanding",
        }
    }
}

/// `body` as a whole program for `profile`.
pub fn wrap(body: &str, profile: Profile) -> String {
    match profile {
        Profile::Hosted => {
            format!("{body}\nfn out(x: i64) {{ print(x) }}\nfn main() -> i64 {{ return entry() }}\n")
        }
        Profile::Native => format!(
            "#![native]\nimport libc\n{body}\nfn out(x: i64) {{ libc.printf(\"%ld\\n\", x) }}\n\
             fn main() -> i64 {{ return entry() }}\n"
        ),
        Profile::Freestanding => format!(
            "#![freestanding]\nimport io\n{body}\nfn out(x: i64) {{ io.print_int(x) }}\n\
             fn _start() -> i64 {{ return entry() }}\n"
        ),
    }
}

/// What a program did: what it printed and how it exited.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub stdout: String,
    /// The exit status, or `None` if a signal killed it (a trap, natively).
    pub status: Option<i32>,
}

/// Runs `body` as a `profile` program in `work`, under the name `name`. An
/// error is a program that does not compile, or a VM error message.
pub fn run(body: &str, profile: Profile, work: &Path, name: &str) -> Result<Outcome, String> {
    let src = work.join(format!("{name}.{}.jh", profile.name()));
    std::fs::write(&src, wrap(body, profile)).unwrap();
    let out = if profile == Profile::Hosted {
        Command::new(JIHOO).arg("run").arg(&src).output().unwrap()
    } else {
        let bin = work.join(format!("{name}.{}", profile.name()));
        let build = Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin).output().unwrap();
        if !build.status.success() {
            return Err(format!("`jihoo build` failed: {}", String::from_utf8_lossy(&build.stderr)));
        }
        let out = Command::new(&bin).output().unwrap();
        std::fs::remove_file(&bin).unwrap();
        out
    };
    std::fs::remove_file(&src).unwrap();
    outcome(out)
}

fn outcome(out: Output) -> Result<Outcome, String> {
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.is_empty() {
        return Err(stderr.into_owned());
    }
    Ok(Outcome { stdout: String::from_utf8_lossy(&out.stdout).into_owned(), status: out.status.code() })
}

/// A fresh directory for one test's files.
pub fn work_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jihoo-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
