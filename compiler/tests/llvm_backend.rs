//! Native-backend tests. Skipped unless the `llvm` feature is on and
//! LLVM_SYS_180_PREFIX is set, since the backend cannot be built otherwise.
//!
//! Each case compiles a program and requires the executable to agree with the
//! interpreter, which is the reference implementation.

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn enabled() -> bool {
    cfg!(feature = "llvm") && std::env::var_os("LLVM_SYS_180_PREFIX").is_some()
}

/// Compile `src` natively and return its stdout, or None if the backend is off.
fn build_and_run(src: &str) -> Option<String> {
    if !enabled() {
        return None;
    }
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf = std::env::temp_dir().join(format!("zarrin-llvm-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("prog.zr");
    std::fs::write(&file, src).unwrap();
    let exe = dir.join("prog");

    let build = Command::new(env!("CARGO_BIN_EXE_zarrinc"))
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("failed to invoke zarrinc");
    assert!(
        build.status.success(),
        "native build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&exe).output().expect("failed to run the executable");
    assert!(
        run.status.success(),
        "executable failed with {:?}; stderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let out = String::from_utf8_lossy(&run.stdout).into_owned();

    // A heap overrun usually lands in malloc's slack and goes unnoticed, which
    // makes it a poor thing to test for directly. Guard Malloc puts every
    // allocation against an unmapped page, so the same overrun always faults.
    const GUARD: &str = "/usr/lib/libgmalloc.dylib";
    if std::path::Path::new(GUARD).exists() {
        let guarded = Command::new(&exe)
            .env("DYLD_INSERT_LIBRARIES", GUARD)
            .env("MALLOC_LOG_FILE", "/dev/null")
            .output()
            .expect("failed to run under Guard Malloc");
        assert!(
            guarded.status.success(),
            "clean run, but faulted under Guard Malloc ({:?}) — out-of-bounds heap access",
            guarded.status
        );
        assert_eq!(
            String::from_utf8_lossy(&guarded.stdout),
            out,
            "output changed under Guard Malloc — reading uninitialised or out-of-bounds memory"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
    Some(out)
}

/// The compiled program must print exactly what the interpreter prints.
fn assert_agrees_with_interpreter(src: &str) {
    let Some(native) = build_and_run(src) else { return };
    let interpreted = common::run(src);
    assert!(interpreted.success, "interpreter failed:\n{}", interpreted.stderr);
    assert_eq!(
        native, interpreted.stdout,
        "native output disagrees with the interpreter"
    );
}

#[test]
fn an_enum_payload_survives_the_frame_that_built_it() {
    // Enum values are passed around as raw addresses. They used to be built
    // with `alloca`, so a function returning one handed back a pointer into
    // its own dead stack frame; `noise` then overwrote it and unwrap read
    // garbage (12345 came back as 6163323728).
    assert_agrees_with_interpreter(
        r#"
fn wrap(a: int) -> Option { return Some(a); }
fn noise(n: int) -> int { let t = 0; let i = 0; while i < n { t = t + i * 7; i = i + 1; } return t; }
fn main() {
    let o = wrap(12345);
    print(noise(60));
    print(o.unwrap());
}
"#,
    );
}

#[test]
fn array_literals_allocate_enough_room() {
    // `malloc(len + 1)` bytes, then `len + 1` 64-bit words written into it:
    // an eightfold overrun. A 32-element array killed the process.
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let a = [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32];
    print(array_len(a));
    print(array_get(a, 0));
    print(array_get(a, 31));
}
"#,
    );
}

#[test]
fn array_set_allocates_enough_room() {
    // Same defect in the copy-on-write path.
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let a = [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32];
    let b = array_set(a, 0, 111);
    let c = array_set(b, 31, 222);
    print(array_get(c, 0));
    print(array_get(c, 31));
    print(array_len(c));
}
"#,
    );
}

#[test]
fn basics_agree_with_the_interpreter() {
    assert_agrees_with_interpreter(
        r#"
fn square(x: int) -> int { return x * x; }
fn main() {
    print(1 + 2 * 3);
    print(square(7));
    print("hello " + "world");
    let i = 0;
    while i < 3 { print(i); i = i + 1; }
    for k in 0..3 { if k == 1 { continue; } print(k * 10); }
}
"#,
    );
}

// --- match lowering ---------------------------------------------------------
//
// Every arm body used to be emitted twice for a payload-carrying pattern: the
// `continue` meant to skip the arm only skipped the pattern, so control fell
// into the shared tail and generated the body again. That gave the phi two
// entries for one block and llc rejected the module.

#[test]
fn match_on_an_enum_with_payloads_compiles() {
    assert_agrees_with_interpreter(
        r#"
enum S { C(int), R(int, int) }
fn area(s: S) -> int { return match s { C(r) => r * r, R(w, h) => w * h }; }
fn main() { print(area(C(5))); print(area(R(3, 4))); }
"#,
    );
}

#[test]
fn match_with_literal_multi_pattern_and_wildcard_arms() {
    assert_agrees_with_interpreter(
        r#"
fn f(n: int) -> int { return match n { 0 => 100, 1 | 2 => 200, _ => 300 }; }
fn main() { print(f(0)); print(f(1)); print(f(2)); print(f(9)); }
"#,
    );
}

#[test]
fn match_on_an_enum_without_payloads() {
    assert_agrees_with_interpreter(
        r#"
enum C { R, G, B }
fn f(c: C) -> int { return match c { R => 1, G => 2, B => 3 }; }
fn main() { print(f(R)); print(f(G)); print(f(B)); }
"#,
    );
}

#[test]
fn a_guarded_arm_falls_through_when_the_guard_fails() {
    // The guard branched from `arm_bb` back to `arm_bb` — a self-loop — and the
    // body was never emitted, so `merge` gained a predecessor that fed the phi
    // nothing. A guarded irrefutable pattern was also treated as the last arm,
    // so the arms after it were unreachable: f(3) returned 0 instead of 2.
    assert_agrees_with_interpreter(
        r#"
fn f(n: int) -> int { return match n { x if x > 10 => 1, _ => 2 }; }
fn main() { print(f(50)); print(f(3)); }
"#,
    );
}

#[test]
fn a_guard_on_a_payload_pattern_compiles() {
    // Pattern mismatch and guard failure each appended their own "next check"
    // block; one of them was branched to and never terminated.
    assert_agrees_with_interpreter(
        r#"
enum S { C(int), R(int, int) }
fn f(s: S) -> int { return match s { C(r) if r > 100 => 999, C(r) => r, R(w, h) => w * h }; }
fn main() { print(f(C(5))); print(f(C(500))); print(f(R(3, 4))); }
"#,
    );
}

#[test]
fn a_guard_on_the_last_arm_compiles() {
    assert_agrees_with_interpreter(
        r#"
fn f(n: int) -> int { return match n { 0 => 7, x if x > 10 => 1 }; }
fn main() { print(f(0)); print(f(50)); }
"#,
    );
}

#[test]
fn the_float_match_example_compiles() {
    // examples/enum_match.zr, which llc used to reject outright. Output is not
    // compared: this backend prints floats with printf's %f ("78.500000")
    // while the interpreter uses Rust's shortest representation ("78.5").
    let Some(out) = build_and_run(
        r#"
enum Shape { Circle(float), Rect(float, float) }
fn area(s: Shape) -> float { return match s { Circle(r) => 3.14 * r * r, Rect(w, h) => w * h }; }
fn main() { print(area(Circle(5.0))); print(area(Rect(3.0, 4.0))); }
"#,
    ) else { return };
    assert!(out.starts_with("78.5"), "unexpected output: {}", out);
}
