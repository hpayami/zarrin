//! One test per bug that has been found and fixed. Each test states the
//! behaviour that was wrong, so a future change that reintroduces it fails
//! loudly instead of silently.

mod common;

use common::{assert_check_error, assert_checks, assert_output, assert_run_fails, run};

// ---------------------------------------------------------------------------
// Operator precedence
//
// `&&` and `||` used to bind tighter than everything else and comparisons
// loosest, so `a == 1 && b == 2` parsed as `a == (1 && b) == 2` and crashed.
// ---------------------------------------------------------------------------

#[test]
fn comparison_binds_tighter_than_logical_and() {
    assert_output(
        r#"fn main() { let a = 1; let b = 2; print(a == 1 && b == 2); }"#,
        &["true"],
    );
}

#[test]
fn or_binds_looser_than_and() {
    // If `||` bound tighter these would be false and false.
    assert_output(
        r#"fn main() { print(true || false && false); print(false && true || true); }"#,
        &["true", "true"],
    );
}

#[test]
fn arithmetic_binds_tighter_than_comparison() {
    assert_output(
        r#"fn main() { print(2 + 3 * 4 == 14); print(10 % 3 == 1); print(1 + 2 == 3); }"#,
        &["true", "true", "true"],
    );
}

#[test]
fn mixed_precedence_tree_shape() {
    // Exact nesting, not just "an And exists somewhere": `a == 1 && b == 2 || c`
    // must be Or(And(Eq(a,1), Eq(b,2)), c). Before the fix it parsed as
    // Eq(Eq(a, And(1,b)), Or(2,c)).
    let r = common::zarrinc("emit-ast", "let z = a == 1 && b == 2 || c;");
    let flat: String = r.stdout.chars().filter(|c| !c.is_whitespace()).collect();
    let expected = "Binary(Binary(Binary(Ident(\"a\",),Eq,Int(1,),),And,\
Binary(Ident(\"b\",),Eq,Int(2,),),),Or,Ident(\"c\",),)";
    assert!(
        flat.contains(expected),
        "precedence tree changed.\n  expected to contain: {}\n  got: {}",
        expected,
        flat
    );
}

// ---------------------------------------------------------------------------
// Scoping
//
// `Env::parent` was always an empty node, so the scope chain was never linked:
// top-level `let` bindings were invisible inside `main`, and block bodies
// leaked their locals into the enclosing scope.
// ---------------------------------------------------------------------------

#[test]
fn top_level_lets_are_visible_everywhere() {
    assert_output(
        r#"
let g = 42;
fn show() { print(g); }
fn main() { print(g); show(); }
"#,
        &["42", "42"],
    );
}

#[test]
fn functions_can_mutate_a_global() {
    assert_output(
        r#"
let counter = 0;
fn bump(x: int) -> int { counter = counter + x; return counter; }
fn main() { print(bump(5)); print(bump(3)); print(counter); }
"#,
        &["5", "8", "8"],
    );
}

#[test]
fn a_callee_cannot_see_its_callers_locals() {
    // Dynamic scoping would make this print 99.
    assert_run_fails(
        r#"
fn helper() { print(secret); }
fn main() { let secret = 99; helper(); }
"#,
        "undefined variable",
    );
}

#[test]
fn assignment_updates_the_outer_binding_not_a_shadow() {
    // If a block-scoped assignment shadowed instead of updating, the loop
    // would never terminate and `total` would stay 0.
    assert_output(
        r#"
fn main() {
    let i = 0;
    while i < 3 { i = i + 1; }
    print(i);
    let total = 0;
    for k in 0..4 { total = total + k; }
    print(total);
}
"#,
        &["3", "6"],
    );
}

#[test]
fn block_locals_do_not_leak() {
    assert_run_fails(
        r#"fn main() { if true { let inner = 5; } print(inner); }"#,
        "undefined variable",
    );
}

#[test]
fn loop_variable_does_not_leak() {
    assert_run_fails(
        r#"fn main() { for k in 0..3 { print(k); } print(k); }"#,
        "undefined variable",
    );
}

#[test]
fn match_bindings_are_scoped_to_their_arm() {
    // The first arm binds `n`, its guard fails; `n` must not escape.
    assert_output(
        r#"
enum E { A(int), B(int) }
fn main() {
    let e = A(5);
    let r = match e { A(n) if n > 100 => 1, A(n) => n + 1, _ => 0 };
    print(r);
}
"#,
        &["6"],
    );
}

#[test]
fn recursion_gets_distinct_frames() {
    assert_output(
        r#"
fn fact(n: int) -> int {
    if n <= 1 { return 1; }
    return n * fact(n - 1);
}
fn main() { print(fact(10)); }
"#,
        &["3628800"],
    );
}

#[test]
fn method_self_is_scoped_to_the_method() {
    assert_run_fails(
        r#"
struct P { x: int, y: int }
impl P { fn sum(self) -> int { return self.x + self.y; } }
fn main() { let p = P { x: 3, y: 4 }; print(p.sum()); print(self); }
"#,
        "undefined variable",
    );
}

// ---------------------------------------------------------------------------
// `continue`
//
// `Stmt::If` never propagated the continue flag, so the rest of the if-body
// still ran; and in the expression-form loops the handler used a Rust
// `continue`, which advanced to the next statement instead of ending the body.
// ---------------------------------------------------------------------------

#[test]
fn continue_inside_an_if_skips_the_rest_of_the_body() {
    assert_output(
        r#"
fn main() {
    let i = 0;
    while i < 4 {
        i = i + 1;
        if i == 2 { continue; print("LEAKED"); }
        print(i);
    }
}
"#,
        &["1", "3", "4"],
    );
}

#[test]
fn continue_propagates_through_nested_ifs() {
    assert_output(
        r#"
fn main() {
    let i = 0;
    while i < 5 {
        i = i + 1;
        if i == 2 { if i > 1 { continue; } print("LEAKED"); }
        print(i);
    }
}
"#,
        &["1", "3", "4", "5"],
    );
}

#[test]
fn continue_works_in_a_while_expression() {
    assert_output(
        r#"
fn main() {
    let i = 0;
    let r = while i < 4 { i = i + 1; continue; print("LEAKED"); };
    print(i);
}
"#,
        &["4"],
    );
}

#[test]
fn continue_works_in_a_for_expression() {
    assert_output(
        r#"
fn main() {
    let r = for k in 0..3 { continue; print("LEAKED"); };
    print("done");
}
"#,
        &["done"],
    );
}

#[test]
fn break_and_return_still_escape_from_inside_an_if() {
    assert_output(
        r#"
fn f(n: int) -> int {
    let i = 0;
    while i < 10 { i = i + 1; if i == n { return i * 100; } }
    return 0;
}
fn main() {
    let i = 0;
    while i < 10 { i = i + 1; if i > 3 { break; } print(i); }
    print(f(3));
    print(f(99));
}
"#,
        &["1", "2", "3", "300", "0"],
    );
}

#[test]
fn break_and_continue_bind_to_the_innermost_loop() {
    assert_output(
        r#"
fn main() {
    for a in 0..3 {
        for b in 0..3 {
            if b == 1 { continue; }
            if b == 2 { break; }
            print(a * 10 + b);
        }
    }
}
"#,
        &["0", "10", "20"],
    );
}

// ---------------------------------------------------------------------------
// Enum variant resolution
//
// "Which enum declares `Foo`?" was answered by scanning a HashMap, so the
// answer varied between runs of the same program.
// ---------------------------------------------------------------------------

const AMBIGUOUS: &str = r#"
enum A { Foo(int) }
enum B { Foo(int) }
fn takesA(x: A) -> int { return 1; }
fn main() { let v = takesA(Foo(1)); print(v); }
"#;

#[test]
fn ambiguous_variant_is_reported_not_guessed() {
    assert_check_error(AMBIGUOUS, "declared by");
}

#[test]
fn variant_resolution_is_deterministic() {
    // The whole point: repeated runs of an identical program must agree.
    // This used to alternate between a type error and a clean pass.
    // Compare the message, not the whole diagnostic: each run writes the
    // program to its own scratch file, so the quoted path differs.
    let message = |src: &str| {
        common::zarrinc("check", src)
            .stderr
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let first = message(AMBIGUOUS);
    assert!(first.contains("declared by"), "unexpected first run: {}", first);
    for i in 0..20 {
        assert_eq!(first, message(AMBIGUOUS), "run {} disagreed with the first run", i);
    }
}

#[test]
fn qualified_patterns_match() {
    // `C::R` used to be compared against the bare variant name "R", never
    // matched, and fell through to the wildcard.
    assert_output(
        r#"
enum C { R, G }
fn main() {
    let c = R;
    print(match c { C::R => 1, C::G => 2, _ => 99 });
    let d = G;
    print(match d { C::R => 1, C::G => 2, _ => 99 });
}
"#,
        &["1", "2"],
    );
}

#[test]
fn builtin_option_payload_accepts_any_type() {
    // The built-in payload is declared `Inferred`; a strict equality check
    // rejected `Some(5)` with "expected Inferred, found Int".
    assert_output(r#"fn main() { let a = Some(5); print(a.is_some()); }"#, &["true"]);
    let r = common::zarrinc("check", r#"fn main() { let a = Some(5); }"#);
    assert!(r.success, "Some(5) should type-check:\n{}", r.stderr);
}

// ---------------------------------------------------------------------------
// Baseline language behaviour, so the suite covers more than past bugs.
// ---------------------------------------------------------------------------

#[test]
fn arithmetic_and_strings() {
    assert_output(
        r#"
fn main() {
    print(7 / 2);
    print(-3 + 1);
    print("a" + "b");
    print(len("hello"));
    print(trim("  hi  "));
}
"#,
        &["3", "-2", "ab", "5", "hi"],
    );
}

#[test]
fn string_interpolation() {
    assert_output(r#"fn main() { let n = 5; print("n is {n}"); }"#, &["n is 5"]);
}

#[test]
fn structs_and_methods() {
    assert_output(
        r#"
struct P { x: int, y: int }
impl P { fn sum(self) -> int { return self.x + self.y; } }
fn main() { let p = P { x: 3, y: 4 }; print(p.x); print(p.sum()); }
"#,
        &["3", "7"],
    );
}

#[test]
fn enums_and_match_with_payloads() {
    assert_output(
        r#"
enum Shape { Circle(int), Rect(int, int) }
fn area(s: Shape) -> int {
    return match s { Circle(r) => 3 * r * r, Rect(w, h) => w * h };
}
fn main() { print(area(Circle(2))); print(area(Rect(3, 4))); }
"#,
        &["12", "12"],
    );
}

#[test]
fn arrays_and_indexing() {
    assert_output(
        r#"fn main() { let a = [10, 20, 30]; print(a[1]); print(len(a)); }"#,
        &["20", "3"],
    );
}

#[test]
fn macros_expand() {
    assert_output(
        r#"
macro double(x) { return x + x; }
fn main() { print(double(21)); }
"#,
        &["42"],
    );
}

#[test]
fn multi_pattern_arms_and_guards() {
    assert_output(
        r#"
fn classify(n: int) -> string {
    return match n {
        0 | 1 => "small",
        x if x > 100 => "huge",
        _ => "middling",
    };
}
fn main() { print(classify(0)); print(classify(1)); print(classify(500)); print(classify(7)); }
"#,
        &["small", "small", "huge", "middling"],
    );
}

#[test]
fn type_checker_accepts_the_simple_examples() {
    assert_checks(r#"fn square(x: int) -> int { return x * x; } fn main() { print(square(3)); }"#);
}

#[test]
fn type_checker_rejects_a_real_mismatch() {
    assert_check_error(
        r#"fn f(x: int) -> int { return x; } fn main() { let a = f("nope"); }"#,
        "type mismatch",
    );
}

#[test]
fn unknown_function_is_reported() {
    assert_run_fails(r#"fn main() { nope(1); }"#, "undefined function");
}

// ---------------------------------------------------------------------------
// Trailing expressions
//
// An expression statement without `;` becomes the block's value. That was
// accepted in any position, so a forgotten semicolon silently turned into an
// early return and the rest of the function never ran.
// ---------------------------------------------------------------------------

#[test]
fn a_forgotten_semicolon_is_an_error_not_an_early_return() {
    let r = run("fn main() {\n    print(\"a\")\n    print(\"b\");\n    print(\"c\");\n}\n");
    assert!(!r.success, "accepted a missing semicolon; printed:\n{}", r.stdout);
    assert!(r.stderr.contains("expected `;`"), "wrong diagnostic:\n{}", r.stderr);
    // the giveaway symptom: it used to print only "a"
    assert!(!r.stdout.contains('a'), "program ran before failing:\n{}", r.stdout);
}

#[test]
fn a_trailing_expression_is_still_the_value_of_its_block() {
    assert_output("fn double(x: int) -> int {\n    x * 2\n}\nfn main() { print(double(21)); }\n", &["42"]);
    assert_output("fn main() {\n    let r = if true { 100 } else { 0 };\n    print(r);\n}\n", &["100"]);
    assert_output("fn main() {\n    let v = match 2 { 1 => 10, _ => 20 };\n    print(v);\n}\n", &["20"]);
}

#[test]
fn a_trailing_expression_at_top_level_is_allowed() {
    // The rule is "last thing in its block"; at top level that means EOF.
    let r = run("fn f() -> int { return 7; }\nprint(f())\n");
    assert!(r.success, "rejected a top-level trailing expression:\n{}", r.stderr);
    assert_eq!(r.lines().as_slice(), &["7"]);
}

// ---------------------------------------------------------------------------
// Escape sequences
//
// The lexer had none: "a\nb" was the four characters a, backslash, n, b, and a
// string could not contain a quote at all — the backslash was ordinary and the
// quote closed the literal.
// ---------------------------------------------------------------------------

#[test]
fn escapes_produce_real_characters() {
    assert_output("fn main() { print(\"a\\nb\"); }\n", &["a", "b"]);
    assert_output("fn main() { print(len(\"a\\nb\")); }\n", &["3"]);
    assert_output("fn main() { print(\"x\\ty\"); }\n", &["x\ty"]);
    assert_output("fn main() { print(\"q\\\"d\"); }\n", &["q\"d"]);
    assert_output("fn main() { print(\"b\\\\s\"); }\n", &["b\\s"]);
}

#[test]
fn braces_can_be_escaped_in_an_interpolated_string() {
    // An unescaped brace still starts an interpolation, so a literal one needs
    // escaping — and the escape has to survive into the interpolation scan.
    assert_output("fn main() { let n = 5; print(\"\\{ {n} \\}\"); }\n", &["{ 5 }"]);
    assert_output("fn main() { print(\"\\{plain\\}\"); }\n", &["{plain}"]);
}

#[test]
fn a_carriage_return_escape_works() {
    assert_output("fn main() { print(len(\"a\\rb\")); }\n", &["3"]);
}
