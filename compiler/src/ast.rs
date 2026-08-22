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
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Literal(Expr),
    Variable(String),
    Wildcard,
    EnumVariant {
        name: String,
        inner: Vec<Pattern>,
    },
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
    FieldAccess(Box<Expr>, String),
    MethodCall(Box<Expr>, String, Vec<Expr>),
    StructLit {
        name: String,
        fields: Vec<(String, Expr)>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<(Pattern, Expr)>,
    },
    If {
        cond: Box<Expr>,
        then_body: Box<Expr>,
        else_body: Option<Box<Expr>>,
    },
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
        generics: Vec<String>,
        params: Vec<(String, Type)>,
        ret: Type,
        body: Vec<Stmt>,
    },
    Struct {
        name: String,
        generics: Vec<String>,
        fields: Vec<(String, Type)>,
    },
    Enum {
        name: String,
        variants: Vec<(String, Vec<Type>)>,
    },
    Trait {
        name: String,
        methods: Vec<TraitMethod>,
    },
    Impl {
        trait_name: String,
        type_name: String,
        methods: Vec<Stmt>,
    },
    ExternFn {
        name: String,
        params: Vec<(String, Type)>,
        ret: Type,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    Break,
    Assign {
        name: String,
        value: Expr,
    },
    If {
        cond: Expr,
        then_body: Vec<Stmt>,
        else_body: Option<Vec<Stmt>>,
    },
    Macro {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    Expr(Expr),
    Return(Option<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraitMethod {
    pub name: String,
    pub params: Vec<(String, Type)>,
    pub ret: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}
