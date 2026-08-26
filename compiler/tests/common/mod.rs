//! Shared harness: compiles nothing, just drives the real `zarrinc` binary.
//!
//! Cargo hands integration tests the path to the built binary via
//! `CARGO_BIN_EXE_zarrinc`, so these tests exercise the same end-to-end path a
//! user does (parse -> resolve imports -> run / check).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::process::Command;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct RunResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

impl RunResult {
    pub fn lines(&self) -> Vec<&str> {
        self.stdout.lines().collect()
    }
}

/// Write `src` to a scratch file and invoke `zarrinc <cmd> <file>`.
pub fn zarrinc(cmd: &str, src: &str) -> RunResult {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("zarrin-test-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let file = dir.join("prog.zr");
    std::fs::write(&file, src).expect("write scratch program");

    let out = Command::new(env!("CARGO_BIN_EXE_zarrinc"))
        .arg(cmd)
        .arg(&file)
        .output()
        .expect("failed to invoke zarrinc");

    let _ = std::fs::remove_dir_all(&dir);
    RunResult {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        success: out.status.success(),
    }
}

pub fn run(src: &str) -> RunResult {
    zarrinc("run", src)
}

/// The program runs to completion and prints exactly these lines.
pub fn assert_output(src: &str, expected: &[&str]) {
    let r = run(src);
    assert!(r.success, "program failed unexpectedly:\n{}", r.stderr);
    assert_eq!(r.lines().as_slice(), expected, "\nstderr was:\n{}", r.stderr);
}

/// The program aborts, and `needle` appears in the diagnostic.
pub fn assert_run_fails(src: &str, needle: &str) {
    let r = run(src);
    assert!(!r.success, "expected the program to fail; it printed:\n{}", r.stdout);
    assert!(
        r.stderr.contains(needle),
        "diagnostic did not mention {:?}; stderr was:\n{}",
        needle,
        r.stderr
    );
}

pub fn assert_checks(src: &str) {
    let r = zarrinc("check", src);
    assert!(r.success, "expected the program to type-check:\n{}", r.stderr);
}

pub fn assert_check_error(src: &str, needle: &str) {
    let r = zarrinc("check", src);
    assert!(!r.success, "expected a type error; got:\n{}", r.stdout);
    assert!(
        r.stderr.contains(needle),
        "type error did not mention {:?}; stderr was:\n{}",
        needle,
        r.stderr
    );
}

pub fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("examples")
}

pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}
