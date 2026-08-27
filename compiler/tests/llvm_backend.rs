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

/// Compile `src` and run it expecting failure; returns (stdout, first stderr
/// line, exit code), or None when the backend is off.
fn build_and_run_failing(src: &str) -> Option<(String, String, Option<i32>)> {
    if !enabled() {
        return None;
    }
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf = std::env::temp_dir().join(format!("zarrin-llvm-fail-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("prog.zr");
    std::fs::write(&file, src).unwrap();
    let exe = dir.join("prog");
    let build = Command::new(env!("CARGO_BIN_EXE_zarrinc"))
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("failed to invoke zarrinc");
    assert!(build.status.success(), "native build failed:\n{}", String::from_utf8_lossy(&build.stderr));
    let run = Command::new(&exe).output().expect("failed to run the executable");
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    let err = String::from_utf8_lossy(&run.stderr).lines().next().unwrap_or_default().to_string();
    let code = run.status.code();
    let _ = std::fs::remove_dir_all(&dir);
    Some((out, err, code))
}

/// A run-time failure must look the same from either backend: same stdout up to
/// the failure, same message, same exit status. Only the first stderr line is
/// compared, since each side writes its program to a different scratch path.
fn assert_fails_like_the_interpreter(src: &str) {
    let Some((native_out, native_err, code)) = build_and_run_failing(src) else { return };
    let interpreted = common::run(src);
    assert!(!interpreted.success, "the interpreter accepted this program");
    assert_eq!(code, Some(1), "native exit status");
    assert_eq!(native_out, interpreted.stdout, "output before the failure differs");
    let want = interpreted.stderr.lines().next().unwrap_or_default();
    assert_eq!(native_err, want, "diagnostic differs");
    assert!(native_err.starts_with("error: "), "not a diagnostic: {}", native_err);
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
fn f(n: int) -> int { return match n { 0 => 7, x if x > 10 => 1, _ => 0 }; }
fn main() { print(f(0)); print(f(50)); print(f(3)); }
"#,
    );
}

#[test]
fn the_float_match_example_agrees() {
    // examples/enum_match.zr, which llc used to reject outright.
    assert_agrees_with_interpreter(
        r#"
enum Shape { Circle(float), Rect(float, float) }
fn area(s: Shape) -> float { return match s { Circle(r) => 3.14 * r * r, Rect(w, h) => w * h }; }
fn main() { print(area(Circle(5.0))); print(area(Rect(3.0, 4.0))); }
"#,
    );
}

// --- float formatting -------------------------------------------------------
//
// This backend printed floats with `sprintf("%.6f")`: 12 came out as
// 12.000000, and 1/3 was truncated to 0.333333. It now searches for the
// shortest decimal that reads back as the same double, which is what the
// interpreter (Rust's Display) produces.

#[test]
fn whole_numbers_print_without_a_fraction() {
    assert_agrees_with_interpreter(r#"fn main() { print(12.0); print(1.0); print(100.0); }"#);
}

#[test]
fn precision_is_not_truncated() {
    assert_agrees_with_interpreter(
        r#"fn main() { print(1.0 / 3.0); print(2.0 / 7.0 * 1000000.0); print(0.1 + 0.2); }"#,
    );
}

#[test]
fn small_and_large_magnitudes() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    print(0.000000123);
    print(123456789012345.6789);
    print(100000.0 * 100000.0);
    print(0.00001);
}
"#,
    );
}

#[test]
fn signs_and_zero() {
    assert_agrees_with_interpreter(r#"fn main() { print(0.0); print(0.0 - 0.0); print(0.0 - 0.5); print(0.0 - 7.25); }"#);
}

#[test]
fn nan_and_infinity() {
    // NaN never compares equal to itself, so the search runs to its cap;
    // infinity round-trips immediately but has no digits to rewrite.
    assert_agrees_with_interpreter(
        r#"fn main() { print(1.0 / 0.0 - 1.0 / 0.0); print(1.0 / 0.0); print(0.0 - 1.0 / 0.0); }"#,
    );
}

#[test]
fn to_string_and_print_agree_on_floats() {
    assert_agrees_with_interpreter(r#"fn main() { print(78.5); print(to_string(78.5)); }"#);
}

// --- printing enums ---------------------------------------------------------
//
// The backend erases every value to i64, so `print` on an enum used to show the
// address it was represented by (Blue came out as 4303841008). The variant is
// now recovered statically from the expression and rendered like the
// interpreter does.

#[test]
fn payload_free_variants_print_their_name() {
    assert_agrees_with_interpreter(
        r#"
enum Color { Red, Green, Blue }
fn main() { print(Red); print(Green); print(Blue); let c = Green; print(c); }
"#,
    );
}

#[test]
fn variants_with_payloads_print_their_fields() {
    assert_agrees_with_interpreter(
        r#"
enum Color { Red, Rgb(int, int, int) }
fn main() { print(Rgb(255, 128, 0)); print(Red); let d = Rgb(7, 8, 9); print(d); }
"#,
    );
}

#[test]
fn payloads_of_every_type_print_correctly() {
    assert_agrees_with_interpreter(
        r#"
enum S { Circle(float), Named(string), Pair(int, float) }
fn main() { print(Circle(2.5)); print(Named("hi")); print(Pair(3, 0.5)); }
"#,
    );
}

#[test]
fn enums_from_calls_and_parameters_print() {
    // The type comes from the function's declared return type, and from the
    // parameter's declared type, not from a literal at the print site.
    assert_agrees_with_interpreter(
        r#"
enum Color { Red, Rgb(int, int, int) }
fn pick(n: int) -> Color { if n == 0 { return Red; } return Rgb(1, 2, 3); }
fn show(c: Color) { print(c); }
fn main() { print(pick(0)); print(pick(1)); show(Red); show(Rgb(4, 5, 6)); }
"#,
    );
}

#[test]
fn builtin_option_prints_like_a_declared_enum() {
    // Option and Result are predeclared and are not in the program's own enum
    // table, so rendering has to consult both.
    assert_agrees_with_interpreter(r#"fn main() { let o = Some(5); print(o); print(None); }"#);
}

#[test]
fn enums_render_through_to_string_and_interpolation() {
    assert_agrees_with_interpreter(
        r#"
enum Color { Red, Rgb(int, int, int) }
fn main() { let d = Rgb(7, 8, 9); print(to_string(Red)); print("value: {d}"); }
"#,
    );
}

#[test]
fn matching_a_payload_free_enum_still_works() {
    // Rendering needed the built-in enums merged into the variant view. Doing
    // that in the table the match lowering reads would have flipped its
    // has-payload flag for every program and broken this.
    assert_agrees_with_interpreter(
        r#"
enum P { A, B, C }
fn f(p: P) -> int { return match p { A => 1, B => 2, C => 3 }; }
fn main() { print(f(A)); print(f(B)); print(f(C)); print(A); }
"#,
    );
}

// --- match on the predeclared enums, and representation per scrutinee -------

#[test]
fn matching_option_and_result_binds_payloads() {
    // The pattern's tag and payload came from a scan of the program's own enum
    // table, which does not contain Option or Result: `Some(v)` bound nothing
    // ("undefined var: v") and every variant compared against tag 0.
    assert_agrees_with_interpreter(
        r#"
fn describe(o: Option) -> int { return match o { Some(v) => v, None => 0 - 1 }; }
fn check(r: Result) -> int { return match r { Ok(v) => v, Err(e) => 0 - e }; }
fn main() {
    print(describe(Some(42)));
    print(describe(None));
    print(check(Ok(7)));
    print(check(Err(3)));
}
"#,
    );
}

#[test]
fn matching_an_integer_works_alongside_a_payload_enum() {
    // Whether a scrutinee is a pointer or a bare tag was decided by one flag
    // over every enum in the program, so declaring a payload variant anywhere
    // made this dereference an integer. The program printed nothing at all.
    assert_agrees_with_interpreter(
        r#"
enum Shape { Circle(int), Square(int) }
fn classify(n: int) -> int { return match n { 0 => 10, 1 => 20, _ => 30 }; }
fn main() { print(classify(0)); print(classify(1)); print(classify(5)); }
"#,
    );
}

#[test]
fn qualified_patterns_match_natively() {
    // `C::G` fell through to the wildcard: the scan compared against the whole
    // string "C::G" and never found it.
    assert_agrees_with_interpreter(
        r#"
enum C { R, G }
fn f(c: C) -> int { return match c { C::R => 1, C::G => 2, _ => 9 }; }
fn main() { print(f(R)); print(f(G)); }
"#,
    );
}

// --- struct fields ----------------------------------------------------------
//
// Field access required a variable bound directly to a struct literal, and the
// loaded word was always treated as an int. Everything else either refused to
// compile ("variable 'p' is not a struct", "cannot determine struct type") or
// printed a bit pattern.

#[test]
fn fields_of_a_struct_returned_from_a_function() {
    assert_agrees_with_interpreter(
        r#"
struct P { x: int, y: int }
fn make(a: int) -> P { return P { x: a, y: a + 1 }; }
fn main() { let p = make(10); print(p.x); print(p.y); print(make(5).x); }
"#,
    );
}

#[test]
fn fields_of_a_struct_parameter() {
    assert_agrees_with_interpreter(
        r#"
struct P { x: int, y: int }
fn sum(p: P) -> int { return p.x + p.y; }
fn main() { print(sum(P { x: 3, y: 4 })); }
"#,
    );
}

#[test]
fn nested_struct_fields() {
    assert_agrees_with_interpreter(
        r#"
struct Inner { v: int }
struct Outer { i: Inner, k: int }
fn main() { let o = Outer { i: Inner { v: 9 }, k: 1 }; print(o.i.v); print(o.k); }
"#,
    );
}

#[test]
fn fields_keep_their_declared_type() {
    // A float field printed its bit pattern (4612811918334230528 for 2.5) and a
    // string field its address.
    assert_agrees_with_interpreter(
        r#"
struct M { a: int, b: float, c: string }
fn main() { let m = M { a: 1, b: 2.5, c: "hi" }; print(m.a); print(m.b); print(m.c); }
"#,
    );
}

#[test]
fn self_resolves_to_the_impl_type() {
    // The parser types a `self` parameter as `Self`, so a method reading its
    // own fields could not tell which struct it belonged to.
    assert_agrees_with_interpreter(
        r#"
struct S { w: float, name: string }
impl S { fn label(self) -> string { return self.name; } fn width(self) -> float { return self.w; } }
fn main() { let s = S { w: 1.5, name: "boxy" }; print(s.label()); print(s.width()); }
"#,
    );
}

#[test]
fn a_struct_field_holding_an_enum_prints_by_name() {
    assert_agrees_with_interpreter(
        r#"
enum Color { Red, Rgb(int, int, int) }
struct Style { c: Color, name: string }
struct Box { s: Style, n: int }
fn main() {
    let b = Box { s: Style { c: Red, name: "inner" }, n: 7 };
    print(b.n);
    print(b.s.c);
    print("{b.s.name} / {b.s.c}");
}
"#,
    );
}

// --- bounds checks ----------------------------------------------------------
//
// The interpreter checks indices and reports a positioned error. The native
// backend read past the allocation, printed whatever was there and carried on:
// `print(a[10])` on a three-element array printed 0 and the program finished
// successfully.

#[test]
fn an_out_of_range_index_stops_the_program() {
    assert_fails_like_the_interpreter(
        "fn main() {\n    let a = [1, 2, 3];\n    print(a[0]);\n    print(a[10]);\n    print(\"never\");\n}\n",
    );
}

#[test]
fn a_negative_index_is_caught() {
    assert_fails_like_the_interpreter("fn main() {\n    let a = [1, 2];\n    print(a[0 - 1]);\n}\n");
}

#[test]
fn array_get_and_array_set_are_checked() {
    assert_fails_like_the_interpreter("fn main() {\n    let a = [1, 2];\n    print(array_get(a, 5));\n}\n");
    assert_fails_like_the_interpreter("fn main() {\n    let a = [1, 2];\n    let b = array_set(a, 9, 0);\n}\n");
}

#[test]
fn substring_bounds_are_checked_natively() {
    assert_fails_like_the_interpreter("fn main() {\n    print(substring(\"hello\", 2, 99));\n}\n");
    assert_fails_like_the_interpreter("fn main() {\n    print(substring(\"hello\", 4, 1));\n}\n");
}

#[test]
fn indices_in_range_still_work() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let a = [10, 20, 30];
    print(a[0]);
    print(a[2]);
    print(array_get(a, 1));
    print(array_get(array_set(a, 1, 99), 1));
    print(substring("hello", 1, 4));
    print(substring("hello", 0, 5));
    print(substring("hello", 3, 3));
}
"#,
    );
}

// --- a match that matches nothing -------------------------------------------
//
// The final arm was treated as always matching, whatever its pattern. Its body
// ran for values nothing covered, so an unmatched value produced a plausible
// wrong answer instead of the interpreter's error.

#[test]
fn an_unmatched_value_stops_the_program() {
    // The last arm used to run whatever its pattern, so an unmatched value
    // produced a plausible wrong answer. The checker rejects most incomplete
    // matches outright now; this one it cannot, because `array_get` gives it
    // no element type to reason about.
    assert_fails_like_the_interpreter(
        "fn main() {\n    let a = [7, 8];\n    print(array_get(a, 0));\n    print(match array_get(a, 0) { 5 => 10, 6 => 20 });\n}\n",
    );
}

#[test]
fn a_match_covering_every_variant_needs_no_wildcard() {
    assert_agrees_with_interpreter(
        r#"
enum S { C(int), R(int, int) }
fn area(s: S) -> int { return match s { C(r) => r * r, R(w, h) => w * h }; }
fn main() { print(area(C(5))); print(area(R(3, 4))); }
"#,
    );
}

#[test]
fn a_wildcard_last_arm_still_catches_everything() {
    assert_agrees_with_interpreter(
        r#"
enum C { R, G, B }
fn f(c: C) -> int { return match c { R => 1, _ => 9 }; }
fn g(n: int) -> int { return match n { 0 => 10, x => x * 2 }; }
fn main() { print(f(R)); print(f(B)); print(g(0)); print(g(21)); }
"#,
    );
}

// --- values that lose their type at the merge -------------------------------
//
// Both found by the cross-backend walk below, once the comprehensive example
// was made comprehensive.

#[test]
fn booleans_print_as_true_and_false() {
    // Booleans are i64 0/1 in this backend, so `print` showed 1.
    assert_agrees_with_interpreter(
        r#"
fn big(n: int) -> bool { return n > 100; }
fn main() {
    print(true); print(false);
    print(1 < 2); print(1 == 2);
    print(true && false); print(!true);
    print(contains("hello", "ell"));
    print(big(500)); print(big(1));
    let b = 3 > 1;
    print(b); print(to_string(b));
    let o = Some(1);
    print(o.is_some()); print(o.is_none());
}
"#,
    );
}

#[test]
fn a_match_or_if_yielding_a_string_keeps_its_type() {
    // Branches merge through an i64 phi; the result was handed back as an int,
    // so a string arm printed the number its pointer happened to be.
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let n = 7;
    print(match n { 7 => "seven", _ => "other" });
    print(if n > 3 { "big" } else { "small" });
    print(if n > 3 { 1.5 } else { 0.25 });
    print(match n { 7 => 2.5, _ => 0.0 });
}
"#,
    );
}

// --- types the backend used to have no rule for ------------------------------
//
// The backend asks the type checker now instead of pattern-matching on the
// shape of an expression, so these follow without a rule for each.

#[test]
fn types_reached_through_other_values() {
    assert_agrees_with_interpreter(
        r#"
enum Color { Red, Rgb(int, int, int) }
struct Style { c: Color, on: bool, w: float }
fn pick() -> Color { return Rgb(1, 2, 3); }
fn main() {
    let s = Style { c: pick(), on: 3 > 1, w: 2.5 };
    print(s.on);
    print(s.c);
    print(s.w);
    let b = match 1 { 1 => 2 > 1, _ => false };
    print(b);
    print(if true { 1 == 1 } else { false });
    let e = match 0 { 0 => Red, _ => pick() };
    print(e);
}
"#,
    );
}

#[test]
fn a_branch_that_opens_blocks_of_its_own() {
    // The phi recorded the block an arm started in. A body containing its own
    // if or match ends somewhere else, and llc rejected the module:
    // "PHI node entries do not match predecessors".
    assert_agrees_with_interpreter(
        r#"
fn main() {
    print(match 2 { 2 => if true { "yes" } else { "no" }, _ => "other" });
    print(if true { match 1 { 1 => 10, _ => 20 } } else { 0 });
    print(match 1 { 1 => match 2 { 2 => 1.5, _ => 0.5 }, _ => 0.0 });
}
"#,
    );
}

// --- locals and parameters carry their type ---------------------------------

#[test]
fn parameters_of_every_type() {
    // Locals were tagged with a string, and a parameter was always tagged
    // "int": `fn twice(x: float)` failed to compile with "expected int
    // operand", and a string parameter with "only int/float args supported".
    assert_agrees_with_interpreter(
        r#"
struct P { x: int }
fn twice(x: float) -> float { return x * 2.0; }
fn label(s: string) -> string { return s + "!"; }
fn mix(a: int, b: float, c: string, d: bool) -> string {
    if d { return c + to_string(a) + to_string(b); }
    return "off";
}
impl P { fn scaled(self, by: float) -> float { return by * 3.0; } }
fn main() {
    print(twice(1.25));
    print(label("hi"));
    print(mix(1, 2.5, "v=", true));
    print(mix(1, 2.5, "v=", false));
    let p = P { x: 1 };
    print(p.scaled(1.5));
}
"#,
    );
}

#[test]
fn reassigning_a_string_or_float_local() {
    // A string local held the string pointer as its slot, so assigning to it
    // wrote into the string data rather than rebinding the variable.
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let s = "one";
    print(s);
    s = "two";
    print(s);
    let f = 1.5;
    f = 2.5;
    print(f);
}
"#,
    );
}

// --- allocation ---------------------------------------------------------------

/// Compile `src` and hand back the LLVM IR the backend emitted alongside it.
fn emitted_ir(src: &str) -> Option<String> {
    if !enabled() {
        return None;
    }
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf = std::env::temp_dir().join(format!("zarrin-ir-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("prog.zr");
    std::fs::write(&file, src).unwrap();
    let exe = dir.join("prog");
    let build = Command::new(env!("CARGO_BIN_EXE_zarrinc"))
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("failed to invoke zarrinc");
    assert!(build.status.success(), "native build failed:\n{}", String::from_utf8_lossy(&build.stderr));
    let ir = std::fs::read_to_string(exe.with_extension("ll")).expect("no .ll beside the executable");
    let _ = std::fs::remove_dir_all(&dir);
    Some(ir)
}

#[test]
fn printing_a_float_allocates_nothing() {
    // The formatter wrote into a 4200-byte heap buffer that was never freed and
    // never escaped the call: printing 200k floats reached 937 MB resident,
    // against 7 MB for the interpreter. It formats into frame memory now.
    let Some(ir) = emitted_ir("fn main() { print(1.5); }\n") else { return };
    assert!(ir.contains("%float_buf = alloca"), "the scratch buffer is not on the frame:\n{}", ir);
    assert_eq!(
        ir.matches("call ptr @malloc").count(),
        0,
        "printing a float still allocates:\n{}",
        ir
    );
}

#[test]
fn a_float_string_that_escapes_is_still_allocated() {
    // to_string hands the text to the program, so it cannot live in the frame.
    let Some(ir) = emitted_ir("fn main() { let s = to_string(1.5); print(s); }\n") else { return };
    assert!(ir.contains("call ptr @malloc"), "an escaping float string was not copied out:\n{}", ir);
}

#[test]
fn float_strings_keep_their_values_when_several_are_live() {
    // Each must get its own allocation; sharing one buffer would have the
    // second overwrite the first.
    assert_agrees_with_interpreter(
        r#"
enum S { Pair(float, float), One(float) }
fn main() {
    let a = to_string(2.5);
    let b = to_string(3.5);
    print(a + "/" + b);
    print(Pair(1.25, 6.75));
    print(One(0.5));
    print("{a} and {b}");
    print(to_string(1.0 / 3.0));
}
"#,
    );
}

#[test]
fn a_printed_expression_allocates_nothing() {
    // printf reads the text and returns, so nothing built to feed it can
    // outlive the call. Those buffers go on the frame and are released when
    // the statement ends. Printing an interpolated string in a loop used to
    // leak 80 bytes an iteration, and printing an enum 98.
    for src in [
        r#"fn main() { print("a" + "b"); }"#,
        r#"fn main() { let i = 1; print("n is {i}"); }"#,
        r#"fn main() { print(substring("hello world", 0, 5)); }"#,
        r#"enum E { V(int) } fn main() { print(V(1)); }"#,
    ] {
        let Some(ir) = emitted_ir(&format!("{}\n", src)) else { return };
        assert_eq!(
            ir.matches("call ptr @malloc").count(),
            0,
            "still allocates: {}\n{}",
            src,
            ir
        );
        assert!(ir.contains("llvm.stackrestore"), "frame space is never released: {}", src);
    }
}

#[test]
fn a_value_that_leaves_a_function_is_still_allocated() {
    // The frame belongs to the call that built it, so a returned string has to
    // be on the heap however it is used at the call site.
    let Some(ir) = emitted_ir("fn make(n: int) -> string { return \"v\" + to_string(n); }\nfn main() { print(make(7)); }\n") else { return };
    assert!(ir.contains("call ptr @malloc"), "a returned string was put on the frame:\n{}", ir);
}

#[test]
fn values_built_for_a_print_stay_valid_through_it() {
    assert_agrees_with_interpreter(
        r#"
enum E { V(int) }
struct S { a: string }
macro shout(x) { return "[" + x + "]"; }
fn make(n: int) -> string { return "v" + to_string(n); }
fn main() {
    let s = make(7);
    print(s);
    print(make(8));
    print(s);
    print("outer " + to_string(1) + " " + to_string(2));
    print(S { a: "x" }.a);
    print(V(3));
    let n = 5;
    print(if n > 3 { "big " + to_string(n) } else { "small" });
    print(match n { 5 => "five " + to_string(n), _ => "other" });
    print(shout("hi"));
    let k = shout("kept");
    print(k);
}
"#,
    );
}

// --- reference counting -------------------------------------------------------

#[test]
fn values_bound_in_a_loop_are_reclaimed() {
    // Each of these allocated once an iteration and kept it. A local holds one
    // reference and gives it up when its block ends, so the loop is now flat:
    // a struct went from 7.5 MB over 200k iterations to the 1.4 MB baseline,
    // an array from 13.6 MB, to_string from 14.7 MB.
    for src in [
        "struct P { x: int, y: int }\nfn main() { let i = 0; while i < 3 { let p = P { x: i, y: i }; i = i + 1; } }\n",
        "fn main() { let i = 0; while i < 3 { let s = \"a\" + \"b\"; i = i + 1; } }\n",
        "fn main() { let i = 0; while i < 3 { let a = [1, 2, 3, 4]; i = i + 1; } }\n",
        "enum E { V(int) }\nfn main() { let i = 0; while i < 3 { let e = V(i); i = i + 1; } }\n",
    ] {
        let Some(ir) = emitted_ir(src) else { return };
        assert!(ir.contains("call void @zarrin.release"), "nothing is released:\n{}", src);
        // the slot itself belongs to the frame, not to each iteration
        let body: String = ir.lines().skip_while(|l| !l.starts_with("while_body")).take(30).collect::<Vec<_>>().join("\n");
        assert!(!body.contains("= alloca"), "a slot is allocated per iteration:\n{}", body);
    }
}

#[test]
fn two_names_for_one_value_both_stay_valid() {
    // The second binding takes a reference of its own, so neither release
    // frees a value the other is still holding.
    assert_agrees_with_interpreter(
        r#"
struct P { x: int, y: int }
enum E { V(string), N }
fn keep(s: string) -> string { return s; }
fn build(n: int) -> P { return P { x: n, y: n + 1 }; }
fn main() {
    let a = "one" + "!";
    let b = a;
    print(a); print(b);
    let p = build(3);
    print(p.x); print(p.y);
    let s = keep(a);
    print(s); print(a);
    let e = V("payload" + "!");
    print(e);
    let arr = ["x" + "1", "y" + "2"];
    print(arr[0]); print(arr[1]);
    let i = 0;
    while i < 3 { let inner = "loop" + to_string(i); print(inner); i = i + 1; }
    print(a); print(b); print(s);
}
"#,
    );
}

#[test]
fn a_string_constant_is_never_freed() {
    // Constants are laid out like heap values but with a count that does not
    // move, so holding one in a variable and letting it go is a no-op.
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let i = 0;
    while i < 3 { let s = "constant"; print(s); i = i + 1; }
    let t = "kept";
    let u = t;
    print(t); print(u);
}
"#,
    );
}

#[test]
fn a_value_owned_by_an_aggregate_outlives_its_local() {
    // The struct owns the string, so the string's own binding going out of
    // scope must not free it.
    assert_agrees_with_interpreter(
        r#"
struct Box { s: string, n: int }
fn wrap() -> Box { let inner = "held" + "!"; return Box { s: inner, n: 1 }; }
fn main() { let b = wrap(); print(b.s); print(b.n); print(b.s); }
"#,
    );
}

// --- every example, both backends -------------------------------------------

/// Walk `examples/` and require the native executable to behave exactly like
/// the interpreter: same stdout, same exit status.
///
/// The inline tests above each pin one thing that was wrong. This one needs no
/// maintenance: an example added later is covered the day it lands. Most of the
/// bugs fixed in this backend were found by running a program both ways and
/// looking at the difference, which is what this automates.
#[test]
fn every_example_agrees_across_backends() {
    if !enabled() {
        // Requires `--features llvm` and LLVM_SYS_180_PREFIX; see the README.
        return;
    }
    let examples = common::examples_dir();
    let mut programs: Vec<PathBuf> = std::fs::read_dir(&examples)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", examples.display(), e))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "zr").unwrap_or(false))
        .collect();
    programs.sort();
    assert!(!programs.is_empty(), "no examples found in {}", examples.display());

    let dir = std::env::temp_dir().join(format!("zarrin-cross-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();

    for path in &programs {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();

        let interpreted = Command::new(env!("CARGO_BIN_EXE_zarrinc"))
            .arg("run")
            .arg(path)
            .output()
            .expect("failed to invoke zarrinc");

        let exe = dir.join(&name);
        let build = Command::new(env!("CARGO_BIN_EXE_zarrinc"))
            .args(["build", path.to_str().unwrap(), "-o", exe.to_str().unwrap()])
            .output()
            .expect("failed to invoke zarrinc");
        if !build.status.success() {
            failures.push(format!(
                "{}: native build failed\n    {}",
                name,
                String::from_utf8_lossy(&build.stderr).trim().replace('\n', "\n    ")
            ));
            continue;
        }

        let native = Command::new(&exe).output().expect("failed to run the executable");
        let (want, got) = (
            String::from_utf8_lossy(&interpreted.stdout),
            String::from_utf8_lossy(&native.stdout),
        );
        if want != got {
            failures.push(format!(
                "{}: output differs\n    interpreter: {:?}\n    native:      {:?}",
                name, want, got
            ));
        } else if interpreted.status.code() != native.status.code() {
            failures.push(format!(
                "{}: exit status differs — interpreter {:?}, native {:?}",
                name,
                interpreted.status.code(),
                native.status.code()
            ));
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "\nthe two backends disagree:\n\n{}\n",
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Generics
//
// The native backend erases values to i64 and asks the checker what each one
// is. A generic function has no answer to give — its parameter is `T` — so
// `id("hi")` printed a pointer as a number and `id(2.5)` printed the bits of
// the double. Specialising before codegen means the backend only ever sees
// concrete types.
// ---------------------------------------------------------------------------

#[test]
fn a_generic_function_prints_the_same_from_either_backend() {
    assert_agrees_with_interpreter(
        r#"
fn id<T>(x: T) -> T { return x; }
fn main() {
    print(id(5));
    print(id("hi"));
    print(id(2.5));
    print(id(true));
}
"#,
    );
}

#[test]
fn specialised_copies_do_not_share_a_call_site() {
    assert_agrees_with_interpreter(
        r#"
fn id<T>(x: T) -> T { return x; }
fn twice<T>(x: T) -> T { let a: T = id(x); return id(a); }
fn main() {
    print(twice("deep"));
    print(twice(4));
    print(twice(1.5));
}
"#,
    );
}

#[test]
fn generic_values_are_released_like_any_other() {
    // A specialised copy takes ownership of a heap value the same way a
    // hand-written one does; if it did not, this loop would grow without bound
    // or free something twice. Guard Malloc in `build_and_run` catches the
    // second, and agreement with the interpreter catches the first.
    assert_agrees_with_interpreter(
        r#"
fn id<T>(x: T) -> T { return x; }
fn main() {
    let i = 0;
    let last = "";
    while i < 200 {
        last = id(id("value") + to_string(i));
        i = i + 1;
    }
    print(last);
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Printing a compound value
//
// An array and a struct are bare addresses in the native backend, and `print`
// had nothing to say about either: `print([1, 2, 3])` printed the address as a
// number, and so did `to_string` and string interpolation. Rendering one needs
// its type, since the value itself is only a word.
// ---------------------------------------------------------------------------

#[test]
fn arrays_print_their_elements() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    print([1, 2, 3]);
    print([true, false]);
    print([1.5, 2.0]);
    print(["a", "b"]);
    print([0]);
    print([-1, -22]);
    print([[1, 2], [3, 4]]);
}
"#,
    );
}

#[test]
fn an_array_reaches_to_string_and_interpolation_too() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let xs = [10, 20];
    print(to_string(xs));
    print("xs = {xs}");
    print(to_string(xs) + "!");
}
"#,
    );
}

#[test]
fn structs_print_their_fields_in_declaration_order() {
    assert_agrees_with_interpreter(
        r#"
struct Inner { a: int, b: string }
struct Outer { name: string, inner: Inner }
fn main() {
    let o = Outer { name: "o", inner: Inner { a: 1, b: "two" } };
    print(o);
    print(to_string(o));
    print([o, o]);
}
"#,
    );
}

#[test]
fn a_struct_literal_may_list_its_fields_in_any_order() {
    // Fields were stored in the order the literal wrote them but read back by
    // declaration index, so an out-of-order literal swapped them.
    assert_agrees_with_interpreter(
        r#"
struct P { x: int, y: int }
fn main() {
    let p = P { y: 20, x: 10 };
    print(p.x);
    print(p.y);
    print(p);
}
"#,
    );
}

#[test]
fn printing_an_array_in_a_loop_does_not_accumulate() {
    // The text is built per element, so a loop that never unwinds what it built
    // would grow the stack or the heap by the length of the array each time
    // round.
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let xs = [1.5, 2.5, 3.5];
    let i = 0;
    let last = "";
    while i < 500 {
        last = to_string(xs);
        i = i + 1;
    }
    print(last);
}
"#,
    );
}

// ---------------------------------------------------------------------------
// What an Option or a Result holds
//
// `Some(1.5)` printed `Some(4609434218613702656)` — the payload's bits read as
// an integer. The built-in enums declared their payload as "inferred", so the
// native backend, which erases everything to i64, had nothing to read it back
// as. They now declare a type parameter, and building one fixes it.
// ---------------------------------------------------------------------------

#[test]
fn an_option_payload_prints_as_what_it_is() {
    assert_agrees_with_interpreter(
        r#"
struct P { x: int, y: int }
fn main() {
    print(Some(1.5));
    print(Some("hi"));
    print(Some(true));
    print(Some([1, 2]));
    print(Some(P { x: 1, y: 2 }));
    print(None);
    print([Some(1.5), None]);
}
"#,
    );
}

#[test]
fn a_result_payload_prints_as_what_it_is() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    print(Ok(2.5));
    print(Err("bad"));
    print(to_string(Ok(1.5)));
    print("r = {Ok(3.5)}");
}
"#,
    );
}

#[test]
fn a_payload_type_survives_a_function_boundary() {
    assert_agrees_with_interpreter(
        r#"
fn maybe(n: int) -> Option<float> {
    if n > 0 { return Some(1.5); }
    return None;
}
fn show(o: Option<string>) -> int {
    print(o);
    return 0;
}
fn main() {
    print(maybe(1));
    print(maybe(0));
    show(Some("through a parameter"));
}
"#,
    );
}

#[test]
fn unwrap_and_match_read_the_payload_back_as_its_type() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let o = Some(3.5);
    print(o.unwrap());
    print(match o { Some(v) => v, None => 0.0 });
    let r = Ok(2.5);
    print(r.unwrap());
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Ranges, and what a `for` can walk
//
// A range evaluated to the constant 0, so `print(1..4)` printed `0`; and the
// loop insisted on a range written out in its own header, so a range held in a
// variable, or handed back by a function, was a compile-time refusal.
// ---------------------------------------------------------------------------

#[test]
fn a_range_is_a_value_that_prints() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let r = 1..4;
    print(r);
    print(1..4);
    print(to_string(2..5));
    print("r = {r}");
    print([1..2, 3..9]);
}
"#,
    );
}

#[test]
fn a_loop_walks_a_range_it_was_handed() {
    assert_agrees_with_interpreter(
        r#"
fn span(n: int) -> range { return 0..n; }
fn main() {
    let r = 1..4;
    for i in r { print(i); }
    for j in 0..3 { print(j); }
    for k in 3 { print(k); }
    for m in span(2) { print(m); }
}
"#,
    );
}

#[test]
fn a_loop_walks_an_array() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    for x in [10, 20, 30] { print(x); }
    let xs = ["a", "b"];
    for y in xs { print(y); }
    for f in [1.5, 2.5] { print(f); }
    let total = for i in 0..5 {
        if i == 3 { break i * 10; }
    };
    print(total);
}
"#,
    );
}

#[test]
fn a_loop_lets_go_of_what_it_walked() {
    // The array and the range are built for the loop and belong to it. Left
    // unreleased, this grew by one allocation per turn; released twice, Guard
    // Malloc in `build_and_run` would fault.
    assert_agrees_with_interpreter(
        r#"
fn span(n: int) -> range { return 0..n; }
fn main() {
    let total = 0;
    let i = 0;
    while i < 300 {
        for m in span(2) { total = total + m; }
        for n in [1, 2, 3] { total = total + n; }
        for s in ["a" + to_string(i), "c"] { total = total + len(s); }
        i = i + 1;
    }
    print(total);
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Comparing strings
//
// A string is an address natively, and `==` compared the two addresses — it
// asked whether both sides were the same allocation, which two equal strings
// built separately never are. `x == "hello"` did not even compile, because the
// operand was not an integer. A string pattern in a `match` had the same fault
// and silently fell through to the wildcard.
// ---------------------------------------------------------------------------

#[test]
fn strings_compare_by_their_characters() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let x = "hello";
    print(x == "hello");
    print(x == "world");
    print(x != "world");
    print("a" + "b" == "ab");
    print("" == "");
}
"#,
    );
}

#[test]
fn strings_order_the_way_they_sort() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    print("a" < "b");
    print("b" > "a");
    print("a" <= "a");
    print("a" >= "b");
    print("abc" > "ab");
    for n in ["carol", "alice", "bob"] {
        if n < "c" { print(n); }
    }
}
"#,
    );
}

#[test]
fn a_string_pattern_matches_by_its_characters() {
    assert_agrees_with_interpreter(
        r#"
fn classify(s: string) -> string {
    return match s {
        "a" => "first",
        "b" | "c" => "middle",
        _ => "other",
    };
}
fn main() {
    print(classify("a"));
    print(classify("b"));
    print(classify("c"));
    print(classify("z"));
    print(match "a" + "b" { "ab" => "joined", _ => "no" });
}
"#,
    );
}

#[test]
fn comparing_keeps_neither_side() {
    // Both operands are read and dropped. Left alone, the built-up string grew
    // the heap once per turn; released twice, Guard Malloc would fault.
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let hits = 0;
    let i = 0;
    while i < 500 {
        if "v" + to_string(i) == "v7" { hits = hits + 1; }
        if to_string(i) < "2" { hits = hits + 1; }
        i = i + 1;
    }
    print(hits);
}
"#,
    );
}

#[test]
fn break_means_the_same_thing_on_both_backends() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let i = 0;
    while i < 5 { print(i); i = i + 1; break; }
    print("after while");
    for j in 0..5 {
        if j == 2 { break; }
        print(j);
    }
    print("after for");
    for a in 0..2 {
        for b in 0..3 {
            if b == 1 { break; }
            print(a * 10 + b);
        }
    }
    print("after nested");
    for s in ["a", "b", "c"] {
        if s == "b" { break; }
        print(s);
    }
    let n = 0;
    let w = while n < 100 { n = n + 1; break n * 2; };
    print(w);
    print(n);
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Block scope
//
// The native backend kept one flat table of locals, so a `let` inside a block
// took over the outer variable of the same name for good: `x` was still 2
// after the `if` that shadowed it. `if` bodies had no block at all, so a
// string declared in one was never released either, and a `match` arm left
// what its pattern bound in scope for the rest of the function.
// ---------------------------------------------------------------------------

#[test]
fn a_let_inside_a_block_is_the_blocks_own() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let x = 1;
    if true {
        let x = 2;
        if true {
            let x = 3;
            print(x);
        }
        print(x);
    }
    print(x);
    let s = "outer";
    while true {
        let s = "inner";
        print(s);
        break;
    }
    print(s);
    for i in 0..2 {
        let s = "loop";
        print(s);
    }
    print(s);
}
"#,
    );
}

#[test]
fn an_else_branch_is_a_block_too() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let tag = "outer";
    if 1 > 2 {
        let tag = "then";
        print(tag);
    } else {
        let tag = "else";
        print(tag);
    }
    print(tag);
}
"#,
    );
}

#[test]
fn a_match_arm_binding_stays_in_its_arm() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let v = "outer";
    print(match Some(1) { Some(v) => "bound", None => "no" });
    print(v);
    let n = 7;
    print(match Some(2) { Some(n) => n, None => 0 });
    print(n);
}
"#,
    );
}

#[test]
fn a_string_declared_inside_an_if_is_released() {
    // No block meant no release: this grew the heap by one string per turn.
    assert_agrees_with_interpreter(
        r#"
fn main() {
    let n = 0;
    while n < 500 {
        if n % 2 == 0 {
            let s = "x" + to_string(n);
            n = n + len(s);
        } else {
            n = n + 1;
        }
    }
    print(n);
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Which type's method
//
// Methods were registered by their bare name, so a second `impl` of the same
// trait shared the first one's function: `A.show()` and `B.show()` both
// answered "A", with no error anywhere.
// ---------------------------------------------------------------------------

#[test]
fn each_type_gets_its_own_method() {
    assert_agrees_with_interpreter(
        r#"
struct A { n: int }
struct B { n: int }
trait Show { fn show(self) -> string; }
impl Show for A { fn show(self) -> string { return "A"; } }
impl Show for B { fn show(self) -> string { return "B"; } }
fn main() {
    print(A { n: 1 }.show());
    print(B { n: 2 }.show());
}
"#,
    );
}

#[test]
fn two_methods_of_one_name_may_return_different_types() {
    assert_agrees_with_interpreter(
        r#"
struct P { n: int }
struct Q { n: float }
impl P { fn get(self) -> int { return self.n; } }
impl Q { fn get(self) -> float { return self.n; } }
fn main() {
    print(P { n: 3 }.get());
    print(Q { n: 1.5 }.get());
}
"#,
    );
}

#[test]
fn an_inherent_impl_still_dispatches() {
    assert_agrees_with_interpreter(
        r#"
struct C { n: int }
impl C {
    fn double(self) -> int { return self.n * 2; }
    fn plus(self, k: int) -> int { return self.n + k; }
}
fn main() {
    let c = C { n: 5 };
    print(c.double());
    print(c.plus(3));
    print(c.double() + c.plus(1));
}
"#,
    );
}

// ---------------------------------------------------------------------------
// The header on a frame value
//
// A value built inside a consumed expression goes on the frame, and had no
// reference-count header. A function that retains what it is given — anything
// that returns its own parameter — then read and wrote a count 16 bytes before
// an `alloca`, which is the caller's stack. Sometimes nothing came of it;
// `print(id([1, 2])); print(id(Some(1.5)));` faulted with SIGBUS.
// ---------------------------------------------------------------------------

#[test]
fn a_frame_value_survives_a_function_that_retains_it() {
    assert_agrees_with_interpreter(
        r#"
fn id<T>(x: T) -> T { return x; }
fn main() {
    print(id([1, 2]));
    print(id(Some(1.5)));
    print(id("s"));
    print(id(1..3));
}
"#,
    );
}

#[test]
fn a_returned_parameter_is_the_same_value_going_in() {
    assert_agrees_with_interpreter(
        r#"
struct P { x: int, y: int }
fn keep(s: string) -> string { return s; }
fn hold(o: Option<float>) -> Option<float> { return o; }
fn same(p: P) -> P { return p; }
fn main() {
    print(keep("a" + "b"));
    print(hold(Some(2.5)));
    print(same(P { x: 1, y: 2 }));
    print(keep(keep(keep("through three"))));
}
"#,
    );
}

// ---------------------------------------------------------------------------
// unwrap on a variant with no payload
//
// The native backend read the payload slot without looking at the tag, so
// `None.unwrap()` handed back whatever word sat there — 0 — and the program
// carried on with a value that was never there.
// ---------------------------------------------------------------------------

#[test]
fn unwrap_on_none_stops_the_program() {
    assert_fails_like_the_interpreter(
        r#"
fn main() {
    print("before");
    let o = None;
    print(o.unwrap());
}
"#,
    );
}

#[test]
fn unwrap_on_err_says_so() {
    assert_fails_like_the_interpreter(
        r#"
fn f(n: int) -> Result<int, string> {
    if n > 0 { return Ok(n); }
    return Err("bad");
}
fn main() {
    print("before");
    print(f(0).unwrap());
}
"#,
    );
}

#[test]
fn unwrap_on_a_value_still_hands_it_over() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    print(Some(5).unwrap());
    print(Ok(2.5).unwrap());
    print(Some("s").unwrap());
    let o = Some(1.5);
    if o.is_some() { print(o.unwrap()); }
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Dividing by zero
//
// Undefined in LLVM: the native backend printed a number with no meaning and
// carried on, where the interpreter stopped.
// ---------------------------------------------------------------------------

#[test]
fn dividing_by_zero_stops_the_program() {
    assert_fails_like_the_interpreter(
        r#"
fn main() {
    print("before");
    let n = 0;
    print(7 / n);
}
"#,
    );
}

#[test]
fn remainder_by_zero_stops_the_program() {
    assert_fails_like_the_interpreter(
        r#"
fn main() {
    print("before");
    let n = 0;
    print(7 % n);
}
"#,
    );
}

#[test]
fn division_that_can_be_done_agrees() {
    assert_agrees_with_interpreter(
        r#"
fn main() {
    print(7 / 2);
    print(0 - 7 / 2);
    print(7 % 3);
    print(0 - 7 % 3);
    print(1.0 / 0.0);
    print(0.0 / 0.0);
    let d = 3;
    print(100 / d);
}
"#,
    );
}
