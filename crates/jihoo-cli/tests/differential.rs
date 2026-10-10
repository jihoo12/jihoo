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
            // Both compiled profiles: freestanding (`_start`, no libc) and
            // native (C's `main`, linked with libc).
            for (profile, entry) in [("freestanding", "_start"), ("native", "main")] {
                let src = work.join(format!("{name}.{profile}.jh"));
                let bin = work.join(format!("{name}.{profile}"));
                std::fs::write(
                    &src,
                    format!("#![{profile}]\n{body}\nfn {entry}() -> i64 {{ return entry() }}\n"),
                )
                .unwrap();
                assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
                let nat = exit_code(&mut Command::new(&bin));
                assert_eq!(vm, nat, "{name}: VM exited with {vm}, {profile} build with {nat}");
            }
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

/// The arena example imports `lib/alloc.jh` and `lib/io.jh`, which use pointers
/// and `mmap`, so it only runs natively; check its output.
#[test]
#[cfg(target_arch = "x86_64")]
fn arena_example_runs_natively() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let bin = std::env::temp_dir().join(format!("jihoo-arena-{}", std::process::id()));
    let src = repo_root().join("examples/arena.jh");
    assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
    let out = Command::new(&bin).output().unwrap();
    std::fs::remove_file(&bin).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1000\n1024\n332833500\n60\n16416\n-2\ndone\n");
    assert_eq!(out.status.code(), Some(1));
}

/// `lib/coro.jh` switches stacks with x86_64 asm, and `lib/io.jh` uses x86_64
/// syscall numbers.
#[test]
#[cfg(target_arch = "x86_64")]
fn coroutine_example_runs_natively() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let bin = std::env::temp_dir().join(format!("jihoo-coro-{}", std::process::id()));
    let src = repo_root().join("examples/coroutines.jh");
    assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
    let out = Command::new(&bin).output().unwrap();
    std::fs::remove_file(&bin).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n2\n8\n34\n");
    assert_eq!(out.status.code(), Some(4));
}

/// Errors in an imported module name that module's file.
#[test]
fn errors_in_imported_modules_name_their_file() {
    let dir = std::env::temp_dir().join(format!("jihoo-mods-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("main.jh"), "import lib.util\nfn main() { print(util.twice(2)) }\n").unwrap();
    std::fs::write(dir.join("lib/util.jh"), "pub fn twice(x: i64) -> i64 {\n  return x * true\n}\n").unwrap();
    let out = Command::new(JIHOO).arg("run").arg(dir.join("main.jh")).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let expected = format!("{}:2:12: cannot apply `*` to i64 and bool", dir.join("lib/util.jh").display());
    assert!(stderr.contains(&expected), "{stderr}");

    std::fs::write(dir.join("lib/util.jh"), "pub fn twice(x: i64) -> i64 { return x * 2 }\n").unwrap();
    let out = Command::new(JIHOO).arg("run").arg(dir.join("main.jh")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "4\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A native program links with libc: printf, malloc, qsort calling back into
/// jihoo, and C's argc/argv.
#[test]
fn native_example_runs() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let bin = std::env::temp_dir().join(format!("jihoo-native-{}", std::process::id()));
    let src = repo_root().join("examples/native.jh");
    assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
    let out = Command::new(&bin).args(["one", "two"]).output().unwrap();
    std::fs::remove_file(&bin).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello from native jihoo, with 2 argument(s)\n  argv[1] = one\n  argv[2] = two\n\
         -49 -29 -20 -2 20 37 \np = (3, 4), len(\"jihoo\") = 5\n"
    );
    assert_eq!(out.status.code(), Some(49));
}

/// `jihoo build` compiles and links C files given with the program, and the
/// two call each other: narrow integers and bools cross with C's extensions.
#[test]
fn native_programs_link_c_files() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("jihoo-ffi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ffi.c"),
        "#include <stdbool.h>\n#include <stdint.h>\n\
         int64_t widen(int8_t a, uint8_t b, int16_t c) { return (int64_t)a * 1000000 + b * 1000 + c; }\n\
         bool is_odd(int32_t n) { return n & 1; }\n\
         int64_t apply(int64_t (*f)(int64_t), int64_t x) { return f(f(x)); }\n\
         void bump(int64_t *p) { *p += 1; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.jh"),
        "#![native]\n\
         extern fn widen(a: i8, b: u8, c: i16) -> i64\n\
         extern fn is_odd(n: i32) -> bool\n\
         extern fn apply(f: fn(i64) -> i64, x: i64) -> i64\n\
         extern fn bump(p: *i64)\n\
         fn triple(x: i64) -> i64 { return x * 3 }\n\
         fn main() -> i64 {\n\
           if widen(-2, 200, -300) != -1800300 { return 1 }\n\
           if !is_odd(7) || is_odd(8) { return 2 }\n\
           if apply(triple, 5) != 45 { return 3 }\n\
           let n = 41\n\
           bump(&n)\n\
           return n\n\
         }\n",
    )
    .unwrap();
    let bin = dir.join("main");
    let status = Command::new(JIHOO)
        .arg("build")
        .arg(dir.join("main.jh"))
        .arg(dir.join("ffi.c"))
        .arg("-o")
        .arg(&bin)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(exit_code(&mut Command::new(&bin)), 42);

    // Freestanding programs link nothing else.
    std::fs::write(dir.join("fs.jh"), "#![freestanding]\nfn _start() {}\n").unwrap();
    let out = Command::new(JIHOO).arg("build").arg(dir.join("fs.jh")).arg(dir.join("ffi.c")).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("use `#![native]` to link with C"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn native_programs_do_not_run_on_the_vm() {
    let dir = std::env::temp_dir().join(format!("jihoo-native-vm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("n.jh"), "#![native]\nfn main() {}\n").unwrap();
    let out = Command::new(JIHOO).arg("run").arg(dir.join("n.jh")).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("native programs do not run on the VM"));
    std::fs::remove_dir_all(&dir).unwrap();
}
