//! LLVM backend (real native code generation).
//!
//! Enabled via the `llvm` Cargo feature. Requires system LLVM 18
//! (e.g. `brew install llvm@18`) and the `LLVM_SYS_180_PREFIX` env var
//! pointing at its prefix.

#![cfg(feature = "llvm")]

use crate::ast::*;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{BasicMetadataTypeEnum, FloatType, IntType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};
use either::Either;
use inkwell::AddressSpace;
use std::collections::HashMap;
use std::process::Command;

#[derive(Clone)]
enum CgValue<'ctx> {
    Int(IntValue<'ctx>),
    Float(FloatValue<'ctx>),
    Str(PointerValue<'ctx>),
}

impl<'ctx> CgValue<'ctx> {
    fn as_basic(&self) -> BasicValueEnum<'ctx> {
        match self {
            CgValue::Int(v) => (*v).into(),
            CgValue::Float(v) => (*v).into(),
            CgValue::Str(p) => (*p).into(),
        }
    }
}

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    printf: FunctionValue<'ctx>,
    functions: HashMap<String, FunctionValue<'ctx>>,
    named: HashMap<String, (PointerValue<'ctx>, String)>,
    i64: IntType<'ctx>,
    f64: FloatType<'ctx>,
    i1: IntType<'ctx>,
}

impl<'ctx> Codegen<'ctx> {
    fn new(context: &'ctx Context) -> Self {
        let module = context.create_module("zarrin");
        let builder = context.create_builder();
        let i64 = context.i64_type();
        let f64 = context.f64_type();
        let i1 = context.bool_type();
        let i8_ptr: BasicMetadataTypeEnum = context
            .ptr_type(AddressSpace::default())
            .into();
        let printf_type = i64.fn_type(&[i8_ptr], true);
        let printf = module.add_function("printf", printf_type, None);
        Codegen {
            context,
            module,
            builder,
            printf,
            functions: HashMap::new(),
            named: HashMap::new(),
            i64,
            f64,
            i1,
        }
    }

    fn to_i1(&self, val: &CgValue<'ctx>) -> IntValue<'ctx> {
        match val {
            CgValue::Int(v) => {
                if v.get_type() == self.i1 {
                    *v
                } else {
                    self.builder.build_int_compare(inkwell::IntPredicate::NE, *v, self.i64.const_int(0, false), "to_bool").unwrap()
                }
            }
            CgValue::Float(v) => self.builder.build_float_compare(inkwell::FloatPredicate::ONE, *v, self.f64.const_float(0.0), "to_bool").unwrap(),
            _ => panic!("cannot convert to bool"),
        }
    }

    fn string_global(&self, s: &str) -> PointerValue<'ctx> {
        let c = format!("{}\0", s);
        let arr = self.context.const_string(c.as_bytes(), false);
        let g = self.module.add_global(arr.get_type(), None, "strlit");
        g.set_initializer(&arr);
        g.as_pointer_value()
    }

    fn type_name(ty: &Type) -> String {
        match ty {
            Type::Int => "int".into(),
            Type::Float => "float".into(),
            Type::Bool => "bool".into(),
            Type::String => "string".into(),
            Type::Unit => "unit".into(),
            Type::Named(n) => n.clone(),
            Type::Fn(_, _) => "fn".into(),
            Type::Array(_) => "array".into(),
            Type::Inferred => "inferred".into(),
        }
    }

    fn gen_expr(&mut self, e: &Expr) -> CgValue<'ctx> {
        match e {
            Expr::Int(n) => CgValue::Int(self.i64.const_int(*n as u64, false)),
            Expr::Float(f) => CgValue::Float(self.f64.const_float(*f)),
            Expr::Bool(b) => {
                let bool_val = self.i1.const_int(*b as u64, false);
                CgValue::Int(self.builder.build_int_z_extend(bool_val, self.i64, "bool_ext").unwrap())
            }
            Expr::Str(s) => CgValue::Str(self.string_global(s)),
            Expr::Ident(name) => {
                if let Some((ptr, ty_name)) = self.named.get(name) {
                    match ty_name.as_str() {
                        "float" => CgValue::Float(self.builder.build_load(self.f64, *ptr, name).unwrap().into_float_value()),
                        "string" => CgValue::Str(*ptr),
                        "bool" => {
                            let bval = self.builder.build_load(self.i1, *ptr, name).unwrap().into_int_value();
                            CgValue::Int(self.builder.build_int_z_extend(bval, self.i64, "bool_ext").unwrap())
                        }
                        _ => {
                            let v = self.builder.build_load(self.i64, *ptr, name).unwrap();
                            CgValue::Int(v.into_int_value())
                        }
                    }
                } else {
                    panic!("undefined var: {}", name)
                }
            }
            Expr::Binary(l, op, r) => {
                let lv = self.gen_expr(l);
                let rv = self.gen_expr(r);
                match (&lv, &rv, op) {
                    (CgValue::Float(a), CgValue::Float(b), _) => {
                        let v = match op {
                            BinOp::Add => self.builder.build_float_add(*a, *b, "fadd").unwrap(),
                            BinOp::Sub => self.builder.build_float_sub(*a, *b, "fsub").unwrap(),
                            BinOp::Mul => self.builder.build_float_mul(*a, *b, "fmul").unwrap(),
                            BinOp::Div => self.builder.build_float_div(*a, *b, "fdiv").unwrap(),
                            BinOp::Mod => self.builder.build_float_rem(*a, *b, "frem").unwrap(),
                            _ => {
                                let cmp = match op {
                                    BinOp::Eq => self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, *a, *b, "feq"),
                                    BinOp::Ne => self.builder.build_float_compare(inkwell::FloatPredicate::ONE, *a, *b, "fne"),
                                    BinOp::Lt => self.builder.build_float_compare(inkwell::FloatPredicate::OLT, *a, *b, "flt"),
                                    BinOp::Le => self.builder.build_float_compare(inkwell::FloatPredicate::OLE, *a, *b, "fle"),
                                    BinOp::Gt => self.builder.build_float_compare(inkwell::FloatPredicate::OGT, *a, *b, "fgt"),
                                    BinOp::Ge => self.builder.build_float_compare(inkwell::FloatPredicate::OGE, *a, *b, "fge"),
                                    _ => unreachable!(),
                                };
                                return CgValue::Int(self.builder.build_int_z_extend(cmp.unwrap(), self.i64, "bool_ext").unwrap());
                            }
                        };
                        CgValue::Float(v)
                    }
                    _ => {
                        let lv = match lv { CgValue::Int(v) => v, _ => panic!("expected int operand") };
                        let rv = match rv { CgValue::Int(v) => v, _ => panic!("expected int operand") };
                        let v = match op {
                            BinOp::Add => self.builder.build_int_add(lv, rv, "add").unwrap(),
                            BinOp::Sub => self.builder.build_int_sub(lv, rv, "sub").unwrap(),
                            BinOp::Mul => self.builder.build_int_mul(lv, rv, "mul").unwrap(),
                            BinOp::Div => self.builder.build_int_signed_div(lv, rv, "div").unwrap(),
                            BinOp::Mod => self.builder.build_int_signed_rem(lv, rv, "rem").unwrap(),
                            BinOp::Eq => self.builder.build_int_compare(inkwell::IntPredicate::EQ, lv, rv, "eq").unwrap(),
                            BinOp::Ne => self.builder.build_int_compare(inkwell::IntPredicate::NE, lv, rv, "ne").unwrap(),
                            BinOp::Lt => self.builder.build_int_compare(inkwell::IntPredicate::SLT, lv, rv, "lt").unwrap(),
                            BinOp::Le => self.builder.build_int_compare(inkwell::IntPredicate::SLE, lv, rv, "le").unwrap(),
                            BinOp::Gt => self.builder.build_int_compare(inkwell::IntPredicate::SGT, lv, rv, "gt").unwrap(),
                            BinOp::Ge => self.builder.build_int_compare(inkwell::IntPredicate::SGE, lv, rv, "ge").unwrap(),
                        };
                        CgValue::Int(v)
                    }
                }
            }
            Expr::Call(callee, args) => {
                let name = match callee.as_ref() {
                    Expr::Ident(n) => n,
                    _ => panic!("cannot call non-function"),
                };
                if name == "print" {
                    let v = self.gen_expr(&args[0]);
                    self.gen_print(&v);
                    return CgValue::Int(self.i64.const_int(0, false));
                }
                let func = self.functions.get(name).copied()
                    .unwrap_or_else(|| panic!("undefined function: {}", name));
                let mut meta_args: Vec<BasicMetadataValueEnum> = Vec::new();
                for a in args {
                    match self.gen_expr(a) {
                        CgValue::Int(v) => meta_args.push(v.into()),
                        CgValue::Float(v) => meta_args.push(v.into()),
                        _ => panic!("only int/float args supported"),
                    }
                }
                let call = self.builder.build_call(func, &meta_args, name).unwrap();
                match call.try_as_basic_value() {
                    Either::Left(v) => {
                        if v.is_float_value() {
                            CgValue::Float(v.into_float_value())
                        } else {
                            CgValue::Int(v.into_int_value())
                        }
                    }
                    Either::Right(_) => CgValue::Int(self.i64.const_int(0, false)),
                }
            }
            Expr::MethodCall(_, method, _) => {
                panic!("LLVM backend: method calls not yet supported (method: {})", method);
            }
            Expr::FieldAccess(_, field) => {
                panic!("LLVM backend: field access not yet supported (field: {})", field);
            }
            Expr::StructLit { name, .. } => {
                panic!("LLVM backend: struct literals not yet supported ({})", name);
            }
            Expr::Match { .. } => {
                panic!("LLVM backend: match expressions not yet supported");
            }
            Expr::If { .. } => {
                panic!("LLVM backend: if expressions not yet supported (use if statement)");
            }
            Expr::Range(_, _) => {
                panic!("LLVM backend: range expressions not yet supported");
            }
            Expr::ArrayLit(_) => {
                panic!("LLVM backend: array literals not yet supported");
            }
            Expr::Index(_, _) => {
                panic!("LLVM backend: array indexing not yet supported");
            }
        }
    }

    fn gen_print(&self, val: &CgValue<'ctx>) {
        match val {
            CgValue::Int(v) => {
                let fmt = self.string_global("%ld\n");
                self.builder
                    .build_call(self.printf, &[fmt.into(), (*v).into()], "call_printf")
                    .unwrap();
            }
            CgValue::Float(v) => {
                let fmt = self.string_global("%f\n");
                self.builder
                    .build_call(self.printf, &[fmt.into(), (*v).into()], "call_printf")
                    .unwrap();
            }
            CgValue::Str(ptr) => {
                let fmt = self.string_global("%s\n");
                let str_val: BasicValueEnum = (*ptr).into();
                self.builder
                    .build_call(self.printf, &[fmt.into(), str_val.into()], "call_printf")
                    .unwrap();
            }
        }
    }

    fn gen_stmt(&mut self, s: &Stmt, fn_val: FunctionValue<'ctx>, terminated: &mut bool) {
        if *terminated { return; }
        match s {
            Stmt::Let { name, value, .. } => {
                let v = self.gen_expr(value);
                let (ptr, ty_name) = match &v {
                    CgValue::Str(p) => {
                        (p.clone(), "string".to_string())
                    }
                    CgValue::Float(_) => {
                        let p = self.builder.build_alloca(self.f64, name).unwrap();
                        self.builder.build_store(p, v.as_basic()).unwrap();
                        (p, "float".to_string())
                    }
                    CgValue::Int(_) => {
                        let p = self.builder.build_alloca(self.i64, name).unwrap();
                        self.builder.build_store(p, v.as_basic()).unwrap();
                        (p, "int".to_string())
                    }
                };
                self.named.insert(name.clone(), (ptr, ty_name));
            }
            Stmt::Assign { name, value } => {
                let v = self.gen_expr(value);
                if let Some((ptr, _)) = self.named.get(name) {
                    self.builder.build_store(*ptr, v.as_basic()).unwrap();
                } else {
                    panic!("undefined variable for assign: {}", name);
                }
            }
            Stmt::Expr(e) => {
                self.gen_expr(e);
            }
            Stmt::Return(e) => {
                match e {
                    Some(x) => {
                        let v = self.gen_expr(x);
                        match v {
                            CgValue::Int(v) => { self.builder.build_return(Some(&v)).unwrap(); }
                            CgValue::Float(v) => { self.builder.build_return(Some(&v)).unwrap(); }
                            _ => panic!("only int/float return supported"),
                        }
                    }
                    None => { self.builder.build_return(None).unwrap(); }
                }
                *terminated = true;
            }
            Stmt::If { cond, then_body, else_body } => {
                let cond_val = self.gen_expr(cond);
                let cond_bool = self.to_i1(&cond_val);

                let then_bb = self.context.append_basic_block(fn_val, "then");
                let merge_bb = self.context.append_basic_block(fn_val, "ifcont");

                if let Some(eb) = else_body {
                    let else_bb = self.context.append_basic_block(fn_val, "else");
                    self.builder.build_conditional_branch(cond_bool, then_bb, else_bb).unwrap();

                    self.builder.position_at_end(then_bb);
                    let mut then_terminated = false;
                    for s in then_body {
                        self.gen_stmt(s, fn_val, &mut then_terminated);
                    }
                    if !then_terminated {
                        self.builder.build_unconditional_branch(merge_bb).unwrap();
                    }

                    self.builder.position_at_end(else_bb);
                    let mut else_terminated = false;
                    for s in eb {
                        self.gen_stmt(s, fn_val, &mut else_terminated);
                    }
                    if !else_terminated {
                        self.builder.build_unconditional_branch(merge_bb).unwrap();
                    }
                } else {
                    self.builder.build_conditional_branch(cond_bool, then_bb, merge_bb).unwrap();
                    self.builder.position_at_end(then_bb);
                    let mut then_terminated = false;
                    for s in then_body {
                        self.gen_stmt(s, fn_val, &mut then_terminated);
                    }
                    if !then_terminated {
                        self.builder.build_unconditional_branch(merge_bb).unwrap();
                    }
                }

                self.builder.position_at_end(merge_bb);
            }
            Stmt::While { cond, body } => {
                let loop_bb = self.context.append_basic_block(fn_val, "while");
                let body_bb = self.context.append_basic_block(fn_val, "while_body");
                let after_bb = self.context.append_basic_block(fn_val, "while_end");

                self.builder.build_unconditional_branch(loop_bb).unwrap();
                self.builder.position_at_end(loop_bb);
                let cond_val = self.gen_expr(cond);
                let cond_bool = self.to_i1(&cond_val);
                self.builder.build_conditional_branch(cond_bool, body_bb, after_bb).unwrap();

                self.builder.position_at_end(body_bb);
                let mut body_terminated = false;
                for s in body {
                    self.gen_stmt(s, fn_val, &mut body_terminated);
                    if body_terminated { break; }
                }
                if !body_terminated {
                    self.builder.build_unconditional_branch(loop_bb).unwrap();
                }

                self.builder.position_at_end(after_bb);
            }
            Stmt::Break | Stmt::Continue => {
                // TODO: proper break/continue with labels
            }
            Stmt::For { var, iter, body } => {
                if let Expr::Range(start_expr, end_expr) = iter {
                    let start_val = self.gen_expr(start_expr);
                    let end_val = self.gen_expr(end_expr);
                    let end_int = match end_val { CgValue::Int(v) => v, _ => panic!("for range end must be int") };

                    let entry_bb = self.context.append_basic_block(fn_val, "for_entry");
                    let body_bb = self.context.append_basic_block(fn_val, "for_body");
                    let cont_bb = self.context.append_basic_block(fn_val, "for_cont");
                    let exit_bb = self.context.append_basic_block(fn_val, "for_exit");

                    let counter_ptr = self.builder.build_alloca(self.i64, var).unwrap();
                    self.builder.build_store(counter_ptr, start_val.as_basic()).unwrap();

                    self.builder.build_unconditional_branch(entry_bb).unwrap();
                    self.builder.position_at_end(entry_bb);
                    let cur = self.builder.build_load(self.i64, counter_ptr, &format!("{}_cur", var)).unwrap().into_int_value();
                    let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, cur, end_int, "for_cmp").unwrap();
                    self.builder.build_conditional_branch(cmp, body_bb, exit_bb).unwrap();

                    self.named.insert(var.clone(), (counter_ptr, "int".to_string()));
                    self.builder.position_at_end(body_bb);
                    let mut body_terminated = false;
                    for s in body {
                        self.gen_stmt(s, fn_val, &mut body_terminated);
                        if body_terminated { break; }
                    }
                    if !body_terminated {
                        self.builder.build_unconditional_branch(cont_bb).unwrap();
                    }

                    self.builder.position_at_end(cont_bb);
                    let cur2 = self.builder.build_load(self.i64, counter_ptr, &format!("{}_next", var)).unwrap().into_int_value();
                    let one = self.i64.const_int(1, false);
                    let next = self.builder.build_int_add(cur2, one, "for_next").unwrap();
                    self.builder.build_store(counter_ptr, next).unwrap();
                    self.builder.build_unconditional_branch(entry_bb).unwrap();

                    self.builder.position_at_end(exit_bb);
                } else {
                    panic!("LLVM backend: for loop requires range expression");
                }
            }
            Stmt::Fn { .. } | Stmt::Struct { .. } | Stmt::Enum { .. } | Stmt::Trait { .. }
            | Stmt::Macro { .. } | Stmt::ExternFn { .. } | Stmt::Impl { .. } => {}
        }
    }
}

/// Compile a program to a native executable at `out_path` using LLVM + system
/// toolchain (llc / clang).
pub fn compile_to_executable(program: &Program, out_path: &str) {
    let context = Context::create();
    let mut cg = Codegen::new(&context);

    // Pre-declare all functions
    for s in &program.stmts {
        if let Stmt::Fn { name, params, ret, .. } = s {
            let param_types: Vec<BasicMetadataTypeEnum> = params.iter().map(|_| cg.i64.into()).collect();
            let fn_type = if matches!(ret, Type::Unit) {
                context.void_type().fn_type(&param_types, false)
            } else {
                cg.i64.fn_type(&param_types, false)
            };
            let func = cg.module.add_function(name, fn_type, None);
            cg.functions.insert(name.clone(), func);
        }
    }

    // Generate function bodies
    for s in &program.stmts {
        if let Stmt::Fn { params, body, ret, .. } = s {
            let name = if let Stmt::Fn { name, .. } = s { name } else { unreachable!() };
            let func = cg.functions[name];
            let entry = context.append_basic_block(func, "entry");
            cg.builder.position_at_end(entry);
            cg.named.clear();

            let mut terminated = false;
            for (i, (pname, _)) in params.iter().enumerate() {
                let ptr = cg.builder.build_alloca(cg.i64, pname).unwrap();
                let arg = func.get_nth_param(i as u32).unwrap().into_int_value();
                cg.builder.build_store(ptr, arg).unwrap();
                cg.named.insert(pname.clone(), (ptr, "int".to_string()));
            }
            for s in body {
                cg.gen_stmt(s, func, &mut terminated);
            }
            if !terminated {
                if matches!(ret, Type::Unit) {
                    cg.builder.build_return(None).unwrap();
                } else {
                    cg.builder.build_return(Some(&cg.i64.const_int(0, false))).unwrap();
                }
            }
        }
    }

    // Generate main
    let main_type = context.i64_type().fn_type(&[], false);
    let main_fn = cg.module.add_function("main", main_type, None);
    let entry = context.append_basic_block(main_fn, "entry");
    cg.builder.position_at_end(entry);
    cg.named.clear();

    let mut terminated = false;
    for s in &program.stmts {
        match s {
            Stmt::Fn { .. } | Stmt::Struct { .. } | Stmt::Enum { .. } | Stmt::Trait { .. }
            | Stmt::Macro { .. } | Stmt::ExternFn { .. } | Stmt::Impl { .. } => {}
            _ => cg.gen_stmt(s, main_fn, &mut terminated),
        }
    }

    if !terminated {
        cg.builder.build_return(Some(&cg.i64.const_int(0, false))).unwrap();
    }

    let ir = cg.module.print_to_string();
    let ll_path = format!("{}.ll", out_path);
    std::fs::write(&ll_path, ir.to_string()).unwrap();

    let llvm_prefix = std::env::var("LLVM_SYS_180_PREFIX")
        .or_else(|_| std::env::var("LLVM_SYS_181_PREFIX"))
        .unwrap_or_else(|_| {
            panic!("set LLVM_SYS_180_PREFIX to your LLVM 18 install prefix")
        });
    let llc = format!("{}/bin/llc", llvm_prefix);
    let obj_path = format!("{}.o", out_path);

    let status = Command::new(&llc)
        .args([&ll_path, "-filetype=obj", "-o", &obj_path])
        .status()
        .expect("failed to run llc");
    if !status.success() {
        panic!("llc failed");
    }

    let status = Command::new("cc")
        .args([&obj_path, "-o", out_path])
        .status()
        .expect("failed to run cc");
    if !status.success() {
        panic!("cc link failed");
    }
    println!("compiled -> {}", out_path);
}
