//! Minimal tree-walk interpreter used as the default backend (no LLVM needed).

use crate::ast::*;
use std::collections::HashMap;

#[derive(Debug, Clone)]
enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Unit,
    Fn(String),
    Struct {
        name: String,
        fields: HashMap<String, Value>,
    },
    EnumVariant {
        enum_name: String,
        variant: String,
        args: Vec<Value>,
    },
}

struct Env {
    vars: HashMap<String, Value>,
    parent: Option<Box<Env>>,
    structs: HashMap<String, Vec<(String, Type)>>,
    enums: HashMap<String, Vec<(String, Vec<Type>)>>,
}

impl Env {
    fn new() -> Self {
        Env {
            vars: HashMap::new(),
            parent: None,
            structs: HashMap::new(),
            enums: HashMap::new(),
        }
    }

    fn get(&self, name: &str) -> Value {
        if let Some(v) = self.vars.get(name) {
            v.clone()
        } else if let Some(p) = &self.parent {
            p.get(name)
        } else {
            panic!("undefined variable: {}", name)
        }
    }

    fn set(&mut self, name: &str, v: Value) {
        self.vars.insert(name.to_string(), v);
    }
}

pub struct Interpreter {
    fns: HashMap<String, Stmt>,
    returned: Option<Value>,
}

impl Interpreter {
    pub fn new(program: &Program) -> Self {
        let mut fns = HashMap::new();
        for s in &program.stmts {
            if let Stmt::Fn { name, .. } = s {
                fns.insert(name.clone(), s.clone());
            }
        }
        Interpreter { fns, returned: None }
    }

    pub fn run(&mut self, program: &Program) {
        let mut env = Env::new();

        // Register struct and enum definitions.
        for s in &program.stmts {
            match s {
                Stmt::Struct { name, fields } => {
                    env.structs.insert(name.clone(), fields.clone());
                }
                Stmt::Enum { name, variants } => {
                    env.enums.insert(name.clone(), variants.clone());
                }
                _ => {}
            }
        }

        // Execute top-level statements (skip fn defs).
        for s in &program.stmts {
            if let Stmt::Fn { .. } = s {
                continue;
            }
            self.eval_stmt(s, &mut env);
        }
    }

    fn eval_stmt(&mut self, stmt: &Stmt, env: &mut Env) {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let v = self.eval_expr(value, env);
                env.set(name, v);
            }
            Stmt::Fn { .. } => {}
            Stmt::Struct { .. } | Stmt::Enum { .. } => {}
            Stmt::Expr(e) => {
                self.eval_expr(e, env);
            }
            Stmt::Return(e) => {
                self.returned = Some(match e {
                    Some(x) => self.eval_expr(x, env),
                    None => Value::Unit,
                });
            }
        }
    }

    fn eval_expr(&mut self, expr: &Expr, env: &mut Env) -> Value {
        match expr {
            Expr::Int(n) => Value::Int(*n),
            Expr::Float(f) => Value::Float(*f),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Ident(name) => env.get(name),
            Expr::Binary(l, op, r) => {
                let lv = self.eval_expr(l, env);
                let rv = self.eval_expr(r, env);
                eval_binop(&lv, op, &rv)
            }
            Expr::Call(callee, args) => {
                let name = match callee.as_ref() {
                    Expr::Ident(n) => n,
                    _ => panic!("cannot call non-function"),
                };
                // Builtin: print
                if name == "print" {
                    let v = self.eval_expr(&args[0], env);
                    println!("{}", value_to_string(&v));
                    return Value::Unit;
                }
                // Check if it's an enum variant constructor.
                if let Some(vdef) = env.enums.values().find_map(|variants| {
                    variants.iter().find(|(vname, _)| vname == name)
                }) {
                    if vdef.1.len() != args.len() {
                        panic!("wrong arity for variant {}", name);
                    }
                    let eval_args: Vec<Value> = args.iter().map(|a| self.eval_expr(a, env)).collect();
                    let enum_name = env.enums.iter()
                        .find(|(_, v)| v.iter().any(|(vn, _)| vn == name))
                        .map(|(n, _)| n.clone())
                        .unwrap();
                    return Value::EnumVariant {
                        enum_name,
                        variant: name.clone(),
                        args: eval_args,
                    };
                }
                // User-defined function.
                let func = self.fns.get(name).unwrap_or_else(|| panic!("undefined function: {}", name)).clone();
                if let Stmt::Fn { params, body, .. } = func {
                    let mut local = Env {
                        vars: HashMap::new(),
                        parent: Some(Box::new(Env::new())),
                        structs: env.structs.clone(),
                        enums: env.enums.clone(),
                    };
                    for (i, (pname, _)) in params.iter().enumerate() {
                        let av = self.eval_expr(&args[i], env);
                        local.set(pname, av);
                    }
                    for s in &body {
                        self.eval_stmt(s, &mut local);
                        if self.returned.is_some() {
                            let v = self.returned.take().unwrap();
                            return v;
                        }
                    }
                    Value::Unit
                } else {
                    Value::Unit
                }
            }
            Expr::FieldAccess(obj, field) => {
                let v = self.eval_expr(obj, env);
                match v {
                    Value::Struct { fields, .. } => fields.get(field).cloned()
                        .unwrap_or_else(|| panic!("field `{}` not found", field)),
                    _ => panic!("cannot access field on non-struct value"),
                }
            }
            Expr::StructLit { name, fields } => {
                let mut field_map = HashMap::new();
                for (fname, fexpr) in fields {
                    field_map.insert(fname.clone(), self.eval_expr(fexpr, env));
                }
                Value::Struct {
                    name: name.clone(),
                    fields: field_map,
                }
            }
            Expr::Match { scrutinee, arms } => {
                let sv = self.eval_expr(scrutinee, env);
                for (pattern, body) in arms {
                    if self.match_pattern(pattern, &sv, env) {
                        return self.eval_expr(body, env);
                    }
                }
                panic!("no matching pattern in match expression");
            }
        }
    }

    fn match_pattern(&mut self, pattern: &Pattern, value: &Value, env: &mut Env) -> bool {
        match pattern {
            Pattern::Literal(expr) => {
                let pv = self.eval_expr(expr, env);
                values_equal(&pv, value)
            }
            Pattern::Variable(name) => {
                env.set(name, value.clone());
                true
            }
            Pattern::Wildcard => true,
            Pattern::EnumVariant { name, inner } => {
                if let Value::EnumVariant { variant, args, .. } = value {
                    if variant == name && args.len() == inner.len() {
                        for (pat, arg) in inner.iter().zip(args.iter()) {
                            if !self.match_pattern(pat, arg, env) {
                                return false;
                            }
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
            BinOp::Add => Value::Int(a + b),
            BinOp::Sub => Value::Int(a - b),
            BinOp::Mul => Value::Int(a * b),
            BinOp::Div => Value::Int(a / b),
            BinOp::Eq => Value::Bool(a == b),
            BinOp::Ne => Value::Bool(a != b),
            BinOp::Lt => Value::Bool(a < b),
            BinOp::Gt => Value::Bool(a > b),
        },
        (Value::Float(a), Value::Float(b)) => match op {
            BinOp::Add => Value::Float(a + b),
            BinOp::Sub => Value::Float(a - b),
            BinOp::Mul => Value::Float(a * b),
            BinOp::Div => Value::Float(a / b),
            _ => panic!("unsupported float op"),
        },
        (Value::Str(a), Value::Str(b)) => match op {
            BinOp::Eq => Value::Bool(a == b),
            BinOp::Ne => Value::Bool(a != b),
            _ => panic!("unsupported string op"),
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
            let fs: Vec<String> = fields.iter()
                .map(|(k, v)| format!("{}: {}", k, value_to_string(v)))
                .collect();
            format!("{} {{ {} }}", name, fs.join(", "))
        }
        Value::EnumVariant { variant, args, .. } => {
            if args.is_empty() {
                variant.clone()
            } else {
                let arg_strs: Vec<String> = args.iter().map(value_to_string).collect();
                format!("{}({})", variant, arg_strs.join(", "))
            }
        }
    }
}
