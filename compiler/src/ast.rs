//! Abstract Syntax Tree for the Zarrin language.

use crate::diagnostic::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Inferred,
    Int,
    Float,
    Bool,
    String,
    Unit,
    /// A declared type by name, with whatever type arguments were worked out
    /// for it. `Option` on its own carries none — its payload is unknown —
    /// while `Some(1.5)` produces `Option<float>`.
    Named(String, Vec<Type>),
    Fn(Vec<Type>, Box<Type>),
    Array(Box<Type>),
}

/// Types appear in diagnostics, so they print the way they are written in
/// source — `[int]`, `Option<float>` — rather than as the Rust enum that holds
/// them, which is what a reader was previously shown.
impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Inferred => write!(f, "unknown"),
            Type::Int => write!(f, "int"),
            Type::Float => write!(f, "float"),
            Type::Bool => write!(f, "bool"),
            Type::String => write!(f, "string"),
            Type::Unit => write!(f, "()"),
            Type::Named(n, args) if args.is_empty() => write!(f, "{}", n),
            Type::Named(n, args) => {
                let parts: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                write!(f, "{}<{}>", n, parts.join(", "))
            }
            Type::Array(el) => write!(f, "[{}]", el),
            Type::Fn(args, ret) => {
                let parts: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                write!(f, "fn({}) -> {}", parts.join(", "), ret)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
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

/// An expression and where it came from. Errors used to be reported against
/// the whole statement; with this they can name the subexpression at fault.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: Box<ExprKind>,
    pub span: Span,
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr { kind: Box::new(kind), span }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Ident(String),
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Range(Box<Expr>, Box<Expr>),
    ArrayLit(Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    FieldAccess(Box<Expr>, String),
    MethodCall(Box<Expr>, String, Vec<Expr>),
    StructLit {
        name: String,
        fields: Vec<(String, Expr)>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<(Vec<Pattern>, Option<Expr>, Expr)>,
    },
    If {
        cond: Box<Expr>,
        then_body: Box<Expr>,
        else_body: Option<Box<Expr>>,
    },
    While {
        cond: Box<Expr>,
        body: Vec<Stmt>,
    },
    For {
        var: String,
        iter: Box<Expr>,
        body: Vec<Stmt>,
    },
}

/// A statement together with where it came from. Errors found after parsing —
/// type errors, and failures at run time — are reported against the statement
/// being processed, which is the granularity the AST records.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

impl Stmt {
    pub fn new(kind: StmtKind, span: Span) -> Self {
        Stmt { kind, span }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
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
    For {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
    },
    Break(Option<Expr>),
    Continue(Option<Expr>),
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
    Import(String),
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
