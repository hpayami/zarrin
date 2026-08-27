//! Source positions and user-facing error reporting.
//!
//! Before this existed every failure was a `panic!` with no location, so a
//! typo produced `expected RBrace, found Ident("x")` and a Rust backtrace
//! note. A `Diagnostic` carries where the problem is and renders it against
//! the source text.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// 1-based.
    pub line: u32,
    /// 1-based, counted in characters.
    pub col: u32,
}

impl Span {
    pub fn new(line: u32, col: u32) -> Self {
        Span { line, col }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Diagnostic { message: message.into(), span }
    }

    /// Render in the style of rustc:
    ///
    /// ```text
    /// error: expected `}`, found `print`
    ///  --> examples/foo.zr:7:5
    ///   |
    /// 7 |     print(x)
    ///   |     ^
    /// ```
    pub fn render(&self, path: &str, src: &str) -> String {
        let mut out = format!("error: {}\n --> {}:{}\n", self.message, path, self.span);
        let line_text = src.lines().nth(self.span.line.saturating_sub(1) as usize);
        let Some(line_text) = line_text else { return out };

        let number = self.span.line.to_string();
        let gutter = " ".repeat(number.len());
        // A tab in the source would misalign the caret, so render tabs as a
        // single space in both the quoted line and the underline.
        let shown: String = line_text.chars().map(|c| if c == '\t' { ' ' } else { c }).collect();
        let pad = " ".repeat(self.span.col.saturating_sub(1) as usize);
        out.push_str(&format!("{} |\n", gutter));
        out.push_str(&format!("{} | {}\n", number, shown));
        out.push_str(&format!("{} | {}^\n", gutter, pad));
        out
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at line {}", self.message, self.span)
    }
}
