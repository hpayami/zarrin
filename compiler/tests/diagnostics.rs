//! Syntax errors must report a position and quote the offending line, instead
//! of aborting with a Rust panic and no location.

mod common;

use common::zarrinc;

/// Assert the program is rejected with a diagnostic mentioning `needle`,
/// pointing at `line:col`, and underlining that column.
fn assert_diagnostic(src: &str, needle: &str, line: u32, col: u32) {
    let r = zarrinc("run", src);
    assert!(!r.success, "expected a syntax error; got:\n{}", r.stdout);
    let err = &r.stderr;
    assert!(err.contains(needle), "message {:?} not in:\n{}", needle, err);
    assert!(
        err.contains(&format!(":{}:{}", line, col)),
        "expected position {}:{} in:\n{}",
        line, col, err
    );
    assert!(err.starts_with("error: "), "not rendered as a diagnostic:\n{}", err);
    // the caret line must sit under the reported column
    let caret = err.lines().last().unwrap_or_default();
    let at = caret.find('^').unwrap_or_else(|| panic!("no caret in:\n{}", err));
    let gutter = caret.find('|').unwrap_or_else(|| panic!("no gutter in:\n{}", err));
    assert_eq!(
        at - gutter - 2,
        (col - 1) as usize,
        "caret is not under column {}:\n{}",
        col, err
    );
}

#[test]
fn unexpected_character() {
    assert_diagnostic("fn f(x: int) -> int {\n    return x @ 1;\n}\n", "unexpected character `@`", 2, 14);
}

#[test]
fn ampersand_suggests_and_and() {
    assert_diagnostic("fn main() {\n    let a = 1 & 2;\n}\n", "did you mean `&&`", 2, 15);
}

#[test]
fn unterminated_string_is_rejected() {
    // Previously the lexer ran silently to end of file and accepted this.
    assert_diagnostic("fn main() {\n    let s = \"oops;\n}\n", "unterminated string literal", 2, 13);
}

#[test]
fn oversized_integer_literal_is_rejected() {
    // `num.parse().unwrap()` used to abort with a Rust ParseIntError panic.
    assert_diagnostic(
        "fn main() {\n    let n = 99999999999999999999;\n}\n",
        "does not fit in a 64-bit int",
        2,
        13,
    );
}

#[test]
fn parse_error_points_at_the_offending_token() {
    assert_diagnostic("fn main() {\n    let 5 = x;\n}\n", "expected identifier after let, found `5`", 2, 9);
}

#[test]
fn missing_token_names_what_was_expected() {
    assert_diagnostic("fn main() {\n    let x = 1\n    print(x);\n}\n", "expected `;`, found `print`", 3, 5);
}

#[test]
fn error_inside_an_interpolated_expression_points_at_the_string() {
    assert_diagnostic(
        "fn main() {\n    print(\"v = {1 +}\");\n}\n",
        "in interpolated expression `1 +`",
        2,
        11,
    );
}

#[test]
fn positions_survive_comments_and_blank_lines() {
    let src = "// a comment\n\nfn main() {\n    // another\n\n    let a = 1 & 2;\n}\n";
    assert_diagnostic(src, "did you mean `&&`", 6, 15);
}

#[test]
fn the_quoted_line_is_the_source_line() {
    let r = zarrinc("run", "fn main() {\n    let a = 1 & 2;\n}\n");
    assert!(r.stderr.contains("let a = 1 & 2;"), "source line not quoted:\n{}", r.stderr);
}

#[test]
fn runtime_failures_are_not_rust_panics() {
    let r = zarrinc("run", "fn main() {\n    nope(1);\n}\n");
    assert!(!r.success);
    assert!(r.stderr.starts_with("error: "), "not a clean error:\n{}", r.stderr);
    assert!(
        !r.stderr.contains("panicked at") && !r.stderr.contains("RUST_BACKTRACE"),
        "Rust panic internals leaked:\n{}",
        r.stderr
    );
}
