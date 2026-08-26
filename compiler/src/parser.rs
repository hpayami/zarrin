//! Recursive-descent parser with Pratt-style expression parsing.

use crate::ast::*;
use crate::diagnostic::{Diagnostic, Span};
use crate::lexer::{Lexer, Token};

pub struct Parser<'a> {
    lexer: Lexer<'a>,
    current: Token,
    /// Span of `current`, so an error can point at the offending token.
    span: Span,
    /// Span of the token `advance` most recently returned. Errors that report
    /// a token already consumed must point here, not at what follows it.
    prev_span: Span,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Result<Self, Diagnostic> {
        let mut lexer = Lexer::new(src);
        let (current, span) = lexer.next_token()?;
        Ok(Parser { lexer, current, span, prev_span: span })
    }

    fn advance(&mut self) -> Result<Token, Diagnostic> {
        let t = self.current.clone();
        self.prev_span = self.span;
        let (next, span) = self.lexer.next_token()?;
        self.current = next;
        self.span = span;
        Ok(t)
    }

    /// Error at the current token.
    fn error(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(message, self.span)
    }

    /// Error at the token just consumed.
    fn error_at_prev(&self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(message, self.prev_span)
    }

    fn expect(&mut self, tok: Token) -> Result<(), Diagnostic> {
        if self.current != tok {
            return Err(self.error(format!(
                "expected {}, found {}",
                describe(&tok),
                describe(&self.current)
            )));
        }
        self.advance()?;
        Ok(())
    }

    pub fn parse_program(&mut self) -> Result<Program, Diagnostic> {
        let mut stmts = Vec::new();
        while self.current != Token::Eof {
            stmts.push(self.parse_stmt()?);
        }
        Ok(Program { stmts })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, Diagnostic> {
        Ok(match self.current.clone() {
            Token::Let => self.parse_let()?,
            Token::Fn => self.parse_fn()?,
            Token::Struct => self.parse_struct()?,
            Token::Enum => self.parse_enum()?,
            Token::Trait => self.parse_trait()?,
            Token::Impl => self.parse_impl()?,
            Token::Extern => self.parse_extern_fn()?,
            Token::Macro => self.parse_macro()?,
            Token::Import => {
                self.advance()?;
                let path = match self.current.clone() {
                    Token::Str(s) => { self.advance()?; s }
                    t => return Err(self.error_at_prev(format!("expected string path after import, found {}", describe(&t)))),
                };
                self.expect(Token::Semicolon)?;
                Stmt::Import(path)
            }
            Token::If => {
                self.advance()?; // if
                let cond = self.parse_expr()?;
                self.expect(Token::LBrace)?;
                let mut then_body = Vec::new();
                while self.current != Token::RBrace {
                    then_body.push(self.parse_stmt()?);
                }
                self.expect(Token::RBrace)?;
                let else_body = if self.current == Token::Else {
                    self.advance()?;
                    if self.current == Token::If {
                        // else if -> desugar to else { if ... }
                        let if_stmt = self.parse_stmt()?;
                        Some(vec![if_stmt])
                    } else {
                        self.expect(Token::LBrace)?;
                        let mut eb = Vec::new();
                        while self.current != Token::RBrace {
                            eb.push(self.parse_stmt()?);
                        }
                        self.expect(Token::RBrace)?;
                        Some(eb)
                    }
                } else {
                    None
                };
                Stmt::If { cond, then_body, else_body }
            }
            Token::While => {
                self.advance()?; // while
                let cond = self.parse_expr()?;
                self.expect(Token::LBrace)?;
                let mut body = Vec::new();
                while self.current != Token::RBrace {
                    body.push(self.parse_stmt()?);
                }
                self.expect(Token::RBrace)?;
                Stmt::While { cond, body }
            }
            Token::For => {
                self.advance()?; // for
                let var = match self.advance()? {
                    Token::Ident(n) => n,
                    t => return Err(self.error_at_prev(format!("expected variable name after for, found {}", describe(&t)))),
                };
                self.expect(Token::In)?;
                let iter = self.parse_expr()?;
                self.expect(Token::LBrace)?;
                let mut body = Vec::new();
                while self.current != Token::RBrace {
                    body.push(self.parse_stmt()?);
                }
                self.expect(Token::RBrace)?;
                Stmt::For { var, iter, body }
            }
            Token::Break => {
                self.advance()?;
                let e = if self.current == Token::Semicolon {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect(Token::Semicolon)?;
                Stmt::Break(e)
            }
            Token::Continue => {
                self.advance()?;
                let e = if self.current == Token::Semicolon {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect(Token::Semicolon)?;
                Stmt::Continue(e)
            }
            Token::Return => {
                self.advance()?;
                let e = if self.current == Token::Semicolon {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect(Token::Semicolon)?;
                Stmt::Return(e)
            }
            _ => {
                let e = self.parse_expr()?;
                if self.current == Token::Assign {
                    self.advance()?;
                    let val = self.parse_expr()?;
                    self.expect(Token::Semicolon)?;
                    if let Expr::Ident(name) = e {
                        Stmt::Assign { name, value: val }
                    } else {
                        return Err(self.error("expected identifier on left side of assignment"));
                    }
                } else if self.current == Token::Semicolon {
                    self.advance()?;
                    Stmt::Expr(e)
                } else {
                    Stmt::Return(Some(e))
                }
            }
        })
    }

    fn parse_generics(&mut self) -> Result<Vec<String>, Diagnostic> {
        if self.current != Token::Lt {
            return Ok(Vec::new());
        }
        self.advance()?;
        let mut generics = Vec::new();
        while self.current != Token::Gt {
            match self.advance()? {
                Token::Ident(n) => generics.push(n),
                t => return Err(self.error_at_prev(format!("expected generic name, found {}", describe(&t)))),
            }
            if self.current == Token::Comma {
                self.advance()?;
            }
        }
        self.expect(Token::Gt)?;
        Ok(generics)
    }

    fn parse_let(&mut self) -> Result<Stmt, Diagnostic> {
        self.advance()?;
        let name = match self.advance()? {
            Token::Ident(n) => n,
            t => return Err(self.error_at_prev(format!("expected identifier after let, found {}", describe(&t)))),
        };
        let ty = if self.current == Token::Colon {
            self.advance()?;
            self.parse_type()?
        } else {
            Type::Inferred
        };
        self.expect(Token::Assign)?;
        let value = self.parse_expr()?;
        self.expect(Token::Semicolon)?;
        Ok(Stmt::Let { name, ty, value })
    }

    fn parse_fn(&mut self) -> Result<Stmt, Diagnostic> {
        self.advance()?;
        let name = match self.advance()? {
            Token::Ident(n) => n,
            t => return Err(self.error_at_prev(format!("expected fn name, found {}", describe(&t)))),
        };
        let generics = self.parse_generics()?;
        self.expect(Token::LParen)?;
        let mut params = Vec::new();
        if self.current != Token::RParen {
            loop {
                let pname = match self.advance()? {
                    Token::Ident(n) => n,
                    t => return Err(self.error_at_prev(format!("expected param name, found {}", describe(&t)))),
                };
                if pname == "self" || pname == "&self" || pname == "&mut" {
                    params.push((pname, Type::Named("Self".into())));
                    if self.current == Token::Comma {
                        self.advance()?;
                    } else {
                        break;
                    }
                    continue;
                }
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                params.push((pname, ty));
                if self.current == Token::Comma {
                    self.advance()?;
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        let ret = if self.current == Token::Arrow {
            self.advance()?;
            self.parse_type()?
        } else {
            Type::Unit
        };
        self.expect(Token::LBrace)?;
        let mut body = Vec::new();
        while self.current != Token::RBrace {
            body.push(self.parse_stmt()?);
        }
        self.expect(Token::RBrace)?;
        Ok(Stmt::Fn { name, generics, params, ret, body })
    }

    fn parse_struct(&mut self) -> Result<Stmt, Diagnostic> {
        self.advance()?;
        let name = match self.advance()? {
            Token::Ident(n) => n,
            t => return Err(self.error_at_prev(format!("expected struct name, found {}", describe(&t)))),
        };
        let generics = self.parse_generics()?;
        self.expect(Token::LBrace)?;
        let mut fields = Vec::new();
        while self.current != Token::RBrace {
            let fname = match self.advance()? {
                Token::Ident(n) => n,
                t => return Err(self.error_at_prev(format!("expected field name, found {}", describe(&t)))),
            };
            self.expect(Token::Colon)?;
            let ty = self.parse_type()?;
            fields.push((fname, ty));
            if self.current == Token::Comma {
                self.advance()?;
            }
        }
        self.expect(Token::RBrace)?;
        Ok(Stmt::Struct { name, generics, fields })
    }

    fn parse_enum(&mut self) -> Result<Stmt, Diagnostic> {
        self.advance()?;
        let name = match self.advance()? {
            Token::Ident(n) => n,
            t => return Err(self.error_at_prev(format!("expected enum name, found {}", describe(&t)))),
        };
        self.expect(Token::LBrace)?;
        let mut variants = Vec::new();
        while self.current != Token::RBrace {
            let vname = match self.advance()? {
                Token::Ident(n) => n,
                t => return Err(self.error_at_prev(format!("expected variant name, found {}", describe(&t)))),
            };
            let args = if self.current == Token::LParen {
                self.advance()?;
                let mut args = Vec::new();
                while self.current != Token::RParen {
                    args.push(self.parse_type()?);
                    if self.current == Token::Comma {
                        self.advance()?;
                    }
                }
                self.expect(Token::RParen)?;
                args
            } else {
                Vec::new()
            };
            variants.push((vname, args));
            if self.current == Token::Comma {
                self.advance()?;
            }
        }
        self.expect(Token::RBrace)?;
        Ok(Stmt::Enum { name, variants })
    }

    fn parse_trait(&mut self) -> Result<Stmt, Diagnostic> {
        self.advance()?;
        let name = match self.advance()? {
            Token::Ident(n) => n,
            t => return Err(self.error_at_prev(format!("expected trait name, found {}", describe(&t)))),
        };
        self.expect(Token::LBrace)?;
        let mut methods = Vec::new();
        while self.current != Token::RBrace {
            self.expect(Token::Fn)?;
            let mname = match self.advance()? {
                Token::Ident(n) => n,
                t => return Err(self.error_at_prev(format!("expected method name, found {}", describe(&t)))),
            };
            self.expect(Token::LParen)?;
            let mut params = Vec::new();
            if self.current != Token::RParen {
                loop {
                    let pname = match self.advance()? {
                        Token::Ident(n) => n,
                        t => return Err(self.error_at_prev(format!("expected param name, found {}", describe(&t)))),
                    };
                    if pname == "self" || pname == "&self" || pname == "&mut" {
                        params.push((pname, Type::Named("Self".into())));
                        if self.current == Token::Comma { self.advance()?; } else { break; }
                        continue;
                    }
                    self.expect(Token::Colon)?;
                    let ty = self.parse_type()?;
                    params.push((pname, ty));
                    if self.current == Token::Comma {
                        self.advance()?;
                    } else {
                        break;
                    }
                }
            }
            self.expect(Token::RParen)?;
            let ret = if self.current == Token::Arrow {
                self.advance()?;
                self.parse_type()?
            } else {
                Type::Unit
            };
            self.expect(Token::Semicolon)?;
            methods.push(TraitMethod { name: mname, params, ret });
        }
        self.expect(Token::RBrace)?;
        Ok(Stmt::Trait { name, methods })
    }

    fn parse_impl(&mut self) -> Result<Stmt, Diagnostic> {
        self.advance()?;
        let first_name = match self.advance()? {
            Token::Ident(n) => n,
            t => return Err(self.error_at_prev(format!("expected name after impl, found {}", describe(&t)))),
        };
        Ok(if self.current == Token::For {
            self.advance()?;
            let type_name = match self.advance()? {
                Token::Ident(n) => n,
                t => return Err(self.error_at_prev(format!("expected type name after for, found {}", describe(&t)))),
            };
            self.expect(Token::LBrace)?;
            let mut methods = Vec::new();
            while self.current != Token::RBrace {
                methods.push(self.parse_fn()?);
            }
            self.expect(Token::RBrace)?;
            Stmt::Impl { trait_name: first_name, type_name, methods }
        } else {
            self.expect(Token::LBrace)?;
            let mut methods = Vec::new();
            while self.current != Token::RBrace {
                methods.push(self.parse_fn()?);
            }
            self.expect(Token::RBrace)?;
            Stmt::Impl { trait_name: String::new(), type_name: first_name, methods }
        })
    }

    fn parse_extern_fn(&mut self) -> Result<Stmt, Diagnostic> {
        self.advance()?; // extern
        // Expect optional "C" string
        if matches!(self.current, Token::Str(_)) {
            self.advance()?;
        }
        self.expect(Token::Fn)?;
        let name = match self.advance()? {
            Token::Ident(n) => n,
            t => return Err(self.error_at_prev(format!("expected fn name, found {}", describe(&t)))),
        };
        self.expect(Token::LParen)?;
        let mut params = Vec::new();
        if self.current != Token::RParen {
            loop {
                let pname = match self.advance()? {
                    Token::Ident(n) => n,
                    t => return Err(self.error_at_prev(format!("expected param name, found {}", describe(&t)))),
                };
                self.expect(Token::Colon)?;
                let ty = self.parse_type()?;
                params.push((pname, ty));
                if self.current == Token::Comma {
                    self.advance()?;
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        let ret = if self.current == Token::Arrow {
            self.advance()?;
            self.parse_type()?
        } else {
            Type::Unit
        };
        self.expect(Token::Semicolon)?;
        Ok(Stmt::ExternFn { name, params, ret })
    }

    fn parse_macro(&mut self) -> Result<Stmt, Diagnostic> {
        self.advance()?; // macro
        let name = match self.advance()? {
            Token::Ident(n) => n,
            t => return Err(self.error_at_prev(format!("expected macro name, found {}", describe(&t)))),
        };
        self.expect(Token::LParen)?;
        let mut params = Vec::new();
        while self.current != Token::RParen {
            match self.advance()? {
                Token::Ident(n) => params.push(n),
                t => return Err(self.error_at_prev(format!("expected macro param, found {}", describe(&t)))),
            }
            if self.current == Token::Comma {
                self.advance()?;
            }
        }
        self.expect(Token::RParen)?;
        self.expect(Token::LBrace)?;
        let mut body = Vec::new();
        while self.current != Token::RBrace {
            body.push(self.parse_stmt()?);
        }
        self.expect(Token::RBrace)?;
        Ok(Stmt::Macro { name, params, body })
    }

    fn parse_type(&mut self) -> Result<Type, Diagnostic> {
        Ok(match self.current.clone() {
            Token::Ident(n) => {
                self.advance()?;
                match n.as_str() {
                    "int" => Type::Int,
                    "float" => Type::Float,
                    "bool" => Type::Bool,
                    "string" => Type::String,
                    other => Type::Named(other.to_string()),
                }
            }
            Token::Fn => {
                self.advance()?;
                self.expect(Token::LParen)?;
                let mut params = Vec::new();
                if self.current != Token::RParen {
                    loop {
                        params.push(self.parse_type()?);
                        if self.current == Token::Comma {
                            self.advance()?;
                        } else {
                            break;
                        }
                    }
                }
                self.expect(Token::RParen)?;
                let ret = if self.current == Token::Arrow {
                    self.advance()?;
                    self.parse_type()?
                } else {
                    Type::Unit
                };
                Type::Fn(params, Box::new(ret))
            }
            t => return Err(self.error_at_prev(format!("expected type, found {}", describe(&t)))),
        })
    }

    pub fn parse_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.parse_range()
    }

    /// Binding power, loosest to tightest: `||` < `&&` < comparison < `+ -` < `* / %`.
    /// All levels are left-associative, hence the `prec + 1` on the right operand.
    fn parse_binop(&mut self, min_prec: u8) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_unary()?;
        loop {
            let (prec, op) = match self.current {
                Token::PipePipe => (1, BinOp::Or),
                Token::AmpAmp => (2, BinOp::And),
                Token::Eq => (3, BinOp::Eq),
                Token::Ne => (3, BinOp::Ne),
                Token::Lt => (3, BinOp::Lt),
                Token::Le => (3, BinOp::Le),
                Token::Gt => (3, BinOp::Gt),
                Token::Ge => (3, BinOp::Ge),
                Token::Plus => (4, BinOp::Add),
                Token::Minus => (4, BinOp::Sub),
                Token::Star => (5, BinOp::Mul),
                Token::Slash => (5, BinOp::Div),
                Token::Percent => (5, BinOp::Mod),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.advance()?;
            let right = self.parse_binop(prec + 1)?;
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn parse_range(&mut self) -> Result<Expr, Diagnostic> {
        let mut left = self.parse_binop(0)?;
        if self.current == Token::DotDot {
            self.advance()?;
            let right = self.parse_binop(0)?;
            left = Expr::Range(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, Diagnostic> {
        Ok(match self.current {
            Token::Minus => {
                self.advance()?;
                let expr = self.parse_unary()?;
                Expr::Unary(UnaryOp::Neg, Box::new(expr))
            }
            Token::Bang => {
                self.advance()?;
                let expr = self.parse_unary()?;
                Expr::Unary(UnaryOp::Not, Box::new(expr))
            }
            _ => self.parse_postfix()?,
        })
    }

    fn parse_postfix(&mut self) -> Result<Expr, Diagnostic> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.current {
                Token::Dot => {
                    self.advance()?;
                    let field = match self.advance()? {
                        Token::Ident(n) => n,
                        t => return Err(self.error_at_prev(format!("expected field/method name after '.', found {}", describe(&t)))),
                    };
                    if self.current == Token::LParen {
                        self.advance()?;
                        let mut args = Vec::new();
                        while self.current != Token::RParen {
                            args.push(self.parse_expr()?);
                            if self.current == Token::Comma {
                                self.advance()?;
                            }
                        }
                        self.expect(Token::RParen)?;
                        expr = Expr::MethodCall(Box::new(expr), field, args);
                    } else {
                        expr = Expr::FieldAccess(Box::new(expr), field);
                    }
                }
                Token::LBrack => {
                    self.advance()?;
                    let index = self.parse_expr()?;
                    self.expect(Token::RBrack)?;
                    expr = Expr::Index(Box::new(expr), Box::new(index));
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, Diagnostic> {
        Ok(match self.current.clone() {
            Token::Int(n) => { self.advance()?; Expr::Int(n) }
            Token::Float(f) => { self.advance()?; Expr::Float(f) }
            Token::Bool(b) => { self.advance()?; Expr::Bool(b) }
            Token::Str(s) => { self.advance()?; Expr::Str(s) }
            Token::InterpStr(s) => {
                let span = self.span;
                self.advance()?;
                self.parse_interp_string(s, span)?
            }
            Token::Match => self.parse_match()?,
            Token::If => self.parse_if_expr()?,
            Token::While => self.parse_while_expr()?,
            Token::For => self.parse_for_expr()?,
            Token::Ident(name) => {
                self.advance()?;
                if self.current == Token::LBrace
                    && name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                {
                    return Ok(self.parse_struct_lit(name)?);
                }
                if self.current == Token::LParen {
                    self.advance()?;
                    let mut args = Vec::new();
                    while self.current != Token::RParen {
                        args.push(self.parse_expr()?);
                        if self.current == Token::Comma {
                            self.advance()?;
                        }
                    }
                    self.expect(Token::RParen)?;
                    return Ok(Expr::Call(Box::new(Expr::Ident(name)), args));
                }
                Expr::Ident(name)
            }
            Token::LParen => {
                self.advance()?;
                let e = self.parse_expr()?;
                self.expect(Token::RParen)?;
                e
            }
            Token::LBrack => {
                self.advance()?;
                let mut elems = Vec::new();
                while self.current != Token::RBrack {
                    elems.push(self.parse_expr()?);
                    if self.current == Token::Comma {
                        self.advance()?;
                    }
                }
                self.expect(Token::RBrack)?;
                Expr::ArrayLit(elems)
            }
            Token::LBrace => {
                self.advance()?;
                let mut stmts = Vec::new();
                while self.current != Token::RBrace {
                    stmts.push(self.parse_stmt()?);
                }
                self.expect(Token::RBrace)?;
                if let Some(last) = stmts.pop() {
                    match last {
                        Stmt::Expr(e) | Stmt::Return(Some(e)) => e,
                        _ => Expr::Bool(false),
                    }
                } else {
                    Expr::Bool(false)
                }
            }
            t => return Err(self.error_at_prev(format!("unexpected token in expression: {}", describe(&t)))),
        })
    }

    fn parse_interp_string(&self, content: String, span: Span) -> Result<Expr, Diagnostic> {
        let mut parts: Vec<Expr> = Vec::new();
        let mut current_str = String::new();
        let mut chars = content.chars().peekable();
        while let Some(&ch) = chars.peek() {
            if ch == '{' {
                chars.next();
                if !current_str.is_empty() {
                    parts.push(Expr::Str(current_str.clone()));
                    current_str.clear();
                }
                let mut depth = 1u32;
                let mut expr_str = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '{' { depth += 1; }
                    if c == '}' {
                        depth -= 1;
                        if depth == 0 {
                            chars.next();
                            break;
                        }
                    }
                    expr_str.push(c);
                    chars.next();
                }
                let mut sub_parser = Parser::new(&expr_str)
                    .map_err(|e| Diagnostic::new(
                        format!("in interpolated expression `{}`: {}", expr_str.trim(), e.message), span))?;
                let expr = sub_parser.parse_expr()
                    .map_err(|e| Diagnostic::new(
                        format!("in interpolated expression `{}`: {}", expr_str.trim(), e.message), span))?;
                parts.push(Expr::Call(Box::new(Expr::Ident("to_string".to_string())), vec![expr]));
            } else {
                current_str.push(ch);
                chars.next();
            }
        }
        if !current_str.is_empty() {
            parts.push(Expr::Str(current_str));
        }
        Ok(if parts.is_empty() {
            Expr::Str(String::new())
        } else if parts.len() == 1 {
            parts.remove(0)
        } else {
            let mut result = parts.remove(0);
            for part in parts {
                result = Expr::Binary(
                    Box::new(result),
                    BinOp::Add,
                    Box::new(part),
                );
            }
            result
        })
    }

    fn parse_struct_lit(&mut self, name: String) -> Result<Expr, Diagnostic> {
        self.advance()?;
        let mut fields = Vec::new();
        while self.current != Token::RBrace {
            let fname = match self.advance()? {
                Token::Ident(n) => n,
                t => return Err(self.error_at_prev(format!("expected field name in struct literal, found {}", describe(&t)))),
            };
            self.expect(Token::Colon)?;
            let value = self.parse_expr()?;
            fields.push((fname, value));
            if self.current == Token::Comma {
                self.advance()?;
            }
        }
        self.expect(Token::RBrace)?;
        Ok(Expr::StructLit { name, fields })
    }

    fn parse_match(&mut self) -> Result<Expr, Diagnostic> {
        self.advance()?;
        let scrutinee = self.parse_expr()?;
        self.expect(Token::LBrace)?;
        let mut arms = Vec::new();
        while self.current != Token::RBrace {
            let mut patterns = vec![self.parse_pattern()?];
            while self.current == Token::Pipe {
                self.advance()?;
                patterns.push(self.parse_pattern()?);
            }
            let guard = if self.current == Token::If {
                self.advance()?;
                Some(self.parse_expr()?)
            } else {
                None
            };
            self.expect(Token::FatArrow)?;
            let body = self.parse_expr()?;
            arms.push((patterns, guard, body));
            if self.current == Token::Comma {
                self.advance()?;
            }
        }
        self.expect(Token::RBrace)?;
        Ok(Expr::Match { scrutinee: Box::new(scrutinee), arms })
    }

    fn parse_if_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.advance()?; // if
        let cond = self.parse_expr()?;
        self.expect(Token::LBrace)?;
        let mut stmts = Vec::new();
        while self.current != Token::RBrace {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(Token::RBrace)?;
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
            self.advance()?;
            if self.current == Token::If {
                Some(Box::new(self.parse_if_expr()?))
            } else {
                self.expect(Token::LBrace)?;
                let mut estmts = Vec::new();
                while self.current != Token::RBrace {
                    estmts.push(self.parse_stmt()?);
                }
                self.expect(Token::RBrace)?;
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
        Ok(Expr::If { cond: Box::new(cond), then_body: Box::new(then_body), else_body })
    }

    fn parse_while_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.advance()?; // while
        let cond = self.parse_expr()?;
        self.expect(Token::LBrace)?;
        let mut body = Vec::new();
        while self.current != Token::RBrace {
            body.push(self.parse_stmt()?);
        }
        self.expect(Token::RBrace)?;
        Ok(Expr::While { cond: Box::new(cond), body })
    }

    fn parse_for_expr(&mut self) -> Result<Expr, Diagnostic> {
        self.advance()?; // for
        let var = match self.advance()? {
            Token::Ident(n) => n,
            t => return Err(self.error_at_prev(format!("expected variable name after for, found {}", describe(&t)))),
        };
        self.expect(Token::In)?;
        let iter = self.parse_expr()?;
        self.expect(Token::LBrace)?;
        let mut body = Vec::new();
        while self.current != Token::RBrace {
            body.push(self.parse_stmt()?);
        }
        self.expect(Token::RBrace)?;
        Ok(Expr::For { var, iter: Box::new(iter), body })
    }

    fn parse_pattern(&mut self) -> Result<Pattern, Diagnostic> {
        Ok(match self.current.clone() {
            Token::Int(n) => { self.advance()?; Pattern::Literal(Expr::Int(n)) }
            Token::Str(s) => { self.advance()?; Pattern::Literal(Expr::Str(s)) }
            Token::Bool(b) => { self.advance()?; Pattern::Literal(Expr::Bool(b)) }
            Token::Ident(name) => {
                self.advance()?;
                if name == "_" {
                    return Ok(Pattern::Wildcard);
                }
                if self.current == Token::ColonColon {
                    self.advance()?;
                    if let Token::Ident(variant) = self.current.clone() {
                        self.advance()?;
                        let full_name = format!("{}::{}", name, variant);
                        if self.current == Token::LParen {
                            self.advance()?;
                            let mut inner = Vec::new();
                            while self.current != Token::RParen {
                                inner.push(self.parse_pattern()?);
                                if self.current == Token::Comma {
                                    self.advance()?;
                                }
                            }
                            self.expect(Token::RParen)?;
                            return Ok(Pattern::EnumVariant { name: full_name, inner });
                        }
                        return Ok(Pattern::EnumVariant { name: full_name, inner: Vec::new() });
                    }
                }
                if self.current == Token::LParen {
                    self.advance()?;
                    let mut inner = Vec::new();
                    while self.current != Token::RParen {
                        inner.push(self.parse_pattern()?);
                        if self.current == Token::Comma {
                            self.advance()?;
                        }
                    }
                    self.expect(Token::RParen)?;
                    return Ok(Pattern::EnumVariant { name, inner });
                }
                if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                    return Ok(Pattern::EnumVariant { name, inner: Vec::new() });
                }
                Pattern::Variable(name)
            }
            _ => return Err(self.error(format!("unexpected token in pattern: {}", describe(&self.current)))),
        })
    }
}

/// Human-readable name for a token, for use in error messages.
fn describe(tok: &Token) -> String {
    match tok {
        Token::Int(n) => format!("`{}`", n),
        Token::Float(f) => format!("`{}`", f),
        Token::Str(_) | Token::InterpStr(_) => "a string literal".to_string(),
        Token::Ident(name) => format!("`{}`", name),
        Token::Bool(b) => format!("`{}`", b),
        Token::Eof => "end of file".to_string(),
        other => format!("`{}`", symbol(other)),
    }
}

/// Source spelling of a fixed token.
fn symbol(tok: &Token) -> &'static str {
    match tok {
        Token::Let => "let", Token::Fn => "fn", Token::Return => "return",
        Token::Struct => "struct", Token::Enum => "enum", Token::Match => "match",
        Token::Trait => "trait", Token::Impl => "impl", Token::For => "for",
        Token::Extern => "extern", Token::Macro => "macro", Token::If => "if",
        Token::Else => "else", Token::While => "while", Token::Break => "break",
        Token::Continue => "continue", Token::In => "in", Token::Import => "import",
        Token::Colon => ":", Token::ColonColon => "::", Token::Dot => ".",
        Token::Plus => "+", Token::Minus => "-", Token::Star => "*",
        Token::Slash => "/", Token::Percent => "%", Token::Eq => "==",
        Token::Ne => "!=", Token::Lt => "<", Token::Le => "<=", Token::Gt => ">",
        Token::Ge => ">=", Token::Assign => "=", Token::LParen => "(",
        Token::RParen => ")", Token::LBrace => "{", Token::RBrace => "}",
        Token::LBrack => "[", Token::RBrack => "]", Token::Comma => ",",
        Token::Arrow => "->", Token::FatArrow => "=>", Token::DotDot => "..",
        Token::Bang => "!", Token::AmpAmp => "&&", Token::PipePipe => "||",
        Token::Pipe => "|", Token::Semicolon => ";",
        _ => "token",
    }
}
