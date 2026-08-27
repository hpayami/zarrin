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
    // Eq(Eq(a, And(1,b)), Or(2,c)). Spans are elided so the shape is readable.
    let r = common::zarrinc("emit-ast", "let z = a == 1 && b == 2 || c;");
    let flat: String = r.stdout.chars().filter(|c| !c.is_whitespace()).collect();
    let shape = regex_lite_strip(&flat);
    let expected = "Binary(Binary(Binary(Ident(\"a\",),Eq,Int(1,),),And,\
Binary(Ident(\"b\",),Eq,Int(2,),),),Or,Ident(\"c\",),)";
    assert!(
        shape.contains(expected),
        "precedence tree changed.\n  expected to contain: {}\n  got: {}",
        expected,
        shape
    );
}

/// Drop the `Expr { kind: .., span: .. }` wrappers so a test can talk about the
/// shape of a tree without restating every position in it.
fn regex_lite_strip(s: &str) -> String {
    let mut out = s.replace("Expr{kind:", "");
    loop {
        let Some(i) = out.find(",span:Span{") else { break };
        let Some(rel) = out[i..].find("},}") else { break };
        out.replace_range(i..i + rel + 3, "");
    }
    out
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

// ---------------------------------------------------------------------------
// Struct field order
//
// The interpreter held a struct's fields in a `HashMap`, so printing one gave
// whatever order that run's hash seed produced — the same program printed
// `P { x: 1, y: 2 }` and `P { y: 2, x: 1 }` on consecutive runs.
// ---------------------------------------------------------------------------

#[test]
fn a_struct_prints_its_fields_in_declaration_order() {
    let src = "struct P { x: int, y: int, z: int }\n\
               fn main() { print(P { z: 3, x: 1, y: 2 }); }\n";
    assert_output(src, &["P { x: 1, y: 2, z: 3 }"]);
}

#[test]
fn the_field_order_is_the_same_every_run() {
    let src = "struct Wide { alpha: int, beta: int, gamma: int, delta: int }\n\
               fn main() { print(Wide { alpha: 1, beta: 2, gamma: 3, delta: 4 }); }\n";
    let want = &["Wide { alpha: 1, beta: 2, gamma: 3, delta: 4 }"];
    for _ in 0..8 {
        assert_output(src, want);
    }
}

// ---------------------------------------------------------------------------
// Ordering strings
//
// `<` on two strings was a run-time failure — "unsupported string op" — which
// is a surprising answer to `"a" < "b"`.
// ---------------------------------------------------------------------------

#[test]
fn strings_can_be_ordered() {
    assert_output(
        "fn main() { print(\"a\" < \"b\"); print(\"b\" <= \"a\"); print(\"abc\" > \"ab\"); }\n",
        &["true", "false", "true"],
    );
}

// ---------------------------------------------------------------------------
// break
//
// The interpreter cleared `should_break` in the loop that walks a body's
// statements and then asked, outside it, whether a break had happened — by
// which point the flag was false. So `break` skipped the rest of the body and
// carried on: it meant `continue`. Where the loop variable was updated after
// the `break`, the program ran forever.
// ---------------------------------------------------------------------------

#[test]
fn break_ends_a_while_loop() {
    assert_output(
        "fn main() {\n\
         \x20   let i = 0;\n\
         \x20   while i < 5 {\n\
         \x20       print(i);\n\
         \x20       i = i + 1;\n\
         \x20       break;\n\
         \x20   }\n\
         \x20   print(\"after\");\n}\n",
        &["0", "after"],
    );
}

#[test]
fn break_ends_a_for_loop() {
    assert_output(
        "fn main() {\n\
         \x20   for j in 0..5 {\n\
         \x20       if j == 2 { break; }\n\
         \x20       print(j);\n\
         \x20   }\n\
         \x20   print(\"after\");\n}\n",
        &["0", "1", "after"],
    );
}

#[test]
fn break_ends_only_the_loop_it_is_in() {
    assert_output(
        "fn main() {\n\
         \x20   for a in 0..2 {\n\
         \x20       for b in 0..3 {\n\
         \x20           if b == 1 { break; }\n\
         \x20           print(a * 10 + b);\n\
         \x20       }\n\
         \x20   }\n\
         \x20   print(\"after\");\n}\n",
        &["0", "10", "after"],
    );
}

#[test]
fn break_ends_a_loop_over_an_array() {
    assert_output(
        "fn main() {\n\
         \x20   for s in [\"a\", \"b\", \"c\"] {\n\
         \x20       if s == \"b\" { break; }\n\
         \x20       print(s);\n\
         \x20   }\n\
         \x20   print(\"after\");\n}\n",
        &["a", "after"],
    );
}

#[test]
fn a_while_expression_takes_the_first_break_value() {
    // Bounded so it terminates either way: with `break` behaving as `continue`
    // the loop ran to its condition and the last turn's value won.
    assert_output(
        "fn main() {\n\
         \x20   let n = 0;\n\
         \x20   let w = while n < 100 {\n\
         \x20       n = n + 1;\n\
         \x20       break n * 2;\n\
         \x20   };\n\
         \x20   print(w);\n\
         \x20   print(n);\n}\n",
        &["2", "1"],
    );
}

#[test]
fn continue_still_skips_the_rest_of_the_body() {
    assert_output(
        "fn main() {\n\
         \x20   for i in 0..5 {\n\
         \x20       if i % 2 == 0 { continue; }\n\
         \x20       print(i);\n\
         \x20   }\n}\n",
        &["1", "3"],
    );
}

// ---------------------------------------------------------------------------
// Dividing by zero
//
// Rust's own panic escaped through the interpreter: `error: attempt to divide
// by zero` with no line to look at and exit 101, where every other failure in
// this language points at the statement that caused it and exits 1.
// ---------------------------------------------------------------------------

#[test]
fn dividing_by_zero_points_at_the_statement() {
    let r = run("fn main() {\n    let n = 0;\n    print(7 / n);\n}\n");
    assert!(!r.success, "expected the program to fail; it printed:\n{}", r.stdout);
    assert!(r.stderr.contains("division by zero"), "stderr was:\n{}", r.stderr);
    // The panic path had no position at all.
    assert!(r.stderr.contains("--> "), "no position in the diagnostic:\n{}", r.stderr);
    assert!(r.stderr.contains("3 |"), "wrong line in the diagnostic:\n{}", r.stderr);
}

#[test]
fn remainder_by_zero_points_at_the_statement() {
    let r = run("fn main() {\n    let n = 0;\n    print(7 % n);\n}\n");
    assert!(!r.success, "expected the program to fail; it printed:\n{}", r.stdout);
    assert!(r.stderr.contains("remainder by zero"), "stderr was:\n{}", r.stderr);
    assert!(r.stderr.contains("--> "), "no position in the diagnostic:\n{}", r.stderr);
}

#[test]
fn division_that_can_be_done_still_is() {
    assert_output(
        "fn main() { print(7 / 2); print(0 - 7 / 2); print(7 % 3); print(1.0 / 0.0); }\n",
        &["3", "-3", "1", "inf"],
    );
}

// ---------------------------------------------------------------------------
// Integer overflow
//
// Another of Rust's own panics coming out of the interpreter: exit 101 and no
// position. Overflow is a failure this language reports, the way it reports an
// index out of range or a zero divisor — not a wrap.
// ---------------------------------------------------------------------------

#[test]
fn overflowing_addition_points_at_the_statement() {
    let r = run("fn main() {\n    print(9223372036854775807 + 1);\n}\n");
    assert!(!r.success, "expected the program to fail; it printed:\n{}", r.stdout);
    assert!(r.stderr.contains("addition overflowed"), "stderr was:\n{}", r.stderr);
    assert!(r.stderr.contains("--> "), "no position in the diagnostic:\n{}", r.stderr);
}

#[test]
fn overflowing_subtraction_and_multiplication_are_caught() {
    assert_run_fails(
        "fn main() { print(0 - 9223372036854775807 - 2); }\n",
        "subtraction overflowed",
    );
    assert_run_fails(
        "fn main() { print(4611686018427387904 * 4); }\n",
        "multiplication overflowed",
    );
}

#[test]
fn the_one_division_that_overflows_is_caught() {
    // The smallest integer has no positive counterpart.
    assert_run_fails(
        "fn main() {\n    let m = 0 - 9223372036854775807 - 1;\n    let d = 0 - 1;\n    print(m / d);\n}\n",
        "division overflowed",
    );
}

#[test]
fn arithmetic_that_fits_is_untouched() {
    assert_output(
        "fn main() {\n\
         \x20   print(2 + 3 * 4 - 1);\n\
         \x20   print(9223372036854775807);\n\
         \x20   print(0 - 9223372036854775807);\n\
         \x20   print(9223372036854775806 + 1);\n}\n",
        &["13", "9223372036854775807", "-9223372036854775807", "9223372036854775807"],
    );
}

// ---------------------------------------------------------------------------
// Strings are counted in characters
//
// `len` counted bytes while `char_at` counted characters, so
// `substring(s, 0, len(s))` was not `s`, and walking a string by index went
// wrong the moment anything in it was not ASCII.
// ---------------------------------------------------------------------------

#[test]
fn len_counts_characters() {
    assert_output(
        "fn main() { print(len(\"héllo\")); print(len(\"日本語\")); print(len(\"abc\")); print(len(\"\")); }\n",
        &["5", "3", "3", "0"],
    );
}

#[test]
fn substring_takes_character_indices() {
    assert_output(
        "fn main() {\n\
         \x20   print(substring(\"héllo\", 0, 2));\n\
         \x20   print(substring(\"héllo\", 1, 3));\n\
         \x20   print(substring(\"日本語\", 1, 3));\n\
         \x20   print(substring(\"héllo\", 0, len(\"héllo\")));\n}\n",
        &["hé", "él", "本語", "héllo"],
    );
}

#[test]
fn walking_a_string_by_index_rebuilds_it() {
    assert_output(
        "fn main() {\n\
         \x20   let s = \"aébc\";\n\
         \x20   let out = \"\";\n\
         \x20   let i = 0;\n\
         \x20   while i < len(s) { out = out + char_at(s, i); i = i + 1; }\n\
         \x20   print(out);\n}\n",
        &["aébc"],
    );
}

#[test]
fn char_at_past_the_end_is_an_error() {
    // It used to answer with a NUL character, which is not a character the
    // program asked for and is one byte long when printed.
    assert_run_fails("fn main() { print(char_at(\"abc\", 3)); }\n", "char_at index 3 is out of bounds");
    assert_run_fails("fn main() { print(char_at(\"\", 0)); }\n", "char_at index 0 is out of bounds");
}

// ---------------------------------------------------------------------------
// Operators the interpreter did not have
//
// Comparing two floats was a run-time failure — "unsupported float op" — and
// had always worked in the native backend, so `1.0 == 1.0` depended on which
// one ran the program. `!` on an integer answered with 1 or 0 rather than a
// bool, though the checker had already said the answer was a bool.
// ---------------------------------------------------------------------------

#[test]
fn floats_compare() {
    assert_output(
        "fn main() {\n\
         \x20   print(1.0 == 1.0); print(1.0 != 2.0);\n\
         \x20   print(1.0 < 2.0); print(2.0 <= 2.0);\n\
         \x20   print(3.0 > 2.0); print(2.0 >= 3.0);\n}\n",
        &["true", "true", "true", "true", "true", "false"],
    );
}

#[test]
fn floats_have_a_remainder() {
    assert_output("fn main() { print(2.5 % 1.0); print(7.5 % 2.0); }\n", &["0.5", "1.5"]);
}

#[test]
fn not_on_an_integer_answers_with_a_bool() {
    assert_output("fn main() { print(!0); print(!1); print(!true); }\n", &["true", "false", "false"]);
}

#[test]
fn and_or_need_something_that_can_be_true_or_false() {
    // A float or a string here type-checked and then failed differently in
    // each backend.
    assert_check_error("fn main() { print(1.0 && 2.0); }\n", "expected `bool`, found `float`");
    assert_check_error("fn main() { print(\"a\" || \"b\"); }\n", "expected `bool`, found `string`");
    assert_checks("fn main() { print(1 && 0); print(true || false); }\n");
}
