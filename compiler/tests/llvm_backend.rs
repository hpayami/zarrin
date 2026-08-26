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
