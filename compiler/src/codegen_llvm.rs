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
use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicMetadataValueEnum, BasicValue, BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};
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
    fn_ret_types: HashMap<String, Type>,
    named: HashMap<String, (PointerValue<'ctx>, String)>,
    struct_fields: HashMap<String, Vec<String>>,
    var_struct_type: HashMap<String, String>,
    enum_variants: HashMap<String, Vec<(String, Vec<Type>)>>,
    loop_exit: Vec<BasicBlock<'ctx>>,
    loop_continue: Vec<BasicBlock<'ctx>>,
    i64: IntType<'ctx>,
    i8: IntType<'ctx>,
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
            fn_ret_types: HashMap::new(),
            named: HashMap::new(),
            struct_fields: HashMap::new(),
            var_struct_type: HashMap::new(),
            enum_variants: HashMap::new(),
            loop_exit: Vec::new(),
            loop_continue: Vec::new(),
            i64,
            i8: context.i8_type(),
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
                } else if let Some((_, variants)) = self.enum_variants.iter().find(|(_, v)| v.iter().any(|(vn, _)| vn == name)) {
                    let tag = variants.iter().position(|(vn, _)| vn == name).unwrap();
                    let has_data = variants.iter().any(|(_, pt)| !pt.is_empty());
                    if has_data {
                        let array_ty = self.i64.array_type(1);
                        let alloca = self.builder.build_alloca(array_ty, &format!("enum_{}", name)).unwrap();
                        let base_ptr = self.builder.build_bit_cast(alloca, self.context.ptr_type(AddressSpace::default()), "enum_base").unwrap().into_pointer_value();
                        let tag_ptr = unsafe {
                            self.builder.build_gep(array_ty, alloca, &[self.i64.const_int(0, false), self.i64.const_int(0, false)], "tag_ptr").unwrap()
                        };
                        self.builder.build_store(tag_ptr, self.i64.const_int(tag as u64, false)).unwrap();
                        CgValue::Int(self.builder.build_ptr_to_int(base_ptr, self.i64, "enum_ptr").unwrap())
                    } else {
                        CgValue::Int(self.i64.const_int(tag as u64, false))
                    }
                } else {
                    panic!("undefined var: {}", name)
                }
            }
            Expr::Binary(l, op, r) => {
                let lv = self.gen_expr(l);
                let rv = self.gen_expr(r);
                if matches!(op, BinOp::Add) {
                    match (&lv, &rv) {
                        (CgValue::Str(a), CgValue::Str(b)) => return self.gen_string_concat(*a, *b),
                        (CgValue::Str(a), CgValue::Int(b)) => {
                            let b_str = self.gen_int_to_str(*b);
                            return self.gen_string_concat(*a, b_str);
                        }
                        (CgValue::Int(a), CgValue::Str(b)) => {
                            let a_str = self.gen_int_to_str(*a);
                            return self.gen_string_concat(a_str, *b);
                        }
                        _ => {}
                    }
                }
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
                        match op {
                            BinOp::Add => CgValue::Int(self.builder.build_int_add(lv, rv, "add").unwrap()),
                            BinOp::Sub => CgValue::Int(self.builder.build_int_sub(lv, rv, "sub").unwrap()),
                            BinOp::Mul => CgValue::Int(self.builder.build_int_mul(lv, rv, "mul").unwrap()),
                            BinOp::Div => CgValue::Int(self.builder.build_int_signed_div(lv, rv, "div").unwrap()),
                            BinOp::Mod => CgValue::Int(self.builder.build_int_signed_rem(lv, rv, "rem").unwrap()),
                            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                                let pred = match op {
                                    BinOp::Eq => inkwell::IntPredicate::EQ,
                                    BinOp::Ne => inkwell::IntPredicate::NE,
                                    BinOp::Lt => inkwell::IntPredicate::SLT,
                                    BinOp::Le => inkwell::IntPredicate::SLE,
                                    BinOp::Gt => inkwell::IntPredicate::SGT,
                                    BinOp::Ge => inkwell::IntPredicate::SGE,
                                    _ => unreachable!(),
                                };
                                let cmp = self.builder.build_int_compare(pred, lv, rv, "cmp").unwrap();
                                CgValue::Int(self.builder.build_int_z_extend(cmp, self.i64, "cmp_ext").unwrap())
                            }
                            BinOp::And => CgValue::Int(self.builder.build_and(lv, rv, "and").unwrap()),
                            BinOp::Or => CgValue::Int(self.builder.build_or(lv, rv, "or").unwrap()),
                        }
                    }
                }
            }
            Expr::Unary(op, e) => {
                let v = self.gen_expr(e);
                match op {
                    UnaryOp::Neg => match v {
                        CgValue::Int(i) => CgValue::Int(self.builder.build_int_neg(i, "neg").unwrap()),
                        CgValue::Float(f) => CgValue::Float(self.builder.build_float_neg(f, "fneg").unwrap()),
                        _ => panic!("cannot negate non-numeric value"),
                    },
                    UnaryOp::Not => match v {
                        CgValue::Int(i) => {
                            let zero = self.i64.const_int(0, false);
                            let cmp = self.builder.build_int_compare(inkwell::IntPredicate::EQ, i, zero, "not_cmp").unwrap();
                            CgValue::Int(self.builder.build_int_z_extend(cmp, self.i64, "not").unwrap())
                        }
                        _ => panic!("cannot negate non-boolean value"),
                    },
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
                if name == "len" {
                    let v = self.gen_expr(&args[0]);
                    return match v {
                        CgValue::Str(ptr) => {
                            let strlen_type = self.i64.fn_type(&[self.context.ptr_type(AddressSpace::default()).into()], false);
                            let strlen_fn = self.module.get_function("strlen").unwrap_or_else(|| self.module.add_function("strlen", strlen_type, None));
                            let len_val = self.builder.build_call(strlen_fn, &[ptr.into()], "len_call").unwrap().try_as_basic_value().left().unwrap().into_int_value();
                            CgValue::Int(len_val)
                        }
                        CgValue::Int(ptr_val) => {
                            let arr_ptr = self.builder.build_int_to_ptr(ptr_val, self.i64.ptr_type(AddressSpace::default()), "arr_ptr").unwrap();
                            let len = self.builder.build_load(self.i64, arr_ptr, "arr_len").unwrap().into_int_value();
                            CgValue::Int(len)
                        }
                        _ => panic!("len supports strings and arrays"),
                    };
                }
                if name == "int_to_str" {
                    let val = self.gen_expr(&args[0]);
                    let int_val = self.value_to_int(&val);
                    let str_ptr = self.gen_int_to_str(int_val);
                    return CgValue::Str(str_ptr);
                }
                if name == "to_string" {
                    let val = self.gen_expr(&args[0]);
                    match &val {
                        CgValue::Int(i) => {
                            let str_ptr = self.gen_int_to_str(*i);
                            return CgValue::Str(str_ptr);
                        }
                        CgValue::Str(s) => return val,
                        CgValue::Float(f) => {
                            let str_ptr = self.gen_float_to_str(*f);
                            return CgValue::Str(str_ptr);
                        }
                    }
                }
                if name == "panic" {
                    let val = self.gen_expr(&args[0]);
                    let str_ptr = match &val {
                        CgValue::Str(s) => *s,
                        _ => {
                            let int_val = self.value_to_int(&val);
                            self.gen_int_to_str(int_val)
                        }
                    };
                    let fmt = self.builder.build_global_string_ptr("%s\n", "panic_fmt").unwrap().as_pointer_value();
                    self.builder.build_call(self.printf, &[fmt.into(), str_ptr.into()], "panic_str").unwrap();
                    let abort_type = self.context.void_type().fn_type(&[], false);
                    let abort_fn = self.module.get_function("exit").unwrap_or_else(|| self.module.add_function("exit", abort_type, None));
                    self.builder.build_call(abort_fn, &[self.i64.const_int(1, false).into()], "panic_exit").unwrap();
                    return CgValue::Int(self.i64.const_int(0, false));
                }
                if name == "array_len" {
                    let arr_val = self.gen_expr(&args[0]);
                    let arr_ptr_val = self.value_to_int(&arr_val);
                    let arr_ptr = self.builder.build_int_to_ptr(arr_ptr_val, self.i64.ptr_type(AddressSpace::default()), "arr_ptr").unwrap();
                    let len = self.builder.build_load(self.i64, arr_ptr, "arr_len").unwrap().into_int_value();
                    return CgValue::Int(len);
                }
                if name == "array_get" {
                    let arr_val = self.gen_expr(&args[0]);
                    let arr_ptr_val = self.value_to_int(&arr_val);
                    let arr_ptr = self.builder.build_int_to_ptr(arr_ptr_val, self.i64.ptr_type(AddressSpace::default()), "arr_ptr").unwrap();
                    let idx_val = self.gen_expr(&args[1]);
                    let idx_int = self.value_to_int(&idx_val);
                    let elem_off = self.builder.build_int_add(idx_int, self.i64.const_int(1, false), "elem_off").unwrap();
                    let elem_ptr = unsafe {
                        self.builder.build_gep(self.i64, arr_ptr, &[elem_off], "elem_ptr").unwrap()
                    };
                    return CgValue::Int(self.builder.build_load(self.i64, elem_ptr, "arr_elem").unwrap().into_int_value());
                }
                if name == "array_set" {
                    let arr_val = self.gen_expr(&args[0]);
                    let arr_ptr_val = self.value_to_int(&arr_val);
                    let arr_ptr = self.builder.build_int_to_ptr(arr_ptr_val, self.i64.ptr_type(AddressSpace::default()), "arr_ptr").unwrap();
                    let idx_val = self.gen_expr(&args[1]);
                    let idx_int = self.value_to_int(&idx_val);
                    let new_val = self.gen_expr(&args[2]);
                    let new_int = self.value_to_int(&new_val);
                    let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
                    let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
                    let memcpy_type = self.context.void_type().fn_type(&[
                        self.context.ptr_type(AddressSpace::default()).into(),
                        self.context.ptr_type(AddressSpace::default()).into(),
                        self.i64.into(),
                    ], false);
                    let memcpy_fn = self.module.get_function("memcpy").unwrap_or_else(|| self.module.add_function("memcpy", memcpy_type, None));
                    let old_len = self.builder.build_load(self.i64, arr_ptr, "old_len").unwrap().into_int_value();
                    let new_len = self.builder.build_int_add(old_len, self.i64.const_int(1, false), "new_len").unwrap();
                    let buf = self.builder.build_call(malloc_fn, &[new_len.into()], "new_arr").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();
                    let byte_count = self.builder.build_int_mul(new_len, self.i64.const_int(8, false), "bytes").unwrap();
                    self.builder.build_call(memcpy_fn, &[buf.into(), arr_ptr_val.into(), byte_count.into()], "cp").unwrap();
                    let elem_off = self.builder.build_int_add(idx_int, self.i64.const_int(1, false), "elem_off").unwrap();
                    let elem_ptr = unsafe {
                        self.builder.build_gep(self.i64, buf, &[elem_off], "elem_ptr").unwrap()
                    };
                    self.builder.build_store(elem_ptr, new_int).unwrap();
                    return CgValue::Int(self.builder.build_ptr_to_int(buf, self.i64, "new_arr_ptr").unwrap());
                }
                if name == "substring" {
                    let s_val = self.gen_expr(&args[0]);
                    let start_val = self.gen_expr(&args[1]);
                    let end_val = self.gen_expr(&args[2]);
                    let s_ptr = match s_val { CgValue::Str(p) => p, _ => panic!("substring expects string") };
                    let start = self.value_to_int(&start_val);
                    let end = self.value_to_int(&end_val);
                    let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
                    let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
                    let memcpy_type = self.context.void_type().fn_type(&[
                        self.context.ptr_type(AddressSpace::default()).into(),
                        self.context.ptr_type(AddressSpace::default()).into(),
                        self.i64.into(),
                    ], false);
                    let memcpy_fn = self.module.get_function("memcpy").unwrap_or_else(|| self.module.add_function("memcpy", memcpy_type, None));
                    let sub_len = self.builder.build_int_sub(end, start, "sub_len").unwrap();
                    let buf = self.builder.build_call(malloc_fn, &[self.builder.build_int_add(sub_len, self.i64.const_int(1, false), "sub_alloc").unwrap().into()], "sub_buf").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();
                    let src_ptr = unsafe {
                        self.builder.build_gep(self.i8, s_ptr, &[start], "src_ptr").unwrap()
                    };
                    self.builder.build_call(memcpy_fn, &[buf.into(), src_ptr.into(), sub_len.into()], "sub_cp").unwrap();
                    let null_ptr = unsafe {
                        self.builder.build_gep(self.i8, buf, &[sub_len], "null_ptr").unwrap()
                    };
                    self.builder.build_store(null_ptr, self.i8.const_int(0, false)).unwrap();
                    return CgValue::Str(buf);
                }
                if name == "contains" {
                    let s_val = self.gen_expr(&args[0]);
                    let needle_val = self.gen_expr(&args[1]);
                    let s_ptr = match s_val { CgValue::Str(p) => p, _ => panic!("contains expects string") };
                    let needle_ptr = match needle_val { CgValue::Str(p) => p, _ => panic!("contains expects string") };
                    let strstr_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[
                        self.context.ptr_type(AddressSpace::default()).into(),
                        self.context.ptr_type(AddressSpace::default()).into(),
                    ], false);
                    let strstr_fn = self.module.get_function("strstr").unwrap_or_else(|| self.module.add_function("strstr", strstr_type, None));
                    let result = self.builder.build_call(strstr_fn, &[s_ptr.into(), needle_ptr.into()], "strstr_call").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();
                    let is_null = self.builder.build_is_null(result, "is_null").unwrap();
                    let not_found = self.builder.build_int_z_extend(is_null, self.i64, "not_found").unwrap();
                    let found = self.builder.build_int_sub(self.i64.const_int(1, false), not_found, "found").unwrap();
                    return CgValue::Int(found);
                }
                if name == "trim" {
                    let s_val = self.gen_expr(&args[0]);
                    let s_ptr = match s_val { CgValue::Str(p) => p, _ => panic!("trim expects string") };
                    let byte_ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let i64_type = self.i64.fn_type(&[byte_ptr_ty.into(), byte_ptr_ty.into()], false);
                    let strspn_fn = self.module.get_function("strspn").unwrap_or_else(|| self.module.add_function("strspn", i64_type, None));
                    let strcspn_fn = self.module.get_function("strcspn").unwrap_or_else(|| self.module.add_function("strcspn", i64_type, None));
                    let ws = self.builder.build_global_string_ptr(" \t\n\r", "ws").unwrap().as_pointer_value();
                    let start_skip = self.builder.build_call(strspn_fn, &[s_ptr.into(), ws.into()], "start_skip").unwrap().try_as_basic_value().left().unwrap().into_int_value();
                    let trimmed_start = unsafe {
                        self.builder.build_gep(self.i8, s_ptr, &[start_skip], "trimmed_start").unwrap()
                    };
                    let len_after = self.builder.build_call(strcspn_fn, &[trimmed_start.into(), ws.into()], "len_after").unwrap().try_as_basic_value().left().unwrap().into_int_value();
                    let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
                    let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
                    let memcpy_type = self.context.void_type().fn_type(&[
                        byte_ptr_ty.into(),
                        byte_ptr_ty.into(),
                        self.i64.into(),
                    ], false);
                    let memcpy_fn = self.module.get_function("memcpy").unwrap_or_else(|| self.module.add_function("memcpy", memcpy_type, None));
                    let buf = self.builder.build_call(malloc_fn, &[self.builder.build_int_add(len_after, self.i64.const_int(1, false), "trim_alloc").unwrap().into()], "trim_buf").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();
                    self.builder.build_call(memcpy_fn, &[buf.into(), trimmed_start.into(), len_after.into()], "trim_cp").unwrap();
                    let null_ptr = unsafe {
                        self.builder.build_gep(self.i8, buf, &[len_after], "null_ptr").unwrap()
                    };
                    self.builder.build_store(null_ptr, self.i8.const_int(0, false)).unwrap();
                    return CgValue::Str(buf);
                }
                if name == "char_at" {
                    let s_val = self.gen_expr(&args[0]);
                    let idx_val = self.gen_expr(&args[1]);
                    let s_ptr = match s_val { CgValue::Str(p) => p, _ => panic!("char_at expects string") };
                    let idx_int = self.value_to_int(&idx_val);
                    let byte_ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let char_ptr = unsafe {
                        self.builder.build_gep(self.i8, s_ptr, &[idx_int], "char_ptr").unwrap()
                    };
                    let buf = {
                        let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
                        let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
                        self.builder.build_call(malloc_fn, &[self.i64.const_int(2, false).into()], "char_buf").unwrap().try_as_basic_value().left().unwrap().into_pointer_value()
                    };
                    let ch = self.builder.build_load(self.i8, char_ptr, "ch").unwrap().into_int_value();
                    let buf_byte = self.builder.build_bit_cast(buf, self.i8.ptr_type(AddressSpace::default()), "buf_byte").unwrap().into_pointer_value();
                    self.builder.build_store(buf_byte, ch).unwrap();
                    let null_ptr = unsafe {
                        self.builder.build_gep(self.i8, buf_byte, &[self.i64.const_int(1, false)], "null_ptr").unwrap()
                    };
                    self.builder.build_store(null_ptr, self.i8.const_int(0, false)).unwrap();
                    return CgValue::Str(buf);
                }
                if let Some((_, variants)) = self.enum_variants.iter().find(|(_, v)| v.iter().any(|(vn, _)| vn == name)) {
                    let tag = variants.iter().position(|(vn, _)| vn == name).unwrap();
                    let (_, payload_types) = variants.iter().find(|(vn, _)| vn == name).unwrap();
                    let has_data = variants.iter().any(|(_, pt)| !pt.is_empty());
                    if !has_data {
                        return CgValue::Int(self.i64.const_int(tag as u64, false));
                    }
                    let num_fields = (payload_types.len() + 1) as u32;
                    let array_ty = self.i64.array_type(num_fields);
                    let alloca = self.builder.build_alloca(array_ty, &format!("enum_{}", name)).unwrap();
                    let base_ptr = self.builder.build_bit_cast(alloca, self.context.ptr_type(AddressSpace::default()), "enum_base").unwrap().into_pointer_value();
                    let tag_ptr = unsafe {
                        self.builder.build_gep(array_ty, alloca, &[self.i64.const_int(0, false), self.i64.const_int(0, false)], "tag_ptr").unwrap()
                    };
                    self.builder.build_store(tag_ptr, self.i64.const_int(tag as u64, false)).unwrap();
                    for (j, arg) in args.iter().enumerate() {
                        let av = self.gen_expr(arg);
                        let av_int = self.value_to_int(&av);
                        let field_ptr = unsafe {
                            self.builder.build_gep(array_ty, alloca, &[self.i64.const_int(0, false), self.i64.const_int((j + 1) as u64, false)], &format!("{}_{}", name, j)).unwrap()
                        };
                        self.builder.build_store(field_ptr, av_int).unwrap();
                    }
                    return CgValue::Int(self.builder.build_ptr_to_int(base_ptr, self.i64, "enum_ptr").unwrap());
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
                        let ret_type = self.fn_ret_types.get(name);
                        if matches!(ret_type, Some(Type::String)) && v.is_int_value() {
                            let int_val = v.into_int_value();
                            let ptr = self.builder.build_int_to_ptr(
                                int_val,
                                self.context.ptr_type(AddressSpace::default()),
                                "str_ptr",
                            ).unwrap();
                            CgValue::Str(ptr)
                        } else if matches!(ret_type, Some(Type::Float)) && v.is_int_value() {
                            let as_float = self.builder.build_bit_cast(v.into_int_value(), self.f64, "i2f").unwrap().into_float_value();
                            CgValue::Float(as_float)
                        } else if v.is_float_value() {
                            CgValue::Float(v.into_float_value())
                        } else {
                            CgValue::Int(v.into_int_value())
                        }
                    }
                    Either::Right(_) => CgValue::Int(self.i64.const_int(0, false)),
                }
            }
            Expr::StructLit { name, fields } => {
                let field_defs = self.struct_fields.get(name).cloned()
                    .unwrap_or_else(|| panic!("unknown struct: {}", name));
                let num_fields = field_defs.len() as u64;
                let array_ty = self.i64.array_type(num_fields as u32);
                let alloca = self.builder.build_alloca(array_ty, &format!("{}_struct", name)).unwrap();
                let base_ptr = self.builder.build_bit_cast(alloca, self.context.ptr_type(AddressSpace::default()), "base_ptr").unwrap().into_pointer_value();
                for (i, (fname, fexpr)) in fields.iter().enumerate() {
                    let fv = self.gen_expr(fexpr);
                    let fv_int = match fv {
                        CgValue::Int(v) => v,
                        CgValue::Float(v) => {
                            let bits = self.builder.build_bit_cast(v, self.i64, "f2i").unwrap();
                            bits.into_int_value()
                        }
                        CgValue::Str(p) => {
                            self.builder.build_ptr_to_int(p, self.i64, "p2i").unwrap()
                        }
                    };
                    let field_ptr = unsafe {
                        self.builder.build_gep(array_ty, alloca, &[self.i64.const_int(0, false), self.i64.const_int(i as u64, false)], &format!("{}_{}", name, fname)).unwrap()
                    };
                    self.builder.build_store(field_ptr, fv_int).unwrap();
                }
                CgValue::Int(self.builder.build_ptr_to_int(base_ptr, self.i64, "struct_ptr").unwrap())
            }
            Expr::FieldAccess(obj, field) => {
                let obj_val = self.gen_expr(obj);
                let obj_ptr_val = match obj_val {
                    CgValue::Int(v) => v,
                    _ => panic!("field access on non-struct"),
                };
                let obj_ptr = self.builder.build_int_to_ptr(obj_ptr_val, self.context.ptr_type(AddressSpace::default()), "obj_ptr").unwrap();
                let struct_name = match obj.as_ref() {
                    Expr::Ident(n) => self.var_struct_type.get(n).cloned()
                        .unwrap_or_else(|| panic!("variable '{}' is not a struct", n)),
                    Expr::StructLit { name, .. } => name.clone(),
                    _ => panic!("cannot determine struct type"),
                };
                let field_defs = self.struct_fields.get(&struct_name)
                    .unwrap_or_else(|| panic!("unknown struct type for field access: {}", struct_name));
                let field_idx = field_defs.iter().position(|f| f == field)
                    .unwrap_or_else(|| panic!("field `{}` not found in struct `{}`", field, struct_name));
                let num_fields = field_defs.len() as u64;
                let array_ty = self.i64.array_type(num_fields as u32);
                let field_ptr = unsafe {
                    self.builder.build_gep(array_ty, obj_ptr, &[self.i64.const_int(0, false), self.i64.const_int(field_idx as u64, false)], &format!("{}_{}", struct_name, field)).unwrap()
                };
                CgValue::Int(self.builder.build_load(self.i64, field_ptr, field).unwrap().into_int_value())
            }
            Expr::MethodCall(obj, method, args) => {
                let obj_val = self.gen_expr(obj);
                let mut meta_args: Vec<BasicMetadataValueEnum> = Vec::new();
                match &obj_val {
                    CgValue::Int(v) => meta_args.push((*v).into()),
                    _ => panic!("method call on non-struct"),
                }
                for a in args {
                    match self.gen_expr(a) {
                        CgValue::Int(v) => meta_args.push(v.into()),
                        CgValue::Float(v) => meta_args.push(v.into()),
                        _ => panic!("only int/float args supported"),
                    }
                }
                let func = self.functions.get(method)
                    .copied()
                    .unwrap_or_else(|| panic!("undefined method: {}", method));
                let call = self.builder.build_call(func, &meta_args, method).unwrap();
                match call.try_as_basic_value() {
                    Either::Left(v) => {
                        let ret_type = self.fn_ret_types.get(method);
                        if matches!(ret_type, Some(Type::String)) && v.is_int_value() {
                            let int_val = v.into_int_value();
                            let ptr = self.builder.build_int_to_ptr(
                                int_val,
                                self.context.ptr_type(AddressSpace::default()),
                                "str_ptr",
                            ).unwrap();
                            CgValue::Str(ptr)
                        } else if matches!(ret_type, Some(Type::Float)) && v.is_int_value() {
                            let as_float = self.builder.build_bit_cast(v.into_int_value(), self.f64, "i2f").unwrap().into_float_value();
                            CgValue::Float(as_float)
                        } else if v.is_float_value() {
                            CgValue::Float(v.into_float_value())
                        } else {
                            CgValue::Int(v.into_int_value())
                        }
                    }
                    Either::Right(_) => CgValue::Int(self.i64.const_int(0, false)),
                }
            }
Expr::Match { scrutinee, arms } => {
                let sv = self.gen_expr(scrutinee);
                let parent = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let merge = self.context.append_basic_block(parent, "match_merge");
                let sv_int_raw = self.value_to_int(&sv);
                let has_data_variants = self.enum_variants.values().flat_map(|v| v.iter()).any(|(_, pt)| !pt.is_empty());
                let (sv_tag, sv_ptr) = if has_data_variants {
                    let sv_ptr = self.builder.build_int_to_ptr(sv_int_raw, self.context.ptr_type(AddressSpace::default()), "enum_ptr").unwrap();
                    let tag_ptr = unsafe {
                        self.builder.build_gep(self.i64, sv_ptr, &[self.i64.const_int(0, false)], "tag_ptr").unwrap()
                    };
                    let tag = self.builder.build_load(self.i64, tag_ptr, "tag").unwrap().into_int_value();
                    (tag, Some(sv_ptr))
                } else {
                    (sv_int_raw, None)
                };
                let mut arm_values: Vec<(IntValue<'ctx>, BasicBlock<'ctx>)> = Vec::new();
                let mut current_check = self.context.append_basic_block(parent, "match_check");
                self.builder.build_unconditional_branch(current_check).unwrap();
                for (i, (pattern, body)) in arms.iter().enumerate() {
                    self.builder.position_at_end(current_check);
                    let arm_bb = self.context.append_basic_block(parent, &format!("arm_{}", i));
                    let is_last = matches!(pattern, Pattern::Wildcard) || i == arms.len() - 1;
                    match pattern {
                        Pattern::Literal(e) => {
                            let pv = self.gen_expr(e);
                            let pv_int = self.value_to_int(&pv);
                            let cmp = self.builder.build_int_compare(inkwell::IntPredicate::EQ, sv_tag, pv_int, "match_cmp").unwrap();
                            if is_last {
                                self.builder.build_unconditional_branch(arm_bb).unwrap();
                            } else {
                                let next_check = self.context.append_basic_block(parent, &format!("match_check_{}", i + 1));
                                self.builder.build_conditional_branch(cmp, arm_bb, next_check).unwrap();
                                current_check = next_check;
                            }
                        }
                        Pattern::EnumVariant { name, inner } => {
                            let mut tag_val: u64 = 0;
                            let mut payload_types: Vec<Type> = Vec::new();
                            for (_, variants) in &self.enum_variants {
                                if let Some(pos) = variants.iter().position(|(vn, _)| vn == name) {
                                    tag_val = pos as u64;
                                    payload_types = variants[pos].1.clone();
                                    break;
                                }
                            }
                            let pv_int = self.i64.const_int(tag_val, false);
                            let cmp = self.builder.build_int_compare(inkwell::IntPredicate::EQ, sv_tag, pv_int, "match_cmp").unwrap();
                            if is_last {
                                self.builder.build_unconditional_branch(arm_bb).unwrap();
                            } else {
                                let next_check = self.context.append_basic_block(parent, &format!("match_check_{}", i + 1));
                                self.builder.build_conditional_branch(cmp, arm_bb, next_check).unwrap();
                                current_check = next_check;
                            }
                            if !inner.is_empty() && sv_ptr.is_some() && !payload_types.is_empty() {
                                self.builder.position_at_end(arm_bb);
                                for (j, pat) in inner.iter().enumerate() {
                                    if let Pattern::Variable(vname) = pat {
                                        let field_ptr = unsafe {
                                            self.builder.build_gep(self.i64, sv_ptr.unwrap(), &[
                                                self.i64.const_int((j + 1) as u64, false),
                                            ], &format!("{}_{}", name, j)).unwrap()
                                        };
                                        let field_val = self.builder.build_load(self.i64, field_ptr, vname).unwrap().into_int_value();
                                        let ptr = self.builder.build_alloca(self.i64, vname).unwrap();
                                        self.builder.build_store(ptr, field_val).unwrap();
                                        if matches!(payload_types.get(j), Some(Type::Float)) {
                                            self.named.insert(vname.clone(), (ptr, "float".to_string()));
                                        } else {
                                            self.named.insert(vname.clone(), (ptr, "int".to_string()));
                                        }
                                    }
                                }
                                let bv = self.gen_expr(body);
                                let bv_int = self.value_to_int(&bv);
                                arm_values.push((bv_int, arm_bb));
                                self.builder.build_unconditional_branch(merge).unwrap();
                                continue;
                            }
                        }
                        Pattern::Wildcard | Pattern::Variable(_) => {
                            self.builder.build_unconditional_branch(arm_bb).unwrap();
                        }
                    }
                    self.builder.position_at_end(arm_bb);
                    if let Pattern::Variable(name) = pattern {
                        let ptr = self.builder.build_alloca(self.i64, name).unwrap();
                        self.builder.build_store(ptr, sv_tag).unwrap();
                        self.named.insert(name.clone(), (ptr, "int".to_string()));
                    }
                    let bv = self.gen_expr(body);
                    let bv_int = self.value_to_int(&bv);
                    arm_values.push((bv_int, arm_bb));
                    self.builder.build_unconditional_branch(merge).unwrap();
                }
                if current_check.get_terminator().is_none() {
                    self.builder.position_at_end(current_check);
                    self.builder.build_unconditional_branch(merge).unwrap();
                }
                self.builder.position_at_end(merge);
                let phi = self.builder.build_phi(self.i64, "match_result").unwrap();
                for (v, bb) in &arm_values {
                    phi.add_incoming(&[(&*v, *bb)]);
                }
                CgValue::Int(phi.as_basic_value().into_int_value())
            }
            Expr::If { cond, then_body, else_body } => {
                let cond_val = self.gen_expr(cond);
                let cond_bool = self.to_i1(&cond_val);

                let fn_val = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let then_bb = self.context.append_basic_block(fn_val, "if.then");
                let else_bb = self.context.append_basic_block(fn_val, "if.else");
                let merge_bb = self.context.append_basic_block(fn_val, "if.merge");

                self.builder.build_conditional_branch(cond_bool, then_bb, else_bb).unwrap();

                self.builder.position_at_end(then_bb);
                let then_val = self.gen_expr(then_body);
                let then_int = match then_val {
                    CgValue::Int(v) => v,
                    CgValue::Float(v) => self.builder.build_bit_cast(v, self.i64, "f2i").unwrap().into_int_value(),
                    CgValue::Str(p) => self.builder.build_ptr_to_int(p, self.i64, "p2i").unwrap(),
                };
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                self.builder.position_at_end(else_bb);
                let else_val = if let Some(eb) = else_body {
                    self.gen_expr(eb)
                } else {
                    CgValue::Int(self.i64.const_int(0, false))
                };
                let else_int = match else_val {
                    CgValue::Int(v) => v,
                    CgValue::Float(v) => self.builder.build_bit_cast(v, self.i64, "f2i").unwrap().into_int_value(),
                    CgValue::Str(p) => self.builder.build_ptr_to_int(p, self.i64, "p2i").unwrap(),
                };
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(self.i64, "if.result").unwrap();
                phi.add_incoming(&[(&then_int, then_bb), (&else_int, else_bb)]);
                CgValue::Int(phi.as_basic_value().into_int_value())
            }
            Expr::Range(_, _) => {
                CgValue::Int(self.i64.const_int(0, false))
            }
            Expr::ArrayLit(elems) => {
                let len = elems.len() as u64;
                let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
                let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
                let total = self.i64.const_int(len + 1, false);
                let buf = self.builder.build_call(malloc_fn, &[total.into()], "arr_alloc").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();
                let buf_i64 = self.builder.build_ptr_to_int(buf, self.i64, "arr_buf").unwrap();
                let len_ptr = self.builder.build_int_to_ptr(buf_i64, self.context.ptr_type(AddressSpace::default()), "arr_len_ptr").unwrap();
                self.builder.build_store(len_ptr, self.i64.const_int(len, false)).unwrap();
                for (i, elem) in elems.iter().enumerate() {
                    let ev = self.gen_expr(elem);
                    let ev_int = self.value_to_int(&ev);
                    let offset = self.i64.const_int((i + 1) as u64, false);
                    let elem_ptr = unsafe {
                        self.builder.build_gep(self.i64, buf, &[offset], &format!("arr_{}", i)).unwrap()
                    };
                    self.builder.build_store(elem_ptr, ev_int).unwrap();
                }
                CgValue::Int(buf_i64)
            }
            Expr::Index(arr, idx) => {
                let arr_val = self.gen_expr(arr);
                let arr_ptr_val = self.value_to_int(&arr_val);
                let arr_ptr = self.builder.build_int_to_ptr(arr_ptr_val, self.context.ptr_type(AddressSpace::default()), "arr_ptr").unwrap();
                let idx_val = self.gen_expr(idx);
                let idx_int = self.value_to_int(&idx_val);
                let elem_offset = self.builder.build_int_add(idx_int, self.i64.const_int(1, false), "elem_off").unwrap();
                let elem_ptr = unsafe {
                    self.builder.build_gep(self.i64, arr_ptr, &[elem_offset], "elem_ptr").unwrap()
                };
                CgValue::Int(self.builder.build_load(self.i64, elem_ptr, "arr_elem").unwrap().into_int_value())
            }
        }
    }

    fn gen_string_concat(&self, a: PointerValue<'ctx>, b: PointerValue<'ctx>) -> CgValue<'ctx> {
        let strlen_type = self.i64.fn_type(&[self.context.ptr_type(AddressSpace::default()).into()], false);
        let strlen_fn = self.module.get_function("strlen").unwrap_or_else(|| self.module.add_function("strlen", strlen_type, None));

        let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
        let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));

        let memcpy_type = self.context.void_type().fn_type(&[
            self.context.ptr_type(AddressSpace::default()).into(),
            self.context.ptr_type(AddressSpace::default()).into(),
            self.i64.into(),
        ], false);
        let memcpy_fn = self.module.get_function("memcpy").unwrap_or_else(|| self.module.add_function("memcpy", memcpy_type, None));

        let len_a = self.builder.build_call(strlen_fn, &[a.into()], "len_a").unwrap().try_as_basic_value().left().unwrap().into_int_value();
        let len_b = self.builder.build_call(strlen_fn, &[b.into()], "len_b").unwrap().try_as_basic_value().left().unwrap().into_int_value();
        let total = self.builder.build_int_add(len_a, len_b, "total_len").unwrap();
        let total_plus1 = self.builder.build_int_add(total, self.i64.const_int(1, false), "total1").unwrap();
        let buf = self.builder.build_call(malloc_fn, &[total_plus1.into()], "buf").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();
        self.builder.build_call(memcpy_fn, &[buf.into(), a.into(), len_a.into()], "cp_a").unwrap();
        let offset = unsafe { self.builder.build_gep(self.i8, buf, &[len_a], "offset").unwrap() };
        self.builder.build_call(memcpy_fn, &[offset.into(), b.into(), self.builder.build_int_add(len_b, self.i64.const_int(1, false), "nb1").unwrap().into()], "cp_b").unwrap();
        CgValue::Str(buf)
    }

    fn gen_int_to_str(&self, val: IntValue<'ctx>) -> PointerValue<'ctx> {
        let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
        let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
        let buf = self.builder.build_call(malloc_fn, &[self.i64.const_int(32, false).into()], "int_buf").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();

        let fn_val = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let entry_bb = self.builder.get_insert_block().unwrap();

        let neg_bb = self.context.append_basic_block(fn_val, "itoa_neg");
        let pos_bb = self.context.append_basic_block(fn_val, "itoa_pos");
        let loop_bb = self.context.append_basic_block(fn_val, "itoa_loop");
        let done_bb = self.context.append_basic_block(fn_val, "itoa_done");

        let is_neg = self.builder.build_int_compare(inkwell::IntPredicate::SLT, val, self.i64.const_int(0, false), "is_neg").unwrap();
        self.builder.build_conditional_branch(is_neg, neg_bb, pos_bb).unwrap();

        self.builder.position_at_end(neg_bb);
        let neg_val = self.builder.build_int_neg(val, "neg_val").unwrap();
        self.builder.build_unconditional_branch(pos_bb).unwrap();

        self.builder.position_at_end(pos_bb);
        let abs_phi = self.builder.build_phi(self.i64, "abs_val").unwrap();
        abs_phi.add_incoming(&[
            (&neg_val as &dyn BasicValue, neg_bb),
            (&val as &dyn BasicValue, entry_bb),
        ]);
        let abs = abs_phi.as_basic_value().into_int_value();

        let idx_ptr = self.builder.build_alloca(self.i64, "itoa_idx").unwrap();
        self.builder.build_store(idx_ptr, self.i64.const_int(30, false)).unwrap();
        let val_ptr = self.builder.build_alloca(self.i64, "itoa_val").unwrap();
        self.builder.build_store(val_ptr, abs).unwrap();

        self.builder.build_unconditional_branch(loop_bb).unwrap();
        self.builder.position_at_end(loop_bb);
        let cur_idx = self.builder.build_load(self.i64, idx_ptr, "cur_idx").unwrap().into_int_value();
        let cur_val = self.builder.build_load(self.i64, val_ptr, "cur_val").unwrap().into_int_value();
        let ten = self.i64.const_int(10, false);
        let digit = self.builder.build_int_unsigned_rem(cur_val, ten, "digit").unwrap();
        let ascii = self.builder.build_int_add(digit, self.i64.const_int(48, false), "ascii").unwrap();

        let byte_ptr_ty = self.i8.ptr_type(AddressSpace::default());
        let buf_byte_ptr = self.builder.build_bit_cast(buf, byte_ptr_ty, "buf_byte").unwrap().into_pointer_value();
        let char_ptr = unsafe {
            self.builder.build_gep(self.i8, buf_byte_ptr, &[cur_idx], "char_ptr").unwrap()
        };
        let trunc_ascii = self.builder.build_int_truncate(ascii, self.i8, "ascii8").unwrap();
        self.builder.build_store(char_ptr, trunc_ascii).unwrap();

        let next_idx = self.builder.build_int_sub(cur_idx, self.i64.const_int(1, false), "next_idx").unwrap();
        self.builder.build_store(idx_ptr, next_idx).unwrap();
        let next_val = self.builder.build_int_unsigned_div(cur_val, ten, "next_val").unwrap();
        self.builder.build_store(val_ptr, next_val).unwrap();

        let is_done = self.builder.build_int_compare(inkwell::IntPredicate::SLT, next_idx, self.i64.const_int(0, false), "is_done").unwrap();
        let is_val_zero = self.builder.build_int_compare(inkwell::IntPredicate::EQ, next_val, self.i64.const_int(0, false), "val_zero").unwrap();
        let should_exit = self.builder.build_or(is_done, is_val_zero, "should_exit").unwrap();
        self.builder.build_conditional_branch(should_exit, done_bb, loop_bb).unwrap();

        self.builder.position_at_end(done_bb);
        let null_term_idx = self.builder.build_load(self.i64, idx_ptr, "null_idx").unwrap().into_int_value();
        let first_digit = self.builder.build_int_add(null_term_idx, self.i64.const_int(1, false), "first_digit").unwrap();
        let buf_byte_ptr2 = self.builder.build_bit_cast(buf, byte_ptr_ty, "buf_byte2").unwrap().into_pointer_value();

        let write_neg_bb = self.context.append_basic_block(fn_val, "write_neg");
        let final_bb = self.context.append_basic_block(fn_val, "itoa_final");
        self.builder.build_conditional_branch(is_neg, write_neg_bb, final_bb).unwrap();

        self.builder.position_at_end(write_neg_bb);
        let first_digit_neg = self.builder.build_int_sub(first_digit, self.i64.const_int(1, false), "first_digit_neg").unwrap();
        let minus_ptr = unsafe {
            self.builder.build_gep(self.i8, buf_byte_ptr2, &[first_digit_neg], "minus_ptr").unwrap()
        };
        self.builder.build_store(minus_ptr, self.i8.const_int(45, false)).unwrap();
        self.builder.build_unconditional_branch(final_bb).unwrap();

        self.builder.position_at_end(final_bb);
        let final_first_digit = self.builder.build_phi(self.i64, "final_first").unwrap();
        final_first_digit.add_incoming(&[
            (&first_digit_neg as &dyn BasicValue, write_neg_bb),
            (&first_digit as &dyn BasicValue, done_bb),
        ]);
        let fd = final_first_digit.as_basic_value().into_int_value();

        let null_ptr = unsafe {
            self.builder.build_gep(self.i8, buf_byte_ptr2, &[self.i64.const_int(31, false)], "null_ptr").unwrap()
        };
        self.builder.build_store(null_ptr, self.i8.const_int(0, false)).unwrap();

        let result_ptr = unsafe {
            self.builder.build_gep(self.i8, buf_byte_ptr2, &[fd], "result_ptr").unwrap()
        };
        result_ptr
    }

    fn gen_float_to_str(&self, val: FloatValue<'ctx>) -> PointerValue<'ctx> {
        let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
        let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
        let buf = self.builder.build_call(malloc_fn, &[self.i64.const_int(64, false).into()], "float_buf").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();

        let byte_ptr_ty = self.i8.ptr_type(AddressSpace::default());
        let byte_ptr = self.builder.build_bit_cast(buf, byte_ptr_ty, "float_byte_ptr").unwrap().into_pointer_value();

        let sprintf_type = self.i64.fn_type(&[
            byte_ptr_ty.into(),
            byte_ptr_ty.into(),
        ], true);
        let sprintf_fn = self.module.get_function("sprintf").unwrap_or_else(|| self.module.add_function("sprintf", sprintf_type, None));

        let fmt = self.builder.build_global_string_ptr("%.6f", "float_fmt").unwrap().as_pointer_value();

        self.builder.build_call(sprintf_fn, &[byte_ptr.into(), fmt.into(), val.into()], "call_sprintf").unwrap();

        buf
    }

    fn value_to_int(&self, v: &CgValue<'ctx>) -> IntValue<'ctx> {
        match v {
            CgValue::Int(val) => *val,
            CgValue::Float(val) => self.builder.build_bit_cast(*val, self.i64, "f2i").unwrap().into_int_value(),
            CgValue::Str(p) => self.builder.build_ptr_to_int(*p, self.i64, "p2i").unwrap(),
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
                if let Expr::StructLit { name: struct_name, .. } = &value {
                    self.var_struct_type.insert(name.clone(), struct_name.clone());
                }
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
                            CgValue::Float(v) => {
                                let as_i64 = self.builder.build_bit_cast(v, self.i64, "f2i_ret").unwrap().into_int_value();
                                self.builder.build_return(Some(&as_i64)).unwrap();
                            }
                            CgValue::Str(p) => {
                                let p_int = self.builder.build_ptr_to_int(p, self.i64, "str_ret").unwrap();
                                self.builder.build_return(Some(&p_int)).unwrap();
                            }
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

                self.loop_exit.push(after_bb);
                self.loop_continue.push(loop_bb);

                self.builder.position_at_end(body_bb);
                let mut body_terminated = false;
                for s in body {
                    self.gen_stmt(s, fn_val, &mut body_terminated);
                    if body_terminated { break; }
                }
                if !body_terminated {
                    self.builder.build_unconditional_branch(loop_bb).unwrap();
                }

                self.loop_exit.pop();
                self.loop_continue.pop();

                self.builder.position_at_end(after_bb);
            }
            Stmt::Break(_) => {
                if let Some(exit_bb) = self.loop_exit.last().cloned() {
                    self.builder.build_unconditional_branch(exit_bb).unwrap();
                    let dead_bb = self.context.append_basic_block(self.builder.get_insert_block().unwrap().get_parent().unwrap(), "dead");
                    self.builder.position_at_end(dead_bb);
                }
            }
            Stmt::Continue(_) => {
                if let Some(cont_bb) = self.loop_continue.last().cloned() {
                    self.builder.build_unconditional_branch(cont_bb).unwrap();
                    let dead_bb = self.context.append_basic_block(self.builder.get_insert_block().unwrap().get_parent().unwrap(), "dead");
                    self.builder.position_at_end(dead_bb);
                }
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
                    self.loop_exit.push(exit_bb);
                    self.loop_continue.push(cont_bb);
                    self.builder.position_at_end(body_bb);
                    let mut body_terminated = false;
                    for s in body {
                        self.gen_stmt(s, fn_val, &mut body_terminated);
                        if body_terminated { break; }
                    }
                    if !body_terminated {
                        self.builder.build_unconditional_branch(cont_bb).unwrap();
                    }
                    self.loop_exit.pop();
                    self.loop_continue.pop();

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

fn expand_macros_in_expr(expr: &Expr, macros: &HashMap<String, (Vec<String>, Vec<Stmt>)>) -> Expr {
    if let Expr::Call(callee, args) = expr {
        if let Expr::Ident(name) = callee.as_ref() {
            if let Some((params, body)) = macros.get(name) {
                let mut arg_map: HashMap<String, Expr> = HashMap::new();
                for (i, p) in params.iter().enumerate() {
                    arg_map.insert(p.clone(), substitute_expr(&args[i], &HashMap::new(), macros));
                }
                if body.len() == 1 {
                    if let Stmt::Expr(e) = &body[0] {
                        return substitute_expr(e, &arg_map, macros);
                    }
                    if let Stmt::Return(Some(e)) = &body[0] {
                        return substitute_expr(e, &arg_map, macros);
                    }
                }
                return expr.clone();
            }
        }
    }
    match expr {
        Expr::Binary(l, op, r) => Expr::Binary(Box::new(expand_macros_in_expr(l, macros)), op.clone(), Box::new(expand_macros_in_expr(r, macros))),
        Expr::Unary(op, e) => Expr::Unary(op.clone(), Box::new(expand_macros_in_expr(e, macros))),
        Expr::Call(callee, args) => Expr::Call(Box::new(expand_macros_in_expr(callee, macros)), args.iter().map(|a| expand_macros_in_expr(a, macros)).collect()),
        Expr::If { cond, then_body, else_body } => Expr::If {
            cond: Box::new(expand_macros_in_expr(cond, macros)),
            then_body: Box::new(expand_macros_in_expr(then_body, macros)),
            else_body: else_body.as_ref().map(|e| Box::new(expand_macros_in_expr(e, macros))),
        },
        Expr::FieldAccess(obj, field) => Expr::FieldAccess(Box::new(expand_macros_in_expr(obj, macros)), field.clone()),
        Expr::MethodCall(obj, method, args) => Expr::MethodCall(Box::new(expand_macros_in_expr(obj, macros)), method.clone(), args.iter().map(|a| expand_macros_in_expr(a, macros)).collect()),
        Expr::Index(arr, idx) => Expr::Index(Box::new(expand_macros_in_expr(arr, macros)), Box::new(expand_macros_in_expr(idx, macros))),
        Expr::StructLit { name, fields } => Expr::StructLit { name: name.clone(), fields: fields.iter().map(|(n, e)| (n.clone(), expand_macros_in_expr(e, macros))).collect() },
        Expr::Match { scrutinee, arms } => Expr::Match { scrutinee: Box::new(expand_macros_in_expr(scrutinee, macros)), arms: arms.iter().map(|(p, e)| (p.clone(), expand_macros_in_expr(e, macros))).collect() },
        _ => expr.clone(),
    }
}

fn substitute_expr(expr: &Expr, arg_map: &HashMap<String, Expr>, macros: &HashMap<String, (Vec<String>, Vec<Stmt>)>) -> Expr {
    match expr {
        Expr::Ident(name) => {
            if let Some(val) = arg_map.get(name) {
                val.clone()
            } else {
                Expr::Ident(name.clone())
            }
        }
        Expr::Binary(l, op, r) => Expr::Binary(
            Box::new(substitute_expr(l, arg_map, macros)),
            op.clone(),
            Box::new(substitute_expr(r, arg_map, macros)),
        ),
        Expr::Unary(op, e) => Expr::Unary(op.clone(), Box::new(substitute_expr(e, arg_map, macros))),
        Expr::Call(callee, args) => {
            let expanded = Expr::Call(
                Box::new(substitute_expr(callee, arg_map, macros)),
                args.iter().map(|a| substitute_expr(a, arg_map, macros)).collect(),
            );
            expand_macros_in_expr(&expanded, macros)
        }
        Expr::If { cond, then_body, else_body } => Expr::If {
            cond: Box::new(substitute_expr(cond, arg_map, macros)),
            then_body: Box::new(substitute_expr(then_body, arg_map, macros)),
            else_body: else_body.as_ref().map(|e| Box::new(substitute_expr(e, arg_map, macros))),
        },
        Expr::FieldAccess(obj, field) => Expr::FieldAccess(Box::new(substitute_expr(obj, arg_map, macros)), field.clone()),
        Expr::MethodCall(obj, method, args) => Expr::MethodCall(
            Box::new(substitute_expr(obj, arg_map, macros)),
            method.clone(),
            args.iter().map(|a| substitute_expr(a, arg_map, macros)).collect(),
        ),
        Expr::Index(arr, idx) => Expr::Index(
            Box::new(substitute_expr(arr, arg_map, macros)),
            Box::new(substitute_expr(idx, arg_map, macros)),
        ),
        Expr::StructLit { name, fields } => Expr::StructLit {
            name: name.clone(),
            fields: fields.iter().map(|(n, e)| (n.clone(), substitute_expr(e, arg_map, macros))).collect(),
        },
        Expr::Match { scrutinee, arms } => Expr::Match {
            scrutinee: Box::new(substitute_expr(scrutinee, arg_map, macros)),
            arms: arms.iter().map(|(p, e)| (p.clone(), substitute_expr(e, arg_map, macros))).collect(),
        },
        Expr::Range(l, r) => Expr::Range(
            Box::new(substitute_expr(l, arg_map, macros)),
            Box::new(substitute_expr(r, arg_map, macros)),
        ),
        Expr::ArrayLit(elems) => Expr::ArrayLit(elems.iter().map(|e| substitute_expr(e, arg_map, macros)).collect()),
        _ => expr.clone(),
    }
}

fn expand_macros_program(program: &Program) -> Program {
    let mut macros: HashMap<String, (Vec<String>, Vec<Stmt>)> = HashMap::new();
    for s in &program.stmts {
        if let Stmt::Macro { name, params, body } = s {
            macros.insert(name.clone(), (params.clone(), body.clone()));
        }
    }
    if macros.is_empty() {
        return program.clone();
    }
    let mut new_stmts = Vec::new();
    for s in &program.stmts {
        match s {
            Stmt::Macro { .. } => {}
            Stmt::Fn { name, generics, params, ret, body } => {
                let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect();
                new_stmts.push(Stmt::Fn { name: name.clone(), generics: generics.clone(), params: params.clone(), ret: ret.clone(), body: new_body });
            }
            Stmt::Expr(e) => { new_stmts.push(Stmt::Expr(expand_macros_in_expr(e, &macros))); }
            Stmt::Let { name, ty, value } => { new_stmts.push(Stmt::Let { name: name.clone(), ty: ty.clone(), value: expand_macros_in_expr(value, &macros) }); }
            Stmt::Assign { name, value } => { new_stmts.push(Stmt::Assign { name: name.clone(), value: expand_macros_in_expr(value, &macros) }); }
            Stmt::Return(e) => { new_stmts.push(Stmt::Return(e.as_ref().map(|e| expand_macros_in_expr(e, &macros)))); }
            Stmt::While { cond, body } => {
                let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect();
                new_stmts.push(Stmt::While { cond: expand_macros_in_expr(cond, &macros), body: new_body });
            }
            Stmt::For { var, iter, body } => {
                let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect();
                new_stmts.push(Stmt::For { var: var.clone(), iter: expand_macros_in_expr(iter, &macros), body: new_body });
            }
            Stmt::If { cond, then_body, else_body } => {
                let new_then: Vec<Stmt> = then_body.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect();
                let new_else = else_body.as_ref().map(|v| v.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect());
                new_stmts.push(Stmt::If { cond: expand_macros_in_expr(cond, &macros), then_body: new_then, else_body: new_else });
            }
            _ => new_stmts.push(s.clone()),
        }
    }
    Program { stmts: new_stmts }
}

fn expand_macros_in_stmt(stmt: &Stmt, macros: &HashMap<String, (Vec<String>, Vec<Stmt>)>) -> Vec<Stmt> {
    match stmt {
        Stmt::Expr(e) => {
            if let Expr::Call(callee, args) = e {
                if let Expr::Ident(mname) = callee.as_ref() {
                    if let Some((params, body)) = macros.get(mname) {
                        if body.len() >= 2 {
                            return expand_macro_call_inline(params, body, args, None, macros);
                        }
                    }
                }
            }
            vec![Stmt::Expr(expand_macros_in_expr(e, macros))]
        }
        Stmt::Let { name, ty, value } => {
            if let Expr::Call(callee, args) = value {
                if let Expr::Ident(mname) = callee.as_ref() {
                    if let Some((params, body)) = macros.get(mname) {
                        if body.len() >= 2 {
                            return expand_macro_call_inline(params, body, args, Some(name.clone()), macros);
                        }
                    }
                }
            }
            vec![Stmt::Let { name: name.clone(), ty: ty.clone(), value: expand_macros_in_expr(value, macros) }]
        }
        Stmt::Assign { name, value } => vec![Stmt::Assign { name: name.clone(), value: expand_macros_in_expr(value, macros) }],
        Stmt::Return(e) => vec![Stmt::Return(e.as_ref().map(|e| expand_macros_in_expr(e, macros)))],
        Stmt::While { cond, body } => {
            let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect();
            vec![Stmt::While { cond: expand_macros_in_expr(cond, macros), body: new_body }]
        }
        Stmt::For { var, iter, body } => {
            let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect();
            vec![Stmt::For { var: var.clone(), iter: expand_macros_in_expr(iter, macros), body: new_body }]
        }
        Stmt::If { cond, then_body, else_body } => {
            let new_then: Vec<Stmt> = then_body.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect();
            let new_else = else_body.as_ref().map(|v| v.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect());
            vec![Stmt::If { cond: expand_macros_in_expr(cond, macros), then_body: new_then, else_body: new_else }]
        }
        _ => vec![stmt.clone()],
    }
}

fn expand_macro_call_inline(params: &[String], body: &[Stmt], args: &[Expr], return_var: Option<String>, macros: &HashMap<String, (Vec<String>, Vec<Stmt>)>) -> Vec<Stmt> {
    let mut arg_map: HashMap<String, Expr> = HashMap::new();
    for (i, p) in params.iter().enumerate() {
        arg_map.insert(p.clone(), substitute_expr(&args[i], &HashMap::new(), macros));
    }
    let mut result = Vec::new();
    for s in body {
        match s {
            Stmt::Let { name, ty: _, value } => {
                let substituted_val = substitute_expr(value, &arg_map, macros);
                result.push(Stmt::Let { name: name.clone(), ty: Type::Inferred, value: substituted_val });
            }
            Stmt::Return(Some(e)) => {
                let substituted = substitute_expr(e, &arg_map, macros);
                if let Some(var) = &return_var {
                    result.push(Stmt::Let { name: var.clone(), ty: Type::Inferred, value: substituted });
                } else {
                    result.push(Stmt::Return(Some(substituted)));
                }
            }
            Stmt::Assign { name, value } => {
                let substituted = substitute_expr(value, &arg_map, macros);
                result.push(Stmt::Assign { name: name.clone(), value: substituted });
            }
            Stmt::If { cond, then_body, else_body } => {
                let substituted_cond = substitute_expr(cond, &arg_map, macros);
                let new_then: Vec<Stmt> = then_body.iter().flat_map(|s| expand_macro_call_inline_single(s, &arg_map, return_var.clone(), macros)).collect();
                let new_else = else_body.as_ref().map(|v| v.iter().flat_map(|s| expand_macro_call_inline_single(s, &arg_map, return_var.clone(), macros)).collect());
                result.push(Stmt::If { cond: substituted_cond, then_body: new_then, else_body: new_else });
            }
            Stmt::Expr(e) => {
                let substituted = substitute_expr(e, &arg_map, macros);
                result.push(Stmt::Expr(substituted));
            }
            _ => result.push(s.clone()),
        }
    }
    result
}

fn expand_macro_call_inline_single(stmt: &Stmt, arg_map: &HashMap<String, Expr>, return_var: Option<String>, macros: &HashMap<String, (Vec<String>, Vec<Stmt>)>) -> Vec<Stmt> {
    match stmt {
        Stmt::Let { name, ty: _, value } => {
            let substituted_val = substitute_expr(value, arg_map, macros);
            vec![Stmt::Let { name: name.clone(), ty: Type::Inferred, value: substituted_val }]
        }
        Stmt::Return(Some(e)) => {
            let substituted = substitute_expr(e, arg_map, macros);
            if let Some(var) = &return_var {
                vec![Stmt::Let { name: var.clone(), ty: Type::Inferred, value: substituted }]
            } else {
                vec![Stmt::Return(Some(substituted))]
            }
        }
        Stmt::Assign { name, value } => {
            let substituted = substitute_expr(value, arg_map, macros);
            vec![Stmt::Assign { name: name.clone(), value: substituted }]
        }
        Stmt::Expr(e) => {
            let substituted = substitute_expr(e, arg_map, macros);
            vec![Stmt::Expr(substituted)]
        }
        _ => vec![stmt.clone()],
    }
}

/// Compile a program to a native executable at `out_path` using LLVM + system
/// toolchain (llc / clang).
pub fn compile_to_executable(program: &Program, out_path: &str) {
    let program = expand_macros_program(program);
    let context = Context::create();
    let mut cg = Codegen::new(&context);

    // Register struct fields and enum variants
    for s in &program.stmts {
        match s {
            Stmt::Struct { name, fields, .. } => {
                cg.struct_fields.insert(name.clone(), fields.iter().map(|(n, _)| n.clone()).collect());
            }
            Stmt::Enum { name, variants } => {
                cg.enum_variants.insert(name.clone(), variants.clone());
            }
            _ => {}
        }
    }

    // Pre-declare all functions (including impl methods)
    for s in &program.stmts {
        match s {
            Stmt::Fn { name, params, ret, .. } => {
                let internal_name = if name == "main" { "_zarrin_main".to_string() } else { name.clone() };
                let param_types: Vec<BasicMetadataTypeEnum> = params.iter().map(|_| cg.i64.into()).collect();
                let fn_type = if matches!(ret, Type::Unit) {
                    context.void_type().fn_type(&param_types, false)
                } else {
                    cg.i64.fn_type(&param_types, false)
                };
                let func = cg.module.add_function(&internal_name, fn_type, None);
                cg.functions.insert(name.clone(), func);
                cg.fn_ret_types.insert(name.clone(), ret.clone());
            }
            Stmt::Impl { methods, .. } => {
                for m in methods {
                    if let Stmt::Fn { name, params, ret, .. } = m {
                        let param_types: Vec<BasicMetadataTypeEnum> = params.iter().map(|_| cg.i64.into()).collect();
                        let fn_type = if matches!(ret, Type::Unit) {
                            context.void_type().fn_type(&param_types, false)
                        } else {
                            cg.i64.fn_type(&param_types, false)
                        };
                        let func = cg.module.add_function(name, fn_type, None);
                        cg.functions.insert(name.clone(), func);
                        cg.fn_ret_types.insert(name.clone(), ret.clone());
                    }
                }
            }
            _ => {}
        }
    }

    // Generate function bodies
    for s in &program.stmts {
        match s {
            Stmt::Fn { params, body, ret, .. } => {
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
            Stmt::Impl { methods, .. } => {
                for m in methods {
                    if let Stmt::Fn { name, params, body, ret, .. } = m {
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
            }
            _ => {}
        }
    }

    // Generate main
    let has_user_main = program.stmts.iter().any(|s| matches!(s, Stmt::Fn { name, .. } if name == "main"));

    let main_type = context.i64_type().fn_type(&[], false);
    let main_fn = cg.module.add_function("main", main_type, None);
    let entry = context.append_basic_block(main_fn, "entry");
    cg.builder.position_at_end(entry);
    cg.named.clear();

    if has_user_main {
        let user_main = cg.module.get_function("_zarrin_main").unwrap();
        cg.builder.build_call(user_main, &[], "").unwrap();
        cg.builder.build_return(Some(&cg.i64.const_int(0, false))).unwrap();
    } else {
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
        .output()
        .expect("failed to run llc");
    if !status.status.success() {
        eprintln!("LLVM IR:\n{}", ir);
        eprintln!("llc stderr:\n{}", String::from_utf8_lossy(&status.stderr));
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
