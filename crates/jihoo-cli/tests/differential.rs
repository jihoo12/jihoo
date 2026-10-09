//! Runs every program in `tests/diff/` on the VM and natively, and compares the
//! exit status. See `tests/diff/README.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

const JIHOO: &str = env!("CARGO_BIN_EXE_jihoo");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn exit_code(cmd: &mut Command) -> i32 {
    let out = cmd.output().expect("failed to spawn");
    assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    out.status.code().expect("killed by a signal")
}

#[test]
fn vm_and_native_agree() {
    let native = std::env::var_os("JIHOO_LLC").is_some();
    if !native {
        eprintln!("JIHOO_LLC is not set: checking the VM only");
    }

    let work = std::env::temp_dir().join(format!("jihoo-diff-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();

    let mut files: Vec<_> = std::fs::read_dir(repo_root().join("tests/diff"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jh"))
        .collect();
    files.sort();
    assert!(!files.is_empty());

    for file in files {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        let body = std::fs::read_to_string(&file).unwrap();

        let hosted = work.join(format!("{name}.hosted.jh"));
        std::fs::write(&hosted, format!("{body}\nfn main() -> i64 {{ return entry() }}\n")).unwrap();
        let vm = exit_code(Command::new(JIHOO).arg("run").arg(&hosted));

        if native {
            let src = work.join(format!("{name}.native.jh"));
            let bin = work.join(&name);
            std::fs::write(
                &src,
                format!("#![freestanding]\n{body}\nfn _start() -> i64 {{ return entry() }}\n"),
            )
            .unwrap();
            assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
            let nat = exit_code(&mut Command::new(&bin));
            assert_eq!(vm, nat, "{name}: VM exited with {vm}, native with {nat}");
        }
        eprintln!("{name}: {vm}");
    }

    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn out_of_bounds_fails_on_both_backends() {
    let work = std::env::temp_dir().join(format!("jihoo-oob-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let body = "fn get(xs: [i64; 4], i: i64) -> i64 { return xs[i] }\nfn entry() -> i64 { return get([1, 2, 3, 4], 4) }\n";

    let hosted = work.join("oob.hosted.jh");
    std::fs::write(&hosted, format!("{body}fn main() -> i64 {{ return entry() }}\n")).unwrap();
    let out = Command::new(JIHOO).arg("run").arg(&hosted).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("index 4 out of bounds for length 4"));

    if std::env::var_os("JIHOO_LLC").is_some() {
        let src = work.join("oob.native.jh");
        let bin = work.join("oob");
        std::fs::write(&src, format!("#![freestanding]\n{body}fn _start() -> i64 {{ return entry() }}\n")).unwrap();
        assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
        let status = Command::new(&bin).status().unwrap();
        // Natively, a failed bounds check traps (SIGILL) instead of exiting.
        assert_eq!(status.code(), None, "expected a trap, got {status}");
    }
    std::fs::remove_dir_all(&work).unwrap();
}

/// Inline asm only exists natively, so it is checked against known results
/// instead of the VM.
#[test]
#[cfg(target_arch = "x86_64")]
fn inline_asm_example_runs_natively() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let bin = std::env::temp_dir().join(format!("jihoo-asm-{}", std::process::id()));
    let src = repo_root().join("examples/asm.jh");
    assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
    let out = Command::new(&bin).output().unwrap();
    std::fs::remove_file(&bin).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "inline asm says hi\n");
    assert_eq!(out.status.code(), Some(4), "every check in examples/asm.jh should pass");
}

/// The allocator and `Vec(T)` example uses pointers and `mmap`, so it only runs
/// natively; check its output.
#[test]
#[cfg(target_arch = "x86_64")]
fn alloc_example_runs_natively() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let bin = std::env::temp_dir().join(format!("jihoo-alloc-{}", std::process::id()));
    let src = repo_root().join("examples/alloc.jh");
    assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
    let out = Command::new(&bin).output().unwrap();
    std::fs::remove_file(&bin).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1000\n1024\n332833500\n60\n16416\n");
    assert_eq!(out.status.code(), Some(2));
}
