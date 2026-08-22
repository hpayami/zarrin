//! Minimal tree-walk interpreter used as the default backend (no LLVM needed).
//! The real LLVM backend is gated behind the `llvm` feature.

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
}

struct Env {
    vars: HashMap<String, Value>,
    parent: Option<Box<Env>>,
}

impl Env {
    fn new() -> Self {
        Env {
            vars: HashMap::new(),
            parent: None,
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
            Stmt::Expr(e) => {
                self.eval_expr(e, env);
            }
            Stmt::Return(e) => {
                self.returned = Some(match e {
                    Some(x) => self.eval_expr(x, env),
                    None => Value::Unit,
                });
            }
            Stmt::Fn { .. } => {}
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
                if let Expr::Ident(name) = callee.as_ref() {
                    if name == "print" {
                        let v = self.eval_expr(&args[0], env);
                        println!("{}", value_to_string(&v));
                        return Value::Unit;
                    }
                    let func = self
                        .fns
                        .get(name)
                        .unwrap_or_else(|| panic!("undefined function: {}", name))
                        .clone();
                    if let Stmt::Fn {
                        params, body, ..
                    } = func
                    {
                        let mut local = Env {
                            vars: HashMap::new(),
                            parent: Some(Box::new(Env::new())),
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
                } else {
                    panic!("cannot call non-function");
                }
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
        _ => panic!("type mismatch in binary op"),
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
    }
}
