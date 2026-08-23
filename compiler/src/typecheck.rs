//! Type checker for the Zarrin language.

use crate::ast::*;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeError {
    UndefinedVariable(String),
    UndefinedFunction(String),
    UndefinedType(String),
    UndefinedTrait(String),
    TypeMismatch { expected: String, found: String },
    NotAFunction(String),
    WrongArity { name: String, expected: usize, found: usize },
    UnknownField { ty: String, field: String },
    MissingImpl { trait_name: String, type_name: String, method: String },
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeError::UndefinedVariable(n) => write!(f, "undefined variable: `{}`", n),
            TypeError::UndefinedFunction(n) => write!(f, "undefined function: `{}`", n),
            TypeError::UndefinedType(n) => write!(f, "undefined type: `{}`", n),
            TypeError::UndefinedTrait(n) => write!(f, "undefined trait: `{}`", n),
            TypeError::TypeMismatch { expected, found } => write!(f, "type mismatch: expected `{}`, found `{}`", expected, found),
            TypeError::NotAFunction(n) => write!(f, "`{}` is not a function", n),
            TypeError::WrongArity { name, expected, found } => write!(f, "`{}` expects {} args, found {}", name, expected, found),
            TypeError::UnknownField { ty, field } => write!(f, "type `{}` has no field `{}`", ty, field),
            TypeError::MissingImpl { trait_name, type_name, method } => write!(f, "trait `{}` for `{}` missing method `{}`", trait_name, type_name, method),
        }
    }
}

struct TypeEnv {
    scopes: Vec<HashMap<String, Type>>,
    functions: HashMap<String, (Vec<Type>, Type)>,
    structs: HashMap<String, Vec<(String, Type)>>,
    enums: HashMap<String, Vec<(String, Vec<Type>)>>,
    traits: HashMap<String, Vec<TraitMethod>>,
    impls: Vec<(String, String, Vec<Stmt>)>,
    extern_fns: HashMap<String, (Vec<Type>, Type)>,
    current_return: Option<Type>,
}

impl TypeEnv {
    fn new() -> Self {
        TypeEnv {
            scopes: vec![HashMap::new()],
            functions: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::new(),
            traits: HashMap::new(),
            impls: Vec::new(),
            extern_fns: HashMap::new(),
            current_return: None,
        }
    }

    fn push_scope(&mut self) { self.scopes.push(HashMap::new()); }
    fn pop_scope(&mut self) { self.scopes.pop(); }

    fn define(&mut self, name: &str, ty: Type) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), ty);
    }

    fn lookup(&self, name: &str) -> Option<Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) { return Some(t.clone()); }
        }
        None
    }

    fn lookup_method(&self, type_name: &str, method_name: &str) -> Option<(Vec<(String, Type)>, Type)> {
        for (_, type_name_impl, methods) in &self.impls {
            if type_name_impl == type_name {
                for m in methods {
                    if let Stmt::Fn { name, params, ret, .. } = m {
                        if name == method_name {
                            return Some((params.clone(), ret.clone()));
                        }
                    }
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

        for s in &program.stmts {
            match s {
                Stmt::Fn { name, params, ret, .. } => {
                    let param_tys: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
                    env.functions.insert(name.clone(), (param_tys, ret.clone()));
                }
                Stmt::Struct { name, fields, .. } => {
                    env.structs.insert(name.clone(), fields.clone());
                }
                Stmt::Enum { name, variants } => {
                    env.enums.insert(name.clone(), variants.clone());
                }
                Stmt::Trait { name, methods } => {
                    env.traits.insert(name.clone(), methods.clone());
                }
                Stmt::ExternFn { name, params, ret } => {
                    let param_tys: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
                    env.extern_fns.insert(name.clone(), (param_tys, ret.clone()));
                }
                Stmt::Impl { .. } => {}
                Stmt::Macro { .. } => {}
                _ => {}
            }
        }

        for s in &program.stmts {
            if let Stmt::Impl { trait_name, type_name, methods } = s {
                if !env.traits.contains_key(trait_name) {
                    return Err(TypeError::UndefinedTrait(trait_name.clone()));
                }
                let trait_methods = env.traits.get(trait_name).unwrap().clone();
                for tm in &trait_methods {
                    let found = methods.iter().any(|m| {
                        if let Stmt::Fn { name, .. } = m { name == &tm.name } else { false }
                    });
                    if !found {
                        return Err(TypeError::MissingImpl {
                            trait_name: trait_name.clone(),
                            type_name: type_name.clone(),
                            method: tm.name.clone(),
                        });
                    }
                }
                env.impls.push((trait_name.clone(), type_name.clone(), methods.clone()));
            }
        }

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
                    return Err(TypeError::TypeMismatch { expected: format!("{:?}", ty), found: format!("{:?}", val_ty) });
                }
                env.define(name, val_ty);
            }
            Stmt::Fn { params, ret, body, .. } => {
                env.push_scope();
                let prev = env.current_return.clone();
                env.current_return = Some(ret.clone());
                for (pname, pty) in params { env.define(pname, pty.clone()); }
                for s in body { Self::check_stmt(s, env)?; }
                env.current_return = prev;
                env.pop_scope();
            }
            Stmt::Struct { .. } | Stmt::Enum { .. } | Stmt::Trait { .. } | Stmt::Macro { .. } | Stmt::ExternFn { .. } | Stmt::Impl { .. } | Stmt::Import(_) => {}
            Stmt::While { cond, body } => {
                Self::check_expr(cond, env)?;
                for s in body { Self::check_stmt(s, env)?; }
            }
            Stmt::For { var, iter, body } => {
                Self::check_expr(iter, env)?;
                env.push_scope();
                env.define(var, Type::Int);
                for s in body { Self::check_stmt(s, env)?; }
                env.pop_scope();
            }
            Stmt::Break(_) => {}
            Stmt::Continue(_) => {}
            Stmt::Assign { name, value } => {
                let val_ty = Self::check_expr(value, env)?;
                if let Some(var_ty) = env.lookup(name) {
                    if var_ty != val_ty {
                        return Err(TypeError::TypeMismatch { expected: format!("{:?}", var_ty), found: format!("{:?}", val_ty) });
                    }
                }
            }
            Stmt::If { cond, then_body, else_body } => {
                Self::check_expr(cond, env)?;
                for s in then_body { Self::check_stmt(s, env)?; }
                if let Some(eb) = else_body {
                    for s in eb { Self::check_stmt(s, env)?; }
                }
            }
            Stmt::Expr(e) => { Self::check_expr(e, env)?; }
            Stmt::Return(e) => {
                let ret_ty = env.current_return.clone().unwrap_or(Type::Unit);
                match e {
                    Some(expr) => {
                        let expr_ty = Self::check_expr(expr, env)?;
                        if ret_ty != expr_ty {
                            return Err(TypeError::TypeMismatch { expected: format!("{:?}", ret_ty), found: format!("{:?}", expr_ty) });
                        }
                    }
                    None => {
                        if ret_ty != Type::Unit {
                            return Err(TypeError::TypeMismatch { expected: format!("{:?}", ret_ty), found: "Unit".into() });
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
                if let Some((_, variants)) = env.enums.iter().find(|(_, v)| v.iter().any(|(vn, _)| vn == name)) {
                    let enum_name = env.enums.iter().find(|(_, v)| v.iter().any(|(vn, _)| vn == name)).map(|(n, _)| n.clone()).unwrap();
                    let _ = variants;
                    return Ok(Type::Named(enum_name));
                }
                env.lookup(name).ok_or_else(|| TypeError::UndefinedVariable(name.clone()))
            }
            Expr::Binary(l, op, r) => {
                let lt = Self::check_expr(l, env)?;
                let rt = Self::check_expr(r, env)?;
                if lt != rt { return Err(TypeError::TypeMismatch { expected: format!("{:?}", lt), found: format!("{:?}", rt) }); }
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => Ok(lt),
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Ok(Type::Bool),
                    BinOp::And | BinOp::Or => Ok(Type::Bool),
                }
            }
            Expr::Unary(op, e) => {
                let et = Self::check_expr(e, env)?;
                match op {
                    UnaryOp::Neg => {
                        if et != Type::Int && et != Type::Float {
                            return Err(TypeError::TypeMismatch { expected: "int or float".into(), found: format!("{:?}", et) });
                        }
                        Ok(et)
                    }
                    UnaryOp::Not => {
                        if et != Type::Bool && et != Type::Int {
                            return Err(TypeError::TypeMismatch { expected: "bool or int".into(), found: format!("{:?}", et) });
                        }
                        Ok(Type::Bool)
                    }
                }
            }
            Expr::Call(callee, args) => {
                let func_name = match callee.as_ref() {
                    Expr::Ident(n) => n,
                    _ => return Err(TypeError::NotAFunction("non-identifier call".into())),
                };
                if func_name == "print" {
                    if args.len() != 1 { return Err(TypeError::WrongArity { name: "print".into(), expected: 1, found: args.len() }); }
                    Self::check_expr(&args[0], env)?;
                    return Ok(Type::Unit);
                }
                if func_name == "substring" {
                    if args.len() != 3 { return Err(TypeError::WrongArity { name: "substring".into(), expected: 3, found: args.len() }); }
                    for a in args.iter() { Self::check_expr(a, env)?; }
                    return Ok(Type::String);
                }
                if func_name == "contains" {
                    if args.len() != 2 { return Err(TypeError::WrongArity { name: "contains".into(), expected: 2, found: args.len() }); }
                    for a in args.iter() { Self::check_expr(a, env)?; }
                    return Ok(Type::Bool);
                }
                if func_name == "split" {
                    if args.len() != 2 { return Err(TypeError::WrongArity { name: "split".into(), expected: 2, found: args.len() }); }
                    for a in args.iter() { Self::check_expr(a, env)?; }
                    return Ok(Type::Array(Box::new(Type::String)));
                }
                if func_name == "trim" {
                    if args.len() != 1 { return Err(TypeError::WrongArity { name: "trim".into(), expected: 1, found: args.len() }); }
                    Self::check_expr(&args[0], env)?;
                    return Ok(Type::String);
                }
                if func_name == "char_at" {
                    if args.len() != 2 { return Err(TypeError::WrongArity { name: "char_at".into(), expected: 2, found: args.len() }); }
                    for a in args.iter() { Self::check_expr(a, env)?; }
                    return Ok(Type::String);
                }
                if let Some(variant_args) = env.enums.values().find_map(|v| v.iter().find(|(vn, _)| vn == func_name).map(|(_, a)| a.clone())) {
                    let enum_name = env.enums.iter().find(|(_, v)| v.iter().any(|(vn, _)| vn == func_name)).map(|(n, _)| n.clone()).unwrap();
                    if variant_args.len() != args.len() { return Err(TypeError::WrongArity { name: func_name.clone(), expected: variant_args.len(), found: args.len() }); }
                    for (arg, expected) in args.iter().zip(variant_args.iter()) {
                        let arg_ty = Self::check_expr(arg, env)?;
                        if arg_ty != *expected { return Err(TypeError::TypeMismatch { expected: format!("{:?}", expected), found: format!("{:?}", arg_ty) }); }
                    }
                    return Ok(Type::Named(enum_name));
                }
                if let Some((param_tys, ret_ty)) = env.functions.get(func_name).cloned().or_else(|| env.extern_fns.get(func_name).cloned()) {
                    if param_tys.len() != args.len() { return Err(TypeError::WrongArity { name: func_name.clone(), expected: param_tys.len(), found: args.len() }); }
                    for (arg, expected) in args.iter().zip(param_tys.iter()) {
                        let arg_ty = Self::check_expr(arg, env)?;
                        if arg_ty != *expected { return Err(TypeError::TypeMismatch { expected: format!("{:?}", expected), found: format!("{:?}", arg_ty) }); }
                    }
                    return Ok(ret_ty);
                }
                Err(TypeError::UndefinedFunction(func_name.clone()))
            }
            Expr::MethodCall(obj, method, args) => {
                let obj_ty = Self::check_expr(obj, env)?;
                let type_name = match &obj_ty {
                    Type::Named(n) => n.clone(),
                    _ => return Err(TypeError::UnknownField { ty: format!("{:?}", obj_ty), field: method.clone() }),
                };
                if let Some((params, ret)) = env.lookup_method(&type_name, method) {
                    let self_count = if params.first().map(|(n, _)| n == "self" || n == "&self" || n == "&mut self").unwrap_or(false) { 1 } else { 0 };
                    let expected_args = params.len() - self_count;
                    if expected_args != args.len() { return Err(TypeError::WrongArity { name: method.clone(), expected: expected_args, found: args.len() }); }
                    for (arg, (_, pty)) in args.iter().zip(params[self_count..].iter()) {
                        let arg_ty = Self::check_expr(arg, env)?;
                        if arg_ty != *pty { return Err(TypeError::TypeMismatch { expected: format!("{:?}", pty), found: format!("{:?}", arg_ty) }); }
                    }
                    return Ok(ret);
                }
                Err(TypeError::UnknownField { ty: type_name, field: method.clone() })
            }
            Expr::FieldAccess(obj, field) => {
                let obj_ty = Self::check_expr(obj, env)?;
                match &obj_ty {
                    Type::Named(name) => {
                        if let Some(fields) = env.structs.get(name) {
                            fields.iter().find(|(fname, _)| fname == field)
                                .map(|(_, fty)| fty.clone())
                                .ok_or_else(|| TypeError::UnknownField { ty: name.clone(), field: field.clone() })
                        } else {
                            Err(TypeError::UnknownField { ty: name.clone(), field: field.clone() })
                        }
                    }
                    _ => Err(TypeError::UnknownField { ty: format!("{:?}", obj_ty), field: field.clone() }),
                }
            }
            Expr::StructLit { name, fields } => {
                let sdef = env.structs.get(name).cloned()
                    .ok_or_else(|| TypeError::UndefinedType(name.clone()))?;
                if sdef.len() != fields.len() { return Err(TypeError::WrongArity { name: name.clone(), expected: sdef.len(), found: fields.len() }); }
                for ((_, fty), (_, expr)) in sdef.iter().zip(fields.iter()) {
                    let expr_ty = Self::check_expr(expr, env)?;
                    if *fty != expr_ty { return Err(TypeError::TypeMismatch { expected: format!("{:?}", fty), found: format!("{:?}", expr_ty) }); }
                }
                Ok(Type::Named(name.clone()))
            }
            Expr::Match { scrutinee, arms } => {
                let scrutinee_ty = Self::check_expr(scrutinee, env)?;
                let mut result_ty = None;
                for (patterns, guard, body) in arms {
                    for pattern in patterns {
                        Self::check_pattern(pattern, &scrutinee_ty, env)?;
                    }
                    if let Some(g) = guard {
                        Self::check_expr(g, env)?;
                    }
                    let body_ty = Self::check_expr(body, env)?;
                    if let Some(prev) = &result_ty {
                        if *prev != body_ty { return Err(TypeError::TypeMismatch { expected: format!("{:?}", prev), found: format!("{:?}", body_ty) }); }
                    } else {
                        result_ty = Some(body_ty);
                    }
                }
                Ok(result_ty.unwrap_or(Type::Unit))
            }
            Expr::If { cond, then_body, else_body } => {
                Self::check_expr(cond, env)?;
                let then_ty = Self::check_expr(then_body, env)?;
                if let Some(eb) = else_body {
                    let else_ty = Self::check_expr(eb, env)?;
                    if then_ty != else_ty { return Err(TypeError::TypeMismatch { expected: format!("{:?}", then_ty), found: format!("{:?}", else_ty) }); }
                    Ok(then_ty)
                } else {
                    Ok(Type::Unit)
                }
            }
            Expr::While { cond, body } => {
                Self::check_expr(cond, env)?;
                for s in body { Self::check_stmt(s, env)?; }
                Ok(Type::Int)
            }
            Expr::For { var, iter, body } => {
                Self::check_expr(iter, env)?;
                env.push_scope();
                env.define(var, Type::Int);
                for s in body { Self::check_stmt(s, env)?; }
                env.pop_scope();
                Ok(Type::Int)
            }
            Expr::Range(a, b) => {
                let at = Self::check_expr(a, env)?;
                let bt = Self::check_expr(b, env)?;
                if at != Type::Int || bt != Type::Int {
                    return Err(TypeError::TypeMismatch { expected: "int".into(), found: format!("{:?}", if at != Type::Int { at } else { bt }) });
                }
                Ok(Type::Int)
            }
            Expr::ArrayLit(elems) => {
                if elems.is_empty() {
                    Ok(Type::Array(Box::new(Type::Inferred)))
                } else {
                    let elem_ty = Self::check_expr(&elems[0], env)?;
                    for e in &elems[1..] {
                        let et = Self::check_expr(e, env)?;
                        if et != elem_ty { return Err(TypeError::TypeMismatch { expected: format!("{:?}", elem_ty), found: format!("{:?}", et) }); }
                    }
                    Ok(Type::Array(Box::new(elem_ty)))
                }
            }
            Expr::Index(arr, idx) => {
                let arr_ty = Self::check_expr(arr, env)?;
                let idx_ty = Self::check_expr(idx, env)?;
                if idx_ty != Type::Int { return Err(TypeError::TypeMismatch { expected: "int".into(), found: format!("{:?}", idx_ty) }); }
                match arr_ty {
                    Type::Array(et) => Ok(*et),
                    _ => Err(TypeError::TypeMismatch { expected: "array".into(), found: format!("{:?}", arr_ty) }),
                }
            }
        }
    }

    fn check_pattern(pattern: &Pattern, expected_ty: &Type, env: &mut TypeEnv) -> Result<(), TypeError> {
        match pattern {
            Pattern::Literal(expr) => {
                let pat_ty = Self::check_expr(expr, env)?;
                if pat_ty != *expected_ty { return Err(TypeError::TypeMismatch { expected: format!("{:?}", expected_ty), found: format!("{:?}", pat_ty) }); }
                Ok(())
            }
            Pattern::Variable(name) => { env.define(name, expected_ty.clone()); Ok(()) }
            Pattern::Wildcard => Ok(()),
            Pattern::EnumVariant { name, inner } => {
                let enum_name = env.enums.iter().find(|(_, v)| v.iter().any(|(vn, _)| vn == name)).map(|(n, _)| n.clone());
                if let Some(en) = enum_name {
                    if *expected_ty != Type::Named(en) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", expected_ty), found: format!("Enum {}", name) }); }
                    let variant_args = env.enums.iter().find_map(|(_, v)| v.iter().find(|(vn, _)| vn == name).map(|(_, a)| a.clone())).unwrap();
                    if variant_args.len() != inner.len() { return Err(TypeError::WrongArity { name: name.clone(), expected: variant_args.len(), found: inner.len() }); }
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
