//! Abstract Syntax Tree for the Zarrin language.

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Inferred,
    Int,
    Float,
    Bool,
    String,
    Unit,
    Named(String),
    Fn(Vec<Type>, Box<Type>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Gt,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Ident(String),
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let {
        name: String,
        ty: Type,
        value: Expr,
    },
    Fn {
        name: String,
        params: Vec<(String, Type)>,
        ret: Type,
        body: Vec<Stmt>,
    },
    Expr(Expr),
    Return(Option<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}
