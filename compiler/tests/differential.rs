//! Every program in `differential/` run through both backends, compared.
//!
//! The two backends are meant to be the same language. `examples/` checks that
//! for programs written to show the language off; this corpus is written to
//! catch the two disagreeing, so it leans on the edges: failing programs, empty
//! and one-element cases, the smallest and largest integers, non-ASCII strings,
//! loops that break out of the middle.
//!
//! Every case here found something once. Fourteen differences turned up the
//! first time it ran, from `break` behaving as `continue` to a `let` inside a
//! block taking over the outer variable of the same name.
//!
//! A case does not have to succeed. What is compared is the whole observable
//! behaviour: what was printed, what the first line of the diagnostic was, and
//! the exit status — a program the two backends refuse in the same words is a
//! program they agree on.
//!
//! Needs `--features llvm` and LLVM_SYS_180_PREFIX; without the native backend
//! there is nothing to compare against and the whole file is skipped.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Long enough for any of these to finish, short enough that a loop which no
/// longer ends is a failed test rather than a hung one. The `break` bug hung
/// the first run of this corpus.
const PATIENCE: Duration = Duration::from_secs(20);

fn enabled() -> bool {
    cfg!(feature = "llvm") && std::env::var_os("LLVM_SYS_180_PREFIX").is_some()
}

struct Outcome {
    stdout: String,
    /// The first line only: each side names its own scratch path after it.
    first_error: String,
    code: Option<i32>,
    timed_out: bool,
}

/// Run a command, and stop waiting for it after `PATIENCE`.
fn run_bounded(mut cmd: Command) -> Outcome {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start");
    let started = Instant::now();
    loop {
        match child.try_wait().expect("failed to wait") {
            Some(_) => break,
            None if started.elapsed() > PATIENCE => {
                let _ = child.kill();
                let _ = child.wait();
                return Outcome {
                    stdout: String::new(),
                    first_error: String::new(),
                    code: None,
                    timed_out: true,
                };
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let out = child.wait_with_output().expect("failed to collect output");
    Outcome {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        first_error: String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string(),
        code: out.status.code(),
        timed_out: false,
    }
}

fn interpret(path: &Path) -> Outcome {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zarrinc"));
    cmd.arg("run").arg(path);
    run_bounded(cmd)
}

/// Compile and run. A program the checker rejects never becomes an executable,
/// and the refusal itself is the behaviour to compare.
fn compile_and_run(path: &Path, exe: &Path) -> Outcome {
    let mut build = Command::new(env!("CARGO_BIN_EXE_zarrinc"));
    build.arg("build").arg(path).arg("-o").arg(exe);
    let built = run_bounded(build);
    if built.code != Some(0) {
        return built;
    }
    run_bounded(Command::new(exe))
}

fn cases() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("differential");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", dir.display(), e))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "zr").unwrap_or(false))
        .collect();
    found.sort();
    assert!(!found.is_empty(), "no cases in {}", dir.display());
    found
}

#[test]
fn both_backends_answer_alike() {
    if !enabled() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("zarrin-differential-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut disagreements = Vec::new();
    for case in cases() {
        let name = case.file_stem().unwrap().to_string_lossy().to_string();
        let exe = dir.join(&name);
        let interpreted = interpret(&case);
        let native = compile_and_run(&case, &exe);

        let mut note = |what: &str, a: &str, b: &str| {
            disagreements.push(format!(
                "{}: {}\n  interpreter: {}\n  native:      {}",
                name, what, a, b
            ));
        };
        if interpreted.timed_out || native.timed_out {
            note(
                "one side never finished",
                if interpreted.timed_out { "still running" } else { "finished" },
                if native.timed_out { "still running" } else { "finished" },
            );
            continue;
        }
        if interpreted.stdout != native.stdout {
            note("different output", &format!("{:?}", interpreted.stdout), &format!("{:?}", native.stdout));
        }
        if interpreted.first_error != native.first_error {
            note("different diagnostic", &interpreted.first_error, &native.first_error);
        }
        // A program the checker rejects is refused before it is an executable,
        // so the native side has no exit status of its own to compare — the
        // message above is what says the two agree.
        let refused_by_the_checker =
            !native.first_error.is_empty() && native.first_error == interpreted.first_error;
        if !refused_by_the_checker && interpreted.code != native.code {
            note("different exit status", &format!("{:?}", interpreted.code), &format!("{:?}", native.code));
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        disagreements.is_empty(),
        "the two backends disagree on {} case(s):\n\n{}",
        disagreements.len(),
        disagreements.join("\n\n")
    );
}
