//! Type checker for the Zarrin language.

use crate::ast::*;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeError {
    UndefinedVariable(String),
    UndefinedFunction(String),
    UndefinedType(String),
    TypeMismatch { expected: String, found: String },
    NotAFunction(String),
    WrongArity { name: String, expected: usize, found: usize },
    UnknownField { ty: String, field: String },
    CantInfer(String),
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeError::UndefinedVariable(n) => write!(f, "undefined variable: `{}`", n),
            TypeError::UndefinedFunction(n) => write!(f, "undefined function: `{}`", n),
            TypeError::UndefinedType(n) => write!(f, "undefined type: `{}`", n),
            TypeError::TypeMismatch { expected, found } => {
                write!(f, "type mismatch: expected `{}`, found `{}`", expected, found)
            }
            TypeError::NotAFunction(n) => write!(f, "`{}` is not a function", n),
            TypeError::WrongArity { name, expected, found } => {
                write!(f, "`{}` expects {} args, found {}", name, expected, found)
            }
            TypeError::UnknownField { ty, field } => {
                write!(f, "type `{}` has no field `{}`", ty, field)
            }
            TypeError::CantInfer(msg) => write!(f, "cannot infer type: {}", msg),
        }
    }
}

#[derive(Debug, Clone)]
struct StructDef {
    fields: Vec<(String, Type)>,
}

#[derive(Debug, Clone)]
struct EnumDef {
    variants: Vec<(String, Vec<Type>)>,
}

struct TypeEnv {
    scopes: Vec<HashMap<String, Type>>,
    functions: HashMap<String, (Vec<Type>, Type)>,
    structs: HashMap<String, StructDef>,
    enums: HashMap<String, EnumDef>,
    current_return: Option<Type>,
}

impl TypeEnv {
    fn new() -> Self {
        TypeEnv {
            scopes: vec![HashMap::new()],
            functions: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::new(),
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

    fn define_struct(&mut self, name: &str, fields: Vec<(String, Type)>) {
        self.structs.insert(name.to_string(), StructDef { fields });
    }

    fn lookup_struct(&self, name: &str) -> Option<&StructDef> {
        self.structs.get(name)
    }

    fn define_enum(&mut self, name: &str, variants: Vec<(String, Vec<Type>)>) {
        self.enums.insert(name.to_string(), EnumDef { variants });
    }

    fn lookup_enum_variant(&self, variant_name: &str) -> Option<(&str, &Vec<Type>)> {
        for (enum_name, def) in &self.enums {
            for (vname, args) in &def.variants {
                if vname == variant_name {
                    return Some((enum_name, args));
                }
            }
        }
        None
    }
}

pub struct TypeChecker;

impl TypeChecker {
    pub fn check(program: &Program) -> Result<(), TypeError> {
        let mut env = TypeEnv::new();

        // First pass: register all definitions.
        for s in &program.stmts {
            match s {
                Stmt::Fn { name, params, ret, .. } => {
                    let param_tys: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
                    env.define_function(name, param_tys, ret.clone());
                }
                Stmt::Struct { name, fields } => {
                    env.define_struct(name, fields.clone());
                }
                Stmt::Enum { name, variants } => {
                    env.define_enum(name, variants.clone());
                }
                _ => {}
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
            Stmt::Fn { params, ret, body, .. } => {
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
            Stmt::Struct { .. } | Stmt::Enum { .. } => {
                // Already registered in first pass.
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
                // Check if it's an enum variant constructor (no args).
                if let Some((enum_name, _)) = env.lookup_enum_variant(name) {
                    return Ok(Type::Named(enum_name.to_string()));
                }
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

                // Check if it's an enum variant constructor.
                if let Some((enum_name, variant_args)) = env.lookup_enum_variant(func_name).map(|(a, b)| (a.to_string(), b.clone())) {
                    if variant_args.len() != args.len() {
                        return Err(TypeError::WrongArity {
                            name: func_name.clone(),
                            expected: variant_args.len(),
                            found: args.len(),
                        });
                    }
                    for (arg, expected) in args.iter().zip(variant_args.iter()) {
                        let arg_ty = Self::check_expr(arg, env)?;
                        if arg_ty != *expected {
                            return Err(TypeError::TypeMismatch {
                                expected: format!("{:?}", expected),
                                found: format!("{:?}", arg_ty),
                            });
                        }
                    }
                    return Ok(Type::Named(enum_name.to_string()));
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
            Expr::FieldAccess(obj, field) => {
                let obj_ty = Self::check_expr(obj, env)?;
                match &obj_ty {
                    Type::Named(name) => {
                        if let Some(sdef) = env.lookup_struct(name) {
                            sdef.fields.iter()
                                .find(|(fname, _)| fname == field)
                                .map(|(_, fty)| fty.clone())
                                .ok_or_else(|| TypeError::UnknownField {
                                    ty: name.clone(),
                                    field: field.clone(),
                                })
                        } else {
                            Err(TypeError::UnknownField {
                                ty: name.clone(),
                                field: field.clone(),
                            })
                        }
                    }
                    _ => Err(TypeError::UnknownField {
                        ty: format!("{:?}", obj_ty),
                        field: field.clone(),
                    }),
                }
            }
            Expr::StructLit { name, fields } => {
                let sdef = env.lookup_struct(name)
                    .ok_or_else(|| TypeError::UndefinedType(name.clone()))?
                    .clone();
                if sdef.fields.len() != fields.len() {
                    return Err(TypeError::WrongArity {
                        name: name.clone(),
                        expected: sdef.fields.len(),
                        found: fields.len(),
                    });
                }
                for ((fname, fty), (_, expr)) in sdef.fields.iter().zip(fields.iter()) {
                    let expr_ty = Self::check_expr(expr, env)?;
                    if *fty != expr_ty {
                        return Err(TypeError::TypeMismatch {
                            expected: format!("{:?}", fty),
                            found: format!("{:?}", expr_ty),
                        });
                    }
                }
                Ok(Type::Named(name.clone()))
            }
            Expr::Match { scrutinee, arms } => {
                let scrutinee_ty = Self::check_expr(scrutinee, env)?;
                let mut result_ty = None;
                for (pattern, body) in arms {
                    Self::check_pattern(pattern, &scrutinee_ty, env)?;
                    let body_ty = Self::check_expr(body, env)?;
                    if let Some(prev) = &result_ty {
                        if *prev != body_ty {
                            return Err(TypeError::TypeMismatch {
                                expected: format!("{:?}", prev),
                                found: format!("{:?}", body_ty),
                            });
                        }
                    } else {
                        result_ty = Some(body_ty);
                    }
                }
                Ok(result_ty.unwrap_or(Type::Unit))
            }
        }
    }

    fn check_pattern(pattern: &Pattern, expected_ty: &Type, env: &mut TypeEnv) -> Result<(), TypeError> {
        match pattern {
            Pattern::Literal(expr) => {
                let pat_ty = Self::check_expr(expr, env)?;
                if pat_ty != *expected_ty {
                    return Err(TypeError::TypeMismatch {
                        expected: format!("{:?}", expected_ty),
                        found: format!("{:?}", pat_ty),
                    });
                }
                Ok(())
            }
            Pattern::Variable(name) => {
                env.define(name, expected_ty.clone());
                Ok(())
            }
            Pattern::Wildcard => Ok(()),
            Pattern::EnumVariant { name, inner } => {
                if let Some((enum_name, variant_args)) = env.lookup_enum_variant(name).map(|(a, b)| (a.to_string(), b.clone())) {
                    if *expected_ty != Type::Named(enum_name.clone()) {
                        return Err(TypeError::TypeMismatch {
                            expected: format!("{:?}", expected_ty),
                            found: format!("{:?}", Type::Named(enum_name)),
                        });
                    }
                    if variant_args.len() != inner.len() {
                        return Err(TypeError::WrongArity {
                            name: name.clone(),
                            expected: variant_args.len(),
                            found: inner.len(),
                        });
                    }
                    for (pat, arg_ty) in inner.iter().zip(variant_args.iter()) {
                        Self::check_pattern(pat, arg_ty, env)?;
                    }
                    Ok(())
                } else {
                    Err(TypeError::UndefinedType(name.clone()))
                }
            }
        }
    }
}
