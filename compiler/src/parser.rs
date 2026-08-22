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
            Token::Struct => self.parse_struct(),
            Token::Enum => self.parse_enum(),
            Token::Trait => self.parse_trait(),
            Token::Impl => self.parse_impl(),
            Token::Extern => self.parse_extern_fn(),
            Token::Macro => self.parse_macro(),
            Token::If => {
                self.advance(); // if
                let cond = self.parse_expr();
                self.expect(Token::LBrace);
                let mut then_body = Vec::new();
                while self.current != Token::RBrace {
                    then_body.push(self.parse_stmt());
                }
                self.expect(Token::RBrace);
                let else_body = if self.current == Token::Else {
                    self.advance();
                    self.expect(Token::LBrace);
                    let mut eb = Vec::new();
                    while self.current != Token::RBrace {
                        eb.push(self.parse_stmt());
                    }
                    self.expect(Token::RBrace);
                    Some(eb)
                } else {
                    None
                };
                Stmt::If { cond, then_body, else_body }
            }
            Token::While => {
                self.advance(); // while
                let cond = self.parse_expr();
                self.expect(Token::LBrace);
                let mut body = Vec::new();
                while self.current != Token::RBrace {
                    body.push(self.parse_stmt());
                }
                self.expect(Token::RBrace);
                Stmt::While { cond, body }
            }
            Token::Break => {
                self.advance();
                self.expect(Token::Semicolon);
                Stmt::Break
            }
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
                if self.current == Token::Assign {
                    self.advance();
                    let val = self.parse_expr();
                    self.expect(Token::Semicolon);
                    if let Expr::Ident(name) = e {
                        Stmt::Assign { name, value: val }
                    } else {
                        panic!("expected identifier on left side of assignment");
                    }
                } else if self.current == Token::Semicolon {
                    self.advance();
                    Stmt::Expr(e)
                } else {
                    Stmt::Return(Some(e))
                }
            }
        }
    }

    fn parse_generics(&mut self) -> Vec<String> {
        if self.current != Token::Lt {
            return Vec::new();
        }
        self.advance();
        let mut generics = Vec::new();
        while self.current != Token::Gt {
            match self.advance() {
                Token::Ident(n) => generics.push(n),
                t => panic!("expected generic name, found {:?}", t),
            }
            if self.current == Token::Comma {
                self.advance();
            }
        }
        self.expect(Token::Gt);
        generics
    }

    fn parse_let(&mut self) -> Stmt {
        self.advance();
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected identifier after let, found {:?}", t),
        };
        self.expect(Token::Assign);
        let value = self.parse_expr();
        self.expect(Token::Semicolon);
        Stmt::Let { name, ty: Type::Inferred, value }
    }

    fn parse_fn(&mut self) -> Stmt {
        self.advance();
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected fn name, found {:?}", t),
        };
        let generics = self.parse_generics();
        self.expect(Token::LParen);
        let mut params = Vec::new();
        if self.current != Token::RParen {
            loop {
                let pname = match self.advance() {
                    Token::Ident(n) => n,
                    t => panic!("expected param name, found {:?}", t),
                };
                if pname == "self" || pname == "&self" || pname == "&mut" {
                    params.push((pname, Type::Named("Self".into())));
                    if self.current == Token::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                    continue;
                }
                self.expect(Token::Colon);
                let ty = self.parse_type();
                params.push((pname, ty));
                if self.current == Token::Comma {
                    self.advance();
                } else {
                    break;
                }
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
        Stmt::Fn { name, generics, params, ret, body }
    }

    fn parse_struct(&mut self) -> Stmt {
        self.advance();
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected struct name, found {:?}", t),
        };
        let generics = self.parse_generics();
        self.expect(Token::LBrace);
        let mut fields = Vec::new();
        while self.current != Token::RBrace {
            let fname = match self.advance() {
                Token::Ident(n) => n,
                t => panic!("expected field name, found {:?}", t),
            };
            self.expect(Token::Colon);
            let ty = self.parse_type();
            fields.push((fname, ty));
            if self.current == Token::Comma {
                self.advance();
            }
        }
        self.expect(Token::RBrace);
        Stmt::Struct { name, generics, fields }
    }

    fn parse_enum(&mut self) -> Stmt {
        self.advance();
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected enum name, found {:?}", t),
        };
        self.expect(Token::LBrace);
        let mut variants = Vec::new();
        while self.current != Token::RBrace {
            let vname = match self.advance() {
                Token::Ident(n) => n,
                t => panic!("expected variant name, found {:?}", t),
            };
            let args = if self.current == Token::LParen {
                self.advance();
                let mut args = Vec::new();
                while self.current != Token::RParen {
                    args.push(self.parse_type());
                    if self.current == Token::Comma {
                        self.advance();
                    }
                }
                self.expect(Token::RParen);
                args
            } else {
                Vec::new()
            };
            variants.push((vname, args));
            if self.current == Token::Comma {
                self.advance();
            }
        }
        self.expect(Token::RBrace);
        Stmt::Enum { name, variants }
    }

    fn parse_trait(&mut self) -> Stmt {
        self.advance();
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected trait name, found {:?}", t),
        };
        self.expect(Token::LBrace);
        let mut methods = Vec::new();
        while self.current != Token::RBrace {
            self.expect(Token::Fn);
            let mname = match self.advance() {
                Token::Ident(n) => n,
                t => panic!("expected method name, found {:?}", t),
            };
            self.expect(Token::LParen);
            let mut params = Vec::new();
            if self.current != Token::RParen {
                loop {
                    let pname = match self.advance() {
                        Token::Ident(n) => n,
                        t => panic!("expected param name, found {:?}", t),
                    };
                    if pname == "self" || pname == "&self" || pname == "&mut" {
                        params.push((pname, Type::Named("Self".into())));
                        if self.current == Token::Comma { self.advance(); } else { break; }
                        continue;
                    }
                    self.expect(Token::Colon);
                    let ty = self.parse_type();
                    params.push((pname, ty));
                    if self.current == Token::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
            self.expect(Token::RParen);
            let ret = if self.current == Token::Arrow {
                self.advance();
                self.parse_type()
            } else {
                Type::Unit
            };
            self.expect(Token::Semicolon);
            methods.push(TraitMethod { name: mname, params, ret });
        }
        self.expect(Token::RBrace);
        Stmt::Trait { name, methods }
    }

    fn parse_impl(&mut self) -> Stmt {
        self.advance();
        let first_name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected name after impl, found {:?}", t),
        };
        if self.current == Token::For {
            self.advance();
            let type_name = match self.advance() {
                Token::Ident(n) => n,
                t => panic!("expected type name after for, found {:?}", t),
            };
            self.expect(Token::LBrace);
            let mut methods = Vec::new();
            while self.current != Token::RBrace {
                methods.push(self.parse_fn());
            }
            self.expect(Token::RBrace);
            Stmt::Impl { trait_name: first_name, type_name, methods }
        } else {
            self.expect(Token::LBrace);
            let mut methods = Vec::new();
            while self.current != Token::RBrace {
                methods.push(self.parse_fn());
            }
            self.expect(Token::RBrace);
            Stmt::Impl { trait_name: String::new(), type_name: first_name, methods }
        }
    }

    fn parse_extern_fn(&mut self) -> Stmt {
        self.advance(); // extern
        // Expect optional "C" string
        if matches!(self.current, Token::Str(_)) {
            self.advance();
        }
        self.expect(Token::Fn);
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected fn name, found {:?}", t),
        };
        self.expect(Token::LParen);
        let mut params = Vec::new();
        if self.current != Token::RParen {
            loop {
                let pname = match self.advance() {
                    Token::Ident(n) => n,
                    t => panic!("expected param name, found {:?}", t),
                };
                self.expect(Token::Colon);
                let ty = self.parse_type();
                params.push((pname, ty));
                if self.current == Token::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RParen);
        let ret = if self.current == Token::Arrow {
            self.advance();
            self.parse_type()
        } else {
            Type::Unit
        };
        self.expect(Token::Semicolon);
        Stmt::ExternFn { name, params, ret }
    }

    fn parse_macro(&mut self) -> Stmt {
        self.advance(); // macro
        let name = match self.advance() {
            Token::Ident(n) => n,
            t => panic!("expected macro name, found {:?}", t),
        };
        self.expect(Token::LParen);
        let mut params = Vec::new();
        while self.current != Token::RParen {
            match self.advance() {
                Token::Ident(n) => params.push(n),
                t => panic!("expected macro param, found {:?}", t),
            }
            if self.current == Token::Comma {
                self.advance();
            }
        }
        self.expect(Token::RParen);
        self.expect(Token::LBrace);
        let mut body = Vec::new();
        while self.current != Token::RBrace {
            body.push(self.parse_stmt());
        }
        self.expect(Token::RBrace);
        Stmt::Macro { name, params, body }
    }

    fn parse_type(&mut self) -> Type {
        match self.current.clone() {
            Token::Ident(n) => {
                self.advance();
                match n.as_str() {
                    "int" => Type::Int,
                    "float" => Type::Float,
                    "bool" => Type::Bool,
                    "string" => Type::String,
                    other => Type::Named(other.to_string()),
                }
            }
            Token::Fn => {
                self.advance();
                self.expect(Token::LParen);
                let mut params = Vec::new();
                if self.current != Token::RParen {
                    loop {
                        params.push(self.parse_type());
                        if self.current == Token::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(Token::RParen);
                let ret = if self.current == Token::Arrow {
                    self.advance();
                    self.parse_type()
                } else {
                    Type::Unit
                };
                Type::Fn(params, Box::new(ret))
            }
            t => panic!("expected type, found {:?}", t),
        }
    }

    pub fn parse_expr(&mut self) -> Expr {
        self.parse_binop(0)
    }

    fn parse_binop(&mut self, min_prec: u8) -> Expr {
        let mut left = self.parse_postfix();
        loop {
            let (prec, op) = match self.current {
                Token::Plus => (1, BinOp::Add),
                Token::Minus => (1, BinOp::Sub),
                Token::Star => (2, BinOp::Mul),
                Token::Slash => (2, BinOp::Div),
                Token::Eq => (0, BinOp::Eq),
                Token::Ne => (0, BinOp::Ne),
                Token::Lt => (0, BinOp::Lt),
                Token::Le => (0, BinOp::Le),
                Token::Gt => (0, BinOp::Gt),
                Token::Ge => (0, BinOp::Ge),
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

    fn parse_postfix(&mut self) -> Expr {
        let mut expr = self.parse_primary();
        loop {
            match self.current {
                Token::Dot => {
                    self.advance();
                    let field = match self.advance() {
                        Token::Ident(n) => n,
                        t => panic!("expected field/method name after '.', found {:?}", t),
                    };
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
                        expr = Expr::MethodCall(Box::new(expr), field, args);
                    } else {
                        expr = Expr::FieldAccess(Box::new(expr), field);
                    }
                }
                _ => break,
            }
        }
        expr
    }

    fn parse_primary(&mut self) -> Expr {
        match self.current.clone() {
            Token::Int(n) => { self.advance(); Expr::Int(n) }
            Token::Float(f) => { self.advance(); Expr::Float(f) }
            Token::Bool(b) => { self.advance(); Expr::Bool(b) }
            Token::Str(s) => { self.advance(); Expr::Str(s) }
            Token::Match => self.parse_match(),
            Token::If => self.parse_if_expr(),
            Token::Ident(name) => {
                self.advance();
                if self.current == Token::LBrace
                    && name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                {
                    return self.parse_struct_lit(name);
                }
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
                    return Expr::Call(Box::new(Expr::Ident(name)), args);
                }
                Expr::Ident(name)
            }
            Token::LParen => {
                self.advance();
                let e = self.parse_expr();
                self.expect(Token::RParen);
                e
            }
            t => panic!("unexpected token in expression: {:?}", t),
        }
    }

    fn parse_struct_lit(&mut self, name: String) -> Expr {
        self.advance();
        let mut fields = Vec::new();
        while self.current != Token::RBrace {
            let fname = match self.advance() {
                Token::Ident(n) => n,
                t => panic!("expected field name in struct literal, found {:?}", t),
            };
            self.expect(Token::Colon);
            let value = self.parse_expr();
            fields.push((fname, value));
            if self.current == Token::Comma {
                self.advance();
            }
        }
        self.expect(Token::RBrace);
        Expr::StructLit { name, fields }
    }

    fn parse_match(&mut self) -> Expr {
        self.advance();
        let scrutinee = self.parse_expr();
        self.expect(Token::LBrace);
        let mut arms = Vec::new();
        while self.current != Token::RBrace {
            let pattern = self.parse_pattern();
            self.expect(Token::FatArrow);
            let body = self.parse_expr();
            arms.push((pattern, body));
            if self.current == Token::Comma {
                self.advance();
            }
        }
        self.expect(Token::RBrace);
        Expr::Match { scrutinee: Box::new(scrutinee), arms }
    }

    fn parse_if_expr(&mut self) -> Expr {
        self.advance(); // if
        let cond = self.parse_expr();
        self.expect(Token::LBrace);
        let mut stmts = Vec::new();
        while self.current != Token::RBrace {
            stmts.push(self.parse_stmt());
        }
        self.expect(Token::RBrace);
        let then_body = if stmts.len() == 1 {
            match &stmts[0] {
                Stmt::Expr(e) => e.clone(),
                Stmt::Return(Some(e)) => e.clone(),
                _ => Expr::Int(0),
            }
        } else {
            Expr::Int(0)
        };
        let else_body = if self.current == Token::Else {
            self.advance();
            if self.current == Token::If {
                Some(Box::new(self.parse_if_expr()))
            } else {
                self.expect(Token::LBrace);
                let mut estmts = Vec::new();
                while self.current != Token::RBrace {
                    estmts.push(self.parse_stmt());
                }
                self.expect(Token::RBrace);
                Some(Box::new(if estmts.len() == 1 {
                    match &estmts[0] {
                        Stmt::Expr(e) => e.clone(),
                        Stmt::Return(Some(e)) => e.clone(),
                        _ => Expr::Int(0),
                    }
                } else {
                    Expr::Int(0)
                }))
            }
        } else {
            None
        };
        Expr::If { cond: Box::new(cond), then_body: Box::new(then_body), else_body }
    }

    fn parse_pattern(&mut self) -> Pattern {
        match self.current.clone() {
            Token::Int(n) => { self.advance(); Pattern::Literal(Expr::Int(n)) }
            Token::Str(s) => { self.advance(); Pattern::Literal(Expr::Str(s)) }
            Token::Bool(b) => { self.advance(); Pattern::Literal(Expr::Bool(b)) }
            Token::Ident(name) => {
                self.advance();
                if name == "_" {
                    return Pattern::Wildcard;
                }
                if self.current == Token::LParen {
                    self.advance();
                    let mut inner = Vec::new();
                    while self.current != Token::RParen {
                        inner.push(self.parse_pattern());
                        if self.current == Token::Comma {
                            self.advance();
                        }
                    }
                    self.expect(Token::RParen);
                    return Pattern::EnumVariant { name, inner };
                }
                if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                    return Pattern::EnumVariant { name, inner: Vec::new() };
                }
                Pattern::Variable(name)
            }
            _ => panic!("unexpected token in pattern: {:?}", self.current),
        }
    }
}
