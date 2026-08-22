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
use inkwell::types::{BasicMetadataTypeEnum, IntType};
use either::Either;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;
use std::collections::HashMap;
use std::process::Command;

enum CgValue<'ctx> {
    Int(IntValue<'ctx>),
    Str(PointerValue<'ctx>),
}

pub struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    printf: FunctionValue<'ctx>,
    functions: HashMap<String, FunctionValue<'ctx>>,
    named: HashMap<String, PointerValue<'ctx>>,
    i64: IntType<'ctx>,
}

impl<'ctx> Codegen<'ctx> {
    fn new(context: &'ctx Context) -> Self {
        let module = context.create_module("zarrin");
        let builder = context.create_builder();
        let i64 = context.i64_type();
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
        }
    }

    fn string_global(&self, s: &str) -> PointerValue<'ctx> {
        let c = format!("{}\0", s);
        let arr = self.context.const_string(c.as_bytes(), false);
        let g = self.module.add_global(arr.get_type(), None, "strlit");
        g.set_initializer(&arr);
        g.as_pointer_value()
    }

    fn gen_expr(&mut self, e: &Expr) -> CgValue<'ctx> {
        match e {
            Expr::Int(n) => CgValue::Int(self.i64.const_int(*n as u64, false)),
            Expr::Float(_) => panic!("float not supported by LLVM backend yet"),
            Expr::Bool(_) => panic!("bool not supported by LLVM backend yet"),
            Expr::Str(s) => CgValue::Str(self.string_global(s)),
            Expr::Ident(name) => {
                let ptr = self
                    .named
                    .get(name)
                    .unwrap_or_else(|| panic!("undefined var: {}", name));
                let v = self.builder.build_load(self.i64, *ptr, name).unwrap();
                CgValue::Int(v.into_int_value())
            }
            Expr::Binary(l, op, r) => {
                let lv = match self.gen_expr(l) {
                    CgValue::Int(v) => v,
                    _ => panic!("expected int operand"),
                };
                let rv = match self.gen_expr(r) {
                    CgValue::Int(v) => v,
                    _ => panic!("expected int operand"),
                };
                let v = match op {
                    BinOp::Add => self.builder.build_int_add(lv, rv, "add").unwrap(),
                    BinOp::Sub => self.builder.build_int_sub(lv, rv, "sub").unwrap(),
                    BinOp::Mul => self.builder.build_int_mul(lv, rv, "mul").unwrap(),
                    BinOp::Div => self
                        .builder
                        .build_int_signed_div(lv, rv, "div")
                        .unwrap(),
                    BinOp::Eq => self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::EQ, lv, rv, "eq")
                        .unwrap(),
                    BinOp::Ne => self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::NE, lv, rv, "ne")
                        .unwrap(),
                    BinOp::Lt => self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::SLT, lv, rv, "lt")
                        .unwrap(),
                    BinOp::Gt => self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::SGT, lv, rv, "gt")
                        .unwrap(),
                };
                CgValue::Int(v)
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
                let func = self
                    .functions
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| panic!("undefined function: {}", name));
                let mut meta_args: Vec<BasicMetadataValueEnum> = Vec::new();
                for a in args {
                    match self.gen_expr(a) {
                        CgValue::Int(v) => meta_args.push(v.into()),
                        _ => panic!("only int args supported"),
                    }
                }
                let call = self
                    .builder
                    .build_call(func, &meta_args, name)
                    .unwrap();
                match call.try_as_basic_value() {
                    Either::Left(v) => CgValue::Int(v.into_int_value()),
                    Either::Right(_) => CgValue::Int(self.i64.const_int(0, false)),
                }
            }
        }
    }

    fn gen_print(&self, val: &CgValue<'ctx>) {
        match val {
            CgValue::Int(v) => {
                let fmt = self.string_global("%ld\n");
                self.builder
                    .build_call(
                        self.printf,
                        &[fmt.into(), (*v).into()],
                        "call_printf",
                    )
                    .unwrap();
            }
            CgValue::Str(ptr) => {
                let fmt = self.string_global("%s\n");
                let str_val: BasicValueEnum = (*ptr).into();
                self.builder
                    .build_call(
                        self.printf,
                        &[fmt.into(), str_val.into()],
                        "call_printf",
                    )
                    .unwrap();
            }
        }
    }

    fn gen_block(
        &mut self,
        fn_val: FunctionValue<'ctx>,
        stmts: &[Stmt],
        is_main: bool,
    ) {
        let entry = self.context.append_basic_block(fn_val, "entry");
        self.builder.position_at_end(entry);
        let mut terminated = false;
        for s in stmts {
            if terminated {
                break;
            }
            match s {
                Stmt::Let { name, value, .. } => {
                    let v = match self.gen_expr(value) {
                        CgValue::Int(v) => v,
                        _ => panic!("only int let supported"),
                    };
                    let ptr = self.builder.build_alloca(self.i64, name).unwrap();
                    self.builder.build_store(ptr, v).unwrap();
                    self.named.insert(name.clone(), ptr);
                }
                Stmt::Expr(e) => {
                    self.gen_expr(e);
                }
                Stmt::Return(e) => {
                    match e {
                        Some(x) => match self.gen_expr(x) {
                            CgValue::Int(v) => {
                                self.builder.build_return(Some(&v)).unwrap();
                            }
                            _ => panic!("only int return supported"),
                        },
                        None => {
                            self.builder.build_return(None).unwrap();
                        }
                    }
                    terminated = true;
                }
                Stmt::Fn { .. } => {}
            }
        }
        if !terminated {
            if is_main {
                self.builder
                    .build_return(Some(&self.i64.const_int(0, false)))
                    .unwrap();
            } else {
                self.builder.build_return(None).unwrap();
            }
        }
    }

    fn gen_function(&mut self, stmt: &Stmt) {
        if let Stmt::Fn {
            name,
            params,
            ret,
            body,
        } = stmt
        {
            let param_types: Vec<BasicMetadataTypeEnum> =
                params.iter().map(|_| self.i64.into()).collect();
            let fn_type = if matches!(ret, Type::Unit) {
                self.context.void_type().fn_type(&param_types, false)
            } else {
                self.i64.fn_type(&param_types, false)
            };
            let func = self.module.add_function(name, fn_type, None);
            self.functions.insert(name.clone(), func);

            let entry = self.context.append_basic_block(func, "entry");
            self.builder.position_at_end(entry);
            self.named.clear();

            let mut terminated = false;
            for (i, (pname, _)) in params.iter().enumerate() {
                let ptr = self.builder.build_alloca(self.i64, pname).unwrap();
                let arg = func.get_nth_param(i as u32).unwrap().into_int_value();
                self.builder.build_store(ptr, arg).unwrap();
                self.named.insert(pname.clone(), ptr);
            }
            for s in body {
                if terminated {
                    break;
                }
                match s {
                    Stmt::Let { name, value, .. } => {
                        let v = match self.gen_expr(value) {
                            CgValue::Int(v) => v,
                            _ => panic!("only int let supported"),
                        };
                        let ptr = self.builder.build_alloca(self.i64, name).unwrap();
                        self.builder.build_store(ptr, v).unwrap();
                        self.named.insert(name.clone(), ptr);
                    }
                    Stmt::Expr(e) => {
                        self.gen_expr(e);
                    }
                    Stmt::Return(e) => {
                        match e {
                            Some(x) => match self.gen_expr(x) {
                                CgValue::Int(v) => {
                                    self.builder.build_return(Some(&v)).unwrap();
                                }
                                _ => panic!("only int return"),
                            },
                            None => {
                                self.builder.build_return(None).unwrap();
                            }
                        }
                        terminated = true;
                    }
                    Stmt::Fn { .. } => {}
                }
            }
            if !terminated {
                if matches!(ret, Type::Unit) {
                    self.builder.build_return(None).unwrap();
                } else {
                    self.builder
                        .build_return(Some(&self.i64.const_int(0, false)))
                        .unwrap();
                }
            }
        }
    }
}

/// Compile a program to a native executable at `out_path` using LLVM + system
/// toolchain (llc / clang).
pub fn compile_to_executable(program: &Program, out_path: &str) {
    let context = Context::create();
    let mut cg = Codegen::new(&context);

    let mut toplevel = Vec::new();
    for s in &program.stmts {
        match s {
            Stmt::Fn { .. } => cg.gen_function(s),
            _ => toplevel.push(s.clone()),
        }
    }

    let main_type = context.i64_type().fn_type(&[], false);
    let main_fn = cg.module.add_function("main", main_type, None);
    cg.gen_block(main_fn, &toplevel, true);

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
