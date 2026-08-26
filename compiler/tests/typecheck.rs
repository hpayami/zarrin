//! The type checker gates `run` and `build`, and knows everything the
//! backends can actually execute.

mod common;

/// Mirror of `builtins::NAMES`. Integration tests cannot import the binary
/// crate, so the list is duplicated here and kept honest by the assertion
/// below: adding a builtin without a test call fails this test.
const BUILTIN_NAMES: &[&str] = &[
    "print", "panic", "len", "to_string", "int_to_str", "substring", "contains",
    "split", "trim", "char_at", "array_len", "array_get", "array_set",
];

use common::{assert_check_error, assert_checks, assert_output, zarrinc};

/// Every builtin the backends implement must be known to the checker, or
/// `check` rejects programs that `run` executes happily.
#[test]
fn every_builtin_is_known_to_the_checker() {
    // one well-typed call per builtin
    let calls = [
        ("print", r#"print(1);"#),
        ("panic", r#"if false { panic("x"); }"#),
        ("len", r#"let a = len("hi");"#),
        ("to_string", r#"let a = to_string(1);"#),
        ("int_to_str", r#"let a = int_to_str(1);"#),
        ("substring", r#"let a = substring("hello", 1, 3);"#),
        ("contains", r#"let a = contains("hello", "ell");"#),
        ("split", r#"let a = split("a,b", ",");"#),
        ("trim", r#"let a = trim("  x  ");"#),
        ("char_at", r#"let a = char_at("abc", 1);"#),
        ("array_len", r#"let a = array_len([1, 2]);"#),
        ("array_get", r#"let a = array_get([1, 2], 0);"#),
        ("array_set", r#"let a = array_set([1, 2], 0, 9);"#),
    ];
    let covered: Vec<&str> = calls.iter().map(|(n, _)| *n).collect();
    for name in BUILTIN_NAMES {
        assert!(covered.contains(name), "builtin `{}` has no test call", name);
    }
    for (name, call) in calls {
        let src = format!("fn main() {{ {} }}", call);
        let r = zarrinc("check", &src);
        assert!(r.success, "builtin `{}` rejected by the checker:\n{}", name, r.stderr);
    }
}

#[test]
fn builtin_arity_is_enforced() {
    assert_check_error(r#"fn main() { let a = substring("abc", 1); }"#, "expects 3 args");
    assert_check_error(r#"fn main() { print(1, 2); }"#, "expects 1 args");
}

#[test]
fn option_and_result_methods_are_known() {
    assert_checks(r#"fn main() { let a = Some(5); let b = a.unwrap(); let c = a.is_some(); }"#);
    assert_checks(r#"fn main() { let a = Ok(5); let b = a.is_ok(); let c = a.is_err(); }"#);
}

#[test]
fn string_interpolation_type_checks() {
    // Desugars to a `to_string` call, which the checker did not know about.
    assert_checks(r#"fn main() { let n = 5; print("n is {n}"); }"#);
}

#[test]
fn inherent_impl_is_accepted() {
    // `impl P { .. }` has an empty trait name; the checker read that as a
    // missing trait and reported "undefined trait: ``".
    assert_checks(
        r#"
struct P { x: int, y: int }
impl P { fn sum(self) -> int { return self.x + self.y; } }
fn main() { let p = P { x: 1, y: 2 }; print(p.sum()); }
"#,
    );
}

#[test]
fn a_missing_trait_method_is_still_reported() {
    assert_check_error(
        r#"
struct P { x: int }
trait Show { fn show(self) -> string; }
impl Show for P { }
fn main() { }
"#,
        "missing method",
    );
}

#[test]
fn an_undefined_trait_is_still_reported() {
    assert_check_error(
        r#"
struct P { x: int }
impl Nope for P { fn f(self) -> int { return 1; } }
fn main() { }
"#,
        "undefined trait",
    );
}

#[test]
fn macro_calls_type_check() {
    assert_checks(r#"macro double(x) { return x + x; } fn main() { print(double(21)); }"#);
    assert_check_error(
        r#"macro double(x) { return x + x; } fn main() { print(double(1, 2)); }"#,
        "expects 1 args",
    );
}

// --- the checker now gates execution -------------------------------------

#[test]
fn run_rejects_a_program_that_does_not_type_check() {
    let r = zarrinc("run", r#"fn f(x: int) -> int { return x; } fn main() { f("nope"); }"#);
    assert!(!r.success, "run executed an ill-typed program:\n{}", r.stdout);
    assert!(r.stderr.contains("type mismatch"), "not a type error:\n{}", r.stderr);
}

#[test]
fn run_reports_the_error_before_executing_anything() {
    // The print must not happen: the checker runs first.
    let r = zarrinc("run", r#"fn main() { print("side effect"); let x = nothing; }"#);
    assert!(!r.success);
    assert!(
        !r.stdout.contains("side effect"),
        "program ran before failing the check:\n{}",
        r.stdout
    );
}

#[test]
fn checker_block_scoping_matches_the_interpreter() {
    // These used to pass `check` and only fail once running.
    assert_check_error(r#"fn main() { if true { let inner = 5; } print(inner); }"#, "undefined variable");
    assert_check_error(r#"fn main() { while false { let w = 1; } print(w); }"#, "undefined variable");
    assert_check_error(r#"fn main() { for k in 0..3 { } print(k); }"#, "undefined variable");
    assert_check_error(
        r#"enum E { A(int) } fn main() { let e = A(1); let r = match e { A(n) => n }; print(n); }"#,
        "undefined variable",
    );
}

#[test]
fn well_typed_programs_still_run() {
    assert_output(
        r#"
struct P { x: int, y: int }
impl P { fn sum(self) -> int { return self.x + self.y; } }
enum Shape { Circle(int) }
macro twice(n) { return n * 2; }
fn main() {
    let p = P { x: 3, y: 4 };
    print(p.sum());
    print(twice(5));
    print(len("hello"));
    let s = Circle(2);
    print(match s { Circle(r) => r * r });
    let o = Some(9);
    print(o.unwrap());
    print("done {p.x}");
}
"#,
        &["7", "10", "5", "4", "9", "done 3"],
    );
}
