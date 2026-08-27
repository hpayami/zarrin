//! Type checker for the Zarrin language.

use crate::ast::*;
use crate::builtins;
use crate::diagnostic::{Diagnostic, Span};
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
    MissingField { ty: String, field: String },
    DuplicateField { ty: String, field: String },
    MissingImpl { trait_name: String, type_name: String, method: String },
    AmbiguousVariant { name: String, candidates: Vec<String> },
    NonExhaustiveMatch { ty: String, missing: Vec<String> },
    UnresolvedTypeParam { func: String, param: String },
    ExternNotCallable(String),
    FunctionAsValue(String),
    CallingAFunctionValue(String),
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
            TypeError::FunctionAsValue(n) => write!(
                f,
                "`{}` is a function; passing one as a value is not supported yet",
                n
            ),
            TypeError::CallingAFunctionValue(n) => write!(
                f,
                "`{}` holds a function, and calling one through a variable is not supported yet",
                n
            ),
            TypeError::ExternNotCallable(n) => write!(
                f,
                "`{}` is declared `extern`, and no backend can call one yet",
                n
            ),
            TypeError::UndefinedType(n) => write!(f, "undefined type: `{}`", n),
            TypeError::UndefinedTrait(n) => write!(f, "undefined trait: `{}`", n),
            TypeError::TypeMismatch { expected, found } => write!(f, "type mismatch: expected `{}`, found `{}`", expected, found),
            TypeError::NotAFunction(n) => write!(f, "`{}` is not a function", n),
            TypeError::WrongArity { name, expected, found } => write!(f, "`{}` expects {} args, found {}", name, expected, found),
            TypeError::UnknownField { ty, field } => write!(f, "type `{}` has no field `{}`", ty, field),
            TypeError::MissingField { ty, field } => write!(f, "`{}` is missing field `{}`", ty, field),
            TypeError::DuplicateField { ty, field } => write!(f, "field `{}` of `{}` is given twice", field, ty),
            TypeError::MissingImpl { trait_name, type_name, method } => write!(f, "trait `{}` for `{}` missing method `{}`", trait_name, type_name, method),
            TypeError::Located(d) => write!(f, "{}", d.message),
            TypeError::UnresolvedTypeParam { func, param } => write!(
                f,
                "cannot tell what `{}` is in this call to `{}`; it appears only in the return type",
                param, func
            ),
            TypeError::NonExhaustiveMatch { ty, missing } if missing.is_empty() => write!(
                f,
                "match on `{}` is not exhaustive; add a `_` arm",
                ty
            ),
            TypeError::NonExhaustiveMatch { ty, missing } => write!(
                f,
                "match on `{}` does not cover {}",
                ty,
                missing.iter().map(|m| format!("`{}`", m)).collect::<Vec<_>>().join(", ")
            ),
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
    /// Type parameters each function declares, so a call can work out what
    /// they stand for instead of reading `T` as the name of a real type.
    pub fn_generics: HashMap<String, Vec<String>>,
    /// Every call to a generic function: which function it appears in, where
    /// it is written, what is called, and what its type parameters turned out
    /// to be. A span alone is not enough to name a call site, because
    /// specialising a function copies its body spans and all; the enclosing
    /// name is what keeps `id(x)` inside `twice$int` apart from the same line
    /// inside `twice$string`.
    pub instantiations: Vec<(String, Span, String, Vec<Type>)>,
    structs: HashMap<String, Vec<(String, Type)>>,
    variants: VariantIndex,
    traits: HashMap<String, Vec<TraitMethod>>,
    impls: Vec<(String, String, Vec<Stmt>)>,
    extern_fns: HashMap<String, (Vec<Type>, Type)>,
    macros: HashMap<String, usize>,
    current_return: Option<Type>,
    /// The function whose body is being checked, for attributing calls.
    current_fn: String,
}

impl TypeEnv {
    fn new(program: &Program) -> Self {
        TypeEnv {
            scopes: vec![HashMap::new()],
            functions: HashMap::new(),
            fn_generics: HashMap::new(),
            instantiations: Vec::new(),
            structs: HashMap::new(),
            variants: VariantIndex::build(program),
            traits: HashMap::new(),
            impls: Vec::new(),
            extern_fns: HashMap::new(),
            macros: HashMap::new(),
            current_return: None,
            current_fn: String::new(),
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
    if *a == Type::Inferred || *b == Type::Inferred {
        return true;
    }
    // `Option` and `Option<float>` are the same type; one of them just does not
    // say what it holds. A declared parameter written `Option` therefore takes
    // any `Option`, which is what keeps existing signatures working.
    if let (Type::Named(an, aa), Type::Named(bn, ba)) = (a, b) {
        if an != bn {
            return false;
        }
        if aa.is_empty() || ba.is_empty() {
            return true;
        }
        return aa.len() == ba.len() && aa.iter().zip(ba).all(|(x, y)| compatible(x, y));
    }
    a == b
}

/// Work out what a function's type parameters stand for at one call, by
/// lining its declared parameter types up against the arguments given.
fn unify(declared: &Type, actual: &Type, params: &[String], subst: &mut HashMap<String, Type>) -> bool {
    match declared {
        Type::Named(n, args) if args.is_empty() && params.contains(n) => match subst.get(n) {
            // Every mention of a parameter has to agree.
            Some(bound) => compatible(bound, actual),
            None => {
                subst.insert(n.clone(), actual.clone());
                true
            }
        },
        Type::Array(d) => match actual {
            Type::Array(a) => unify(d, a, params, subst),
            _ => compatible(declared, actual),
        },
        Type::Named(dn, dargs) => match actual {
            Type::Named(an, aargs) if dn == an && dargs.len() == aargs.len() => dargs
                .iter()
                .zip(aargs)
                .all(|(d, a)| unify(d, a, params, subst)),
            _ => compatible(declared, actual),
        },
        _ => compatible(declared, actual),
    }
}

/// Replace type parameters with what they were found to stand for.
pub fn substitute(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Named(n, args) if args.is_empty() => subst.get(n).cloned().unwrap_or_else(|| ty.clone()),
        Type::Named(n, args) => Type::Named(
            n.clone(),
            args.iter().map(|a| substitute(a, subst)).collect(),
        ),
        Type::Array(el) => Type::Array(Box::new(substitute(el, subst))),
        Type::Fn(args, ret) => Type::Fn(
            args.iter().map(|a| substitute(a, subst)).collect(),
            Box::new(substitute(ret, subst)),
        ),
        other => other.clone(),
    }
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
                StmtKind::Fn { name, generics, params, ret, .. } => {
                    let param_tys: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
                    env.functions.insert(name.clone(), (param_tys, ret.clone()));
                    if !generics.is_empty() {
                        env.fn_generics.insert(name.clone(), generics.clone());
                    }
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
        Self::check_collecting(program).map(|_| ())
    }

    /// Type-check, and hand back the environment so a caller can read the
    /// generic instantiations it discovered.
    pub fn check_collecting(program: &Program) -> Result<TypeEnv, Diagnostic> {
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
        Ok(env)
    }

    /// What a `for` can walk: a range, an array, or an integer, which counts
    /// from zero. Anything else was a run-time failure the interpreter reported
    /// only once the loop was reached, and a compile-time panic natively.
    fn check_iterable(ty: &Type) -> Result<(), TypeError> {
        match ty {
            Type::Range | Type::Int | Type::Array(_) | Type::Inferred => Ok(()),
            other => Err(TypeError::TypeMismatch {
                expected: "range, array or int".into(),
                found: other.to_string(),
            }),
        }
    }

    /// What the loop variable is bound to on each turn.
    fn element_of(ty: &Type) -> Type {
        match ty {
            Type::Array(el) => (**el).clone(),
            _ => Type::Int,
        }
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
                    return Err(TypeError::TypeMismatch { expected: format!("{}", ty), found: format!("{}", val_ty) });
                }
                env.define(name, val_ty);
            }
            StmtKind::Fn { name, params, ret, body, .. } => {
                env.push_scope();
                let prev = env.current_return.clone();
                let prev_fn = std::mem::replace(&mut env.current_fn, name.clone());
                env.current_return = Some(ret.clone());
                for (pname, pty) in params { env.define(pname, pty.clone()); }
                for s in body { Self::check_stmt(s, env)?; }
                env.current_return = prev;
                env.current_fn = prev_fn;
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
                let iter_ty = Self::check_expr(iter, env)?;
                Self::check_iterable(&iter_ty)?;
                env.push_scope();
                env.define(var, Self::element_of(&iter_ty));
                for s in body { Self::check_stmt(s, env)?; }
                env.pop_scope();
            }
            StmtKind::Break(_) => {}
            StmtKind::Continue(_) => {}
            StmtKind::Assign { name, value } => {
                let val_ty = Self::check_expr(value, env)?;
                if let Some(var_ty) = env.lookup(name) {
                    if !compatible(&var_ty, &val_ty) {
                        return Err(TypeError::TypeMismatch { expected: format!("{}", var_ty), found: format!("{}", val_ty) });
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
                            return Err(TypeError::TypeMismatch { expected: format!("{}", ret_ty), found: format!("{}", expr_ty) });
                        }
                    }
                    None => {
                        if ret_ty != Type::Unit {
                            return Err(TypeError::TypeMismatch { expected: format!("{}", ret_ty), found: "()".into() });
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

    /// Now that expressions carry positions, an error picks up the one of the
    /// subexpression that raised it rather than the whole statement's.
    pub fn check_expr(expr: &Expr, env: &mut TypeEnv) -> Result<Type, TypeError> {
        Self::check_expr_inner(expr, env).map_err(|e| match e {
            // Something further in already said where it was.
            TypeError::Located(d) => TypeError::Located(d),
            other => TypeError::Located(Box::new(Diagnostic::new(other.to_string(), expr.span))),
        })
    }

    fn check_expr_inner(expr: &Expr, env: &mut TypeEnv) -> Result<Type, TypeError> {
        match &*expr.kind {
            ExprKind::Int(_) => Ok(Type::Int),
            ExprKind::Float(_) => Ok(Type::Float),
            ExprKind::Bool(_) => Ok(Type::Bool),
            ExprKind::Str(_) => Ok(Type::String),
            ExprKind::Ident(name) => {
                match env.variants.lookup(name) {
                    Lookup::Unique(v) => {
                        // A payload-free variant says nothing about what its
                        // enum holds: `None` is an `Option` of anything.
                        let params = env.variants.params_of(&v.enum_name);
                        return Ok(Type::Named(v.enum_name, vec![Type::Inferred; params.len()]));
                    }
                    Lookup::Ambiguous(candidates) => {
                        return Err(TypeError::AmbiguousVariant { name: name.clone(), candidates })
                    }
                    Lookup::Unknown => {}
                }
                // A `fn` type can be written in a signature, so this used to
                // come out as "undefined variable" at the point one was passed.
                env.lookup(name).ok_or_else(|| {
                    if env.functions.contains_key(name) || env.extern_fns.contains_key(name) {
                        TypeError::FunctionAsValue(name.clone())
                    } else {
                        TypeError::UndefinedVariable(name.clone())
                    }
                })
            }
            ExprKind::Binary(l, op, r) => {
                let lt = Self::check_expr(l, env)?;
                let rt = Self::check_expr(r, env)?;
                if !compatible(&lt, &rt) { return Err(TypeError::TypeMismatch { expected: format!("{}", lt), found: format!("{}", rt) }); }
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => Ok(lt),
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Ok(Type::Bool),
                    // Only something that can be true or false. A float or a
                    // string here was accepted and then failed differently in
                    // each backend — an unreachable branch in one, "expected
                    // int operand" in the other.
                    BinOp::And | BinOp::Or => match lt {
                        Type::Bool | Type::Int | Type::Inferred => Ok(Type::Bool),
                        other => Err(TypeError::TypeMismatch {
                            expected: "bool".into(),
                            found: other.to_string(),
                        }),
                    },
                }
            }
            ExprKind::Unary(op, e) => {
                let et = Self::check_expr(e, env)?;
                match op {
                    UnaryOp::Neg => {
                        if et != Type::Int && et != Type::Float {
                            return Err(TypeError::TypeMismatch { expected: "int or float".into(), found: format!("{}", et) });
                        }
                        Ok(et)
                    }
                    UnaryOp::Not => {
                        if et != Type::Bool && et != Type::Int {
                            return Err(TypeError::TypeMismatch { expected: "bool or int".into(), found: format!("{}", et) });
                        }
                        Ok(Type::Bool)
                    }
                }
            }
            ExprKind::Call(callee, args) => {
                let func_name = match &*callee.kind {
                    ExprKind::Ident(n) => n,
                    _ => return Err(TypeError::NotAFunction("non-identifier call".into())),
                };
                if let Some((arity, ret)) = builtins::signature(func_name) {
                    if args.len() != arity {
                        return Err(TypeError::WrongArity { name: func_name.clone(), expected: arity, found: args.len() });
                    }
                    let mut arg_types = Vec::new();
                    for a in args.iter() { arg_types.push(Self::check_expr(a, env)?); }
                    // What these two answer with depends on the array they were
                    // given, which the table of signatures cannot see. Left as
                    // unknown, indexing what `array_set` returned was a type
                    // error, so the older accessors could not be chained.
                    return Ok(match (func_name.as_str(), arg_types.first()) {
                        ("array_set", Some(t)) => t.clone(),
                        ("array_get", Some(Type::Array(el))) => (**el).clone(),
                        _ => ret,
                    });
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
                        // A built-in variant's payload is a type parameter, so
                        // building one is where the enum's argument gets fixed:
                        // `Some(1.5)` is an `Option<float>`, and that is what
                        // later lets it be printed as a float rather than as
                        // the bits of one.
                        let params = env.variants.params_of(&v.enum_name);
                        let mut subst: HashMap<String, Type> = HashMap::new();
                        for (arg, expected) in args.iter().zip(v.payload.iter()) {
                            let arg_ty = Self::check_expr(arg, env)?;
                            if !unify(expected, &arg_ty, &params, &mut subst) {
                                return Err(TypeError::TypeMismatch { expected: format!("{}", expected), found: format!("{}", arg_ty) });
                            }
                        }
                        let type_args = params
                            .iter()
                            .map(|p| subst.get(p).cloned().unwrap_or(Type::Inferred))
                            .collect();
                        return Ok(Type::Named(v.enum_name, type_args));
                    }
                    Lookup::Ambiguous(candidates) => {
                        return Err(TypeError::AmbiguousVariant { name: func_name.clone(), candidates })
                    }
                    Lookup::Unknown => {}
                }
                // Neither backend implements one: the interpreter said so when
                // the call was reached, and the native one when it was
                // compiled, so the same program failed at different moments.
                if !env.functions.contains_key(func_name) && env.extern_fns.contains_key(func_name) {
                    return Err(TypeError::ExternNotCallable(func_name.clone()));
                }
                if let Some((param_tys, ret_ty)) = env.functions.get(func_name).cloned().or_else(|| env.extern_fns.get(func_name).cloned()) {
                    if param_tys.len() != args.len() { return Err(TypeError::WrongArity { name: func_name.clone(), expected: param_tys.len(), found: args.len() }); }
                    let generics = env.fn_generics.get(func_name).cloned().unwrap_or_default();
                    // What each type parameter stands for at this call, read off
                    // the arguments. Every mention of one has to agree.
                    let mut subst: HashMap<String, Type> = HashMap::new();
                    for (arg, expected) in args.iter().zip(param_tys.iter()) {
                        let arg_ty = Self::check_expr(arg, env)?;
                        if !generics.is_empty() {
                            if !unify(expected, &arg_ty, &generics, &mut subst) {
                                return Err(Self::mismatch_at(&substitute(expected, &subst), &arg_ty, arg));
                            }
                        } else if !compatible(&arg_ty, expected) {
                            return Err(Self::mismatch_at(expected, &arg_ty, arg));
                        }
                    }
                    if let Some(missing) = generics.iter().find(|g| !subst.contains_key(*g)) {
                        return Err(TypeError::UnresolvedTypeParam {
                            func: func_name.clone(),
                            param: missing.clone(),
                        });
                    }
                    if !generics.is_empty() {
                        let args: Vec<Type> = generics.iter().map(|g| subst[g].clone()).collect();
                        env.instantiations.push((
                            env.current_fn.clone(),
                            expr.span,
                            func_name.clone(),
                            args,
                        ));
                    }
                    return Ok(substitute(&ret_ty, &subst));
                }
                // Calling what a variable holds, rather than a function by
                // name: the parser accepts `f: fn(int) -> int`, and this is
                // where the language admits it cannot run one.
                if matches!(env.lookup(func_name), Some(Type::Fn(..))) {
                    return Err(TypeError::CallingAFunctionValue(func_name.clone()));
                }
                Err(TypeError::UndefinedFunction(func_name.clone()))
            }
            ExprKind::MethodCall(obj, method, args) => {
                let obj_ty = Self::check_expr(obj, env)?;
                let (type_name, type_args) = match &obj_ty {
                    Type::Named(n, a) => (n.clone(), a.clone()),
                    _ => return Err(TypeError::UnknownField { ty: format!("{}", obj_ty), field: method.clone() }),
                };
                if let Some((arity, ret)) = builtins::method_signature(&type_name, &type_args, method) {
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
                        if !compatible(&arg_ty, pty) { return Err(Self::mismatch_at(pty, &arg_ty, arg)); }
                    }
                    return Ok(ret);
                }
                Err(TypeError::UnknownField { ty: type_name, field: method.clone() })
            }
            ExprKind::FieldAccess(obj, field) => {
                let obj_ty = Self::check_expr(obj, env)?;
                match &obj_ty {
                    Type::Named(name, _) => {
                        if let Some(fields) = env.structs.get(name) {
                            fields.iter().find(|(fname, _)| fname == field)
                                .map(|(_, fty)| fty.clone())
                                .ok_or_else(|| TypeError::UnknownField { ty: name.clone(), field: field.clone() })
                        } else {
                            Err(TypeError::UnknownField { ty: name.clone(), field: field.clone() })
                        }
                    }
                    _ => Err(TypeError::UnknownField { ty: format!("{}", obj_ty), field: field.clone() }),
                }
            }
            ExprKind::StructLit { name, fields } => {
                let sdef = env.structs.get(name).cloned()
                    .ok_or_else(|| TypeError::UndefinedType(name.clone()))?;
                // By name. Lining the literal's fields up with the declaration
                // in order meant `P { y: 2, x: 1 }` was checked against `x`'s
                // type for `y` — a type error on a literal with every field
                // right, and a swap when the types happened to match.
                let mut seen: Vec<&str> = Vec::new();
                for (fname, expr) in fields.iter() {
                    let Some((_, fty)) = sdef.iter().find(|(n, _)| n == fname) else {
                        return Err(TypeError::UnknownField { ty: name.clone(), field: fname.clone() });
                    };
                    if seen.contains(&fname.as_str()) {
                        return Err(TypeError::DuplicateField { ty: name.clone(), field: fname.clone() });
                    }
                    seen.push(fname);
                    let expr_ty = Self::check_expr(expr, env)?;
                    if !compatible(fty, &expr_ty) { return Err(Self::mismatch_at(fty, &expr_ty, expr)); }
                }
                if let Some((missing, _)) = sdef.iter().find(|(n, _)| !seen.contains(&n.as_str())) {
                    return Err(TypeError::MissingField { ty: name.clone(), field: missing.clone() });
                }
                Ok(Type::Named(name.clone(), Vec::new()))
            }
            ExprKind::Match { scrutinee, arms } => {
                let scrutinee_ty = Self::check_expr(scrutinee, env)?;
                Self::check_exhaustive(&scrutinee_ty, arms, env)?;
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
                        if !compatible(prev, &body_ty) { return Err(TypeError::TypeMismatch { expected: format!("{}", prev), found: format!("{}", body_ty) }); }
                    } else {
                        result_ty = Some(body_ty);
                    }
                }
                Ok(result_ty.unwrap_or(Type::Unit))
            }
            ExprKind::If { cond, then_body, else_body } => {
                Self::check_expr(cond, env)?;
                let then_ty = Self::check_expr(then_body, env)?;
                if let Some(eb) = else_body {
                    let else_ty = Self::check_expr(eb, env)?;
                    if !compatible(&then_ty, &else_ty) { return Err(TypeError::TypeMismatch { expected: format!("{}", then_ty), found: format!("{}", else_ty) }); }
                    Ok(then_ty)
                } else {
                    Ok(Type::Unit)
                }
            }
            ExprKind::While { cond, body } => {
                Self::check_expr(cond, env)?;
                env.push_scope();
                for s in body { Self::check_stmt(s, env)?; }
                env.pop_scope();
                Ok(Type::Int)
            }
            ExprKind::For { var, iter, body } => {
                let iter_ty = Self::check_expr(iter, env)?;
                Self::check_iterable(&iter_ty)?;
                env.push_scope();
                env.define(var, Self::element_of(&iter_ty));
                for s in body { Self::check_stmt(s, env)?; }
                env.pop_scope();
                Ok(Type::Int)
            }
            ExprKind::Range(a, b) => {
                let at = Self::check_expr(a, env)?;
                let bt = Self::check_expr(b, env)?;
                if at != Type::Int || bt != Type::Int {
                    return Err(TypeError::TypeMismatch { expected: "int".into(), found: format!("{}", if at != Type::Int { at } else { bt }) });
                }
                Ok(Type::Range)
            }
            ExprKind::ArrayLit(elems) => {
                if elems.is_empty() {
                    Ok(Type::Array(Box::new(Type::Inferred)))
                } else {
                    let elem_ty = Self::check_expr(&elems[0], env)?;
                    for e in &elems[1..] {
                        let et = Self::check_expr(e, env)?;
                        if !compatible(&et, &elem_ty) { return Err(TypeError::TypeMismatch { expected: format!("{}", elem_ty), found: format!("{}", et) }); }
                    }
                    Ok(Type::Array(Box::new(elem_ty)))
                }
            }
            ExprKind::Index(arr, idx) => {
                let arr_ty = Self::check_expr(arr, env)?;
                let idx_ty = Self::check_expr(idx, env)?;
                if idx_ty != Type::Int { return Err(TypeError::TypeMismatch { expected: "int".into(), found: format!("{}", idx_ty) }); }
                match arr_ty {
                    Type::Array(et) => Ok(*et),
                    _ => Err(TypeError::TypeMismatch { expected: "array".into(), found: format!("{}", arr_ty) }),
                }
            }
        }
    }

    /// Every value the scrutinee can take has to be matched by something. An
    /// arm carrying a guard covers nothing, because the guard can turn it down.
    fn check_exhaustive(
        ty: &Type,
        arms: &[(Vec<Pattern>, Option<Expr>, Expr)],
        env: &TypeEnv,
    ) -> Result<(), TypeError> {
        let irrefutable = |p: &Pattern| matches!(p, Pattern::Wildcard | Pattern::Variable(_));
        let unguarded: Vec<&Vec<Pattern>> = arms
            .iter()
            .filter(|(_, guard, _)| guard.is_none())
            .map(|(pats, _, _)| pats)
            .collect();

        if unguarded.iter().any(|pats| pats.iter().any(|p| irrefutable(p))) {
            return Ok(());
        }

        let name = |t: &Type| match t {
            Type::Named(n, _) => n.clone(),
            Type::Int => "int".into(),
            Type::Float => "float".into(),
            Type::Bool => "bool".into(),
            Type::String => "string".into(),
            other => other.to_string(),
        };

        match ty {
            // Nothing is known about the value, so nothing can be proven.
            Type::Inferred => Ok(()),
            Type::Bool => {
                let mut seen = [false, false];
                for pats in &unguarded {
                    for p in pats.iter() {
                        if let Pattern::Literal(lit) = p {
                            if let ExprKind::Bool(b) = &*lit.kind {
                                seen[*b as usize] = true;
                            }
                        }
                    }
                }
                let missing: Vec<String> = [("false", seen[0]), ("true", seen[1])]
                    .iter()
                    .filter(|(_, hit)| !hit)
                    .map(|(n, _)| (*n).to_string())
                    .collect();
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(TypeError::NonExhaustiveMatch { ty: "bool".into(), missing })
                }
            }
            Type::Named(n, _) if env.variants.is_enum(n) => {
                let mut covered: Vec<String> = Vec::new();
                for pats in &unguarded {
                    for p in pats.iter() {
                        // A variant is only covered when its payload is bound
                        // wholesale; `C(0)` leaves the rest of `C` unmatched.
                        if let Pattern::EnumVariant { name: vn, inner } = p {
                            if inner.iter().all(|i| irrefutable(i)) {
                                if let Lookup::Unique(v) = env.variants.lookup(vn) {
                                    covered.push(v.name);
                                }
                            }
                        }
                    }
                }
                let missing: Vec<String> = env
                    .variants
                    .variants_of(n)
                    .into_iter()
                    .filter(|v| !covered.contains(&v.name))
                    .map(|v| v.name)
                    .collect();
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(TypeError::NonExhaustiveMatch { ty: n.clone(), missing })
                }
            }
            other => Err(TypeError::NonExhaustiveMatch { ty: name(other), missing: Vec::new() }),
        }
    }

    /// A mismatch found by comparing against a declaration belongs to the
    /// value that was supplied, not to the call or literal around it.
    fn mismatch_at(expected: &Type, found: &Type, at: &Expr) -> TypeError {
        TypeError::Located(Box::new(Diagnostic::new(
            TypeError::TypeMismatch {
                expected: format!("{}", expected),
                found: format!("{}", found),
            }
            .to_string(),
            at.span,
        )))
    }

    fn check_pattern(pattern: &Pattern, expected_ty: &Type, env: &mut TypeEnv) -> Result<(), TypeError> {
        match pattern {
            Pattern::Literal(expr) => {
                let pat_ty = Self::check_expr(expr, env)?;
                if !compatible(&pat_ty, expected_ty) { return Err(TypeError::TypeMismatch { expected: format!("{}", expected_ty), found: format!("{}", pat_ty) }); }
                Ok(())
            }
            Pattern::Variable(name) => { env.define(name, expected_ty.clone()); Ok(()) }
            Pattern::Wildcard => Ok(()),
            Pattern::EnumVariant { name, inner } => {
                match env.variants.lookup(name) {
                    Lookup::Unique(v) => {
                        if !compatible(expected_ty, &Type::Named(v.enum_name.clone(), Vec::new())) { return Err(TypeError::TypeMismatch { expected: format!("{}", expected_ty), found: format!("Enum {}", v.enum_name) }); }
                        if v.payload.len() != inner.len() { return Err(TypeError::WrongArity { name: name.clone(), expected: v.payload.len(), found: inner.len() }); }
                        // What the payload is depends on the value being
                        // matched: `Some(v)` binds a float out of an
                        // `Option<float>` and nothing definite out of a plain
                        // `Option`.
                        let subst: HashMap<String, Type> = match expected_ty {
                            Type::Named(_, targs) => env
                                .variants
                                .params_of(&v.enum_name)
                                .into_iter()
                                .zip(targs.iter().cloned())
                                .collect(),
                            _ => HashMap::new(),
                        };
                        let params = env.variants.params_of(&v.enum_name);
                        for (pat, arg_ty) in inner.iter().zip(v.payload.iter()) {
                            let mut arg_ty = substitute(arg_ty, &subst);
                            // An unbound parameter says nothing, rather than
                            // naming a type that does not exist.
                            if let Type::Named(n, a) = &arg_ty {
                                if a.is_empty() && params.contains(n) {
                                    arg_ty = Type::Inferred;
                                }
                            }
                            Self::check_pattern(pat, &arg_ty, env)?;
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
