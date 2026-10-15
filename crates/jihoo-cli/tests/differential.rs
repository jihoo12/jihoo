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

/// The programs in `tests/diff-lists/` use lists, so they run hosted and native
/// (freestanding programs have no allocator). Natively, the ownership pass
/// decides where lists are copied and freed; with `JIHOO_VALGRIND` set to
/// valgrind's path, each native program also runs under it, and any leak or
/// invalid access fails the test (memory the C library keeps for a thread
/// still finishing when the program ends is not a leak).
#[test]
fn vm_and_native_agree_on_lists() {
    let compiled = common::compiled_enabled();
    let work = common::work_dir("diff-lists");
    let valgrind: Option<Vec<String>> = std::env::var("JIHOO_VALGRIND").ok().map(|v| {
        [v.as_str(), "-q", "--leak-check=full", "--show-leak-kinds=definite,indirect", "--errors-for-leak-kinds=definite,indirect", "--error-exitcode=99"]
            .map(String::from)
            .to_vec()
    });
    let mut files: Vec<_> = std::fs::read_dir(repo_root().join("tests/diff-lists"))
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
        if compiled {
            let nat = common::run_under(&body, Profile::Native, &work, &name, valgrind.as_deref())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(vm, nat, "{name}: the VM and the native build differ");
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
    std::fs::write(&hosted, format!("#![hosted]\n{body}fn main() -> i64 {{ return entry() }}\n")).unwrap();
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
    assert!(String::from_utf8_lossy(&out.stderr).contains("runs programs on this machine"));
}

/// `-O0` ... `-O3` reach the backend, and give the same program.
#[test]
fn optimization_levels() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let src = repo_root().join("tests/diff-lists/basics.jh");
    let body = std::fs::read_to_string(&src).unwrap();
    let work = common::work_dir("opt-levels");
    let path = work.join("basics.jh");
    std::fs::write(&path, common::wrap(&body, Profile::Native)).unwrap();
    let mut outputs = Vec::new();
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let out = Command::new(JIHOO).arg("run").arg(level).arg(&path).output().unwrap();
        assert!(out.stderr.is_empty(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        outputs.push(out.stdout);
    }
    assert!(outputs.iter().all(|o| *o == outputs[0] && !o.is_empty()));
    let out = Command::new(JIHOO).args(["emit-ir", "-O0"]).arg(&path).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("only `jihoo build` and `jihoo run` use it"));
    std::fs::remove_dir_all(&work).unwrap();
}

/// `jihoo build --cost` lists the copies a native program makes, where and
/// why, and only those of functions it runs.
#[test]
fn cost_lists_copies() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let work = common::work_dir("cost");
    let path = work.join("cost.jh");
    let src = "fn grow(xs: [i64]) -> [i64] {\n    push(xs, 1)\n    return xs\n}\n\n\
               fn main() {\n    let xs: [i64] = [1]\n    let ys = grow(xs)\n    let r = ref [1]\n    let s = r\n    \
               print(len(xs) + len(ys) + len(*s))\n    let zs = grow(ys)\n    print(len(zs))\n}\n";
    std::fs::write(&path, src).unwrap();
    let out = Command::new(JIHOO).arg("build").arg("--cost").arg(&path).arg("-o").arg(work.join("cost")).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).replace(&format!("{}/", work.display()), "");
    // `xs` is still printed after `grow(xs)`, so that call copies it; `ys`
    // is handed over at its last use, to `grow.own`, which copies nothing.
    // The copy of a `ref` counts one more holder and is not listed.
    let want = "cost.jh:1:1: copies [i64] in @grow: parameter 1 is copied on entry, since the function changes or keeps it; \
                a caller that hands it over at its last use calls a version that does not copy it\n1 copy\n";
    assert_eq!(text, want, "{}", String::from_utf8_lossy(&out.stderr));
    let out = Command::new(JIHOO).args(["emit-ir", "--cost"]).arg(&path).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("`--cost` is for `jihoo build`"));
    std::fs::remove_dir_all(&work).unwrap();
}

/// `nocopy` holds for the library's iterator adapters over a list the
/// function was handed, and names the library function that copies when it
/// does not hold; positions the check needed do not end up in the JIR.
#[test]
fn nocopy_with_the_library() {
    let work = common::work_dir("nocopy");
    let path = work.join("nocopy.jh");
    let src = "import iters\nimport lists\n\n\
               fn odd_squares(xs: [i64]) -> i64 nocopy {\n    \
               let sq = iters.map(iters.of(xs), fn(x: i64) -> i64 { return x * x })\n    \
               return iters.sum(iters.filter(sq, fn(x: i64) -> bool { return x % 2 == 1 }))\n}\n\n\
               fn main() {\n    print(odd_squares([1, 2, 3]))\n}\n";
    std::fs::write(&path, src).unwrap();
    let out = Command::new(JIHOO).arg("emit-ir").arg(&path).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("loc "), "positions left in the JIR");

    // Sorting a list handed over copies nothing either; reversing a list of
    // strings copies each one, which the old list still holds.
    let src = src.replace("print(odd_squares([1, 2, 3]))", "print(odd_squares([1, 2, 3]))\n    print(sorted([2, 1]))\n    print(rev([\"a\"]))")
        + "\nfn sorted(xs: [i64]) -> [i64] nocopy {\n    return lists.sort(xs)\n}\n\
           \nfn rev(xs: [str]) -> [str] nocopy {\n    return lists.reverse(xs)\n}\n";
    std::fs::write(&path, src).unwrap();
    let out = Command::new(JIHOO).arg("emit-ir").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).replace(&format!("{}/", work.display()), "");
    assert!(err.contains("nocopy.jh:20:26: `rev` is `nocopy`, but this call of `reverse` copies str (in `reverse`"), "{err}");
    assert!(!err.contains("sorted"), "{err}");
    std::fs::remove_dir_all(&work).unwrap();
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
    assert!(String::from_utf8_lossy(&out.stderr).contains("a native program (without `#![freestanding]`) links with C"));
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

/// A program without a profile attribute is native, and `jihoo run` compiles
/// and runs it: arguments after `--` reach `main`, and the exit status is the
/// program's. A trap keeps what was printed before it, even through a pipe.
#[test]
fn jihoo_run_runs_native_programs() {
    let dir = std::env::temp_dir().join(format!("jihoo-run-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("n.jh");
    std::fs::write(&src, "fn main(argc: i32, argv: **u8) -> i64 {\n let p = &argc\n print(*p)\n return 40 + argc as i64 }\n")
        .unwrap();
    if std::env::var_os("JIHOO_LLC").is_none() {
        // Without the backend, a native program cannot run; the error says why.
        let out = Command::new(JIHOO).arg("run").arg(&src).output().unwrap();
        assert!(String::from_utf8_lossy(&out.stderr).contains("`#![hosted]`"));
        std::fs::remove_dir_all(&dir).unwrap();
        return;
    }
    let out = Command::new(JIHOO).arg("run").arg(&src).args(["--", "a", "b"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n");
    assert_eq!(out.status.code(), Some(43), "{}", String::from_utf8_lossy(&out.stderr));

    std::fs::write(&src, "fn main() { print(1)\n trap() }\n").unwrap();
    let out = Command::new(JIHOO).arg("run").arg(&src).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("was killed by signal"));
    assert!(matches!(out.status.code(), Some(132 | 133)), "{:?}", out.status); // SIGILL, or SIGTRAP on aarch64

    // Hosted programs still run on the VM, which gives them the arguments.
    std::fs::write(&src, "#![hosted]\nfn main() -> i64 { print(args()[1])\n return len(args()) }\n").unwrap();
    let out = Command::new(JIHOO).arg("run").arg(&src).args(["--", "a", "b"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b\n");
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
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

/// `args`, `read_file`, `write_file` and `append_file` (and `lib/files.jh`)
/// give the same results on the VM and natively, failures included: the same
/// error numbers for a missing file, a directory, a path with a NUL byte in
/// it and a file in a missing directory.
#[test]
fn files_and_arguments_agree() {
    let work = common::work_dir("files");
    let program = r#"import files
import strings

fn show(what: str, done: Result(unit, str)) {
    match done {
        Ok(()) => print(what + " worked")
        Err(msg) => print(what + " failed: " + msg)
    }
}

fn main() -> i64 {
    let a = args()
    print(len(a))
    for x in a {
        print("[" + x + "]")
    }
    let dir = a[0]
    let path = dir + "/notes.txt"
    show("write", files.write(path, "one\n"))
    show("append", files.append(path, "two\n"))
    show("append", files.append(path, ""))
    match files.read(path) {
        Ok(text) => print(strings.join(strings.lines(text), "+"))
        Err(msg) => print(msg)
    }
    show("write", files.write(path, ""))
    print(len(read_file(path).0))
    let (text, err) = read_file(dir + "/missing")
    print(len(text) + err)
    print(read_file(dir).1)
    print(write_file(dir, "x"))
    print(write_file(dir + "/no/such/file", "x"))
    print(read_file(from_bytes([97 as u8, 0 as u8, 98 as u8])).1)
    let big = strings.repeat("0123456789", 100000)
    show("write", files.write(path, big))
    print(read_file(path).0 == big)
    return len(a)
}
"#;
    let mut outputs = Vec::new();
    for attr in ["#![hosted]\n", ""] {
        if attr.is_empty() && !common::compiled_enabled() {
            break;
        }
        let dir = work.join(if attr.is_empty() { "native" } else { "hosted" });
        std::fs::create_dir_all(&dir).unwrap();
        let src = work.join(format!("files{}.jh", outputs.len()));
        std::fs::write(&src, format!("{attr}{program}")).unwrap();
        let out = Command::new(JIHOO).arg("run").arg(&src).arg("--").arg(&dir).arg("two words").arg("").output().unwrap();
        assert!(out.stderr.is_empty(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        // The directories differ; nothing else may.
        let stdout = String::from_utf8_lossy(&out.stdout).replace(dir.to_str().unwrap(), "DIR");
        outputs.push((out.status.code(), stdout));
    }
    let expected = "3\n[DIR]\n[two words]\n[]\nwrite worked\nappend worked\nappend worked\none+two\n\
                    write worked\n0\n2\n21\n21\n2\n22\nwrite worked\ntrue\n";
    assert_eq!(outputs[0], (Some(3), expected.to_string()), "on the VM");
    if let Some(native) = outputs.get(1) {
        assert_eq!(native, &outputs[0], "natively");
    }
    std::fs::remove_dir_all(&work).unwrap();
}

/// `examples/json.jh`, a program that puts arguments, files, `parse_float`,
/// recursive enums and `Result` together, does the same on the VM and
/// natively: prints a document or a part of it, writes one with `-o`, and
/// reports errors with the same messages and exit statuses.
#[test]
fn json_example_agrees() {
    let work = common::work_dir("json");
    let doc = work.join("doc.json");
    std::fs::write(
        &doc,
        r#"{"users": [{"name": "Ann \"A\" \u00e9\ud83d\ude00", "age": 31, "score": 1.5e2, "tags": ["a"]},
            {"name": "Bo\nb", "age": -0, "score": 0.1, "tags": [], "extra": {}}], "big": 1e400, "none": null}"#,
    )
    .unwrap();
    std::fs::write(work.join("bad.json"), "{\"a\": [1, 2,, 3]}").unwrap();
    let example = std::fs::read_to_string(repo_root().join("examples/json.jh")).unwrap();
    let hosted = work.join("json_hosted.jh");
    std::fs::write(&hosted, format!("#![hosted]\n{example}")).unwrap();
    let mut programs = vec![hosted];
    if common::compiled_enabled() {
        programs.push(repo_root().join("examples/json.jh"));
    }
    let cases: [&[&str]; 7] = [
        &["doc.json"],
        &["doc.json", ".users[0].name"],
        &["doc.json", ".users[-1]", "-o", "OUT"],
        &["doc.json", ".users[2]"],
        &["doc.json", ".big"],
        &["bad.json"],
        &["missing.json"],
    ];
    let mut results = Vec::new();
    for (k, program) in programs.iter().enumerate() {
        let mut runs = Vec::new();
        for case in cases {
            let out_file = work.join(format!("out{k}.json"));
            // Files are in `work`; paths (`.x`) and `-o` are as written.
            let args: Vec<String> = case
                .iter()
                .map(|a| match *a {
                    "OUT" => out_file.display().to_string(),
                    a if a.starts_with('.') || a.starts_with('-') => a.to_string(),
                    a => work.join(a).display().to_string(),
                })
                .collect();
            let out = Command::new(JIHOO).arg("run").arg(program).arg("--").args(&args).output().unwrap();
            let written = std::fs::read_to_string(&out_file).unwrap_or_default();
            let text = |b: &[u8]| String::from_utf8_lossy(b).replace(&work.display().to_string(), "WORK");
            runs.push((out.status.code(), text(&out.stdout), text(&out.stderr), written));
        }
        results.push(runs);
    }
    let hosted = &results[0];
    assert_eq!(hosted[0].0, Some(0));
    assert!(hosted[0].1.contains("\"name\": \"Ann \\\"A\\\" \u{e9}\u{1f600}\""), "{}", hosted[0].1);
    assert_eq!(hosted[1].1, "\"Ann \\\"A\\\" \u{e9}\u{1f600}\"\n");
    assert_eq!(hosted[2].3, "{\n  \"name\": \"Bo\\nb\",\n  \"age\": 0,\n  \"score\": 0.1,\n  \"tags\": [],\n  \"extra\": {}\n}\n");
    assert_eq!((hosted[3].0, hosted[3].2.as_str()), (Some(1), ".users[2]: out of range, the array has 2\n"));
    assert_eq!(hosted[4].1, "1.7976931348623157e308\n");
    assert_eq!((hosted[5].0, hosted[5].2.as_str()), (Some(1), "WORK/bad.json:1:13: expected a value\n"));
    assert_eq!((hosted[6].0, hosted[6].2.as_str()), (Some(1), "WORK/missing.json: No such file or directory\n"));
    if let Some(native) = results.get(1) {
        for (k, (h, n)) in hosted.iter().zip(native).enumerate() {
            assert_eq!(h, n, "case {k} ({:?}) differs natively", cases[k]);
        }
    }
    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn trap_fails_on_both_backends() {
    let body = "fn check(x: i64) -> i64 {\n  if x < 10 { return x }\n  trap()\n}\nfn entry() -> i64 { return check(3) + check(30) }\n";
    fails_on_both_backends("trap", body, "trap: the program called `trap()`");
}

/// Builds the native or freestanding program `src` and runs it: its exit status, or
/// `None` if it trapped.
fn run_compiled(name: &str, src: &str) -> Option<i32> {
    let work = std::env::temp_dir().join(format!("jihoo-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let (file, bin) = (work.join(format!("{name}.jh")), work.join(name));
    std::fs::write(&file, src).unwrap();
    assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&file).arg("-o").arg(&bin)), 0, "{name}");
    let status = Command::new(&bin).status().unwrap();
    std::fs::remove_dir_all(&work).unwrap();
    status.code()
}

/// `lib/alloc.jh` traps on misuse instead of reading or writing outside what
/// it allocated, and reports running out of memory.
#[test]
fn alloc_traps_on_misuse() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let program = |body: &str| {
        format!(
            "#![freestanding]\nimport alloc\nfn _start() -> i64 {{\n  let a = alloc.arena_new(4096)\n  \
             let v = alloc.vec_new(i64, &a)\n  alloc.push(i64, &v, 7)\n{body}\n}}\n"
        )
    };
    let misuses = [
        ("get-past-end", "  return alloc.get(i64, &v, 1)"),
        ("get-negative", "  return alloc.get(i64, &v, -1)"),
        ("set-past-end", "  alloc.set(i64, &v, 1, 0)\n  return 0"),
        ("pop-empty", "  alloc.pop(i64, &v)\n  return alloc.pop(i64, &v)"),
        ("align-not-power-of-two", "  alloc.alloc(&a, 8, 3)\n  return 0"),
        ("align-zero", "  alloc.alloc(&a, 8, 0)\n  return 0"),
        ("negative-size", "  alloc.alloc(&a, -1, 8)\n  return 0"),
        ("negative-count", "  alloc.alloc_array(i64, &a, -1)\n  return 0"),
    ];
    for (name, body) in misuses {
        assert_eq!(run_compiled(name, &program(body)), None, "{name} should trap");
    }

    // Running out is not misuse: null or false, and nothing changes. Each
    // check adds a bit to the exit status, which is 127 if all hold.
    let body = "  let ok = 0\n\
        if alloc.get(i64, &v, 0) == 7 { ok += 1 }\n\
        if alloc.alloc(&a, 1 << 40, 8) == 0 as *u8 { ok += 2 }\n\
        if alloc.alloc_array(i64, &a, 9223372036854775807) == 0 as *i64 { ok += 4 }\n\
        let p = alloc.alloc(&a, 1, 1)\n\
        let q = alloc.alloc(&a, 8, 16)\n\
        if (q as i64) % 16 == 0 && q as i64 > p as i64 { ok += 8 }\n\
        alloc.arena_free(&a)\n\
        alloc.arena_free(&a)\n\
        if alloc.arena_cap(&a) == 0 && alloc.alloc(&a, 1, 1) == 0 as *u8 { ok += 16 }\n\
        let none = alloc.arena_new(-5)\n\
        if alloc.arena_cap(&none) == 0 { ok += 32 }\n\
        let w = alloc.vec_new(u8, &none)\n\
        if !alloc.push(u8, &w, 1) && alloc.vec_len(u8, &w) == 0 { ok += 64 }\n\
        return ok";
    assert_eq!(run_compiled("alloc-limits", &program(body)), Some(127));
}

/// `lib/thread.jh` runs threads on POSIX threads, and `lib/sync.jh` makes
/// them take turns.
#[test]
fn thread_example_runs_natively() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let bin = std::env::temp_dir().join(format!("jihoo-threads-{}", std::process::id()));
    let src = repo_root().join("examples/threads.jh");
    assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
    let out = Command::new(&bin).output().unwrap();
    std::fs::remove_file(&bin).unwrap();
    let expected = "1000000 steps, total 1999998\nthe workers' own sums add up to 1999998\nreceived 55 through the mailbox\n";
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
    assert_eq!(out.status.code(), Some(0));
}

/// Threads that fight over a Mutex, a spin lock made of atomics, and a Cond
/// lose no updates; misusing the locks or threads traps.
#[test]
fn sync_holds_under_contention() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let src = "#![native]\nimport sync\nimport thread\n\
        struct S {\n  m: sync.Mutex\n  locked: i64\n  spun: i64\n  flag: u8\n  counted: i64\n  c: sync.Cond\n  turn: i64\n  g: sync.Group\n}\n\
        fn hammer(arg: *u8) -> i64 {\n  let s = arg as *S\n  let i = 0\n  while i < 20000 {\n\
            sync.lock(&s.m)\n    s.locked += 1\n    sync.unlock(&s.m)\n\
            while atomic_swap(&s.flag, 1) != 0 { thread.yield() }\n    s.spun += 1\n    atomic_store(&s.flag, 0)\n\
            atomic_add(&s.counted, 1)\n    i += 1\n  }\n  sync.group_done(&s.g)\n  return i\n}\n\
        // Two players take turns, each waiting for its parity.\n\
        fn play(s: *S, parity: i64) {\n  let k = 0\n  while k < 2000 {\n    sync.lock(&s.m)\n\
            while s.turn % 2 != parity { sync.wait(&s.c, &s.m) }\n\
            s.turn += 1\n    sync.signal(&s.c)\n    sync.unlock(&s.m)\n    k += 1\n  }\n}\n\
        fn odd(arg: *u8) -> i64 {\n  play(arg as *S, 1)\n  return 0\n}\n\
        fn main() -> i64 {\n  let s = S { m: sync.mutex_new(), locked: 0, spun: 0, flag: 0, counted: 0, c: sync.cond_new(), turn: 0, g: sync.group_new() }\n\
          let ts: [Option(thread.Thread); 6] = [Option.None; 6]\n  sync.group_add(&s.g, 6)\n\
          let i = 0\n  while i < 6 {\n    ts[i] = thread.spawn(hammer, &s as *u8)\n    i += 1\n  }\n\
          sync.group_wait(&s.g)\n  let joined = 0\n  i = 0\n  while i < 6 {\n\
            match ts[i] {\n      Some(t) => joined += thread.join(&t)\n      None => return 1\n    }\n    i += 1\n  }\n\
          let o = thread.spawn(odd, &s as *u8)\n  play(&s, 0)\n\
          match o {\n    Some(t) => thread.join(&t)\n    None => return 1\n  }\n\
          let ok = 0\n  if s.locked == 120000 { ok += 1 }\n  if s.spun == 120000 { ok += 2 }\n\
          if atomic_load(&s.counted) == 120000 { ok += 4 }\n  if joined == 120000 { ok += 8 }\n\
          if s.turn == 4000 { ok += 16 }\n  if sync.try_lock(&s.m) { ok += 32 }\n\
          if !sync.try_lock(&s.m) { ok += 64 }\n  return ok\n}\n";
    assert_eq!(run_compiled("sync-contention", src), Some(127));

    let program = |body: &str| format!("#![native]\nimport sync\nimport thread\nfn id(x: *u8) -> i64 {{ return 0 }}\nfn main() -> i64 {{\n{body}\n  return 0\n}}\n");
    let misuses = [
        ("unlock-unlocked", "  let m = sync.mutex_new()\n  sync.unlock(&m)"),
        ("unlock-twice", "  let m = sync.mutex_new()\n  sync.lock(&m)\n  sync.unlock(&m)\n  sync.unlock(&m)"),
        ("group-below-zero", "  let g = sync.group_new()\n  sync.group_add(&g, 1)\n  sync.group_done(&g)\n  sync.group_done(&g)"),
        (
            "join-twice",
            "  match thread.spawn(id, 0 as *u8) {\n    Some(t) => {\n      thread.join(&t)\n      thread.join(&t)\n    }\n    None => {}\n  }",
        ),
    ];
    for (name, body) in misuses {
        assert_eq!(run_compiled(name, &program(body)), None, "{name} should trap");
    }
}

/// `lib/sync.jh` needs no libc: freestanding programs can use its locks.
#[test]
fn sync_works_freestanding() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let src = "#![freestanding]\nimport sync\nfn _start() -> i64 {\n  let m = sync.mutex_new()\n  let ok = 0\n\
        if sync.try_lock(&m) { ok += 1 }\n  if !sync.try_lock(&m) { ok += 2 }\n  sync.unlock(&m)\n\
        sync.lock(&m)\n  if !sync.try_lock(&m) { ok += 4 }\n  sync.unlock(&m)\n\
        let g = sync.group_new()\n  sync.group_add(&g, 2)\n  sync.group_done(&g)\n  sync.group_done(&g)\n  sync.group_wait(&g)\n  ok += 8\n\
        let c = sync.cond_new()\n  sync.signal(&c)\n  sync.broadcast(&c)\n  ok += 16\n  return ok\n}\n";
    assert_eq!(run_compiled("sync-freestanding", src), Some(31));
}

/// Threads making tail calls at once each pass aggregates through a buffer of
/// their own (`tests/threads/tail_calls.jh`).
#[test]
fn tail_calls_on_many_threads() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let src = std::fs::read_to_string(repo_root().join("tests/threads/tail_calls.jh")).unwrap();
    assert_eq!(run_compiled("tail_calls", &src), Some(0));
}

/// `lib/channel.jh` passes values between threads: the worker pool example,
/// and many senders and receivers at once (`tests/threads/channels.jh`).
#[test]
fn channels_pass_values_between_threads() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let bin = std::env::temp_dir().join(format!("jihoo-pipeline-{}", std::process::id()));
    let src = repo_root().join("examples/pipeline.jh");
    assert_eq!(exit_code(Command::new(JIHOO).arg("build").arg(&src).arg("-o").arg(&bin)), 0);
    let out = Command::new(&bin).output().unwrap();
    std::fs::remove_file(&bin).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "100 jobs done, 3142 steps in all, 118 at most\n");
    assert_eq!(out.status.code(), Some(0));

    let src = std::fs::read_to_string(repo_root().join("tests/threads/channels.jh")).unwrap();
    assert_eq!(run_compiled("channels", &src), Some(7));

    let program = |body: &str| format!("#![native]\nimport channel\nfn main() -> i64 {{\n{body}\n  return 0\n}}\n");
    let misuses = [
        ("send-closed", "  match channel.new(i64, 1) {\n    Some(c) => {\n      channel.close(i64, c)\n      channel.send(i64, c, 1)\n    }\n    None => {}\n  }"),
        ("close-twice", "  match channel.new(i64, 1) {\n    Some(c) => {\n      channel.close(i64, c)\n      channel.close(i64, c)\n    }\n    None => {}\n  }"),
        ("negative-capacity", "  channel.new(i64, -1)"),
    ];
    for (name, body) in misuses {
        assert_eq!(run_compiled(name, &program(body)), None, "{name} should trap");
    }
}

/// A `Select` waits on native channels of different types at once
/// (`tests/threads/select.jh`); misusing one traps.
#[test]
fn select_waits_on_native_channels() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let src = std::fs::read_to_string(repo_root().join("tests/threads/select.jh")).unwrap();
    assert_eq!(run_compiled("select", &src), Some(63));

    let program = |body: &str| {
        format!(
            "#![native]\nimport channel\nfn main() -> i64 {{\n  let sel = channel.select_new()\n  let x: i64 = 0\n\
             match channel.new(i64, 1) {{\n    Some(c) => {{\n{body}\n    }}\n    None => {{}}\n  }}\n  return 0\n}}\n"
        )
    };
    let misuses = [
        ("wait-no-cases", "      channel.wait(&sel)"),
        ("wait-all-off", "      let i = channel.on_recv(i64, &sel, c, &x)\n      channel.off(&sel, i)\n      channel.wait(&sel)"),
        ("off-unknown-case", "      channel.off(&sel, 0)"),
        ("too-many-cases", "      let k = 0\n      while k <= channel.MAX_CASES {\n        channel.on_recv(i64, &sel, c, &x)\n        k += 1\n      }"),
        ("send-case-closed", "      channel.on_send(i64, &sel, c, &x)\n      channel.close(i64, c)\n      channel.poll(&sel)"),
    ];
    for (name, body) in misuses {
        assert_eq!(run_compiled(name, &program(body)), None, "{name} should trap");
    }
}

/// `go f(x)` in native code starts a detached thread, with the arguments
/// copied (`tests/threads/go.jh`).
#[test]
fn go_starts_native_threads() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let src = std::fs::read_to_string(repo_root().join("tests/threads/go.jh")).unwrap();
    assert_eq!(run_compiled("go", &src), Some(15));
}

/// `channel.wait_ms` gives up on a Select after a while
/// (`tests/threads/timeout.jh`).
#[test]
fn select_times_out() {
    if std::env::var_os("JIHOO_LLC").is_none() {
        eprintln!("JIHOO_LLC is not set: skipping");
        return;
    }
    let src = std::fs::read_to_string(repo_root().join("tests/threads/timeout.jh")).unwrap();
    assert_eq!(run_compiled("timeout", &src), Some(31));
}
