//! Minimal tree-walk interpreter used as the default backend.

use crate::ast::*;
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

struct Env {
    vars: HashMap<String, Value>,
    parent: Option<Box<Env>>,
    structs: HashMap<String, Vec<(String, Type)>>,
    enums: HashMap<String, Vec<(String, Vec<Type>)>>,
    impls: Vec<(String, String, Vec<Stmt>)>,
    macros: HashMap<String, Stmt>,
    extern_fns: HashMap<String, Stmt>,
}

impl Env {
    fn new() -> Self {
        Env { vars: HashMap::new(), parent: None, structs: HashMap::new(), enums: HashMap::new(), impls: Vec::new(), macros: HashMap::new(), extern_fns: HashMap::new() }
    }
    fn get(&self, name: &str) -> Value {
        if let Some(v) = self.vars.get(name) { return v.clone(); }
        if let Some(p) = &self.parent { return p.get(name); }
        panic!("undefined variable: {}", name)
    }
    fn set(&mut self, name: &str, v: Value) { self.vars.insert(name.to_string(), v); }
}

pub struct Interpreter {
    fns: HashMap<String, Stmt>,
    returned: Option<Value>,
    should_break: bool,
    should_continue: bool,
}

impl Interpreter {
    pub fn new(program: &Program) -> Self {
        let mut fns = HashMap::new();
        for s in &program.stmts {
            if let Stmt::Fn { name, .. } = s { fns.insert(name.clone(), s.clone()); }
        }
        Interpreter { fns, returned: None, should_break: false, should_continue: false }
    }

    pub fn run(&mut self, program: &Program) {
        let mut env = Env::new();
        for s in &program.stmts {
            match s {
                Stmt::Struct { name, fields, .. } => { env.structs.insert(name.clone(), fields.clone()); }
                Stmt::Enum { name, variants } => { env.enums.insert(name.clone(), variants.clone()); }
                Stmt::Impl { trait_name, type_name, methods } => { env.impls.push((trait_name.clone(), type_name.clone(), methods.clone())); }
                Stmt::Macro { name, .. } => { env.macros.insert(name.clone(), s.clone()); }
                Stmt::ExternFn { name, .. } => { env.extern_fns.insert(name.clone(), s.clone()); }
                _ => {}
            }
        }
        for s in &program.stmts {
            if matches!(s, Stmt::Fn { .. } | Stmt::Struct { .. } | Stmt::Enum { .. } | Stmt::Trait { .. } | Stmt::Macro { .. } | Stmt::ExternFn { .. } | Stmt::Impl { .. }) { continue; }
            self.eval_stmt(s, &mut env);
        }
        if let Some(main_fn) = self.fns.get("main").cloned() {
            if let Stmt::Fn { body, .. } = main_fn {
                let mut local = Env { vars: HashMap::new(), parent: None, structs: env.structs.clone(), enums: env.enums.clone(), impls: env.impls.clone(), macros: env.macros.clone(), extern_fns: env.extern_fns.clone() };
                for s in &body {
                    self.eval_stmt(s, &mut local);
                    if self.returned.is_some() { break; }
                }
            }
        }
    }

    fn eval_stmt(&mut self, stmt: &Stmt, env: &mut Env) {
        match stmt {
            Stmt::Let { name, value, .. } => { let v = self.eval_expr(value, env); env.set(name, v); }
            Stmt::Fn { .. } | Stmt::Struct { .. } | Stmt::Enum { .. } | Stmt::Trait { .. } | Stmt::Macro { .. } | Stmt::ExternFn { .. } | Stmt::Impl { .. } => {}
            Stmt::Expr(e) => { self.eval_expr(e, env); }
            Stmt::Return(e) => { self.returned = Some(match e { Some(x) => self.eval_expr(x, env), None => Value::Unit }); }
            Stmt::While { cond, body } => {
                loop {
                    let cv = self.eval_expr(cond, env);
                    let truthy = match cv { Value::Bool(b) => b, Value::Int(n) => n != 0, _ => true };
                    if !truthy { break; }
                    for s in body {
                        self.eval_stmt(s, env);
                        if self.returned.is_some() { return; }
                        if self.should_continue { self.should_continue = false; break; }
                        if self.should_break { self.should_break = false; break; }
                    }
                    if self.should_break { self.should_break = false; break; }
                }
            }
            Stmt::For { var, iter, body } => {
                let iter_val = self.eval_expr(iter, env);
                match iter_val {
                    Value::Range(start, end) => {
                        let mut i = start;
                        while i < end {
                            env.set(var, Value::Int(i));
                            for s in body {
                                self.eval_stmt(s, env);
                                if self.returned.is_some() { return; }
                                if self.should_continue { self.should_continue = false; break; }
                                if self.should_break { self.should_break = false; break; }
                            }
                            if self.should_break { self.should_break = false; break; }
                            i += 1;
                        }
                    }
                    Value::Int(n) => {
                        let mut i = 0;
                        while i < n {
                            env.set(var, Value::Int(i));
                            for s in body {
                                self.eval_stmt(s, env);
                                if self.returned.is_some() { return; }
                                if self.should_continue { self.should_continue = false; break; }
                                if self.should_break { self.should_break = false; break; }
                            }
                            if self.should_break { self.should_break = false; break; }
                            i += 1;
                        }
                    }
                    _ => panic!("for loop requires int or range"),
                }
            }
            Stmt::Break => { self.should_break = true; return; }
            Stmt::Continue => { self.should_continue = true; return; }
            Stmt::Assign { name, value } => {
                let v = self.eval_expr(value, env);
                env.set(name, v);
            }
            Stmt::If { cond, then_body, else_body } => {
                let cv = self.eval_expr(cond, env);
                let truthy = match cv { Value::Bool(b) => b, Value::Int(n) => n != 0, Value::Str(s) => !s.is_empty(), Value::Unit => false, _ => true };
                if truthy {
                    for s in then_body {
                        self.eval_stmt(s, env);
                        if self.returned.is_some() { return; }
                        if self.should_break { return; }
                    }
                } else if let Some(eb) = else_body {
                    for s in eb {
                        self.eval_stmt(s, env);
                        if self.returned.is_some() { return; }
                        if self.should_break { return; }
                    }
                }
            }
        }
    }

    fn eval_expr(&mut self, expr: &Expr, env: &mut Env) -> Value {
        match expr {
            Expr::Int(n) => Value::Int(*n),
            Expr::Float(f) => Value::Float(*f),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Ident(name) => {
                if let Some(_) = env.enums.values().find_map(|v| v.iter().find(|(vn, _)| vn == name)) {
                    let enum_name = env.enums.iter().find(|(_, v)| v.iter().any(|(vn, _)| vn == name)).map(|(n, _)| n.clone()).unwrap();
                    return Value::EnumVariant { enum_name, variant: name.clone(), args: Vec::new() };
                }
                env.get(name)
            }
            Expr::Binary(l, op, r) => { let lv = self.eval_expr(l, env); let rv = self.eval_expr(r, env); eval_binop(&lv, op, &rv) }
            Expr::Call(callee, args) => {
                let name = match callee.as_ref() { Expr::Ident(n) => n, _ => panic!("cannot call non-function") };
                if name == "print" { let v = self.eval_expr(&args[0], env); println!("{}", value_to_string(&v)); return Value::Unit; }
                if name == "len" { let v = self.eval_expr(&args[0], env); return match v { Value::Str(s) => Value::Int(s.len() as i64), _ => panic!("len expects string") }; }
                if name == "to_string" { let v = self.eval_expr(&args[0], env); return Value::Str(value_to_string(&v)); }
                if name == "int_to_str" { let v = self.eval_expr(&args[0], env); return match v { Value::Int(n) => Value::Str(n.to_string()), _ => panic!("int_to_str expects int") }; }
                if name == "panic" { let v = self.eval_expr(&args[0], env); panic!("{}", value_to_string(&v)); }
                if name == "array_len" { let v = self.eval_expr(&args[0], env); return match v { Value::Array(a) => Value::Int(a.len() as i64), _ => panic!("array_len expects array") }; }
                if name == "array_get" { let arr = self.eval_expr(&args[0], env); let idx = self.eval_expr(&args[1], env); return match (arr, idx) { (Value::Array(a), Value::Int(i)) => a[i as usize].clone(), _ => panic!("array_get expects array and int") }; }
                if name == "array_set" { let arr = self.eval_expr(&args[0], env); let idx = self.eval_expr(&args[1], env); let val = self.eval_expr(&args[2], env); if let (Value::Array(mut a), Value::Int(i)) = (arr, idx) { a[i as usize] = val; return Value::Array(a); } else { panic!("array_set expects array, int, value"); } }
                if let Some(vdef) = env.enums.values().find_map(|v| v.iter().find(|(vn, _)| vn == name)) {
                    let _ = vdef;
                    let eval_args: Vec<Value> = args.iter().map(|a| self.eval_expr(a, env)).collect();
                    let enum_name = env.enums.iter().find(|(_, v)| v.iter().any(|(vn, _)| vn == name)).map(|(n, _)| n.clone()).unwrap();
                    return Value::EnumVariant { enum_name, variant: name.clone(), args: eval_args };
                }
                if let Some(macro_stmt) = env.macros.get(name).cloned() {
                    return self.expand_macro(&macro_stmt, args, env);
                }
                if let Some(extern_fn) = env.extern_fns.get(name).cloned() {
                    return self.call_extern_fn(&extern_fn, args, env);
                }
                let func = self.fns.get(name).unwrap_or_else(|| panic!("undefined function: {}", name)).clone();
                if let Stmt::Fn { params, body, .. } = func {
                    let mut local = Env { vars: HashMap::new(), parent: Some(Box::new(Env::new())), structs: env.structs.clone(), enums: env.enums.clone(), impls: env.impls.clone(), macros: env.macros.clone(), extern_fns: env.extern_fns.clone() };
                    for (i, (pname, _)) in params.iter().enumerate() { local.set(pname, self.eval_expr(&args[i], env)); }
                    for s in &body { self.eval_stmt(s, &mut local); if self.returned.is_some() { return self.returned.take().unwrap(); } }
                    Value::Unit
                } else { Value::Unit }
            }
            Expr::MethodCall(obj, method, args) => {
                let obj_val = self.eval_expr(obj, env);
                let type_name = match &obj_val {
                    Value::Struct { name, .. } => name.clone(),
                    Value::EnumVariant { enum_name, .. } => enum_name.clone(),
                    _ => panic!("no methods on this value"),
                };
                let impls_clone = env.impls.clone();
                for (tn, impl_type, methods) in &impls_clone {
                    if impl_type == &type_name {
                        let _ = tn;
                        for m in methods {
                            if let Stmt::Fn { name: mname, params, body, .. } = m {
                                if mname == method {
                                    let self_count = if params.first().map(|(n, _)| n == "self" || n == "&self").unwrap_or(false) { 1 } else { 0 };
                                    let mut local = Env { vars: HashMap::new(), parent: Some(Box::new(Env::new())), structs: env.structs.clone(), enums: env.enums.clone(), impls: env.impls.clone(), macros: env.macros.clone(), extern_fns: env.extern_fns.clone() };
                                    if self_count > 0 { local.set("self", obj_val.clone()); }
                                    let args_clone = args.clone();
                                    let arg_vals: Vec<Value> = args_clone.iter().map(|a| self.eval_expr(a, env)).collect();
                                    for (i, (pname, _)) in params[self_count..].iter().enumerate() { local.set(pname, arg_vals[i].clone()); }
                                    for s in body { self.eval_stmt(s, &mut local); if self.returned.is_some() { return self.returned.take().unwrap(); } }
                                }
                            }
                        }
                    }
                }
                panic!("method `{}` not found for `{}`", method, type_name);
            }
            Expr::FieldAccess(obj, field) => {
                match self.eval_expr(obj, env) {
                    Value::Struct { fields, .. } => fields.get(field).cloned().unwrap_or_else(|| panic!("field `{}` not found", field)),
                    _ => panic!("cannot access field on non-struct"),
                }
            }
            Expr::StructLit { name, fields } => {
                let mut field_map = HashMap::new();
                for (fname, fexpr) in fields { field_map.insert(fname.clone(), self.eval_expr(fexpr, env)); }
                Value::Struct { name: name.clone(), fields: field_map }
            }
            Expr::Match { scrutinee, arms } => {
                let sv = self.eval_expr(scrutinee, env);
                for (pattern, body) in arms { if self.match_pattern(pattern, &sv, env) { return self.eval_expr(body, env); } }
                panic!("no matching pattern");
            }
            Expr::If { cond, then_body, else_body } => {
                let cv = self.eval_expr(cond, env);
                let truthy = match cv { Value::Bool(b) => b, Value::Int(n) => n != 0, Value::Str(s) => !s.is_empty(), Value::Unit => false, _ => true };
                if truthy {
                    self.eval_expr(then_body, env)
                } else if let Some(eb) = else_body {
                    self.eval_expr(eb, env)
                } else {
                    Value::Unit
                }
            }
            Expr::Range(a, b) => {
                let av = self.eval_expr(a, env);
                let bv = self.eval_expr(b, env);
                match (av, bv) {
                    (Value::Int(a), Value::Int(b)) => Value::Range(a, b),
                    _ => panic!("range requires two ints"),
                }
            }
            Expr::ArrayLit(elems) => {
                let vals: Vec<Value> = elems.iter().map(|e| self.eval_expr(e, env)).collect();
                Value::Array(vals)
            }
            Expr::Index(arr, idx) => {
                let arr_val = self.eval_expr(arr, env);
                let idx_val = self.eval_expr(idx, env);
                match (arr_val, idx_val) {
                    (Value::Array(a), Value::Int(i)) => a[i as usize].clone(),
                    _ => panic!("array indexing requires array and int"),
                }
            }
        }
    }

    fn expand_macro(&mut self, macro_stmt: &Stmt, args: &[Expr], env: &mut Env) -> Value {
        if let Stmt::Macro { params, body, .. } = macro_stmt {
            let mut local = Env { vars: HashMap::new(), parent: None, structs: env.structs.clone(), enums: env.enums.clone(), impls: env.impls.clone(), macros: env.macros.clone(), extern_fns: env.extern_fns.clone() };
            for (i, pname) in params.iter().enumerate() {
                let v = self.eval_expr(&args[i], env);
                local.set(pname, v);
            }
            for s in body {
                self.eval_stmt(s, &mut local);
                if self.returned.is_some() {
                    return self.returned.take().unwrap();
                }
            }
            Value::Unit
        } else {
            Value::Unit
        }
    }

    fn call_extern_fn(&mut self, extern_fn: &Stmt, args: &[Expr], env: &mut Env) -> Value {
            if let Stmt::ExternFn { name, .. } = extern_fn {
            match name.as_str() {
                "clock" => {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
                    Value::Int(t)
                }
                "sleep_ms" => {
                    let v = self.eval_expr(&args[0], env);
                    if let Value::Int(ms) = v {
                        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
                    }
                    Value::Unit
                }
                _ => {
                    panic!("extern fn `{}` not yet implemented in interpreter", name);
                }
            }
        } else {
            Value::Unit
        }
    }

    fn match_pattern(&mut self, pattern: &Pattern, value: &Value, env: &mut Env) -> bool {
        match pattern {
            Pattern::Literal(expr) => { let pv = self.eval_expr(expr, env); values_equal(&pv, value) }
            Pattern::Variable(name) => { env.set(name, value.clone()); true }
            Pattern::Wildcard => true,
            Pattern::EnumVariant { name, inner } => {
                if let Value::EnumVariant { variant, args, .. } = value {
                    if variant == name && args.len() == inner.len() {
                        for (pat, arg) in inner.iter().zip(args.iter()) {
                            if !self.match_pattern(pat, arg, env) { return false; }
                        }
                        return true;
                    }
                }
                false
            }
        }
    }
}

fn eval_binop(l: &Value, op: &BinOp, r: &Value) -> Value {
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => match op {
            BinOp::Add => Value::Int(a + b), BinOp::Sub => Value::Int(a - b),
            BinOp::Mul => Value::Int(a * b), BinOp::Div => Value::Int(a / b), BinOp::Mod => Value::Int(a % b),
            BinOp::Eq => Value::Bool(a == b), BinOp::Ne => Value::Bool(a != b),
            BinOp::Lt => Value::Bool(a < b), BinOp::Le => Value::Bool(a <= b),
            BinOp::Gt => Value::Bool(a > b), BinOp::Ge => Value::Bool(a >= b),
        },
        (Value::Float(a), Value::Float(b)) => match op {
            BinOp::Add => Value::Float(a + b), BinOp::Sub => Value::Float(a - b),
            BinOp::Mul => Value::Float(a * b), BinOp::Div => Value::Float(a / b),
            _ => panic!("unsupported float op"),
        },
        (Value::Str(a), Value::Str(b)) => match op {
            BinOp::Add => Value::Str(format!("{}{}", a, b)),
            BinOp::Eq => Value::Bool(a == b), BinOp::Ne => Value::Bool(a != b),
            _ => panic!("unsupported string op"),
        },
        (Value::Str(a), Value::Int(b)) => match op {
            BinOp::Add => Value::Str(format!("{}{}", a, b)),
            _ => panic!("unsupported string+int op"),
        },
        (Value::Int(a), Value::Str(b)) => match op {
            BinOp::Add => Value::Str(format!("{}{}", a, b)),
            _ => panic!("unsupported int+string op"),
        },
        _ => panic!("type mismatch in binary op"),
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
