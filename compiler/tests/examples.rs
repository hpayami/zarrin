//! Golden-output tests over `examples/`.
//!
//! Every `.zr` file in `examples/` is run through the interpreter and its
//! stdout compared against `compiler/tests/golden/<name>.out`. When an example
//! legitimately changes, regenerate the goldens with:
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test
//! ```

mod common;

use std::fs;
use std::process::Command;

#[test]
fn examples_produce_their_golden_output() {
    let examples = common::examples_dir();
    let golden = common::golden_dir();
    fs::create_dir_all(&golden).expect("create golden dir");
    let updating = std::env::var_os("UPDATE_GOLDEN").is_some();

    let mut programs: Vec<_> = fs::read_dir(&examples)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", examples.display(), e))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "zr").unwrap_or(false))
        .collect();
    programs.sort();
    assert!(!programs.is_empty(), "no examples found in {}", examples.display());

    let mut failures = Vec::new();
    for path in &programs {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let out = Command::new(env!("CARGO_BIN_EXE_zarrinc"))
            .arg("run")
            .arg(path)
            .output()
            .expect("failed to invoke zarrinc");
        let actual = String::from_utf8_lossy(&out.stdout).into_owned();

        if !out.status.success() {
            failures.push(format!(
                "{}: exited with failure\n  stderr: {}",
                name,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            continue;
        }

        let golden_file = golden.join(format!("{}.out", name));
        if updating {
            fs::write(&golden_file, &actual).expect("write golden");
            continue;
        }
        match fs::read_to_string(&golden_file) {
            Ok(expected) if expected == actual => {}
            Ok(expected) => failures.push(format!(
                "{}: output changed\n  expected: {:?}\n  actual:   {:?}",
                name, expected, actual
            )),
            Err(_) => failures.push(format!(
                "{}: no golden file yet ({})",
                name,
                golden_file.display()
            )),
        }
    }

    assert!(
        failures.is_empty(),
        "\n{}\n\nIf these changes are intended, re-record with: UPDATE_GOLDEN=1 cargo test\n",
        failures.join("\n")
    );
}
