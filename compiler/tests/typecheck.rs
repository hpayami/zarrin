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

// --- match exhaustiveness ----------------------------------------------------
//
// An incomplete match used to compile and fail at run time — or, in the native
// backend before it was fixed, quietly run the last arm for a value nothing
// matched. The checker rejects it now.

#[test]
fn a_match_missing_variants_names_them() {
    assert_check_error(
        "enum C { R, G, B }\nfn f(c: C) -> int { return match c { R => 1 }; }\nfn main() { print(f(R)); }\n",
        "does not cover `G`, `B`",
    );
}

#[test]
fn covering_every_variant_needs_no_wildcard() {
    assert_checks("enum C { R, G, B }\nfn f(c: C) -> int { return match c { R => 1, G => 2, B => 3 }; }\nfn main() { print(f(R)); }\n");
    assert_checks("fn f(o: Option) -> int { return match o { Some(v) => v, None => 0 }; }\nfn main() { print(f(None)); }\n");
}

#[test]
fn a_match_on_a_scalar_needs_a_catch_all() {
    assert_check_error(
        "fn f(n: int) -> int { return match n { 0 => 10, 1 => 20 }; }\nfn main() { print(f(0)); }\n",
        "add a `_` arm",
    );
    assert_checks("fn f(n: int) -> int { return match n { 0 => 10, _ => 20 }; }\nfn main() { print(f(0)); }\n");
}

#[test]
fn a_guarded_arm_covers_nothing() {
    // The guard can turn the arm down, so it cannot be what makes a match
    // complete — f(3) would have matched no arm at all.
    assert_check_error(
        "fn f(n: int) -> int { return match n { 0 => 7, x if x > 10 => 1 }; }\nfn main() { print(f(0)); }\n",
        "add a `_` arm",
    );
    assert_checks("fn f(n: int) -> int { return match n { 0 => 7, x if x > 10 => 1, _ => 0 }; }\nfn main() { print(f(0)); }\n");
}

#[test]
fn a_variant_is_only_covered_when_its_payload_is_bound() {
    // `C(0)` matches one value of C, not all of it.
    assert_check_error(
        "enum S { C(int), R(int, int) }\nfn f(s: S) -> int { return match s { C(0) => 1, R(w, h) => w * h }; }\nfn main() { print(f(C(1))); }\n",
        "does not cover `C`",
    );
    assert_checks(
        "enum S { C(int), R(int, int) }\nfn f(s: S) -> int { return match s { C(r) => r, R(w, h) => w * h }; }\nfn main() { print(f(C(1))); }\n",
    );
}

#[test]
fn both_booleans_or_a_catch_all() {
    assert_checks("fn f(b: bool) -> int { return match b { true => 1, false => 0 }; }\nfn main() { print(f(true)); }\n");
    assert_check_error(
        "fn f(b: bool) -> int { return match b { true => 1 }; }\nfn main() { print(f(true)); }\n",
        "does not cover `false`",
    );
}

#[test]
fn an_unknown_scrutinee_type_is_left_alone() {
    // `array_get` has no element type to reason about, so nothing can be
    // proven and nothing is claimed.
    assert_checks("fn main() { let a = [1, 2]; let v = match array_get(a, 0) { 5 => 10, 6 => 20 }; }\n");
}

// ---------------------------------------------------------------------------
// What an Option or a Result holds
// ---------------------------------------------------------------------------

#[test]
fn an_option_remembers_its_payload_type() {
    assert_check_error(
        "fn take(o: Option<int>) -> int { return 0; }\n\
         fn main() { print(take(Some(\"s\"))); }\n",
        "expected `Option<int>`, found `Option<string>`",
    );
}

#[test]
fn a_bare_option_still_accepts_any_payload() {
    // Signatures written before payload types existed have to keep working.
    assert_checks(
        "fn take(o: Option) -> int { return 0; }\n\
         fn main() { print(take(Some(1.5))); print(take(Some(\"s\"))); print(take(None)); }\n",
    );
}

#[test]
fn a_result_carries_two_payload_types() {
    assert_checks(
        "fn f(n: int) -> Result<int, string> {\n\
         \x20   if n > 0 { return Ok(n); }\n\
         \x20   return Err(\"negative\");\n}\n\
         fn main() { print(f(1)); }\n",
    );
    assert_check_error(
        "fn f(n: int) -> Result<int, string> { return Ok(\"wrong\"); }\n\
         fn main() { print(f(1)); }\n",
        "Result<int, string>",
    );
}

#[test]
fn unwrap_has_the_payload_type() {
    assert_check_error(
        "fn main() { let n: int = Some(1.5).unwrap(); print(n); }\n",
        "expected `int`, found `float`",
    );
}

#[test]
fn a_type_error_names_types_the_way_they_are_written() {
    assert_check_error("fn main() { let x: int = \"s\"; }\n", "expected `int`, found `string`");
    assert_check_error("fn main() { let x: int = [1]; }\n", "expected `int`, found `[int]`");
}

// ---------------------------------------------------------------------------
// Ranges
// ---------------------------------------------------------------------------

#[test]
fn a_range_is_not_an_integer() {
    // `1..4` was typed as `int`, so arithmetic on one type-checked and then did
    // whatever each backend happened to do with it.
    assert_check_error("fn main() { let n: int = 1..4; print(n); }\n", "expected `int`, found `range`");
    assert_checks("fn span(n: int) -> range {\n    return 0..n;\n}\nfn main() { print(span(2)); }\n");
}

#[test]
fn a_for_loop_says_what_it_can_walk() {
    assert_check_error(
        "fn main() { for x in \"nope\" { print(x); } }\n",
        "expected `range, array or int`, found `string`",
    );
    assert_checks("fn main() { for x in 0..3 { print(x); } for y in [1, 2] { print(y); } for z in 3 { print(z); } }\n");
}

#[test]
fn a_loop_variable_has_the_element_type() {
    assert_check_error(
        "fn main() { for s in [\"a\"] { let n: int = s; print(n); } }\n",
        "expected `int`, found `string`",
    );
}

#[test]
fn an_extern_function_cannot_be_called_yet() {
    // Neither backend implements one, and they used to say so at different
    // moments: the interpreter when the call was reached, the native compiler
    // when the program was built.
    assert_check_error(
        "extern fn abs(n: int) -> int;\nfn main() { print(abs(0 - 5)); }\n",
        "declared `extern`",
    );
    // declaring one is still fine
    assert_checks("extern fn abs(n: int) -> int;\nfn main() { print(1); }\n");
}
