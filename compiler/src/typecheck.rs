//! Type checker for the Zarrin language.

use crate::ast::*;
use crate::builtins;
use crate::diagnostic::Diagnostic;
use crate::variants::{Lookup, VariantIndex};
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
    AmbiguousVariant { name: String, candidates: Vec<String> },
    /// Raised inside a nested statement, which already knows where it is.
    Located(Box<Diagnostic>),
}

impl From<Diagnostic> for TypeError {
    fn from(d: Diagnostic) -> Self {
        TypeError::Located(Box::new(d))
    }
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
            TypeError::Located(d) => write!(f, "{}", d.message),
            TypeError::AmbiguousVariant { name, candidates } => write!(
                f,
                "variant `{}` is declared by {}; rename one of them to disambiguate",
                name,
                candidates.iter().map(|c| format!("`{}`", c)).collect::<Vec<_>>().join(" and ")
            ),
        }
    }
}

/// The type checker's view of a program. Exposed so the backends can ask the
/// same question the checker answers — "what type is this expression?" —
/// instead of each re-deriving the rules and drifting from them.
pub struct TypeEnv {
    scopes: Vec<HashMap<String, Type>>,
    functions: HashMap<String, (Vec<Type>, Type)>,
    structs: HashMap<String, Vec<(String, Type)>>,
    variants: VariantIndex,
    traits: HashMap<String, Vec<TraitMethod>>,
    impls: Vec<(String, String, Vec<Stmt>)>,
    extern_fns: HashMap<String, (Vec<Type>, Type)>,
    macros: HashMap<String, usize>,
    current_return: Option<Type>,
}

impl TypeEnv {
    fn new(program: &Program) -> Self {
        TypeEnv {
            scopes: vec![HashMap::new()],
            functions: HashMap::new(),
            structs: HashMap::new(),
            variants: VariantIndex::build(program),
            traits: HashMap::new(),
            impls: Vec::new(),
            extern_fns: HashMap::new(),
            macros: HashMap::new(),
            current_return: None,
        }
    }

    pub fn push_scope(&mut self) { self.scopes.push(HashMap::new()); }
    pub fn pop_scope(&mut self) { self.scopes.pop(); }

    pub fn define(&mut self, name: &str, ty: Type) {
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
                    if let StmtKind::Fn { name, params, ret, .. } = &m.kind {
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

/// Two types agree, treating `Inferred` — "not known statically" — as
/// compatible with anything. Without generics that is the only sound reading:
/// the alternative is rejecting `let x = Some(5);` because the built-in
/// payload is declared `Inferred` and the argument is `Int`.
fn compatible(a: &Type, b: &Type) -> bool {
    a == b || *a == Type::Inferred || *b == Type::Inferred
}

pub struct TypeChecker;

impl TypeChecker {
    /// Declarations only: signatures, structs, traits, externs, macros. No
    /// statement bodies are walked, so this is what a backend needs before it
    /// can ask about an expression.
    pub fn env_for(program: &Program) -> TypeEnv {
        let mut env = TypeEnv::new(program);
        for s in &program.stmts {
            match &s.kind {
                StmtKind::Fn { name, params, ret, .. } => {
                    let param_tys: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
                    env.functions.insert(name.clone(), (param_tys, ret.clone()));
                }
                StmtKind::Struct { name, fields, .. } => {
                    env.structs.insert(name.clone(), fields.clone());
                }
                StmtKind::Trait { name, methods } => {
                    env.traits.insert(name.clone(), methods.clone());
                }
                StmtKind::ExternFn { name, params, ret } => {
                    let param_tys: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
                    env.extern_fns.insert(name.clone(), (param_tys, ret.clone()));
                }
                StmtKind::Impl { .. } => {}
                StmtKind::Macro { name, params, .. } => {
                    // A macro is substituted, not called, so its result type is
                    // only known after expansion.
                    env.macros.insert(name.clone(), params.len());
                }
                _ => {}
            }
        }
        for s in &program.stmts {
            if let StmtKind::Impl { trait_name, type_name, methods } = &s.kind {
                env.impls.push((trait_name.clone(), type_name.clone(), methods.clone()));
            }
        }
        env
    }

    pub fn check(program: &Program) -> Result<(), Diagnostic> {
        let mut env = Self::env_for(program);

        for s in &program.stmts {
            if let StmtKind::Impl { trait_name, type_name, methods } = &s.kind {
                // An inherent `impl T { .. }` has no trait name and nothing to
                // check against; only a named trait imposes requirements.
                let trait_methods = if trait_name.is_empty() {
                    Vec::new()
                } else {
                    match env.traits.get(trait_name) {
                        Some(m) => m.clone(),
                        None => return Err(Diagnostic::new(
                            TypeError::UndefinedTrait(trait_name.clone()).to_string(), s.span)),
                    }
                };
                for tm in &trait_methods {
                    let found = methods.iter().any(|m| {
                        if let StmtKind::Fn { name, .. } = &m.kind { name == &tm.name } else { false }
                    });
                    if !found {
                        return Err(Diagnostic::new(TypeError::MissingImpl {
                            trait_name: trait_name.clone(),
                            type_name: type_name.clone(),
                            method: tm.name.clone(),
                        }.to_string(), s.span));
                    }
                }
            }
        }

        for s in &program.stmts {
            Self::check_stmt(s, &mut env)?;
        }
        Ok(())
    }

    /// Statements are the granularity the AST records positions at, so this is
    /// where a type error picks up the location it is reported against. An error
    /// coming back from a nested statement already carries one.
    fn check_stmt(stmt: &Stmt, env: &mut TypeEnv) -> Result<(), Diagnostic> {
        Self::check_stmt_inner(stmt, env).map_err(|e| match e {
            // keep the innermost position rather than the enclosing statement's
            TypeError::Located(d) => *d,
            other => Diagnostic::new(other.to_string(), stmt.span),
        })
    }

    fn check_stmt_inner(stmt: &Stmt, env: &mut TypeEnv) -> Result<(), TypeError> {
        match &stmt.kind {
            StmtKind::Let { name, ty, value } => {
                let val_ty = Self::check_expr(value, env)?;
                if *ty != Type::Inferred && !compatible(ty, &val_ty) {
                    return Err(TypeError::TypeMismatch { expected: format!("{:?}", ty), found: format!("{:?}", val_ty) });
                }
                env.define(name, val_ty);
            }
            StmtKind::Fn { params, ret, body, .. } => {
                env.push_scope();
                let prev = env.current_return.clone();
                env.current_return = Some(ret.clone());
                for (pname, pty) in params { env.define(pname, pty.clone()); }
                for s in body { Self::check_stmt(s, env)?; }
                env.current_return = prev;
                env.pop_scope();
            }
            StmtKind::Struct { .. } | StmtKind::Enum { .. } | StmtKind::Trait { .. } | StmtKind::Macro { .. } | StmtKind::ExternFn { .. } | StmtKind::Impl { .. } | StmtKind::Import(_) => {}
            StmtKind::While { cond, body } => {
                Self::check_expr(cond, env)?;
                env.push_scope();
                for s in body { Self::check_stmt(s, env)?; }
                env.pop_scope();
            }
            StmtKind::For { var, iter, body } => {
                Self::check_expr(iter, env)?;
                env.push_scope();
                env.define(var, Type::Int);
                for s in body { Self::check_stmt(s, env)?; }
                env.pop_scope();
            }
            StmtKind::Break(_) => {}
            StmtKind::Continue(_) => {}
            StmtKind::Assign { name, value } => {
                let val_ty = Self::check_expr(value, env)?;
                if let Some(var_ty) = env.lookup(name) {
                    if !compatible(&var_ty, &val_ty) {
                        return Err(TypeError::TypeMismatch { expected: format!("{:?}", var_ty), found: format!("{:?}", val_ty) });
                    }
                }
            }
            StmtKind::If { cond, then_body, else_body } => {
                Self::check_expr(cond, env)?;
                env.push_scope();
                for s in then_body { Self::check_stmt(s, env)?; }
                env.pop_scope();
                if let Some(eb) = else_body {
                    env.push_scope();
                    for s in eb { Self::check_stmt(s, env)?; }
                    env.pop_scope();
                }
            }
            StmtKind::Expr(e) => { Self::check_expr(e, env)?; }
            StmtKind::Return(e) => {
                let ret_ty = env.current_return.clone().unwrap_or(Type::Unit);
                match e {
                    Some(expr) => {
                        let expr_ty = Self::check_expr(expr, env)?;
                        if !compatible(&ret_ty, &expr_ty) {
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

    /// The type of an expression. The backends call this rather than
    /// re-deriving the rules; on a program that already type-checked it cannot
    /// fail, so they treat an error as "unknown" and fall back.
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    pub fn type_of(expr: &Expr, env: &mut TypeEnv) -> Option<Type> {
        Self::check_expr(expr, env).ok()
    }

    pub fn check_expr(expr: &Expr, env: &mut TypeEnv) -> Result<Type, TypeError> {
        match expr {
            Expr::Int(_) => Ok(Type::Int),
            Expr::Float(_) => Ok(Type::Float),
            Expr::Bool(_) => Ok(Type::Bool),
            Expr::Str(_) => Ok(Type::String),
            Expr::Ident(name) => {
                match env.variants.lookup(name) {
                    Lookup::Unique(v) => return Ok(Type::Named(v.enum_name)),
                    Lookup::Ambiguous(candidates) => {
                        return Err(TypeError::AmbiguousVariant { name: name.clone(), candidates })
                    }
                    Lookup::Unknown => {}
                }
                env.lookup(name).ok_or_else(|| TypeError::UndefinedVariable(name.clone()))
            }
            Expr::Binary(l, op, r) => {
                let lt = Self::check_expr(l, env)?;
                let rt = Self::check_expr(r, env)?;
                if !compatible(&lt, &rt) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", lt), found: format!("{:?}", rt) }); }
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
                if let Some((arity, ret)) = builtins::signature(func_name) {
                    if args.len() != arity {
                        return Err(TypeError::WrongArity { name: func_name.clone(), expected: arity, found: args.len() });
                    }
                    for a in args.iter() { Self::check_expr(a, env)?; }
                    return Ok(ret);
                }
                if let Some(arity) = env.macros.get(func_name).copied() {
                    if args.len() != arity {
                        return Err(TypeError::WrongArity { name: func_name.clone(), expected: arity, found: args.len() });
                    }
                    for a in args.iter() { Self::check_expr(a, env)?; }
                    return Ok(Type::Inferred);
                }
                match env.variants.lookup(func_name) {
                    Lookup::Unique(v) => {
                        if v.payload.len() != args.len() { return Err(TypeError::WrongArity { name: func_name.clone(), expected: v.payload.len(), found: args.len() }); }
                        for (arg, expected) in args.iter().zip(v.payload.iter()) {
                            let arg_ty = Self::check_expr(arg, env)?;
                            if !compatible(expected, &arg_ty) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", expected), found: format!("{:?}", arg_ty) }); }
                        }
                        return Ok(Type::Named(v.enum_name));
                    }
                    Lookup::Ambiguous(candidates) => {
                        return Err(TypeError::AmbiguousVariant { name: func_name.clone(), candidates })
                    }
                    Lookup::Unknown => {}
                }
                if let Some((param_tys, ret_ty)) = env.functions.get(func_name).cloned().or_else(|| env.extern_fns.get(func_name).cloned()) {
                    if param_tys.len() != args.len() { return Err(TypeError::WrongArity { name: func_name.clone(), expected: param_tys.len(), found: args.len() }); }
                    for (arg, expected) in args.iter().zip(param_tys.iter()) {
                        let arg_ty = Self::check_expr(arg, env)?;
                        if !compatible(&arg_ty, expected) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", expected), found: format!("{:?}", arg_ty) }); }
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
                if let Some((arity, ret)) = builtins::method_signature(&type_name, method) {
                    if args.len() != arity {
                        return Err(TypeError::WrongArity { name: method.clone(), expected: arity, found: args.len() });
                    }
                    for a in args.iter() { Self::check_expr(a, env)?; }
                    return Ok(ret);
                }
                if let Some((params, ret)) = env.lookup_method(&type_name, method) {
                    let self_count = if params.first().map(|(n, _)| n == "self" || n == "&self" || n == "&mut self").unwrap_or(false) { 1 } else { 0 };
                    let expected_args = params.len() - self_count;
                    if expected_args != args.len() { return Err(TypeError::WrongArity { name: method.clone(), expected: expected_args, found: args.len() }); }
                    for (arg, (_, pty)) in args.iter().zip(params[self_count..].iter()) {
                        let arg_ty = Self::check_expr(arg, env)?;
                        if !compatible(&arg_ty, pty) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", pty), found: format!("{:?}", arg_ty) }); }
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
                    if !compatible(fty, &expr_ty) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", fty), found: format!("{:?}", expr_ty) }); }
                }
                Ok(Type::Named(name.clone()))
            }
            Expr::Match { scrutinee, arms } => {
                let scrutinee_ty = Self::check_expr(scrutinee, env)?;
                let mut result_ty = None;
                for (patterns, guard, body) in arms {
                    env.push_scope();
                    for pattern in patterns {
                        Self::check_pattern(pattern, &scrutinee_ty, env)?;
                    }
                    if let Some(g) = guard {
                        Self::check_expr(g, env)?;
                    }
                    let body_ty = Self::check_expr(body, env)?;
                    env.pop_scope();
                    if let Some(prev) = &result_ty {
                        if !compatible(prev, &body_ty) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", prev), found: format!("{:?}", body_ty) }); }
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
                    if !compatible(&then_ty, &else_ty) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", then_ty), found: format!("{:?}", else_ty) }); }
                    Ok(then_ty)
                } else {
                    Ok(Type::Unit)
                }
            }
            Expr::While { cond, body } => {
                Self::check_expr(cond, env)?;
                env.push_scope();
                for s in body { Self::check_stmt(s, env)?; }
                env.pop_scope();
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
                        if !compatible(&et, &elem_ty) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", elem_ty), found: format!("{:?}", et) }); }
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
                if !compatible(&pat_ty, expected_ty) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", expected_ty), found: format!("{:?}", pat_ty) }); }
                Ok(())
            }
            Pattern::Variable(name) => { env.define(name, expected_ty.clone()); Ok(()) }
            Pattern::Wildcard => Ok(()),
            Pattern::EnumVariant { name, inner } => {
                match env.variants.lookup(name) {
                    Lookup::Unique(v) => {
                        if !compatible(expected_ty, &Type::Named(v.enum_name.clone())) { return Err(TypeError::TypeMismatch { expected: format!("{:?}", expected_ty), found: format!("Enum {}", v.enum_name) }); }
                        if v.payload.len() != inner.len() { return Err(TypeError::WrongArity { name: name.clone(), expected: v.payload.len(), found: inner.len() }); }
                        for (pat, arg_ty) in inner.iter().zip(v.payload.iter()) {
                            Self::check_pattern(pat, arg_ty, env)?;
                        }
                        Ok(())
                    }
                    Lookup::Ambiguous(candidates) => Err(TypeError::AmbiguousVariant { name: name.clone(), candidates }),
                    Lookup::Unknown => Err(TypeError::UndefinedType(name.clone())),
                }
            }
        }
    }
}
