//! Minimal tree-walk interpreter used as the default backend.

use crate::ast::*;
use crate::diagnostic::{Diagnostic, Span};
use crate::variants::{Lookup, VariantIndex};
use std::collections::HashMap;

#[derive(Debug, Clone)]
enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Unit,
    #[allow(dead_code)]
    Fn(String),
    Struct { name: String, fields: HashMap<String, Value> },
    EnumVariant { enum_name: String, variant: String, args: Vec<Value> },
    Range(i64, i64),
    Array(Vec<Value>),
}

/// Report a run-time failure against the statement being evaluated.
macro_rules! rt_fail {
    ($self:expr, $($arg:tt)*) => { $self.fail(format!($($arg)*)) };
}

pub struct Interpreter {
    /// Source text and path, so a failure can be shown the way a syntax error is.
    path: String,
    src: String,
    /// Statement currently being evaluated. Positions are recorded per
    /// statement, which is as precise as the AST gets.
    current_span: Span,
    // Static program context: fixed once the program is loaded.
    fns: HashMap<String, Stmt>,
    structs: HashMap<String, Vec<(String, Type)>>,
    variants: VariantIndex,
    impls: Vec<(String, String, Vec<Stmt>)>,
    macros: HashMap<String, Stmt>,
    extern_fns: HashMap<String, Stmt>,
    // Variable state. `globals` holds top-level `let` bindings and is visible
    // everywhere; `scopes` is the block-scope stack of the *current* call frame,
    // so a callee never sees its caller's locals.
    globals: HashMap<String, Value>,
    scopes: Vec<HashMap<String, Value>>,
    returned: Option<Value>,
    should_break: bool,
    should_continue: bool,
    break_value: Option<Value>,
}

impl Interpreter {
    pub fn new(program: &Program, path: &str, src: &str) -> Self {
        let mut interp = Interpreter {
            path: path.to_string(),
            src: src.to_string(),
            current_span: Span::new(1, 1),
            fns: HashMap::new(),
            structs: HashMap::new(),
            variants: VariantIndex::build(program),
            impls: Vec::new(),
            macros: HashMap::new(),
            extern_fns: HashMap::new(),
            globals: HashMap::new(),
            scopes: Vec::new(),
            returned: None,
            should_break: false,
            should_continue: false,
            break_value: None,
        };
        for s in &program.stmts {
            match &s.kind {
                StmtKind::Fn { name, .. } => { interp.fns.insert(name.clone(), s.clone()); }
                StmtKind::Struct { name, fields, .. } => { interp.structs.insert(name.clone(), fields.clone()); }
                StmtKind::Impl { trait_name, type_name, methods } => { interp.impls.push((trait_name.clone(), type_name.clone(), methods.clone())); }
                StmtKind::Macro { name, .. } => { interp.macros.insert(name.clone(), s.clone()); }
                StmtKind::ExternFn { name, .. } => { interp.extern_fns.insert(name.clone(), s.clone()); }
                _ => {}
            }
        }
        interp
    }

    /// Resolve a variant name to its enum, or `None` if it isn't one.
    /// An ambiguous name is a hard error rather than a coin flip.
    fn resolve_variant(&self, name: &str) -> Option<crate::variants::Variant> {
        match self.variants.lookup(name) {
            Lookup::Unique(v) => Some(v),
            Lookup::Unknown => None,
            Lookup::Ambiguous(c) => rt_fail!(self, "variant `{}` is declared by {}; rename one of them to disambiguate", name, c.join(" and ")),
        }
    }

    /// Print a diagnostic and stop. The interpreter has no error type to
    /// propagate; before this these were bare panics with no location and a
    /// Rust backtrace note.
    fn fail(&self, message: String) -> ! {
        eprint!("{}", Diagnostic::new(message, self.current_span).render(&self.path, &self.src));
        std::process::exit(1);
    }

    /// Bounds-check an index, reporting through `fail` rather than letting
    /// Rust's own panic surface with no position and no source line.
    fn checked_index(&self, len: usize, i: i64, what: &str) -> usize {
        if i < 0 || i as usize >= len {
            rt_fail!(self, "{} index {} is out of bounds for length {}", what, i, len);
        }
        i as usize
    }

    fn push_scope(&mut self) { self.scopes.push(HashMap::new()); }
    fn pop_scope(&mut self) { self.scopes.pop(); }

    /// Introduce a new binding in the innermost scope (`let`, params, patterns,
    /// loop variables). At top level `scopes` is empty, so it lands in `globals`.
    fn declare(&mut self, name: &str, v: Value) {
        match self.scopes.last_mut() {
            Some(scope) => { scope.insert(name.to_string(), v); }
            None => { self.globals.insert(name.to_string(), v); }
        }
    }

    /// Update an existing binding wherever it lives, so `i = i + 1` inside a
    /// block mutates the outer `i` instead of shadowing it.
    fn assign(&mut self, name: &str, v: Value) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) { *slot = v; return; }
        }
        if let Some(slot) = self.globals.get_mut(name) { *slot = v; return; }
        self.declare(name, v);
    }

    fn lookup(&self, name: &str) -> Value {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) { return v.clone(); }
        }
        if let Some(v) = self.globals.get(name) { return v.clone(); }
        rt_fail!(self, "undefined variable: {}", name)
    }

    /// Run a function/method/macro body in a fresh frame: globals stay visible,
    /// the caller's locals do not. Returns the body's return value, if any.
    fn call_frame(&mut self, frame: HashMap<String, Value>, body: &[Stmt]) -> Value {
        let saved = std::mem::replace(&mut self.scopes, vec![frame]);
        let mut result = Value::Unit;
        for s in body {
            self.eval_stmt(s);
            if self.returned.is_some() { result = self.returned.take().unwrap(); break; }
        }
        self.scopes = saved;
        result
    }

    pub fn run(&mut self, program: &Program) {
        // Top-level statements run with an empty scope stack, so their `let`
        // bindings become globals that `main` and every function can see.
        for s in &program.stmts {
            if matches!(s.kind, StmtKind::Fn { .. } | StmtKind::Struct { .. } | StmtKind::Enum { .. } | StmtKind::Trait { .. } | StmtKind::Macro { .. } | StmtKind::ExternFn { .. } | StmtKind::Impl { .. }) { continue; }
            self.eval_stmt(s);
        }
        if let Some(Stmt { kind: StmtKind::Fn { body, .. }, .. }) = self.fns.get("main").cloned() {
            self.call_frame(HashMap::new(), &body);
        }
    }

    fn eval_stmt(&mut self, stmt: &Stmt) {
        self.current_span = stmt.span;
        match &stmt.kind {
            StmtKind::Let { name, value, .. } => { let v = self.eval_expr(value); self.declare(name, v); }
            StmtKind::Fn { .. } | StmtKind::Struct { .. } | StmtKind::Enum { .. } | StmtKind::Trait { .. } | StmtKind::Macro { .. } | StmtKind::ExternFn { .. } | StmtKind::Impl { .. } | StmtKind::Import(_) => {}
            StmtKind::Expr(e) => { self.eval_expr(e); }
            StmtKind::Return(e) => { self.returned = Some(match e { Some(x) => self.eval_expr(x), None => Value::Unit }); }
            StmtKind::While { cond, body } => {
                loop {
                    let cv = self.eval_expr(cond);
                    let truthy = match cv { Value::Bool(b) => b, Value::Int(n) => n != 0, _ => true };
                    if !truthy { break; }
                    self.push_scope();
                    for s in body {
                        self.eval_stmt(s);
                        if self.returned.is_some() { self.pop_scope(); return; }
                        if self.should_continue { self.should_continue = false; break; }
                        if self.should_break { self.should_break = false; self.break_value = None; break; }
                    }
                    self.pop_scope();
                    if self.should_break { self.should_break = false; self.break_value = None; break; }
                }
            }
            StmtKind::For { var, iter, body } => {
                let iter_val = self.eval_expr(iter);
                match iter_val {
                    Value::Range(start, end) => {
                        let mut i = start;
                        while i < end {
                            self.push_scope();
                            self.declare(var, Value::Int(i));
                            for s in body {
                                self.eval_stmt(s);
                                if self.returned.is_some() { self.pop_scope(); return; }
                                if self.should_continue { self.should_continue = false; break; }
                                if self.should_break { self.should_break = false; self.break_value = None; break; }
                            }
                            self.pop_scope();
                            if self.should_break { self.should_break = false; self.break_value = None; break; }
                            i += 1;
                        }
                    }
                    Value::Int(n) => {
                        let mut i = 0;
                        while i < n {
                            self.push_scope();
                            self.declare(var, Value::Int(i));
                            for s in body {
                                self.eval_stmt(s);
                                if self.returned.is_some() { self.pop_scope(); return; }
                                if self.should_continue { self.should_continue = false; break; }
                                if self.should_break { self.should_break = false; self.break_value = None; break; }
                            }
                            self.pop_scope();
                            if self.should_break { self.should_break = false; self.break_value = None; break; }
                            i += 1;
                        }
                    }
                    _ => rt_fail!(self, "for loop requires int or range"),
                }
            }
            StmtKind::Break(val) => {
                if let Some(expr) = val {
                    self.break_value = Some(self.eval_expr(expr));
                }
                self.should_break = true;
                return;
            }
            StmtKind::Continue(_) => { self.should_continue = true; return; }
            StmtKind::Assign { name, value } => {
                let v = self.eval_expr(value);
                self.assign(name, v);
            }
            StmtKind::If { cond, then_body, else_body } => {
                let cv = self.eval_expr(cond);
                let truthy = match cv { Value::Bool(b) => b, Value::Int(n) => n != 0, Value::Str(s) => !s.is_empty(), Value::Unit => false, _ => true };
                let branch = if truthy { Some(then_body) } else { else_body.as_ref() };
                if let Some(branch) = branch {
                    self.push_scope();
                    for s in branch {
                        self.eval_stmt(s);
                        if self.returned.is_some() { self.pop_scope(); return; }
                        // Leave the flags set: the enclosing loop clears them.
                        if self.should_break || self.should_continue { self.pop_scope(); return; }
                    }
                    self.pop_scope();
                }
            }
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Value {
        match expr {
            Expr::Int(n) => Value::Int(*n),
            Expr::Float(f) => Value::Float(*f),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Ident(name) => {
                if let Some(v) = self.resolve_variant(name) {
                    return Value::EnumVariant { enum_name: v.enum_name, variant: v.name, args: Vec::new() };
                }
                self.lookup(name)
            }
            Expr::Binary(l, op, r) => { let lv = self.eval_expr(l); let rv = self.eval_expr(r); self.eval_binop(&lv, op, &rv) }
            Expr::Unary(op, e) => {
                let v = self.eval_expr(e);
                match (op, &v) {
                    (UnaryOp::Neg, Value::Int(n)) => Value::Int(-n),
                    (UnaryOp::Neg, Value::Float(f)) => Value::Float(-f),
                    (UnaryOp::Not, Value::Bool(b)) => Value::Bool(!b),
                    (UnaryOp::Not, Value::Int(n)) => Value::Int(if *n == 0 { 1 } else { 0 }),
                    _ => rt_fail!(self, "invalid unary operation {:?} on {:?}", op, v),
                }
            }
            Expr::Call(callee, args) => {
                let name = match callee.as_ref() { Expr::Ident(n) => n, _ => rt_fail!(self, "cannot call non-function") };
                if name == "print" { let v = self.eval_expr(&args[0]); println!("{}", value_to_string(&v)); return Value::Unit; }
                if name == "len" { let v = self.eval_expr(&args[0]); return match v { Value::Str(s) => Value::Int(s.len() as i64), Value::Array(a) => Value::Int(a.len() as i64), _ => rt_fail!(self, "len expects string or array") }; }
                if name == "to_string" { let v = self.eval_expr(&args[0]); return Value::Str(value_to_string(&v)); }
                if name == "int_to_str" { let v = self.eval_expr(&args[0]); return match v { Value::Int(n) => Value::Str(n.to_string()), _ => rt_fail!(self, "int_to_str expects int") }; }
                if name == "panic" { let v = self.eval_expr(&args[0]); rt_fail!(self, "{}", value_to_string(&v)); }
                if name == "array_len" { let v = self.eval_expr(&args[0]); return match v { Value::Array(a) => Value::Int(a.len() as i64), _ => rt_fail!(self, "array_len expects array") }; }
                if name == "array_get" { let arr = self.eval_expr(&args[0]); let idx = self.eval_expr(&args[1]); return match (arr, idx) { (Value::Array(a), Value::Int(i)) => { let k = self.checked_index(a.len(), i, "array"); a[k].clone() }, _ => rt_fail!(self, "array_get expects array and int") }; }
                if name == "array_set" { let arr = self.eval_expr(&args[0]); let idx = self.eval_expr(&args[1]); let val = self.eval_expr(&args[2]); if let (Value::Array(mut a), Value::Int(i)) = (arr, idx) { let k = self.checked_index(a.len(), i, "array"); a[k] = val; return Value::Array(a); } else { rt_fail!(self, "array_set expects array, int, value"); } }
                if name == "substring" { let s = self.eval_expr(&args[0]); let start = self.eval_expr(&args[1]); let end = self.eval_expr(&args[2]); if let (Value::Str(s), Value::Int(start), Value::Int(end)) = (s, start, end) { let b = self.checked_index(s.len() + 1, start, "substring start");
                    let e = self.checked_index(s.len() + 1, end, "substring end");
                    if b > e { rt_fail!(self, "substring start {} is past end {}", b, e); }
                    if !s.is_char_boundary(b) || !s.is_char_boundary(e) { rt_fail!(self, "substring bounds fall inside a character"); }
                    return Value::Str(s[b..e].to_string()); } else { rt_fail!(self, "substring expects string, int, int"); } }
                if name == "contains" { let s = self.eval_expr(&args[0]); let needle = self.eval_expr(&args[1]); if let (Value::Str(s), Value::Str(needle)) = (s, needle) { return Value::Bool(s.contains(&needle)); } else { rt_fail!(self, "contains expects string, string"); } }
                if name == "split" { let s = self.eval_expr(&args[0]); let delim = self.eval_expr(&args[1]); if let (Value::Str(s), Value::Str(delim)) = (s, delim) { return Value::Array(s.split(&delim).map(|part| Value::Str(part.to_string())).collect()); } else { rt_fail!(self, "split expects string, string"); } }
                if name == "trim" { let s = self.eval_expr(&args[0]); if let Value::Str(s) = s { return Value::Str(s.trim().to_string()); } else { rt_fail!(self, "trim expects string"); } }
                if name == "char_at" { let s = self.eval_expr(&args[0]); let idx = self.eval_expr(&args[1]); if let (Value::Str(s), Value::Int(i)) = (s, idx) { let ch = s.chars().nth(i as usize).unwrap_or('\0'); return Value::Str(ch.to_string()); } else { rt_fail!(self, "char_at expects string, int"); } }
                if let Some(v) = self.resolve_variant(name) {
                    let eval_args: Vec<Value> = args.iter().map(|a| self.eval_expr(a)).collect();
                    return Value::EnumVariant { enum_name: v.enum_name, variant: v.name, args: eval_args };
                }
                if let Some(macro_stmt) = self.macros.get(name).cloned() {
                    return self.expand_macro(&macro_stmt, args);
                }
                if let Some(extern_fn) = self.extern_fns.get(name).cloned() {
                    return self.call_extern_fn(&extern_fn, args);
                }
                let func = self.fns.get(name).unwrap_or_else(|| rt_fail!(self, "undefined function: {}", name)).clone();
                if let StmtKind::Fn { params, body, .. } = func.kind {
                    // Arguments are evaluated in the caller's frame, then bound in a fresh one.
                    let mut frame = HashMap::new();
                    for (i, (pname, _)) in params.iter().enumerate() {
                        let v = self.eval_expr(&args[i]);
                        frame.insert(pname.clone(), v);
                    }
                    self.call_frame(frame, &body)
                } else { Value::Unit }
            }
            Expr::MethodCall(obj, method, args) => {
                let obj_val = self.eval_expr(obj);
                if method == "unwrap" {
                    match &obj_val {
                        Value::EnumVariant { enum_name, variant, args: inner } if enum_name == "Option" => {
                            if variant == "Some" { return inner[0].clone(); }
                            rt_fail!(self, "unwrap() called on None");
                        }
                        Value::EnumVariant { enum_name, variant, args: inner } if enum_name == "Result" => {
                            if variant == "Ok" { return inner[0].clone(); }
                            if variant == "Err" { rt_fail!(self, "unwrap() called on Err"); }
                            rt_fail!(self, "unknown Result variant: {}", variant);
                        }
                        _ => {}
                    }
                }
                if method == "is_some" {
                    if let Value::EnumVariant { variant, .. } = &obj_val {
                        return Value::Bool(variant == "Some");
                    }
                }
                if method == "is_none" {
                    if let Value::EnumVariant { variant, .. } = &obj_val {
                        return Value::Bool(variant == "None");
                    }
                }
                if method == "is_ok" {
                    if let Value::EnumVariant { variant, .. } = &obj_val {
                        return Value::Bool(variant == "Ok");
                    }
                }
                if method == "is_err" {
                    if let Value::EnumVariant { variant, .. } = &obj_val {
                        return Value::Bool(variant == "Err");
                    }
                }
                let type_name = match &obj_val {
                    Value::Struct { name, .. } => name.clone(),
                    Value::EnumVariant { enum_name, .. } => enum_name.clone(),
                    _ => rt_fail!(self, "no methods on this value"),
                };
                let impls_clone = self.impls.clone();
                for (tn, impl_type, methods) in &impls_clone {
                    if impl_type == &type_name {
                        let _ = tn;
                        for m in methods {
                            if let StmtKind::Fn { name: mname, params, body, .. } = &m.kind {
                                if mname == method {
                                    let self_count = if params.first().map(|(n, _)| n == "self" || n == "&self").unwrap_or(false) { 1 } else { 0 };
                                    let arg_vals: Vec<Value> = args.iter().map(|a| self.eval_expr(a)).collect();
                                    let mut frame = HashMap::new();
                                    if self_count > 0 { frame.insert("self".to_string(), obj_val.clone()); }
                                    for (i, (pname, _)) in params[self_count..].iter().enumerate() { frame.insert(pname.clone(), arg_vals[i].clone()); }
                                    return self.call_frame(frame, body);
                                }
                            }
                        }
                    }
                }
                rt_fail!(self, "method `{}` not found for `{}`", method, type_name);
            }
            Expr::FieldAccess(obj, field) => {
                match self.eval_expr(obj) {
                    Value::Struct { fields, .. } => fields.get(field).cloned().unwrap_or_else(|| rt_fail!(self, "field `{}` not found", field)),
                    _ => rt_fail!(self, "cannot access field on non-struct"),
                }
            }
            Expr::StructLit { name, fields } => {
                let mut field_map = HashMap::new();
                for (fname, fexpr) in fields { field_map.insert(fname.clone(), self.eval_expr(fexpr)); }
                Value::Struct { name: name.clone(), fields: field_map }
            }
            Expr::Match { scrutinee, arms } => {
                let sv = self.eval_expr(scrutinee);
                for (patterns, guard, body) in arms {
                    for pattern in patterns {
                        self.push_scope();
                        if self.match_pattern(pattern, &sv) {
                            let taken = match guard {
                                Some(g) => match self.eval_expr(g) {
                                    Value::Bool(b) => b,
                                    Value::Int(n) => n != 0,
                                    _ => true,
                                },
                                None => true,
                            };
                            if taken {
                                let v = self.eval_expr(body);
                                self.pop_scope();
                                return v;
                            }
                        }
                        self.pop_scope();
                    }
                }
                rt_fail!(self, "no matching pattern");
            }
            Expr::If { cond, then_body, else_body } => {
                let cv = self.eval_expr(cond);
                let truthy = match cv { Value::Bool(b) => b, Value::Int(n) => n != 0, Value::Str(s) => !s.is_empty(), Value::Unit => false, _ => true };
                if truthy {
                    self.eval_expr(then_body)
                } else if let Some(eb) = else_body {
                    self.eval_expr(eb)
                } else {
                    Value::Unit
                }
            }
            Expr::While { cond, body } => {
                let mut result = Value::Unit;
                loop {
                    let cv = self.eval_expr(cond);
                    let truthy = match cv { Value::Bool(b) => b, Value::Int(n) => n != 0, _ => true };
                    if !truthy { break; }
                    self.push_scope();
                    for s in body {
                        self.eval_stmt(s);
                        if self.should_break {
                            if let Some(bv) = self.break_value.take() {
                                result = bv;
                            }
                            self.should_break = false;
                            break;
                        }
                        if self.should_continue { self.should_continue = false; break; }
                        if self.returned.is_some() { self.pop_scope(); return self.returned.take().unwrap(); }
                    }
                    self.pop_scope();
                    if self.should_break {
                        if let Some(bv) = self.break_value.take() {
                            result = bv;
                        }
                        self.should_break = false;
                        break;
                    }
                }
                result
            }
            Expr::For { var, iter, body } => {
                let iter_val = self.eval_expr(iter);
                let mut result = Value::Unit;
                match iter_val {
                    Value::Range(start, end) => {
                        'outer: for i in start..end {
                            self.push_scope();
                            self.declare(&var, Value::Int(i));
                            for s in body {
                                self.eval_stmt(s);
                                if self.should_break {
                                    if let Some(bv) = self.break_value.take() { result = bv; }
                                    self.should_break = false;
                                    self.pop_scope();
                                    break 'outer;
                                }
                                if self.should_continue { self.should_continue = false; break; }
                                if self.returned.is_some() { self.pop_scope(); return self.returned.take().unwrap(); }
                            }
                            self.pop_scope();
                        }
                    }
                    Value::Array(arr) => {
                        'outer2: for elem in arr {
                            self.push_scope();
                            self.declare(&var, elem);
                            for s in body {
                                self.eval_stmt(s);
                                if self.should_break {
                                    if let Some(bv) = self.break_value.take() { result = bv; }
                                    self.should_break = false;
                                    self.pop_scope();
                                    break 'outer2;
                                }
                                if self.should_continue { self.should_continue = false; break; }
                                if self.returned.is_some() { self.pop_scope(); return self.returned.take().unwrap(); }
                            }
                            self.pop_scope();
                        }
                    }
                    _ => rt_fail!(self, "for loop requires range or array"),
                }
                result
            }
            Expr::Range(a, b) => {
                let av = self.eval_expr(a);
                let bv = self.eval_expr(b);
                match (av, bv) {
                    (Value::Int(a), Value::Int(b)) => Value::Range(a, b),
                    _ => rt_fail!(self, "range requires two ints"),
                }
            }
            Expr::ArrayLit(elems) => {
                let vals: Vec<Value> = elems.iter().map(|e| self.eval_expr(e)).collect();
                Value::Array(vals)
            }
            Expr::Index(arr, idx) => {
                let arr_val = self.eval_expr(arr);
                let idx_val = self.eval_expr(idx);
                match (arr_val, idx_val) {
                    (Value::Array(a), Value::Int(i)) => { let k = self.checked_index(a.len(), i, "array"); a[k].clone() }
                    _ => rt_fail!(self, "array indexing requires array and int"),
                }
            }
        }
    }

    fn expand_macro(&mut self, macro_stmt: &Stmt, args: &[Expr]) -> Value {
        if let StmtKind::Macro { params, body, .. } = &macro_stmt.kind {
            let mut frame = HashMap::new();
            for (i, pname) in params.iter().enumerate() {
                let v = self.eval_expr(&args[i]);
                frame.insert(pname.clone(), v);
            }
            self.call_frame(frame, body)
        } else {
            Value::Unit
        }
    }

    fn call_extern_fn(&mut self, extern_fn: &Stmt, args: &[Expr]) -> Value {
            if let StmtKind::ExternFn { name, .. } = &extern_fn.kind {
            match name.as_str() {
                "clock" => {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
                    Value::Int(t)
                }
                "sleep_ms" => {
                    let v = self.eval_expr(&args[0]);
                    if let Value::Int(ms) = v {
                        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
                    }
                    Value::Unit
                }
                _ => {
                    rt_fail!(self, "extern fn `{}` not yet implemented in interpreter", name);
                }
            }
        } else {
            Value::Unit
        }
    }

    fn match_pattern(&mut self, pattern: &Pattern, value: &Value) -> bool {
        match pattern {
            Pattern::Literal(expr) => { let pv = self.eval_expr(expr); values_equal(&pv, value) }
            Pattern::Variable(name) => { self.declare(name, value.clone()); true }
            Pattern::Wildcard => true,
            Pattern::EnumVariant { name, inner } => {
                let pat = match self.resolve_variant(name) { Some(v) => v, None => return false };
                if let Value::EnumVariant { enum_name, variant, args } = value {
                    if *enum_name == pat.enum_name && *variant == pat.name && args.len() == inner.len() {
                        for (pat, arg) in inner.iter().zip(args.iter()) {
                            if !self.match_pattern(pat, arg) { return false; }
                        }
                        return true;
                    }
                }
                false
            }
        }
    }
}

impl Interpreter {
fn eval_binop(&self, l: &Value, op: &BinOp, r: &Value) -> Value {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => match op {
            BinOp::Add => Value::Int(a + b), BinOp::Sub => Value::Int(a - b),
            BinOp::Mul => Value::Int(a * b), BinOp::Div => Value::Int(a / b), BinOp::Mod => Value::Int(a % b),
            BinOp::Eq => Value::Bool(a == b), BinOp::Ne => Value::Bool(a != b),
            BinOp::Lt => Value::Bool(a < b), BinOp::Le => Value::Bool(a <= b),
            BinOp::Gt => Value::Bool(a > b), BinOp::Ge => Value::Bool(a >= b),
            BinOp::And => Value::Bool(*a != 0 && *b != 0), BinOp::Or => Value::Bool(*a != 0 || *b != 0),
        },
        (Value::Float(a), Value::Float(b)) => match op {
            BinOp::Add => Value::Float(a + b), BinOp::Sub => Value::Float(a - b),
            BinOp::Mul => Value::Float(a * b), BinOp::Div => Value::Float(a / b),
            _ => rt_fail!(self, "unsupported float op"),
        },
        (Value::Bool(a), Value::Bool(b)) => match op {
            BinOp::Eq => Value::Bool(a == b), BinOp::Ne => Value::Bool(a != b),
            BinOp::And => Value::Bool(*a && *b), BinOp::Or => Value::Bool(*a || *b),
            _ => rt_fail!(self, "unsupported bool op"),
        },
        (Value::Str(a), Value::Str(b)) => match op {
            BinOp::Add => Value::Str(format!("{}{}", a, b)),
            BinOp::Eq => Value::Bool(a == b), BinOp::Ne => Value::Bool(a != b),
            _ => rt_fail!(self, "unsupported string op"),
        },
        (Value::Str(a), Value::Int(b)) => match op {
            BinOp::Add => Value::Str(format!("{}{}", a, b)),
            _ => rt_fail!(self, "unsupported string+int op"),
        },
        (Value::Int(a), Value::Str(b)) => match op {
            BinOp::Add => Value::Str(format!("{}{}", a, b)),
            _ => rt_fail!(self, "unsupported int+string op"),
        },
        _ => rt_fail!(self, "type mismatch in binary operation"),
    }
}
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Str(a), Value::Str(b)) => a == b,
        _ => false,
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Str(s) => s.clone(),
        Value::Unit => "()".to_string(),
        Value::Fn(n) => format!("<fn {}>", n),
        Value::Struct { name, fields } => {
            let fs: Vec<String> = fields.iter().map(|(k, v)| format!("{}: {}", k, value_to_string(v))).collect();
            format!("{} {{ {} }}", name, fs.join(", "))
        }
        Value::EnumVariant { variant, args, .. } => {
            if args.is_empty() { variant.clone() } else {
                let as_: Vec<String> = args.iter().map(value_to_string).collect();
                format!("{}({})", variant, as_.join(", "))
            }
        }
        Value::Range(a, b) => format!("{}..{}", a, b),
        Value::Array(elems) => {
            let es: Vec<String> = elems.iter().map(value_to_string).collect();
            format!("[{}]", es.join(", "))
        }
    }
}
