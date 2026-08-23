//! Lexer: turns Zarrin source text into a stream of tokens.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Float(f64),
    Str(String),
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
    Semicolon,
    Eof,
}

pub struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            chars: src.chars().peekable(),
        }
    }

    fn skip_ignored(&mut self) {
        loop {
            match self.chars.peek() {
                Some(' ') | Some('\t') | Some('\n') | Some('\r') => {
                    self.chars.next();
                }
                Some('/') => {
                    // line comment starting with //
                    let mut it = self.chars.clone();
                    it.next();
                    if it.peek() == Some(&'/') {
                        for c in self.chars.by_ref() {
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

    pub fn next_token(&mut self) -> Token {
        self.skip_ignored();
        match self.chars.next() {
            None => Token::Eof,
            Some(c) => match c {
                '+' => Token::Plus,
                '-' => {
                    if self.chars.peek() == Some(&'>') {
                        self.chars.next();
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
                        self.chars.next();
                        Token::AmpAmp
                    } else {
                        panic!("unexpected character: '&', did you mean '&&'?");
                    }
                }
                '|' => {
                    if self.chars.peek() == Some(&'|') {
                        self.chars.next();
                        Token::PipePipe
                    } else {
                        panic!("unexpected character: '|', did you mean '||'?");
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
                        self.chars.next();
                        Token::Eq
                    } else if self.chars.peek() == Some(&'>') {
                        self.chars.next();
                        Token::FatArrow
                    } else {
                        Token::Assign
                    }
                }
                '!' => {
                    if self.chars.peek() == Some(&'=') {
                        self.chars.next();
                        Token::Ne
                    } else {
                        Token::Bang
                    }
                }
                '<' => {
                    if self.chars.peek() == Some(&'=') {
                        self.chars.next();
                        Token::Le
                    } else {
                        Token::Lt
                    }
                }
                '>' => {
                    if self.chars.peek() == Some(&'=') {
                        self.chars.next();
                        Token::Ge
                    } else {
                        Token::Gt
                    }
                }
                ':' => {
                    if self.chars.peek() == Some(&':') {
                        self.chars.next();
                        Token::ColonColon
                    } else {
                        Token::Colon
                    }
                }
                '.' => {
                    if self.chars.peek() == Some(&'.') {
                        self.chars.next();
                        Token::DotDot
                    } else {
                        Token::Dot
                    }
                }
                '"' => {
                    let mut s = String::new();
                    while let Some(&ch) = self.chars.peek() {
                        if ch == '"' {
                            self.chars.next();
                            break;
                        }
                        s.push(ch);
                        self.chars.next();
                    }
                    Token::Str(s)
                }
                ch if ch.is_ascii_digit() => {
                    let mut num = String::new();
                    num.push(ch);
                    let mut is_float = false;
                    while let Some(&n) = self.chars.peek() {
                        if n.is_ascii_digit() {
                            num.push(n);
                            self.chars.next();
                        } else if n == '.' && !is_float {
                            let mut it = self.chars.clone();
                            it.next();
                            if it.peek() == Some(&'.') {
                                break;
                            }
                            is_float = true;
                            num.push(n);
                            self.chars.next();
                        } else {
                            break;
                        }
                    }
                    if is_float {
                        Token::Float(num.parse().unwrap())
                    } else {
                        Token::Int(num.parse().unwrap())
                    }
                }
                ch if ch.is_alphabetic() || ch == '_' => {
                    let mut id = String::new();
                    id.push(ch);
                    while let Some(&n) = self.chars.peek() {
                        if n.is_alphanumeric() || n == '_' {
                            id.push(n);
                            self.chars.next();
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
                        "true" => Token::Bool(true),
                        "false" => Token::Bool(false),
                        _ => Token::Ident(id),
                    }
                }
                other => panic!("unexpected character: {:?}", other),
            },
        }
    }
}
