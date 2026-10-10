//! JIR files as input: every example and differential test, written with
//! `emit-ir`, reads back as the same module (printing it again gives the same
//! text), and a hosted one runs from its JIR as it runs from source.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{repo_root, Profile, JIHOO};

/// The JIR `emit-ir` writes for `src`, saved as `jir`.
fn emit(src: &Path, jir: &Path) -> String {
    let out = Command::new(JIHOO).arg("emit-ir").arg(src).arg("-o").arg(jir).output().unwrap();
    assert!(out.status.success(), "{}: {}", src.display(), String::from_utf8_lossy(&out.stderr));
    std::fs::read_to_string(jir).unwrap()
}

/// What `file` prints and how it exits. (Not what it writes to stderr: a
/// source file may print there while compiling, which its JIR has done.)
fn run(file: &Path) -> (String, Option<i32>) {
    let out = Command::new(JIHOO).arg("run").arg(file).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code())
}

#[test]
fn programs_read_back_from_jir() {
    let work = common::work_dir("jir");
    let mut sources: Vec<PathBuf> = std::fs::read_dir(repo_root().join("examples"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jh"))
        .collect();
    sources.sort();
    // The differential tests, as hosted programs.
    let mut diff: Vec<PathBuf> = std::fs::read_dir(repo_root().join("tests/diff"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jh"))
        .collect();
    diff.sort();
    for d in diff {
        let wrapped = work.join(d.file_name().unwrap());
        std::fs::write(&wrapped, common::wrap(&std::fs::read_to_string(&d).unwrap(), Profile::Hosted)).unwrap();
        sources.push(wrapped);
    }

    for src in &sources {
        let name = src.file_stem().unwrap().to_string_lossy();
        let jir = work.join(format!("{name}.jir"));
        let text = emit(src, &jir);
        let again = emit(&jir, &work.join(format!("{name}.again.jir")));
        assert_eq!(text, again, "{}: JIR read back prints differently", src.display());
        if text.contains("\nprofile hosted\n") {
            assert_eq!(run(src), run(&jir), "{}: runs differently from JIR", src.display());
        }
    }
    std::fs::remove_dir_all(&work).unwrap();
}

/// A JIR file builds like its source: here a freestanding program that uses
/// the float builtins.
#[test]
fn jir_builds_natively() {
    if !common::compiled_enabled() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let work = common::work_dir("jir-build");
    let src = work.join("p.jh");
    std::fs::write(&src, "#![freestanding]\nfn _start() -> i64 { return (floor(-2.5) + sqrt(81.0)) as i64 }\n").unwrap();
    let jir = work.join("p.jir");
    emit(&src, &jir);
    let bin = work.join("p");
    let status = Command::new(JIHOO).arg("build").arg(&jir).arg("-o").arg(&bin).status().unwrap();
    assert!(status.success());
    assert_eq!(Command::new(&bin).status().unwrap().code(), Some(6));

    // A module is for the target it says.
    let other = if cfg!(target_arch = "aarch64") { "x86_64" } else { "aarch64" };
    let out = Command::new(JIHOO).args(["build", "--target", other]).arg(&jir).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("JIR is made for one target"));
    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn broken_jir_is_refused() {
    let work = common::work_dir("jir-broken");
    let jir = work.join("bad.jir");
    std::fs::write(&jir, "jir 1\nprofile hosted\n").unwrap();
    let out = Command::new(JIHOO).arg("run").arg(&jir).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("bad.jir: line 1: this is JIR version 1"), "{stderr}");
    // Read, but not valid: the verifier says why.
    let head = format!("jir {}\nprofile hosted\n", jihoo_ir_version());
    std::fs::write(&jir, format!("{head}fn @main() -> unit {{\n  regs bool\nbb0:\n  %0 = const 2\n  ret %0\n}}\n")).unwrap();
    let out = Command::new(JIHOO).arg("run").arg(&jir).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("invalid IR") && stderr.contains("`const 2` is not a valid bool"), "{stderr}");
    std::fs::remove_dir_all(&work).unwrap();
}

/// The JIR version, from the header `emit-ir` writes.
fn jihoo_ir_version() -> String {
    let out = Command::new(JIHOO).arg("emit-ir").arg(repo_root().join("examples/hello.jh")).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines().find_map(|l| l.strip_prefix("jir ")).unwrap().to_string()
}
