//! The JIR version is written in three places that must agree: `JIR_VERSION`
//! here, `jir::kVersion` in the LLVM backend and the title of `docs/jir.md`.
//! See "Versions" in `docs/jir.md`.

use std::path::PathBuf;

/// The fingerprint of `docs/jir.md` when `JIR_VERSION` was last decided on.
/// When the spec changes, `spec_changes_decide_the_version` fails until this is
/// updated: first bump the version if the change needs one.
const SPEC_FINGERPRINT: u64 = 0x437b76f8165e1957;

fn repo_file(path: &str) -> String {
    let full = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(path);
    std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("cannot read {}: {e}", full.display()))
}

/// The number after `prefix` on the first line that has it.
fn number_after(text: &str, prefix: &str) -> u32 {
    let start = text.find(prefix).unwrap_or_else(|| panic!("no `{prefix}` found")) + prefix.len();
    let digits: String = text[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or_else(|_| panic!("no number after `{prefix}`"))
}

#[test]
fn backend_and_spec_have_the_same_version() {
    let backend = number_after(&repo_file("backend-llvm/src/jir.h"), "constexpr int64_t kVersion = ");
    assert_eq!(backend, jihoo_ir::JIR_VERSION, "jir::kVersion in backend-llvm/src/jir.h");
    let spec = number_after(&repo_file("docs/jir.md"), "text format (version ");
    assert_eq!(spec, jihoo_ir::JIR_VERSION, "the title of docs/jir.md");
}

#[test]
fn printed_modules_carry_the_version() {
    let m = jihoo_ir::Module {
        profile: jihoo_ir::Profile::Freestanding,
        structs: vec![],
        enums: vec![],
        externs: vec![],
        funcs: vec![],
    };
    let text = m.to_string();
    let header = text.lines().find(|l| !l.starts_with(';')).unwrap();
    assert_eq!(header, format!("jir {}", jihoo_ir::JIR_VERSION));
}

#[test]
fn spec_changes_decide_the_version() {
    // FNV-1a: small, and stable across Rust versions, unlike `DefaultHasher`.
    let fingerprint = repo_file("docs/jir.md")
        .bytes()
        .filter(|&b| b != b'\r') // the same on a checkout with CRLF line endings
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    assert!(
        fingerprint == SPEC_FINGERPRINT,
        "docs/jir.md changed. If the change lets the frontend and the backend disagree \
         (see \"Versions\" there), bump JIR_VERSION in crates/jihoo-ir/src/lib.rs, \
         jir::kVersion in backend-llvm/src/jir.h and the version in the title of \
         docs/jir.md, and add a row to its table. Then set SPEC_FINGERPRINT in \
         crates/jihoo-ir/tests/version.rs to {fingerprint:#x}."
    );
}
