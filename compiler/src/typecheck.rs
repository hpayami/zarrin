//! Type checker for the Zarrin language.
//!
//! Walks the AST, infers/verifies types, and reports errors.

use crate::ast::*;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeError {
    UndefinedVariable(String),
    UndefinedFunction(String),
    TypeMismatch { expected: String, found: String },
    NotAFunction(String),
    WrongArity { name: String, expected: usize, found: usize },
    CantInfer(String),
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeError::UndefinedVariable(n) => write!(f, "undefined variable: `{}`", n),
            TypeError::UndefinedFunction(n) => write!(f, "undefined function: `{}`", n),
            TypeError::TypeMismatch { expected, found } => {
                write!(f, "type mismatch: expected `{}`, found `{}`", expected, found)
            }
            TypeError::NotAFunction(n) => write!(f, "`{}` is not a function", n),
            TypeError::WrongArity { name, expected, found } => {
                write!(f, "`{}` expects {} args, found {}", name, expected, found)
            }
            TypeError::CantInfer(msg) => write!(f, "cannot infer type: {}", msg),
        }
    }
}

struct TypeEnv {
    scopes: Vec<HashMap<String, Type>>,
    functions: HashMap<String, (Vec<Type>, Type)>,
    current_return: Option<Type>,
}

impl TypeEnv {
    fn new() -> Self {
        TypeEnv {
            scopes: vec![HashMap::new()],
            functions: HashMap::new(),
            current_return: None,
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn define(&mut self, name: &str, ty: Type) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), ty);
    }

    fn lookup(&self, name: &str) -> Option<Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(t.clone());
            }
        }
        None
    }

    fn define_function(&mut self, name: &str, params: Vec<Type>, ret: Type) {
        self.functions.insert(name.to_string(), (params, ret));
    }

    fn lookup_function(&self, name: &str) -> Option<&(Vec<Type>, Type)> {
        self.functions.get(name)
    }
}

pub struct TypeChecker;

impl TypeChecker {
    pub fn check(program: &Program) -> Result<(), TypeError> {
        let mut env = TypeEnv::new();

        // First pass: register all function signatures.
        for s in &program.stmts {
            if let Stmt::Fn { name, params, ret, .. } = s {
                let param_tys: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
                env.define_function(name, param_tys, ret.clone());
            }
        }

        // Second pass: check each statement.
        for s in &program.stmts {
            Self::check_stmt(s, &mut env)?;
        }

        Ok(())
    }

    fn check_stmt(stmt: &Stmt, env: &mut TypeEnv) -> Result<(), TypeError> {
        match stmt {
            Stmt::Let { name, ty, value } => {
                let val_ty = Self::check_expr(value, env)?;
                if *ty != Type::Inferred && *ty != val_ty {
                    return Err(TypeError::TypeMismatch {
                        expected: format!("{:?}", ty),
                        found: format!("{:?}", val_ty),
                    });
                }
                env.define(name, val_ty);
            }
            Stmt::Fn { name, params, ret, body } => {
                env.push_scope();
                let prev_return = env.current_return.clone();
                env.current_return = Some(ret.clone());

                for (pname, pty) in params {
                    env.define(pname, pty.clone());
                }

                for s in body {
                    Self::check_stmt(s, env)?;
                }

                env.current_return = prev_return;
                env.pop_scope();
            }
            Stmt::Expr(e) => {
                Self::check_expr(e, env)?;
            }
            Stmt::Return(e) => {
                let ret_ty = env.current_return.clone().unwrap_or(Type::Unit);
                match e {
                    Some(expr) => {
                        let expr_ty = Self::check_expr(expr, env)?;
                        if ret_ty != expr_ty {
                            return Err(TypeError::TypeMismatch {
                                expected: format!("{:?}", ret_ty),
                                found: format!("{:?}", expr_ty),
                            });
                        }
                    }
                    None => {
                        if ret_ty != Type::Unit {
                            return Err(TypeError::TypeMismatch {
                                expected: format!("{:?}", ret_ty),
                                found: "Unit".into(),
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn check_expr(expr: &Expr, env: &mut TypeEnv) -> Result<Type, TypeError> {
        match expr {
            Expr::Int(_) => Ok(Type::Int),
            Expr::Float(_) => Ok(Type::Float),
            Expr::Bool(_) => Ok(Type::Bool),
            Expr::Str(_) => Ok(Type::String),
            Expr::Ident(name) => {
                env.lookup(name)
                    .ok_or_else(|| TypeError::UndefinedVariable(name.clone()))
            }
            Expr::Binary(l, op, r) => {
                let lt = Self::check_expr(l, env)?;
                let rt = Self::check_expr(r, env)?;
                if lt != rt {
                    return Err(TypeError::TypeMismatch {
                        expected: format!("{:?}", lt),
                        found: format!("{:?}", rt),
                    });
                }
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => Ok(lt),
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt => Ok(Type::Bool),
                }
            }
            Expr::Call(callee, args) => {
                let func_name = match callee.as_ref() {
                    Expr::Ident(n) => n,
                    _ => return Err(TypeError::NotAFunction("non-identifier call".into())),
                };

                // Builtin: print accepts any type.
                if func_name == "print" {
                    if args.len() != 1 {
                        return Err(TypeError::WrongArity {
                            name: "print".into(),
                            expected: 1,
                            found: args.len(),
                        });
                    }
                    Self::check_expr(&args[0], env)?;
                    return Ok(Type::Unit);
                }

                let (param_tys, ret_ty) = env
                    .lookup_function(func_name)
                    .ok_or_else(|| TypeError::UndefinedFunction(func_name.clone()))?
                    .clone();

                if param_tys.len() != args.len() {
                    return Err(TypeError::WrongArity {
                        name: func_name.clone(),
                        expected: param_tys.len(),
                        found: args.len(),
                    });
                }

                for (arg, expected) in args.iter().zip(param_tys.iter()) {
                    let arg_ty = Self::check_expr(arg, env)?;
                    if arg_ty != *expected {
                        return Err(TypeError::TypeMismatch {
                            expected: format!("{:?}", expected),
                            found: format!("{:?}", arg_ty),
                        });
                    }
                }

                Ok(ret_ty)
            }
        }
    }
}
