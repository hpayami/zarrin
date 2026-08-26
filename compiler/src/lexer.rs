//! Lexer: turns Zarrin source text into a stream of tokens.

use crate::diagnostic::{Diagnostic, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Float(f64),
    Str(String),
    InterpStr(String),
    Ident(String),
    // literals
    Bool(bool),
    // keywords
    Let,
    Fn,
    Return,
    Struct,
    Enum,
    Match,
    Trait,
    Impl,
    For,
    Extern,
    Macro,
    If,
    Else,
    While,
    Break,
    Continue,
    In,
    Import,
    // symbols
    Colon,
    ColonColon,
    Dot,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Assign,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    Comma,
    Arrow,
    FatArrow,
    DotDot,
    Bang,
    AmpAmp,
    PipePipe,
    Pipe,
    Semicolon,
    Eof,
}

pub struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    line: u32,
    col: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            chars: src.chars().peekable(),
            line: 1,
            col: 1,
        }
    }

    /// Consume one character, keeping the line/column cursor in step. Every
    /// read goes through here so positions cannot drift.
    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next();
        match c {
            Some('\n') => { self.line += 1; self.col = 1; }
            Some(_) => { self.col += 1; }
            None => {}
        }
        c
    }

    /// Position of the next character to be read.
    fn here(&self) -> Span {
        Span::new(self.line, self.col)
    }

    fn error(&self, message: impl Into<String>, span: Span) -> Diagnostic {
        Diagnostic::new(message, span)
    }

    fn skip_ignored(&mut self) {
        loop {
            match self.chars.peek() {
                Some(' ') | Some('\t') | Some('\n') | Some('\r') => {
                    self.bump();
                }
                Some('/') => {
                    // line comment starting with //
                    let mut it = self.chars.clone();
                    it.next();
                    if it.peek() == Some(&'/') {
                        while let Some(c) = self.bump() {
                            if c == '\n' {
                                break;
                            }
                        }
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    /// Returns the token together with the span of its first character.
    pub fn next_token(&mut self) -> Result<(Token, Span), Diagnostic> {
        self.skip_ignored();
        let start = self.here();
        let token = match self.bump() {
            None => Token::Eof,
            Some(c) => match c {
                '+' => Token::Plus,
                '-' => {
                    if self.chars.peek() == Some(&'>') {
                        self.bump();
                        Token::Arrow
                    } else {
                        Token::Minus
                    }
                }
                '*' => Token::Star,
                '/' => Token::Slash,
                '%' => Token::Percent,
                '&' => {
                    if self.chars.peek() == Some(&'&') {
                        self.bump();
                        Token::AmpAmp
                    } else {
                        return Err(self.error("unexpected `&`; did you mean `&&`?", start));
                    }
                }
                '|' => {
                    if self.chars.peek() == Some(&'|') {
                        self.bump();
                        Token::PipePipe
                    } else {
                        Token::Pipe
                    }
                }
                '(' => Token::LParen,
                ')' => Token::RParen,
                '{' => Token::LBrace,
                '}' => Token::RBrace,
                '[' => Token::LBrack,
                ']' => Token::RBrack,
                ',' => Token::Comma,
                ';' => Token::Semicolon,
                '=' => {
                    if self.chars.peek() == Some(&'=') {
                        self.bump();
                        Token::Eq
                    } else if self.chars.peek() == Some(&'>') {
                        self.bump();
                        Token::FatArrow
                    } else {
                        Token::Assign
                    }
                }
                '!' => {
                    if self.chars.peek() == Some(&'=') {
                        self.bump();
                        Token::Ne
                    } else {
                        Token::Bang
                    }
                }
                '<' => {
                    if self.chars.peek() == Some(&'=') {
                        self.bump();
                        Token::Le
                    } else {
                        Token::Lt
                    }
                }
                '>' => {
                    if self.chars.peek() == Some(&'=') {
                        self.bump();
                        Token::Ge
                    } else {
                        Token::Gt
                    }
                }
                ':' => {
                    if self.chars.peek() == Some(&':') {
                        self.bump();
                        Token::ColonColon
                    } else {
                        Token::Colon
                    }
                }
                '.' => {
                    if self.chars.peek() == Some(&'.') {
                        self.bump();
                        Token::DotDot
                    } else {
                        Token::Dot
                    }
                }
                '"' => {
                    let mut s = String::new();
                    let mut has_interp = false;
                    let mut closed = false;
                    while let Some(&ch) = self.chars.peek() {
                        if ch == '"' {
                            self.bump();
                            closed = true;
                            break;
                        }
                        if ch == '{' {
                            has_interp = true;
                        }
                        if ch == '}' && has_interp {
                            // Will be handled during re-lexing in parser
                        }
                        s.push(ch);
                        self.bump();
                    }
                    if !closed {
                        return Err(self.error("unterminated string literal", start));
                    }
                    if has_interp {
                        Token::InterpStr(s)
                    } else {
                        Token::Str(s)
                    }
                }
                ch if ch.is_ascii_digit() => {
                    let mut num = String::new();
                    num.push(ch);
                    let mut is_float = false;
                    while let Some(&n) = self.chars.peek() {
                        if n.is_ascii_digit() {
                            num.push(n);
                            self.bump();
                        } else if n == '.' && !is_float {
                            let mut it = self.chars.clone();
                            it.next();
                            if it.peek() == Some(&'.') {
                                break;
                            }
                            is_float = true;
                            num.push(n);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    if is_float {
                        match num.parse() {
                            Ok(f) => Token::Float(f),
                            Err(_) => return Err(self.error(format!("invalid float literal `{}`", num), start)),
                        }
                    } else {
                        match num.parse() {
                            Ok(i) => Token::Int(i),
                            Err(_) => return Err(self.error(
                                format!("integer literal `{}` does not fit in a 64-bit int", num), start)),
                        }
                    }
                }
                ch if ch.is_alphabetic() || ch == '_' => {
                    let mut id = String::new();
                    id.push(ch);
                    while let Some(&n) = self.chars.peek() {
                        if n.is_alphanumeric() || n == '_' {
                            id.push(n);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    match id.as_str() {
                        "let" => Token::Let,
                        "fn" => Token::Fn,
                        "return" => Token::Return,
                        "struct" => Token::Struct,
                        "enum" => Token::Enum,
                        "match" => Token::Match,
                        "trait" => Token::Trait,
                        "impl" => Token::Impl,
                        "for" => Token::For,
                        "extern" => Token::Extern,
                        "macro" => Token::Macro,
                        "if" => Token::If,
                        "else" => Token::Else,
                        "while" => Token::While,
                        "break" => Token::Break,
                        "continue" => Token::Continue,
                        "in" => Token::In,
                        "import" => Token::Import,
                        "true" => Token::Bool(true),
                        "false" => Token::Bool(false),
                        _ => Token::Ident(id),
                    }
                }
                other => return Err(self.error(format!("unexpected character `{}`", other), start)),
            },
        };
        Ok((token, start))
    }
}
