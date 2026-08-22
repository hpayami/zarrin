//! Recursive-descent parser with Pratt-style expression parsing.

use crate::ast::*;
use crate::lexer::{Lexer, Token};

pub struct Parser<'a> {
    lexer: Lexer<'a>,
    current: Token,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Self {
        let mut lexer = Lexer::new(src);
        let current = lexer.next_token();
        Parser { lexer, current }
    }

    fn advance(&mut self) -> Token {
        let t = self.current.clone();
        self.current = self.lexer.next_token();
        t
    }

    fn expect(&mut self, tok: Token) {
        if self.current != tok {
            panic!("expected {:?}, found {:?}", tok, self.current);
        }
        self.advance();
    }

    pub fn parse_program(&mut self) -> Program {
        let mut stmts = Vec::new();
        while self.current != Token::Eof {
            stmts.push(self.parse_stmt());
        }
        Program { stmts }
    }

    fn parse_stmt(&mut self) -> Stmt {
        match self.current.clone() {
            Token::Let => self.parse_let(),
            Token::Fn => self.parse_fn(),
            Token::Return => {
                self.advance();
                let e = if self.current == Token::Semicolon {
                    None
                } else {
                    Some(self.parse_expr())
                };
                self.expect(Token::Semicolon);
                Stmt::Return(e)
            }
            _ => {
                let e = self.parse_expr();
                self.expect(Token::Semicolon);
                Stmt::Expr(e)
            }
        }
    }

    fn parse_let(&mut self) -> Stmt {
        self.advance(); // let
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected identifier after let, found {:?}", t),
        };
        self.expect(Token::Assign);
        let value = self.parse_expr();
        self.expect(Token::Semicolon);
        Stmt::Let {
            name,
            ty: Type::Inferred,
            value,
        }
    }

    fn parse_fn(&mut self) -> Stmt {
        self.advance(); // fn
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected fn name, found {:?}", t),
        };
        self.expect(Token::LParen);
        let mut params = Vec::new();
        while self.current != Token::RParen {
            let pname = match self.advance() {
                Token::Ident(n) => n,
                t => panic!("expected param name, found {:?}", t),
            };
            self.expect(Token::Colon);
            let ty = self.parse_type();
            params.push((pname, ty));
            if self.current == Token::Comma {
                self.advance();
            }
        }
        self.expect(Token::RParen);
        let ret = if self.current == Token::Arrow {
            self.advance();
            self.parse_type()
        } else {
            Type::Unit
        };
        self.expect(Token::LBrace);
        let mut body = Vec::new();
        while self.current != Token::RBrace {
            body.push(self.parse_stmt());
        }
        self.expect(Token::RBrace);
        Stmt::Fn {
            name,
            params,
            ret,
            body,
        }
    }

    fn parse_type(&mut self) -> Type {
        match self.advance() {
            Token::Ident(n) => match n.as_str() {
                "int" => Type::Int,
                "float" => Type::Float,
                "bool" => Type::Bool,
                "string" => Type::String,
                other => Type::Named(other.to_string()),
            },
            t => panic!("expected type, found {:?}", t),
        }
    }

    // Pratt parsing for binary expressions.
    pub fn parse_expr(&mut self) -> Expr {
        self.parse_binop(0)
    }

    fn parse_binop(&mut self, min_prec: u8) -> Expr {
        let mut left = self.parse_primary();
        loop {
            let (prec, op) = match self.current {
                Token::Plus => (1, BinOp::Add),
                Token::Minus => (1, BinOp::Sub),
                Token::Star => (2, BinOp::Mul),
                Token::Slash => (2, BinOp::Div),
                Token::Eq => (0, BinOp::Eq),
                Token::Ne => (0, BinOp::Ne),
                Token::Lt => (0, BinOp::Lt),
                Token::Gt => (0, BinOp::Gt),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.advance();
            let right = self.parse_binop(prec + 1);
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        left
    }

    fn parse_primary(&mut self) -> Expr {
        match self.advance() {
            Token::Int(n) => Expr::Int(n),
            Token::Float(f) => Expr::Float(f),
            Token::Bool(b) => Expr::Bool(b),
            Token::Str(s) => Expr::Str(s),
            Token::Ident(name) => {
                if self.current == Token::LParen {
                    self.advance();
                    let mut args = Vec::new();
                    while self.current != Token::RParen {
                        args.push(self.parse_expr());
                        if self.current == Token::Comma {
                            self.advance();
                        }
                    }
                    self.expect(Token::RParen);
                    Expr::Call(Box::new(Expr::Ident(name)), args)
                } else {
                    Expr::Ident(name)
                }
            }
            t => panic!("unexpected token in expression: {:?}", t),
        }
    }
}
