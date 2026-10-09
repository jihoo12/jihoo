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
