//! Runs every program in `tests/diff/` on the VM and natively, and compares
//! what they print with `out` and their exit status. See `tests/diff/README.md`.

mod common;

use std::process::Command;

use common::{repo_root, Profile, JIHOO};

fn exit_code(cmd: &mut Command) -> i32 {
    let out = cmd.output().expect("failed to spawn");
    assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    out.status.code().expect("killed by a signal")
}

#[test]
fn vm_and_native_agree() {
    let compiled = common::compiled_enabled();
    if !compiled {
        eprintln!("JIHOO_LLC is not set: checking the VM only");
    }
    let work = common::work_dir("diff");

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
        let vm = common::run(&body, Profile::Hosted, &work, &name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(vm.status.is_some());
        if compiled {
            // Both compiled profiles: freestanding (`_start`, no libc) and
            // native (C's `main`, linked with libc).
            for profile in [Profile::Freestanding, Profile::Native] {
                let nat = common::run(&body, profile, &work, &name).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(vm, nat, "{name}: the VM and the {} build differ", profile.name());
            }
        }
        eprintln!("{name}: {:?}, {} lines of output", vm.status, vm.stdout.lines().count());
    }

    std::fs::remove_dir_all(&work).unwrap();
}

/// Checks that `body`'s `entry` stops with a runtime error on the VM, whose
/// message contains `vm_error`, and traps when compiled.
fn fails_on_both_backends(name: &str, body: &str, vm_error: &str) {
    let work = std::env::temp_dir().join(format!("jihoo-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();

    let hosted = work.join(format!("{name}.hosted.jh"));
    std::fs::write(&hosted, format!("{body}fn main() -> i64 {{ return entry() }}\n")).unwrap();
    let out = Command::new(JIHOO).arg("run").arg(&hosted).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(vm_error), "{}", String::from_utf8_lossy(&out.stderr));

    if std::env::var_os("JIHOO_LLC").is_some() {
        let src = work.join(format!("{name}.native.jh"));
        let bin = work.join(name);
        std::fs::write(&src, format!("#![freestanding]\n{body}fn _start() -> i64 {{ return entry() }}\n")).unwrap();
        assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
        let status = Command::new(&bin).status().unwrap();
        // Natively, a failed check traps (SIGILL) instead of exiting.
        assert_eq!(status.code(), None, "expected a trap, got {status}");
    }
    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn out_of_bounds_fails_on_both_backends() {
    let body = "fn get(xs: [i64; 4], i: i64) -> i64 { return xs[i] }\nfn entry() -> i64 { return get([1, 2, 3, 4], 4) }\n";
    fails_on_both_backends("oob", body, "index 4 out of bounds for length 4");
}

#[test]
fn division_by_zero_fails_on_both_backends() {
    let body = "fn div(a: i64, b: i64) -> i64 { return a / b }\nfn entry() -> i64 { return div(1, 0) }\n";
    fails_on_both_backends("div0", body, "division by zero");
    let body = "fn rem(a: u32, b: u32) -> u32 { return a % b }\nfn entry() -> i64 { return rem(1, 0) as i64 }\n";
    fails_on_both_backends("rem0", body, "division by zero");
}

/// The default output of a source without an extension is the source itself,
/// which `build` must refuse rather than overwrite; so must `-o` naming it.
#[test]
fn build_never_overwrites_its_source() {
    let work = std::env::temp_dir().join(format!("jihoo-overwrite-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let src = work.join("prog");
    let text = "#![freestanding]\nfn _start() -> i64 { return 0 }\n";
    std::fs::write(&src, text).unwrap();
    for args in [vec![], vec!["-o".into(), work.join("./prog").into_os_string()]] {
        let out = Command::new(JIHOO).arg("build").arg(&src).args(&args).output().unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("overwrite its own source"));
        assert_eq!(std::fs::read_to_string(&src).unwrap(), text);
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
/// and `mmap`, so it only runs natively; check its output. They make syscalls
/// through `lib/sys.<target>.jh`.
#[test]
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

/// `lib/coro.jh` switches stacks with the C library's `swapcontext`.
#[test]
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

/// `--target` builds for the other machine: the binary is for that machine,
/// and its syscalls come from that machine's `lib/sys` file.
#[test]
fn builds_for_the_other_target() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let (other, machine, write) = if cfg!(target_arch = "aarch64") {
        ("x86_64", 62, "const 1\n")
    } else {
        ("aarch64", 183, "const 64\n")
    };
    let src = repo_root().join("examples/arena.jh");
    let ir = Command::new(JIHOO).args(["emit-ir", "--target", other]).arg(&src).output().unwrap();
    let ir = String::from_utf8_lossy(&ir.stdout);
    assert!(ir.contains(&format!("\ntarget {other}\n")), "{ir}");
    // `io.write`, whose syscall number is the other machine's.
    let write_fn = &ir[ir.find("fn @io.write(").unwrap()..];
    assert!(write_fn[..write_fn.find("\n}").unwrap()].contains(write), "{write_fn}");

    let bin = std::env::temp_dir().join(format!("jihoo-cross-{}", std::process::id()));
    assert_eq!(exit_code(Command::new(JIHOO).args(["build", "--target", other]).arg(&src).arg("-o").arg(&bin)), 0);
    let elf = std::fs::read(&bin).unwrap();
    std::fs::remove_file(&bin).unwrap();
    assert_eq!(u16::from_le_bytes([elf[18], elf[19]]), machine, "ELF e_machine");

    let out = Command::new(JIHOO).args(["run", "--target", other]).arg(repo_root().join("examples/hello.jh")).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("the VM runs on this machine"));
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

/// Floats cross into C: `float` and `double` parameters and results, mixed
/// with integers, a callback taking a `double`, `f32` through `printf`'s
/// variable arguments (promoted to `double`), and libm.
#[test]
fn native_floats_cross_into_c() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("jihoo-ffi-float-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("f.c"),
        "float scale(float x, int n, double k) { return (float)(x * k) + n; }\n\
         double integrate(double (*f)(double), double a, double b, int n) {\n\
           double h = (b - a) / n, s = (f(a) + f(b)) / 2;\n\
           for (int i = 1; i < n; i++) s += f(a + i * h);\n\
           return s * h;\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.jh"),
        "#![native]\n\
         import libc\n\
         extern fn scale(x: f32, n: i32, k: f64) -> f32\n\
         extern fn integrate(f: fn(f64) -> f64, a: f64, b: f64, n: i32) -> f64\n\
         fn square(x: f64) -> f64 { return x * x }\n\
         fn main() -> i64 {\n\
           let half: f32 = 0.5\n\
           libc.printf(\"%.2f %.1f %g\\n\", scale(1.5, 2, 3.0), half, libc.sqrt(2.0))\n\
           libc.printf(\"%.4f\\n\", integrate(square, 0.0, 3.0, 3000))\n\
           return libc.floor(libc.atof(\"41.9\")) as i64\n\
         }\n",
    )
    .unwrap();
    let bin = dir.join("main");
    let status =
        Command::new(JIHOO).arg("build").arg(dir.join("main.jh")).arg(dir.join("f.c")).arg("-o").arg(&bin).status().unwrap();
    assert!(status.success());
    let out = Command::new(&bin).output().unwrap();
    // The trapezoid rule is off by (b - a) * h^2 * f'' / 12 = 5e-7 here.
    assert_eq!(String::from_utf8_lossy(&out.stdout), "6.50 0.5 1.41421\n9.0000\n");
    assert_eq!(out.status.code(), Some(41));
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

/// `print` in native code writes what the VM writes: integers of every type,
/// bools, text, and floats as the shortest text that reads back exactly (every
/// power of two, where the nearest decimal of a length may not read back but
/// the next one does, and random bit patterns of both float types).
#[test]
fn native_print_matches_the_vm() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let body = "fn main() -> i64 {\n\
        print(\"text\")\n  print(true)\n  print(1 == 2)\n\
        print(-128 as i8)\n  print(255 as u8)\n  print(-32768 as i16)\n  print(65535 as u16)\n\
        print(-2147483648 as i32)\n  print(4294967295 as u32)\n  print(-9223372036854775807 - 1)\n  let big: u64 = 18446744073709551615\n  print(big)\n\
        let fixed = [0.0, -0.0, 1.0, -1.0, 0.1, 0.3, 1.0 / 3.0, 1e15, 1e16, 1e17, 123456789.125, 1e-4, 9.999e-5, 1e-5,\n\
                     5e-324, 1.7976931348623157e308, 2.0 / 0.0, -2.0 / 0.0, 0.0 / 0.0, 100.0, 1234.5, 0.000123]\n\
        let i = 0\n  while i < len(fixed) {\n    print(fixed[i])\n    print(fixed[i] as f32)\n    i += 1\n  }\n\
        let p = 1.0\n  i = 0\n  while i < 1100 {\n    print(p)\n    print(1.0 / p)\n    p *= 2.0\n    i += 1\n  }\n\
        let q: f32 = 1.0\n  i = 0\n  while i < 150 {\n    print(q)\n    print(1.0 / q)\n    q *= 2.0\n    i += 1\n  }\n\
        let s: u64 = 99\n  i = 0\n  while i < 4000 {\n    s = s * 6364136223846793005 + 1442695040888963407\n\
            print(from_bits(s))\n    print(from_bits((s >> 32) as u32))\n    i += 1\n  }\n\
        return 0\n}\n";
    let work = common::work_dir("print");
    let hosted = work.join("p.jh");
    std::fs::write(&hosted, body).unwrap();
    let vm = Command::new(JIHOO).arg("run").arg(&hosted).output().unwrap();
    assert!(vm.status.success(), "{}", String::from_utf8_lossy(&vm.stderr));

    let native = work.join("pn.jh");
    std::fs::write(&native, format!("#![native]\n{body}")).unwrap();
    let bin = work.join("pn");
    assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&native).arg("-o").arg(&bin)), 0);
    let nat = Command::new(&bin).output().unwrap();
    let (vm, nat) = (String::from_utf8_lossy(&vm.stdout), String::from_utf8_lossy(&nat.stdout));
    assert!(vm.lines().count() > 10000);
    for (n, (a, b)) in vm.lines().zip(nat.lines()).enumerate() {
        assert_eq!(a, b, "line {}", n + 1);
    }
    assert_eq!(vm, nat);
    std::fs::remove_dir_all(&work).unwrap();
}

/// Structs cross into C by value both ways, as each target's C ABI says:
/// `tests/ffi/structs.jh` calls the C functions of `tests/ffi/structs.c`, and
/// they call its exported functions back. With `JIHOO_CROSS_CC` (a C compiler
/// for the other target) and `JIHOO_CROSS_RUN` (a way to run its programs,
/// such as `qemu-aarch64`), it runs for the other target too.
#[test]
fn native_structs_cross_into_c() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let work = common::work_dir("ffi-structs");
    let ffi = repo_root().join("tests/ffi");
    let bin = work.join("structs");
    let status =
        Command::new(JIHOO).arg("build").arg(ffi.join("structs.jh")).arg(ffi.join("structs.c")).arg("-o").arg(&bin).status().unwrap();
    assert!(status.success());
    let out = Command::new(&bin).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "the check with this number failed");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");

    if let (Some(cc), Some(run)) = (std::env::var_os("JIHOO_CROSS_CC"), std::env::var_os("JIHOO_CROSS_RUN")) {
        let other = if cfg!(target_arch = "aarch64") { "x86_64" } else { "aarch64" };
        let cross = work.join("structs-cross");
        let status = Command::new(JIHOO)
            .args(["build", "--target", other])
            .arg(ffi.join("structs.jh"))
            .arg(ffi.join("structs.c"))
            .arg("-o")
            .arg(&cross)
            .env("JIHOO_CC", cc)
            .status()
            .unwrap();
        assert!(status.success());
        let out = Command::new(run).arg(&cross).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "on {other}, the check with this number failed");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ok\n");
    }
    std::fs::remove_dir_all(&work).unwrap();
}

/// The hosted standard library together: `read_line` from standard input,
/// lists, and `lib/strings.jh` and `lib/lists.jh`.
#[test]
fn words_example_reads_standard_input() {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(JIHOO)
        .arg("run")
        .arg(repo_root().join("examples/words.jh"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"The cat and the dog\n| a table |\nthe END and\r\nend").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let expected = "11 words\n     3  the\n     2  and\n     2  end\n     1  a\n     1  cat\n";
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
}
