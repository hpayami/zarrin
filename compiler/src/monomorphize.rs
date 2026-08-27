//! Replace generic functions with one concrete copy per way they are called.
//!
//! Both backends need to know a value's type: the interpreter to print it, the
//! native one to lay it out, count references to it and release it. A single
//! function whose parameter is `T` cannot answer that, so `id(5)` and
//! `id("hi")` become separate functions with `T` replaced throughout.
//!
//! Which call means which type is the type checker's answer, read back from the
//! instantiations it records. Call sites are identified by span, which is why
//! this became possible only once expressions carried one.

use crate::ast::*;
use crate::diagnostic::{Diagnostic, Span};
use crate::typecheck::{substitute, TypeChecker};
use std::collections::HashMap;

/// A specialised name: `id` called with `string` becomes `id$string`.
fn mangle(name: &str, args: &[Type]) -> String {
    let mut out = name.to_string();
    for a in args {
        out.push('$');
        out.push_str(&type_tag(a));
    }
    out
}

fn type_tag(t: &Type) -> String {
    match t {
        Type::Int => "int".into(),
        Type::Float => "float".into(),
        Type::Bool => "bool".into(),
        Type::String => "string".into(),
        Type::Unit => "unit".into(),
        Type::Range => "range".into(),
        Type::Inferred => "any".into(),
        Type::Named(n, args) if args.is_empty() => n.clone(),
        Type::Named(n, args) => {
            let parts: Vec<String> = args.iter().map(type_tag).collect();
            format!("{}.{}", n, parts.join("."))
        }
        Type::Array(el) => format!("arr.{}", type_tag(el)),
        Type::Fn(args, ret) => {
            let parts: Vec<String> = args.iter().map(type_tag).collect();
            format!("fn.{}.{}", parts.join("."), type_tag(ret))
        }
    }
}

/// Rewrite the program so no generic function is left called.
///
/// Runs to a fixed point: specialising one function can expose calls inside it
/// that are only now concrete. Programs where that never settles are rejected
/// rather than expanded forever.
pub fn expand(program: &Program) -> Result<Program, Diagnostic> {
    const ROUNDS: usize = 16;
    let mut current = program.clone();
    let mut last_call = Span::new(1, 1);

    for round in 0..ROUNDS {
        // Round zero checks the program as written, so its errors are the
        // user's. Later rounds check copies this pass made, and the only way
        // one of those fails is a generic function whose type argument grows
        // every time it recurses — an infinite family of copies.
        let env = match TypeChecker::check_collecting(&current) {
            Ok(env) => env,
            Err(d) if round > 0 => return Err(unbounded(d.span)),
            Err(d) => return Err(d),
        };
        if env.instantiations.is_empty() {
            return Ok(current);
        }

        // enclosing function -> call position -> specialised name
        let mut rename: Renames = HashMap::new();
        // name+args -> the copy to emit, deduplicated
        let mut wanted: Vec<(String, Vec<Type>)> = Vec::new();
        for (enclosing, span, name, args) in &env.instantiations {
            let target = mangle(name, args);
            rename
                .entry(enclosing.clone())
                .or_default()
                .insert((span.line, span.col), target.clone());
            if !wanted.iter().any(|(n, a)| n == name && a == args) {
                wanted.push((name.clone(), args.clone()));
                last_call = *span;
            }
        }

        let templates: HashMap<String, Stmt> = current
            .stmts
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Fn { name, generics, .. } if !generics.is_empty() => {
                    Some((name.clone(), s.clone()))
                }
                _ => None,
            })
            .collect();

        let mut stmts: Vec<Stmt> =
            current.stmts.iter().map(|s| rewrite_stmt(s, &rename, "")).collect();

        let mut added = false;
        for (name, args) in &wanted {
            let target = mangle(name, args);
            if stmts.iter().any(|s| matches!(&s.kind, StmtKind::Fn { name, .. } if *name == target)) {
                continue;
            }
            let Some(template) = templates.get(name) else { continue };
            let StmtKind::Fn { generics, params, ret, body, .. } = &template.kind else { continue };
            let subst: HashMap<String, Type> =
                generics.iter().cloned().zip(args.iter().cloned()).collect();
            stmts.push(Stmt::new(
                StmtKind::Fn {
                    name: target,
                    generics: Vec::new(),
                    params: params
                        .iter()
                        .map(|(p, t)| (p.clone(), substitute(t, &subst)))
                        .collect(),
                    ret: substitute(ret, &subst),
                    body: body.iter().map(|s| substitute_stmt(s, &subst)).collect(),
                },
                template.span,
            ));
            added = true;
        }

        current = Program { stmts };
        if !added {
            return Ok(current);
        }
    }

    Err(unbounded(last_call))
}

fn unbounded(span: Span) -> Diagnostic {
    Diagnostic::new(
        "this call needs a new copy of the function every time it runs, so there is no \
         finite set of copies to compile; give the recursive call a type that does not \
         grow"
            .to_string(),
        span,
    )
}

// --- rewriting call sites ---------------------------------------------------

/// Which call, in which function, becomes which specialised name.
type Renames = HashMap<String, HashMap<(u32, u32), String>>;

/// The generic originals are left in the program. Nothing a backend can reach
/// calls them any more, but bodies this pass cannot see into — trait `impl`
/// methods, which the checker does not walk — may still, and dropping them
/// would turn a program the interpreter runs today into an undefined-function
/// error.

fn rewrite_stmt(s: &Stmt, rename: &Renames, enclosing: &str) -> Stmt {
    let kind = match &s.kind {
        StmtKind::Let { name, ty, value } => StmtKind::Let {
            name: name.clone(),
            ty: ty.clone(),
            value: rewrite_expr(value, rename, enclosing),
        },
        StmtKind::Assign { name, value } => StmtKind::Assign {
            name: name.clone(),
            value: rewrite_expr(value, rename, enclosing),
        },
        StmtKind::Return(e) => StmtKind::Return(e.as_ref().map(|e| rewrite_expr(e, rename, enclosing))),
        StmtKind::Expr(e) => StmtKind::Expr(rewrite_expr(e, rename, enclosing)),
        StmtKind::Break(e) => StmtKind::Break(e.as_ref().map(|e| rewrite_expr(e, rename, enclosing))),
        StmtKind::Continue(e) => StmtKind::Continue(e.as_ref().map(|e| rewrite_expr(e, rename, enclosing))),
        StmtKind::While { cond, body } => StmtKind::While {
            cond: rewrite_expr(cond, rename, enclosing),
            body: body.iter().map(|b| rewrite_stmt(b, rename, enclosing)).collect(),
        },
        StmtKind::For { var, iter, body } => StmtKind::For {
            var: var.clone(),
            iter: rewrite_expr(iter, rename, enclosing),
            body: body.iter().map(|b| rewrite_stmt(b, rename, enclosing)).collect(),
        },
        StmtKind::If { cond, then_body, else_body } => StmtKind::If {
            cond: rewrite_expr(cond, rename, enclosing),
            then_body: then_body.iter().map(|b| rewrite_stmt(b, rename, enclosing)).collect(),
            else_body: else_body
                .as_ref()
                .map(|v| v.iter().map(|b| rewrite_stmt(b, rename, enclosing)).collect()),
        },
        StmtKind::Fn { name, generics, params, ret, body } => StmtKind::Fn {
            name: name.clone(),
            generics: generics.clone(),
            params: params.clone(),
            ret: ret.clone(),
            body: body.iter().map(|b| rewrite_stmt(b, rename, name)).collect(),
        },
        StmtKind::Impl { trait_name, type_name, methods } => StmtKind::Impl {
            trait_name: trait_name.clone(),
            type_name: type_name.clone(),
            methods: methods.iter().map(|m| rewrite_stmt(m, rename, name_of(m))).collect(),
        },
        other => other.clone(),
    };
    Stmt::new(kind, s.span)
}

fn rewrite_expr(e: &Expr, rename: &Renames, enclosing: &str) -> Expr {
    let r = |x: &Expr| rewrite_expr(x, rename, enclosing);
    let b = |x: &Expr| Box::new(rewrite_expr(x, rename, enclosing));
    let here = rename.get(enclosing);
    let kind = match &*e.kind {
        ExprKind::Call(callee, args) => {
            let args: Vec<Expr> = args.iter().map(&r).collect();
            match (here.and_then(|m| m.get(&(e.span.line, e.span.col))), &*callee.kind) {
                (Some(target), ExprKind::Ident(_)) => ExprKind::Call(
                    Box::new(Expr::new(ExprKind::Ident(target.clone()), callee.span)),
                    args,
                ),
                _ => ExprKind::Call(b(callee), args),
            }
        }
        ExprKind::Binary(l, op, rr) => ExprKind::Binary(b(l), op.clone(), b(rr)),
        ExprKind::Unary(op, x) => ExprKind::Unary(op.clone(), b(x)),
        ExprKind::Range(a, bb) => ExprKind::Range(b(a), b(bb)),
        ExprKind::ArrayLit(xs) => ExprKind::ArrayLit(xs.iter().map(&r).collect()),
        ExprKind::Index(a, i) => ExprKind::Index(b(a), b(i)),
        ExprKind::FieldAccess(o, f) => ExprKind::FieldAccess(b(o), f.clone()),
        ExprKind::MethodCall(o, m, args) => {
            ExprKind::MethodCall(b(o), m.clone(), args.iter().map(&r).collect())
        }
        ExprKind::StructLit { name, fields } => ExprKind::StructLit {
            name: name.clone(),
            fields: fields.iter().map(|(n, v)| (n.clone(), r(v))).collect(),
        },
        ExprKind::Match { scrutinee, arms } => ExprKind::Match {
            scrutinee: b(scrutinee),
            arms: arms
                .iter()
                .map(|(p, g, body)| (p.clone(), g.as_ref().map(&r), r(body)))
                .collect(),
        },
        ExprKind::If { cond, then_body, else_body } => ExprKind::If {
            cond: b(cond),
            then_body: b(then_body),
            else_body: else_body.as_ref().map(|e| Box::new(rewrite_expr(e, rename, enclosing))),
        },
        ExprKind::While { cond, body } => ExprKind::While {
            cond: b(cond),
            body: body.iter().map(|s| rewrite_stmt(s, rename, enclosing)).collect(),
        },
        ExprKind::For { var, iter, body } => ExprKind::For {
            var: var.clone(),
            iter: b(iter),
            body: body.iter().map(|s| rewrite_stmt(s, rename, enclosing)).collect(),
        },
        other => other.clone(),
    };
    Expr::new(kind, e.span)
}

/// A method's own name is how the checker attributes calls inside it.
fn name_of(s: &Stmt) -> &str {
    match &s.kind {
        StmtKind::Fn { name, .. } => name,
        _ => "",
    }
}

// --- substituting type parameters in a copy ---------------------------------

fn substitute_stmt(s: &Stmt, subst: &HashMap<String, Type>) -> Stmt {
    let kind = match &s.kind {
        StmtKind::Let { name, ty, value } => StmtKind::Let {
            name: name.clone(),
            ty: substitute(ty, subst),
            value: value.clone(),
        },
        StmtKind::While { cond, body } => StmtKind::While {
            cond: cond.clone(),
            body: body.iter().map(|b| substitute_stmt(b, subst)).collect(),
        },
        StmtKind::For { var, iter, body } => StmtKind::For {
            var: var.clone(),
            iter: iter.clone(),
            body: body.iter().map(|b| substitute_stmt(b, subst)).collect(),
        },
        StmtKind::If { cond, then_body, else_body } => StmtKind::If {
            cond: cond.clone(),
            then_body: then_body.iter().map(|b| substitute_stmt(b, subst)).collect(),
            else_body: else_body
                .as_ref()
                .map(|v| v.iter().map(|b| substitute_stmt(b, subst)).collect()),
        },
        other => other.clone(),
    };
    Stmt::new(kind, s.span)
}
