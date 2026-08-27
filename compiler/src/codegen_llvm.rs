//! LLVM backend (real native code generation).
//!
//! Enabled via the `llvm` Cargo feature. Requires system LLVM 18
//! (e.g. `brew install llvm@18`) and the `LLVM_SYS_180_PREFIX` env var
//! pointing at its prefix.

#![cfg(feature = "llvm")]

use crate::ast::*;
use crate::typecheck::{TypeChecker, TypeEnv};
use crate::diagnostic::{Diagnostic, Span};
use crate::variants::{builtin_enums, Lookup, Variant, VariantIndex};
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
    /// Locals: where the value lives, and the type that says how to read it
    /// back. This used to be tagged with a string, always "int" for
    /// parameters, so a `float` or `string` parameter was read as an integer.
    named: HashMap<String, (PointerValue<'ctx>, Type)>,
    struct_fields: HashMap<String, Vec<(String, Type)>>,
    var_struct_type: HashMap<String, String>,
    /// The type checker's view of the program, kept in step with the locals in
    /// scope. Everything this backend needs to know about a value's type is
    /// asked here, rather than re-derived from the shape of the expression.
    types: TypeEnv,
    /// Source of the program being compiled, so a check that fails at run time
    /// can print the same diagnostic the interpreter would.
    path: String,
    src: String,
    current_span: Span,
    /// Set while building a value that is consumed on the spot and discarded.
    /// Allocations made under it go on the frame and are released when the
    /// statement ends, instead of being heap-allocated and left.
    transient: bool,
    /// Names bound in each open block, innermost last, so a block can release
    /// what it introduced. The backend had no block scoping before this.
    owned: Vec<Vec<String>>,
    enum_variants: HashMap<String, Vec<(String, Vec<Type>)>>,
    variants: VariantIndex,
    loop_exit: Vec<BasicBlock<'ctx>>,
    loop_continue: Vec<BasicBlock<'ctx>>,
    loop_result_ptr: Vec<PointerValue<'ctx>>,
    i64: IntType<'ctx>,
    i8: IntType<'ctx>,
    f64: FloatType<'ctx>,
    i1: IntType<'ctx>,
}

impl<'ctx> Codegen<'ctx> {
    fn new(context: &'ctx Context, program: &Program) -> Self {
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
            types: TypeChecker::env_for(program),
            path: String::new(),
            src: String::new(),
            current_span: Span::new(1, 1),
            transient: false,
            owned: Vec::new(),
            enum_variants: HashMap::new(),
            variants: VariantIndex::build(program),
            loop_exit: Vec::new(),
            loop_continue: Vec::new(),
            loop_result_ptr: Vec::new(),
            i64,
            i8: context.i8_type(),
            f64,
            i1,
        }
    }

    fn malloc_fn(&self) -> FunctionValue<'ctx> {
        self.module.get_function("malloc").unwrap_or_else(|| {
            let ty = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
            self.module.add_function("malloc", ty, None)
        })
    }

    fn exit_fn(&self) -> FunctionValue<'ctx> {
        self.module.get_function("exit").unwrap_or_else(|| {
            let ty = self.context.void_type().fn_type(&[self.context.i32_type().into()], false);
            self.module.add_function("exit", ty, None)
        })
    }

    /// Emit code that prints a diagnostic and stops.
    ///
    /// The frame — path, line, the quoted source line, the caret — is known at
    /// compile time and baked into the format string.
    /// `\x01` in `message` marks a value filled in at run time.
    fn gen_abort(&self, message: &str, args: &[IntValue<'ctx>]) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let snprintf = self.module.get_function("snprintf").unwrap_or_else(|| {
            let ty = i32_ty.fn_type(&[ptr_ty.into(), self.i64.into(), ptr_ty.into()], true);
            self.module.add_function("snprintf", ty, None)
        });
        let write = self.module.get_function("write").unwrap_or_else(|| {
            let ty = self.i64.fn_type(&[i32_ty.into(), ptr_ty.into(), self.i64.into()], false);
            self.module.add_function("write", ty, None)
        });

        // Anything already printed is sitting in stdio's buffer; without this
        // the error reaches a pipe before the output that preceded it.
        let fflush = self.module.get_function("fflush").unwrap_or_else(|| {
            let ty = i32_ty.fn_type(&[ptr_ty.into()], false);
            self.module.add_function("fflush", ty, None)
        });
        self.builder.build_call(fflush, &[ptr_ty.const_null().into()], "").unwrap();

        let rendered = Diagnostic::new(message, self.current_span).render(&self.path, &self.src);
        // the source line may itself contain a percent sign
        let text = rendered.replace('%', "%%").replace('\x01', "%ld");
        let fmt = self.builder.build_global_string_ptr(&text, "abort_fmt").unwrap().as_pointer_value();

        let cap = self.i64.const_int(4096, false);
        // This block ends in exit, so the frame is the right place and the
        // allocation cannot accumulate.
        let buf = self.builder.build_array_alloca(self.i8, cap, "abort_buf").unwrap();
        let mut call_args: Vec<BasicMetadataValueEnum> = vec![buf.into(), cap.into(), fmt.into()];
        for a in args { call_args.push((*a).into()); }
        let n = self.builder.build_call(snprintf, &call_args, "abort_len").unwrap()
            .try_as_basic_value().left().unwrap().into_int_value();
        let n64 = self.builder.build_int_s_extend(n, self.i64, "abort_len64").unwrap();
        self.builder.build_call(write, &[i32_ty.const_int(2, false).into(), buf.into(), n64.into()], "").unwrap();
        self.builder.build_call(self.exit_fn(), &[i32_ty.const_int(1, false).into()], "").unwrap();
        self.builder.build_unreachable().unwrap();
    }

    /// Stop with the interpreter's message when an index is out of range. The
    /// native backend read past the allocation instead, printed whatever was
    /// there and carried on.
    fn gen_bounds_check(&self, idx: IntValue<'ctx>, len: IntValue<'ctx>, what: &str) {
        let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let fail = self.context.append_basic_block(f, "bounds_fail");
        let ok = self.context.append_basic_block(f, "bounds_ok");
        let neg = self.builder.build_int_compare(inkwell::IntPredicate::SLT, idx, self.i64.const_zero(), "neg_idx").unwrap();
        let past = self.builder.build_int_compare(inkwell::IntPredicate::SGE, idx, len, "past_end").unwrap();
        let bad = self.builder.build_or(neg, past, "out_of_range").unwrap();
        self.builder.build_conditional_branch(bad, fail, ok).unwrap();
        self.builder.position_at_end(fail);
        self.gen_abort(&format!("{} index \x01 is out of bounds for length \x01", what), &[idx, len]);
        self.builder.position_at_end(ok);
    }

    /// An alloca in the function's entry block, so a buffer inside a loop is
    /// reserved once for the frame rather than on every iteration.
    fn entry_alloca(&self, ty: inkwell::types::BasicTypeEnum<'ctx>, name: &str) -> PointerValue<'ctx> {
        let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let entry = f.get_first_basic_block().unwrap();
        let b = self.context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => b.position_before(&first),
            None => b.position_at_end(entry),
        }
        b.build_alloca(ty, name).unwrap()
    }

    /// Space for a value that cannot outlive the expression producing it.
    /// Inside a consumed expression that is the frame; anywhere else it has to
    /// be the heap, because the value may escape.
    /// Mark the stack, so frame allocations made while building a consumed
    /// value are released again. Without this a `print` inside a loop would
    /// grow the stack instead of the heap, which is no better.
    fn stack_mark(&self) -> PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let save = self.module.get_function("llvm.stacksave.p0").unwrap_or_else(|| {
            self.module.add_function("llvm.stacksave.p0", ptr_ty.fn_type(&[], false), None)
        });
        self.builder.build_call(save, &[], "sp").unwrap()
            .try_as_basic_value().left().unwrap().into_pointer_value()
    }

    fn stack_release(&self, mark: PointerValue<'ctx>) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let restore = self.module.get_function("llvm.stackrestore.p0").unwrap_or_else(|| {
            self.module.add_function("llvm.stackrestore.p0", self.context.void_type().fn_type(&[ptr_ty.into()], false), None)
        });
        self.builder.build_call(restore, &[mark.into()], "").unwrap();
    }

    /// Build `f`'s value knowing it is consumed here and kept by nobody.
    fn consumed<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let outer = self.transient;
        let mark = self.stack_mark();
        self.transient = true;
        let r = f(self);
        self.transient = outer;
        self.stack_release(mark);
        r
    }

    fn scratch_bytes(&self, bytes: IntValue<'ctx>, name: &str) -> PointerValue<'ctx> {
        if self.transient {
            self.builder.build_array_alloca(self.i8, bytes, name).unwrap()
        } else {
            self.heap_bytes(bytes, name)
        }
    }

    fn scratch_slots(&self, slots: u64, name: &str) -> PointerValue<'ctx> {
        if self.transient {
            self.builder.build_alloca(self.i64.array_type(slots as u32), name).unwrap()
        } else {
            self.heap_slots(slots, name)
        }
    }

    fn field_type(&self, struct_name: &str, field: &str) -> Option<Type> {
        self.struct_fields.get(struct_name)?.iter().find(|(n, _)| n == field).map(|(_, t)| t.clone())
    }

    /// Does a value of this type live behind a counted pointer?
    /// Does evaluating this expression hand back a reference nobody else
    /// holds? A fresh allocation does; naming something that already exists
    /// does not, and has to be retained before a second owner records it.
    fn produces_owned(&mut self, e: &Expr) -> bool {
        match &*e.kind {
            ExprKind::Ident(_) | ExprKind::FieldAccess(..) | ExprKind::Index(..) => false,
            ExprKind::If { then_body, .. } => self.produces_owned(then_body),
            ExprKind::Match { arms, .. } => arms.first().map(|(_, _, b)| b.clone()).map(|b| self.produces_owned(&b)).unwrap_or(true),
            _ => true,
        }
    }

    fn open_block(&mut self) {
        self.owned.push(Vec::new());
    }

    /// The pointer a managed local currently holds.
    fn managed_ptr(&self, name: &str) -> Option<(PointerValue<'ctx>, Type)> {
        let (_, ty) = self.named.get(name)?;
        let ty = ty.clone();
        if !self.is_managed(&ty) {
            return None;
        }
        let v = self.load_local(name)?;
        let p = match v {
            CgValue::Str(p) => p,
            other => {
                let raw = self.value_to_int(&other);
                self.builder
                    .build_int_to_ptr(raw, self.context.ptr_type(AddressSpace::default()), "as_ptr")
                    .unwrap()
            }
        };
        Some((p, ty))
    }

    /// An aggregate becomes an owner of what it is given. A value built on the
    /// spot hands over the reference it already has; one that is merely named
    /// still belongs to whoever named it, so the aggregate takes its own.
    fn retain_stored(&mut self, raw: IntValue<'ctx>, ty: &Type, from: &Expr) {
        if !self.is_managed(ty) || self.produces_owned(from) {
            return;
        }
        let p = self.builder
            .build_int_to_ptr(raw, self.context.ptr_type(AddressSpace::default()), "stored")
            .unwrap();
        self.gen_retain(p);
    }

    fn record_owned(&mut self, name: &str) {
        if let Some(block) = self.owned.last_mut() {
            block.push(name.to_string());
        }
    }

    /// Release everything the block introduced, innermost first.
    fn close_block(&mut self) {
        let Some(names) = self.owned.pop() else { return };
        self.release_names(&names);
    }

    /// Leaving early: everything every open block introduced goes, but the
    /// blocks stay open because code generation is still inside them.
    fn release_all_open(&mut self) {
        for block in self.owned.clone().iter().rev() {
            self.release_names(block);
        }
    }

    fn release_names(&mut self, names: &[String]) {
        for name in names.iter().rev() {
            if let Some((p, ty)) = self.managed_ptr(name) {
                self.gen_release(p, &ty);
            }
        }
    }

    fn is_managed(&self, ty: &Type) -> bool {
        match ty {
            Type::String | Type::Array(_) => true,
            Type::Named(n) => self.struct_fields.contains_key(n) || self.enum_has_body(n),
            _ => false,
        }
    }

    /// An enum is behind a pointer only when some variant carries a payload;
    /// otherwise a value of it is just the tag.
    fn enum_has_body(&self, name: &str) -> bool {
        self.is_enum(name) && self.variants_of(name).iter().any(|(_, p)| !p.is_empty())
    }

    fn header_of(&self, p: PointerValue<'ctx>) -> PointerValue<'ctx> {
        unsafe {
            self.builder
                .build_in_bounds_gep(self.i8, p, &[self.i64.const_int(Self::HEADER, false).const_neg()], "hdr")
                .unwrap()
        }
    }

    /// One more owner.
    fn gen_retain(&self, p: PointerValue<'ctx>) {
        let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let bump = self.context.append_basic_block(f, "retain");
        let done = self.context.append_basic_block(f, "retain_done");
        let hdr = self.header_of(p);
        let n = self.builder.build_load(self.i64, hdr, "rc").unwrap().into_int_value();
        let immortal = self.builder
            .build_int_compare(inkwell::IntPredicate::EQ, n, self.i64.const_int(Self::IMMORTAL, false), "immortal")
            .unwrap();
        self.builder.build_conditional_branch(immortal, done, bump).unwrap();
        self.builder.position_at_end(bump);
        let inc = self.builder.build_int_add(n, self.i64.const_int(1, false), "rc1").unwrap();
        self.builder.build_store(hdr, inc).unwrap();
        self.builder.build_unconditional_branch(done).unwrap();
        self.builder.position_at_end(done);
    }

    /// A key naming the release function for a type. One function per type,
    /// generated on demand, because what a value owns depends on its shape.
    fn release_key(&self, ty: &Type) -> String {
        match ty {
            Type::String => "str".into(),
            Type::Array(el) => format!("arr.{}", self.release_key(el)),
            Type::Named(n) => format!("t.{}", n),
            other => format!("{:?}", other),
        }
    }

    /// One fewer owner; at zero, let go of what it owns and free it.
    fn release_fn(&mut self, ty: &Type) -> FunctionValue<'ctx> {
        let name = format!("zarrin.release.{}", self.release_key(ty));
        if let Some(f) = self.module.get_function(&name) {
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let func = self.module.add_function(&name, self.context.void_type().fn_type(&[ptr_ty.into()], false), None);
        // Registered before the body is built, so a type that owns its own kind
        // does not send this into a loop.
        let saved = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(func, "entry");
        let drop_bb = self.context.append_basic_block(func, "drop");
        let done = self.context.append_basic_block(func, "done");
        self.builder.position_at_end(entry);

        let p = func.get_nth_param(0).unwrap().into_pointer_value();
        let hdr = self.header_of(p);
        let n = self.builder.build_load(self.i64, hdr, "rc").unwrap().into_int_value();
        // Test before touching the count. Decrementing first corrupted the
        // sentinel on a constant, and the second release then freed it.
        let mortal_bb = self.context.append_basic_block(func, "mortal");
        let immortal = self.builder
            .build_int_compare(inkwell::IntPredicate::EQ, n, self.i64.const_int(Self::IMMORTAL, false), "immortal")
            .unwrap();
        self.builder.build_conditional_branch(immortal, done, mortal_bb).unwrap();

        self.builder.position_at_end(mortal_bb);
        let dec = self.builder.build_int_sub(n, self.i64.const_int(1, false), "rc1").unwrap();
        self.builder.build_store(hdr, dec).unwrap();
        let last = self.builder
            .build_int_compare(inkwell::IntPredicate::SLE, dec, self.i64.const_zero(), "last")
            .unwrap();
        self.builder.build_conditional_branch(last, drop_bb, done).unwrap();

        self.builder.position_at_end(drop_bb);
        self.gen_release_children(p, ty);
        let free_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let free_fn = self.module.get_function("free").unwrap_or_else(|| self.module.add_function("free", free_ty, None));
        self.builder.build_call(free_fn, &[hdr.into()], "").unwrap();
        self.builder.build_unconditional_branch(done).unwrap();

        self.builder.position_at_end(done);
        self.builder.build_return(None).unwrap();
        if let Some(b) = saved {
            self.builder.position_at_end(b);
        }
        func
    }

    /// Release whatever a value of this type holds on to.
    fn gen_release_children(&mut self, p: PointerValue<'ctx>, ty: &Type) {
        match ty {
            Type::String => {}
            Type::Array(el) => {
                if !self.is_managed(el) {
                    return;
                }
                let el = (**el).clone();
                let child = self.release_fn(&el);
                let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let head = self.context.append_basic_block(f, "elems");
                let body = self.context.append_basic_block(f, "elem");
                let out = self.context.append_basic_block(f, "elems_done");
                let len = self.builder.build_load(self.i64, p, "len").unwrap().into_int_value();
                let i = self.entry_alloca(self.i64.into(), "i");
                self.builder.build_store(i, self.i64.const_int(1, false)).unwrap();
                self.builder.build_unconditional_branch(head).unwrap();

                self.builder.position_at_end(head);
                let iv = self.builder.build_load(self.i64, i, "iv").unwrap().into_int_value();
                let limit = self.builder.build_int_add(len, self.i64.const_int(1, false), "limit").unwrap();
                let more = self.builder.build_int_compare(inkwell::IntPredicate::SLT, iv, limit, "more").unwrap();
                self.builder.build_conditional_branch(more, body, out).unwrap();

                self.builder.position_at_end(body);
                let slot = unsafe { self.builder.build_gep(self.i64, p, &[iv], "slot").unwrap() };
                let raw = self.builder.build_load(self.i64, slot, "elem").unwrap().into_int_value();
                let cp = self.builder.build_int_to_ptr(raw, self.context.ptr_type(AddressSpace::default()), "elemp").unwrap();
                self.builder.build_call(child, &[cp.into()], "").unwrap();
                let next = self.builder.build_int_add(iv, self.i64.const_int(1, false), "i1").unwrap();
                self.builder.build_store(i, next).unwrap();
                self.builder.build_unconditional_branch(head).unwrap();
                self.builder.position_at_end(out);
            }
            Type::Named(n) => {
                if let Some(fields) = self.struct_fields.get(n).cloned() {
                    for (idx, (_, fty)) in fields.iter().enumerate() {
                        if !self.is_managed(fty) {
                            continue;
                        }
                        let child = self.release_fn(fty);
                        let slot = unsafe {
                            self.builder.build_gep(self.i64, p, &[self.i64.const_int(idx as u64, false)], "field").unwrap()
                        };
                        let raw = self.builder.build_load(self.i64, slot, "fieldv").unwrap().into_int_value();
                        let cp = self.builder.build_int_to_ptr(raw, self.context.ptr_type(AddressSpace::default()), "fieldp").unwrap();
                        self.builder.build_call(child, &[cp.into()], "").unwrap();
                    }
                } else if self.enum_has_body(n) {
                    // Only the live variant's payload is owned, so switch on the tag.
                    let variants = self.variants_of(n);
                    let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let after = self.context.append_basic_block(f, "variant_done");
                    let tag = self.builder.build_load(self.i64, p, "tag").unwrap().into_int_value();
                    for (vi, (vname, payload)) in variants.iter().enumerate() {
                        if !payload.iter().any(|t| self.is_managed(t)) {
                            continue;
                        }
                        let hit = self.context.append_basic_block(f, &format!("drop_{}", vname));
                        let miss = self.context.append_basic_block(f, &format!("not_{}", vname));
                        let eq = self.builder
                            .build_int_compare(inkwell::IntPredicate::EQ, tag, self.i64.const_int(vi as u64, false), "is_v")
                            .unwrap();
                        self.builder.build_conditional_branch(eq, hit, miss).unwrap();
                        self.builder.position_at_end(hit);
                        for (j, pty) in payload.iter().enumerate() {
                            if !self.is_managed(pty) {
                                continue;
                            }
                            let child = self.release_fn(pty);
                            let slot = unsafe {
                                self.builder.build_gep(self.i64, p, &[self.i64.const_int((j + 1) as u64, false)], "payload").unwrap()
                            };
                            let raw = self.builder.build_load(self.i64, slot, "payloadv").unwrap().into_int_value();
                            let cp = self.builder.build_int_to_ptr(raw, self.context.ptr_type(AddressSpace::default()), "payloadp").unwrap();
                            self.builder.build_call(child, &[cp.into()], "").unwrap();
                        }
                        self.builder.build_unconditional_branch(after).unwrap();
                        self.builder.position_at_end(miss);
                    }
                    self.builder.build_unconditional_branch(after).unwrap();
                    self.builder.position_at_end(after);
                }
            }
            _ => {}
        }
    }

    fn gen_release(&mut self, p: PointerValue<'ctx>, ty: &Type) {
        // Inside a consumed expression every allocation is on the frame and has
        // no header to read; the stack mark reclaims it instead.
        if self.transient {
            return;
        }
        let f = self.release_fn(ty);
        self.builder.build_call(f, &[p.into()], "").unwrap();
    }

    fn heap_bytes(&self, bytes: IntValue<'ctx>, name: &str) -> PointerValue<'ctx> {
        let total = self.builder
            .build_int_add(bytes, self.i64.const_int(Self::HEADER, false), "with_header")
            .unwrap();
        let base = self.builder
            .build_call(self.malloc_fn(), &[total.into()], name)
            .unwrap()
            .try_as_basic_value()
            .left()
            .unwrap()
            .into_pointer_value();
        self.builder.build_store(base, self.i64.const_int(1, false)).unwrap();
        unsafe {
            self.builder
                .build_in_bounds_gep(self.i8, base, &[self.i64.const_int(Self::HEADER, false)], name)
                .unwrap()
        }
    }

    /// Allocate `slots` 64-bit words on the heap.
    ///
    /// Structs and enum payloads are passed around as raw addresses, so they
    /// have to outlive the frame that built them. They used to be `alloca`d:
    /// a function returning one handed back a pointer into its own dead stack
    /// frame, and the caller read whatever overwrote it. Nothing is freed —
    /// the backend has no ownership model yet, and leaking beats corrupting.
    fn heap_slots(&self, slots: u64, name: &str) -> PointerValue<'ctx> {
        // Through heap_bytes, so this gets a header like every other heap
        // value; releasing one allocated without a header reads rubbish.
        self.heap_bytes(self.i64.const_int(slots * 8, false), name)
    }

    /// Variants of `enum_name`, including the predeclared `Option`/`Result`,
    /// which are not part of the program's own declarations.
    fn variants_of(&self, enum_name: &str) -> Vec<(String, Vec<Type>)> {
        if let Some(v) = self.enum_variants.get(enum_name) {
            return v.clone();
        }
        builtin_enums()
            .into_iter()
            .find(|(n, _)| n == enum_name)
            .map(|(_, v)| v)
            .unwrap_or_default()
    }

    fn is_enum(&self, name: &str) -> bool {
        self.enum_variants.contains_key(name) || builtin_enums().iter().any(|(n, _)| n == name)
    }

    /// The declared or inferred type of an expression.
    fn type_of(&mut self, e: &Expr) -> Option<Type> {
        TypeChecker::type_of(e, &mut self.types)
    }

    fn named_of_kind(&mut self, e: &Expr, is_kind: impl Fn(&Self, &str) -> bool) -> Option<String> {
        match self.type_of(e) {
            Some(Type::Named(n)) if is_kind(self, &n) => Some(n),
            _ => None,
        }
    }

    /// Read a value as a double, whatever shape it arrived in.
    fn as_float(&self, v: &CgValue<'ctx>) -> FloatValue<'ctx> {
        match v {
            CgValue::Float(f) => *f,
            other => self
                .builder
                .build_bit_cast(self.value_to_int(other), self.f64, "i2f")
                .unwrap()
                .into_float_value(),
        }
    }

    /// Put a value into a local's slot, in the representation its type calls
    /// for: an f64 for a float, a pointer for a string, an i64 otherwise.
    fn store_into(&self, ptr: PointerValue<'ctx>, ty: &Type, v: &CgValue<'ctx>) {
        let stored: BasicValueEnum = match ty {
            Type::Float => self.as_float(v).into(),
            Type::String => match v {
                CgValue::Str(p) => (*p).into(),
                other => self
                    .builder
                    .build_int_to_ptr(self.value_to_int(other), self.context.ptr_type(AddressSpace::default()), "s2p")
                    .unwrap()
                    .into(),
            },
            _ => self.value_to_int(v).into(),
        };
        self.builder.build_store(ptr, stored).unwrap();
    }

    /// Declare a local of type `ty` and give it `v`.
    fn bind_local(&mut self, name: &str, ty: &Type, v: &CgValue<'ctx>) {
        let slot_ty: inkwell::types::BasicTypeEnum = match ty {
            Type::Float => self.f64.into(),
            Type::String => self.context.ptr_type(AddressSpace::default()).into(),
            _ => self.i64.into(),
        };
        // In the entry block: a `let` inside a loop runs every iteration, and a
        // slot allocated there would grow the stack instead of being reused.
        let ptr = self.entry_alloca(slot_ty, name);
        self.store_into(ptr, ty, v);
        self.named.insert(name.to_string(), (ptr, ty.clone()));
        self.types.define(name, ty.clone());
    }

    fn load_local(&self, name: &str) -> Option<CgValue<'ctx>> {
        let (ptr, ty) = self.named.get(name)?;
        Some(match ty {
            Type::Float => CgValue::Float(self.builder.build_load(self.f64, *ptr, name).unwrap().into_float_value()),
            Type::String => CgValue::Str(
                self.builder
                    .build_load(self.context.ptr_type(AddressSpace::default()), *ptr, name)
                    .unwrap()
                    .into_pointer_value(),
            ),
            _ => CgValue::Int(self.builder.build_load(self.i64, *ptr, name).unwrap().into_int_value()),
        })
    }

    fn is_bool_expr(&mut self, e: &Expr) -> bool {
        self.type_of(e) == Some(Type::Bool)
    }

    /// Which struct, if any, this expression produces.
    fn infer_struct_type(&mut self, e: &Expr) -> Option<String> {
        self.named_of_kind(e, |cg, n| cg.struct_fields.contains_key(n))
    }

    /// Which enum, if any, this expression produces.
    fn infer_enum_type(&mut self, e: &Expr) -> Option<String> {
        self.named_of_kind(e, |cg, n| cg.is_enum(n))
    }


    /// Render an enum value the way the interpreter does: the variant name, and
    /// for a variant with a payload, its fields in parentheses.
    fn gen_enum_to_str(&self, val: IntValue<'ctx>, enum_name: &str, depth: u32) -> PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let snprintf = self.module.get_function("snprintf").unwrap_or_else(|| {
            let ty = i32_ty.fn_type(&[ptr_ty.into(), self.i64.into(), ptr_ty.into()], true);
            self.module.add_function("snprintf", ty, None)
        });
        let variants = self.variants_of(enum_name);
        let has_payload = variants.iter().any(|(_, p)| !p.is_empty());
        let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let done = self.context.append_basic_block(f, "enum_str_done");
        let slot = self.entry_alloca(ptr_ty.into(), "enum_str");

        // A payload-free enum is just its tag; otherwise the tag is the first word.
        let (tag, base) = if has_payload {
            let p = self.builder.build_int_to_ptr(val, ptr_ty, "enum_p").unwrap();
            let t = self.builder.build_load(self.i64, p, "enum_tag").unwrap().into_int_value();
            (t, Some(p))
        } else {
            (val, None)
        };

        for (i, (vname, ptypes)) in variants.iter().enumerate() {
            let case = self.context.append_basic_block(f, &format!("enum_is_{}", vname));
            let next = self.context.append_basic_block(f, &format!("enum_not_{}", vname));
            let cmp = self.builder.build_int_compare(inkwell::IntPredicate::EQ, tag, self.i64.const_int(i as u64, false), "is_variant").unwrap();
            self.builder.build_conditional_branch(cmp, case, next).unwrap();

            self.builder.position_at_end(case);
            if ptypes.is_empty() || base.is_none() {
                let text = self.string_global(vname);
                self.builder.build_store(slot, text).unwrap();
            } else {
                let mut parts: Vec<PointerValue<'ctx>> = Vec::new();
                for (j, ty) in ptypes.iter().enumerate() {
                    let fp = unsafe {
                        self.builder.build_gep(self.i64, base.unwrap(), &[self.i64.const_int((j + 1) as u64, false)], "payload").unwrap()
                    };
                    let raw = self.builder.build_load(self.i64, fp, "payload_v").unwrap().into_int_value();
                    parts.push(match ty {
                        Type::Float => {
                            // Several payload strings are live at once while the
                            // variant text is assembled, so each needs its own.
                            let fv = self.builder.build_bit_cast(raw, self.f64, "i2f").unwrap().into_float_value();
                            self.gen_float_to_owned(fv)
                        }
                        Type::String => self.builder.build_int_to_ptr(raw, ptr_ty, "payload_s").unwrap(),
                        Type::Named(n) if self.is_enum(n) && depth < 3 => {
                            self.gen_enum_to_str(raw, n, depth + 1)
                        }
                        _ => self.gen_int_to_str(raw),
                    });
                }
                let holes = vec!["%s"; parts.len()].join(", ");
                let fmt = self.builder.build_global_string_ptr(&format!("{}({})", vname, holes), "variant_fmt").unwrap().as_pointer_value();
                let mut probe: Vec<BasicMetadataValueEnum> =
                    vec![ptr_ty.const_null().into(), self.i64.const_zero().into(), fmt.into()];
                for p in &parts { probe.push((*p).into()); }
                let need = self.builder.build_call(snprintf, &probe, "need").unwrap()
                    .try_as_basic_value().left().unwrap().into_int_value();
                let need64 = self.builder.build_int_s_extend(need, self.i64, "need64").unwrap();
                let cap = self.builder.build_int_add(need64, self.i64.const_int(1, false), "cap").unwrap();
                let buf = self.scratch_bytes(cap, "variant_buf");
                let mut write: Vec<BasicMetadataValueEnum> = vec![buf.into(), cap.into(), fmt.into()];
                for p in &parts { write.push((*p).into()); }
                self.builder.build_call(snprintf, &write, "").unwrap();
                self.builder.build_store(slot, buf).unwrap();
            }
            self.builder.build_unconditional_branch(done).unwrap();
            self.builder.position_at_end(next);
        }

        // Tag outside the declared range: fall back to the number.
        let fallback = self.gen_int_to_str(tag);
        self.builder.build_store(slot, fallback).unwrap();
        self.builder.build_unconditional_branch(done).unwrap();

        self.builder.position_at_end(done);
        self.builder.build_load(ptr_ty, slot, "enum_text").unwrap().into_pointer_value()
    }

    /// Resolve a variant name to its enum. An ambiguous name is a hard error
    /// rather than whichever enum the hash map happened to yield first.
    fn resolve_variant(&self, name: &str) -> Option<Variant> {
        match self.variants.lookup(name) {
            Lookup::Unique(v) => Some(v),
            Lookup::Unknown => None,
            Lookup::Ambiguous(c) => panic!("variant `{}` is declared by {}; rename one of them to disambiguate", name, c.join(" and ")),
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

    /// Every heap value carries a reference count in the 16 bytes before it,
    /// so a pointer can be retained and released without knowing where it came
    /// from. Constants get the same shape with a count that never moves.
    const HEADER: u64 = 16;
    const IMMORTAL: u64 = u64::MAX;

    /// A string constant, laid out like a heap value so it can be held in a
    /// variable and released like one — the release is a no-op on it.
    fn string_global(&self, s: &str) -> PointerValue<'ctx> {
        let c = format!("{}\0", s);
        let bytes = self.context.const_string(c.as_bytes(), false);
        let pad = self.i8.array_type((Self::HEADER - 8) as u32).const_zero();
        let ty = self.context.struct_type(
            &[self.i64.into(), pad.get_type().into(), bytes.get_type().into()],
            false,
        );
        let g = self.module.add_global(ty, None, "strlit");
        g.set_initializer(&self.context.const_struct(
            &[self.i64.const_int(Self::IMMORTAL, false).into(), pad.into(), bytes.into()],
            false,
        ));
        unsafe {
            self.builder
                .build_in_bounds_gep(
                    self.i8,
                    g.as_pointer_value(),
                    &[self.i64.const_int(Self::HEADER, false)],
                    "strlit_body",
                )
                .unwrap()
        }
    }


    fn gen_expr(&mut self, e: &Expr) -> CgValue<'ctx> {
        match &*e.kind {
            ExprKind::Int(n) => CgValue::Int(self.i64.const_int(*n as u64, false)),
            ExprKind::Float(f) => CgValue::Float(self.f64.const_float(*f)),
            ExprKind::Bool(b) => {
                let bool_val = self.i1.const_int(*b as u64, false);
                CgValue::Int(self.builder.build_int_z_extend(bool_val, self.i64, "bool_ext").unwrap())
            }
            ExprKind::Str(s) => CgValue::Str(self.string_global(s)),
            ExprKind::Ident(name) => {
                if let Some(v) = self.load_local(name) {
                    v
                } else if let Some(variant) = self.resolve_variant(name) {
                    let tag = variant.tag;
                    if variant.enum_has_payload {
                        let array_ty = self.i64.array_type(1);
                        let alloca = self.scratch_slots(1, &format!("enum_{}", name));
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
            ExprKind::Binary(l, op, r) => {
                let lv = self.gen_expr(l);
                let rv = self.gen_expr(r);
                if matches!(op, BinOp::Add) {
                    // Joining reads both sides and produces something new, so an
                    // operand built on the spot has no owner afterwards. Without
                    // this the intermediate in `"v" + to_string(i)` was left.
                    let (drop_l, drop_r) = (self.produces_owned(l), self.produces_owned(r));
                    let mut spent: Vec<PointerValue<'ctx>> = Vec::new();
                    let joined = match (&lv, &rv) {
                        (CgValue::Str(a), CgValue::Str(b)) => {
                            if drop_l { spent.push(*a); }
                            if drop_r { spent.push(*b); }
                            Some(self.gen_string_concat(*a, *b))
                        }
                        (CgValue::Str(a), CgValue::Int(b)) => {
                            let b_str = self.gen_int_to_str(*b);
                            if drop_l { spent.push(*a); }
                            spent.push(b_str);
                            Some(self.gen_string_concat(*a, b_str))
                        }
                        (CgValue::Int(a), CgValue::Str(b)) => {
                            let a_str = self.gen_int_to_str(*a);
                            spent.push(a_str);
                            if drop_r { spent.push(*b); }
                            Some(self.gen_string_concat(a_str, *b))
                        }
                        _ => None,
                    };
                    if let Some(v) = joined {
                        for p in spent {
                            self.gen_release(p, &Type::String);
                        }
                        return v;
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
            ExprKind::Unary(op, e) => {
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
            ExprKind::Call(callee, args) => {
                let name = match &*callee.kind {
                    ExprKind::Ident(n) => n,
                    _ => panic!("cannot call non-function"),
                };
                if name == "print" {
                    let arg = args[0].clone();
                    return self.consumed(|cg| cg.gen_print_arg(&arg));
                }
                if name == "print_unreachable" {
                    if self.is_bool_expr(&args[0]) {
                        let v = self.gen_expr(&args[0]);
                        let iv = self.value_to_int(&v);
                        let text = self.gen_bool_to_str(iv);
                        let fmt = self.string_global("%s\n");
                        self.builder.build_call(self.printf, &[fmt.into(), text.into()], "call_printf").unwrap();
                        return CgValue::Int(self.i64.const_int(0, false));
                    }
                    if let Some(en) = self.infer_enum_type(&args[0]) {
                        let v = self.gen_expr(&args[0]);
                        let iv = self.value_to_int(&v);
                        let text = self.gen_enum_to_str(iv, &en, 0);
                        let fmt = self.string_global("%s\n");
                        self.builder.build_call(self.printf, &[fmt.into(), text.into()], "call_printf").unwrap();
                        return CgValue::Int(self.i64.const_int(0, false));
                    }
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
                    if self.is_bool_expr(&args[0]) {
                        let v = self.gen_expr(&args[0]);
                        let iv = self.value_to_int(&v);
                        return CgValue::Str(self.gen_bool_to_str(iv));
                    }
                    if let Some(en) = self.infer_enum_type(&args[0]) {
                        let v = self.gen_expr(&args[0]);
                        let iv = self.value_to_int(&v);
                        return CgValue::Str(self.gen_enum_to_str(iv, &en, 0));
                    }
                    let val = self.gen_expr(&args[0]);
                    match &val {
                        CgValue::Int(i) => {
                            let str_ptr = self.gen_int_to_str(*i);
                            return CgValue::Str(str_ptr);
                        }
                        CgValue::Str(s) => return val,
                        CgValue::Float(f) => {
                            let str_ptr = self.gen_float_to_owned(*f);
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
                    self.builder.build_call(self.exit_fn(), &[self.context.i32_type().const_int(1, false).into()], "panic_exit").unwrap();
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
                    let get_len = self.builder.build_load(self.i64, arr_ptr, "arr_len").unwrap().into_int_value();
                    self.gen_bounds_check(idx_int, get_len, "array");
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
                    self.gen_bounds_check(idx_int, old_len, "array");
                    // Layout is [len, elem0, ..]; `slots` counts the header too.
                    let slots = self.builder.build_int_add(old_len, self.i64.const_int(1, false), "slots").unwrap();
                    // This asked malloc for `slots` *bytes* and then memcpy'd
                    // `slots * 8` of them, overrunning the allocation eightfold.
                    let byte_count = self.builder.build_int_mul(slots, self.i64.const_int(8, false), "bytes").unwrap();
                    let buf = self.scratch_bytes(byte_count, "new_arr");
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
                    // The interpreter bounds-checks against len + 1, so an index
                    // one past the last byte is a valid end.
                    let strlen_ty = self.i64.fn_type(&[self.context.ptr_type(AddressSpace::default()).into()], false);
                    let strlen_f = self.module.get_function("strlen").unwrap_or_else(|| self.module.add_function("strlen", strlen_ty, None));
                    let s_len = self.builder.build_call(strlen_f, &[s_ptr.into()], "sub_srclen").unwrap()
                        .try_as_basic_value().left().unwrap().into_int_value();
                    let limit = self.builder.build_int_add(s_len, self.i64.const_int(1, false), "sub_limit").unwrap();
                    self.gen_bounds_check(start, limit, "substring start");
                    self.gen_bounds_check(end, limit, "substring end");
                    {
                        let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                        let bad = self.context.append_basic_block(f, "sub_inverted");
                        let good = self.context.append_basic_block(f, "sub_ordered");
                        let inverted = self.builder.build_int_compare(inkwell::IntPredicate::SGT, start, end, "inverted").unwrap();
                        self.builder.build_conditional_branch(inverted, bad, good).unwrap();
                        self.builder.position_at_end(bad);
                        self.gen_abort("substring start \x01 is past end \x01", &[start, end]);
                        self.builder.position_at_end(good);
                    }
                    let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
                    let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
                    let memcpy_type = self.context.void_type().fn_type(&[
                        self.context.ptr_type(AddressSpace::default()).into(),
                        self.context.ptr_type(AddressSpace::default()).into(),
                        self.i64.into(),
                    ], false);
                    let memcpy_fn = self.module.get_function("memcpy").unwrap_or_else(|| self.module.add_function("memcpy", memcpy_type, None));
                    let sub_len = self.builder.build_int_sub(end, start, "sub_len").unwrap();
                    let buf = self.scratch_bytes(self.builder.build_int_add(sub_len, self.i64.const_int(1, false), "sub_alloc").unwrap(), "sub_buf");
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
                    let buf = self.scratch_bytes(self.builder.build_int_add(len_after, self.i64.const_int(1, false), "trim_alloc").unwrap(), "trim_buf");
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
                        self.scratch_bytes(self.i64.const_int(2, false), "char_buf")
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
                if name == "split" {
                    let s_val = self.gen_expr(&args[0]);
                    let delim_val = self.gen_expr(&args[1]);
                    let s_ptr = match s_val { CgValue::Str(p) => p, _ => panic!("split expects string") };
                    let delim_ptr = match delim_val { CgValue::Str(p) => p, _ => panic!("split expects string") };
                    let byte_ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let malloc_type = byte_ptr_ty.fn_type(&[self.i64.into()], false);
                    let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
                    let strstr_type = byte_ptr_ty.fn_type(&[byte_ptr_ty.into(), byte_ptr_ty.into()], false);
                    let strstr_fn = self.module.get_function("strstr").unwrap_or_else(|| self.module.add_function("strstr", strstr_type, None));
                    let strlen_type = self.i64.fn_type(&[byte_ptr_ty.into()], false);
                    let strlen_fn = self.module.get_function("strlen").unwrap_or_else(|| self.module.add_function("strlen", strlen_type, None));
                    let memcpy_type = self.context.void_type().fn_type(&[byte_ptr_ty.into(), byte_ptr_ty.into(), self.i64.into()], false);
                    let memcpy_fn = self.module.get_function("memcpy").unwrap_or_else(|| self.module.add_function("memcpy", memcpy_type, None));
                    let fn_val = self.builder.get_insert_block().unwrap().get_parent().unwrap();

                    let delim_len_val = self.builder.build_call(strlen_fn, &[delim_ptr.into()], "delim_len").unwrap().try_as_basic_value().left().unwrap().into_int_value();

                    let count_ptr = self.entry_alloca(self.i64.into(), "split_count");
                    self.builder.build_store(count_ptr, self.i64.const_int(1, false)).unwrap();

                    let cur_ptr_ptr = self.entry_alloca(byte_ptr_ty.into(), "split_cur");
                    self.builder.build_store(cur_ptr_ptr, s_ptr).unwrap();

                    let loop_bb = self.context.append_basic_block(fn_val, "split_loop");
                    let body_bb = self.context.append_basic_block(fn_val, "split_body");
                    let after_bb = self.context.append_basic_block(fn_val, "split_after");

                    self.builder.build_unconditional_branch(loop_bb).unwrap();
                    self.builder.position_at_end(loop_bb);
                    let cur_ptr = self.builder.build_load(byte_ptr_ty, cur_ptr_ptr, "cur_ptr").unwrap().into_pointer_value();
                    let found = self.builder.build_call(strstr_fn, &[cur_ptr.into(), delim_ptr.into()], "found_ptr").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();
                    let is_null = self.builder.build_is_null(found, "is_null").unwrap();
                    self.builder.build_conditional_branch(is_null, after_bb, body_bb).unwrap();

                    self.builder.position_at_end(body_bb);
                    let cnt = self.builder.build_load(self.i64, count_ptr, "cnt").unwrap().into_int_value();
                    let new_cnt = self.builder.build_int_add(cnt, self.i64.const_int(1, false), "new_cnt").unwrap();
                    self.builder.build_store(count_ptr, new_cnt).unwrap();
                    let next_pos = unsafe {
                        self.builder.build_gep(self.i8, found, &[delim_len_val], "next_pos").unwrap()
                    };
                    self.builder.build_store(cur_ptr_ptr, next_pos).unwrap();
                    self.builder.build_unconditional_branch(loop_bb).unwrap();

                    self.builder.position_at_end(after_bb);
                    let total_count = self.builder.build_load(self.i64, count_ptr, "total_count").unwrap().into_int_value();

                    let arr_alloc_size = self.builder.build_int_mul(self.builder.build_int_add(total_count, self.i64.const_int(1, false), "arr_size_tmp").unwrap(), self.i64.const_int(8, false), "arr_bytes").unwrap();
                    let arr_buf = self.scratch_bytes(arr_alloc_size, "arr_buf");
                    self.builder.build_store(arr_buf, total_count).unwrap();

                    let idx_ptr = self.entry_alloca(self.i64.into(), "split_idx");
                    self.builder.build_store(idx_ptr, self.i64.const_int(0, false)).unwrap();

                    let extract_loop = self.context.append_basic_block(fn_val, "split_extract_loop");
                    let extract_body = self.context.append_basic_block(fn_val, "split_extract_body");
                    let extract_done = self.context.append_basic_block(fn_val, "split_extract_done");

                    let s_cur2 = self.entry_alloca(byte_ptr_ty.into(), "split_s_cur2");
                    self.builder.build_store(s_cur2, s_ptr).unwrap();

                    self.builder.build_unconditional_branch(extract_loop).unwrap();
                    self.builder.position_at_end(extract_loop);
                    let eidx = self.builder.build_load(self.i64, idx_ptr, "eidx").unwrap().into_int_value();
                    let ecmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, eidx, total_count, "ecmp").unwrap();
                    self.builder.build_conditional_branch(ecmp, extract_body, extract_done).unwrap();

                    self.builder.position_at_end(extract_body);
                    let ecur = self.builder.build_load(byte_ptr_ty, s_cur2, "ecur").unwrap().into_pointer_value();
                    let efound = self.builder.build_call(strstr_fn, &[ecur.into(), delim_ptr.into()], "efound").unwrap().try_as_basic_value().left().unwrap().into_pointer_value();
                    let efound_is_null = self.builder.build_is_null(efound, "efound_is_null").unwrap();

                    let part_start_bb = self.context.append_basic_block(fn_val, "split_part_start");
                    let part_null_bb = self.context.append_basic_block(fn_val, "split_part_null");
                    self.builder.build_conditional_branch(efound_is_null, part_null_bb, part_start_bb).unwrap();

                    self.builder.position_at_end(part_start_bb);
                    let part_len = self.builder.build_ptr_diff(self.i8, efound, ecur, "part_len").unwrap();
                    let part_buf = self.scratch_bytes(self.builder.build_int_add(part_len, self.i64.const_int(1, false), "part_alloc").unwrap(), "part_buf");
                    self.builder.build_call(memcpy_fn, &[part_buf.into(), ecur.into(), part_len.into()], "part_cp").unwrap();
                    let part_null_ptr = unsafe {
                        self.builder.build_gep(self.i8, part_buf, &[part_len], "part_null").unwrap()
                    };
                    self.builder.build_store(part_null_ptr, self.i8.const_int(0, false)).unwrap();
                    let part_ptr_int = self.builder.build_ptr_to_int(part_buf, self.i64, "part_ptr_int").unwrap();
                    let arr_slot_idx = self.builder.build_int_add(eidx, self.i64.const_int(1, false), "arr_slot_idx").unwrap();
                    let arr_slot = unsafe {
                        self.builder.build_gep(self.i64, arr_buf, &[arr_slot_idx], "arr_slot").unwrap()
                    };
                    self.builder.build_store(arr_slot, part_ptr_int).unwrap();
                    let next_ecur = unsafe {
                        self.builder.build_gep(self.i8, efound, &[delim_len_val], "next_ecur").unwrap()
                    };
                    self.builder.build_store(s_cur2, next_ecur).unwrap();

                    let part_done_bb = self.context.append_basic_block(fn_val, "split_part_done");
                    self.builder.build_unconditional_branch(part_done_bb).unwrap();

                    self.builder.position_at_end(part_null_bb);
                    let ecur_len = self.builder.build_call(strlen_fn, &[ecur.into()], "ecur_len").unwrap().try_as_basic_value().left().unwrap().into_int_value();
                    let last_buf = self.scratch_bytes(self.builder.build_int_add(ecur_len, self.i64.const_int(1, false), "last_alloc").unwrap(), "last_buf");
                    self.builder.build_call(memcpy_fn, &[last_buf.into(), ecur.into(), ecur_len.into()], "last_cp").unwrap();
                    let last_null_ptr = unsafe {
                        self.builder.build_gep(self.i8, last_buf, &[ecur_len], "last_null").unwrap()
                    };
                    self.builder.build_store(last_null_ptr, self.i8.const_int(0, false)).unwrap();
                    let last_ptr_int = self.builder.build_ptr_to_int(last_buf, self.i64, "last_ptr_int").unwrap();
                    let last_arr_slot_idx = self.builder.build_int_add(eidx, self.i64.const_int(1, false), "last_arr_slot_idx").unwrap();
                    let last_arr_slot = unsafe {
                        self.builder.build_gep(self.i64, arr_buf, &[last_arr_slot_idx], "last_arr_slot").unwrap()
                    };
                    self.builder.build_store(last_arr_slot, last_ptr_int).unwrap();
                    self.builder.build_unconditional_branch(part_done_bb).unwrap();

                    self.builder.position_at_end(part_done_bb);
                    let next_eidx = self.builder.build_int_add(eidx, self.i64.const_int(1, false), "next_eidx").unwrap();
                    self.builder.build_store(idx_ptr, next_eidx).unwrap();
                    self.builder.build_unconditional_branch(extract_loop).unwrap();

                    self.builder.position_at_end(extract_done);
                    return CgValue::Int(self.builder.build_ptr_to_int(arr_buf, self.i64, "arr_ret").unwrap());
                }
                if let Some(variant) = self.resolve_variant(name) {
                    let tag = variant.tag;
                    let payload_types = &variant.payload;
                    if !variant.enum_has_payload {
                        return CgValue::Int(self.i64.const_int(tag as u64, false));
                    }
                    let num_fields = (payload_types.len() + 1) as u32;
                    let array_ty = self.i64.array_type(num_fields);
                    let alloca = self.scratch_slots(num_fields as u64, &format!("enum_{}", name));
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
                        if let Some(pty) = payload_types.get(j).cloned() {
                            self.retain_stored(av_int, &pty, arg);
                        }
                    }
                    return CgValue::Int(self.builder.build_ptr_to_int(base_ptr, self.i64, "enum_ptr").unwrap());
                }
                let func = self.functions.get(name).copied()
                    .unwrap_or_else(|| panic!("undefined function: {}", name));
                let mut meta_args: Vec<BasicMetadataValueEnum> = Vec::new();
                for a in args {
                    // Every parameter is an i64, so pack the argument the same
                    // way the callee unpacks it. Passing a float or a string
                    // through unchanged was rejected outright.
                    let a_val = self.gen_expr(a);
                    meta_args.push(self.value_to_int(&a_val).into());
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
            ExprKind::StructLit { name, fields } => {
                let field_defs = self.struct_fields.get(name).cloned()
                    .unwrap_or_else(|| panic!("unknown struct: {}", name));
                let field_defs: Vec<String> = field_defs.into_iter().map(|(n, _)| n).collect();
                let num_fields = field_defs.len() as u64;
                let array_ty = self.i64.array_type(num_fields as u32);
                let alloca = self.scratch_slots(num_fields, &format!("{}_struct", name));
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
                    if let Some(fty) = self.field_type(name, fname) {
                        self.retain_stored(fv_int, &fty, fexpr);
                    }
                }
                CgValue::Int(self.builder.build_ptr_to_int(base_ptr, self.i64, "struct_ptr").unwrap())
            }
            ExprKind::FieldAccess(obj, field) => {
                let obj_val = self.gen_expr(obj);
                let obj_ptr_val = match obj_val {
                    CgValue::Int(v) => v,
                    _ => panic!("field access on non-struct"),
                };
                let obj_ptr = self.builder.build_int_to_ptr(obj_ptr_val, self.context.ptr_type(AddressSpace::default()), "obj_ptr").unwrap();
                let struct_name = self.infer_struct_type(obj)
                    .unwrap_or_else(|| panic!("cannot determine the struct type of `.{}`", field));
                let field_defs = self.struct_fields.get(&struct_name)
                    .unwrap_or_else(|| panic!("unknown struct type for field access: {}", struct_name));
                let field_idx = field_defs.iter().position(|(n, _)| n == field)
                    .unwrap_or_else(|| panic!("field `{}` not found in struct `{}`", field, struct_name));
                let field_ty = field_defs[field_idx].1.clone();
                let num_fields = field_defs.len() as u64;
                let array_ty = self.i64.array_type(num_fields as u32);
                let field_ptr = unsafe {
                    self.builder.build_gep(array_ty, obj_ptr, &[self.i64.const_int(0, false), self.i64.const_int(field_idx as u64, false)], &format!("{}_{}", struct_name, field)).unwrap()
                };
                let raw = self.builder.build_load(self.i64, field_ptr, field).unwrap().into_int_value();
                // Fields are stored as raw words; the declared type says how to
                // read one back. Without this a float field printed its bit
                // pattern and a string field its address.
                self.typed_value(raw, &field_ty)
            }
            ExprKind::MethodCall(obj, method, args) => {
                let obj_val = self.gen_expr(obj);
                if method == "unwrap" || method == "is_some" || method == "is_none" || method == "is_ok" || method == "is_err" {
                    let obj_int = self.value_to_int(&obj_val);
                    let sv_ptr = self.builder.build_int_to_ptr(obj_int, self.context.ptr_type(AddressSpace::default()), "enum_ptr").unwrap();
                    let tag_ptr = unsafe {
                        self.builder.build_gep(self.i64, sv_ptr, &[self.i64.const_int(0, false)], "tag_ptr").unwrap()
                    };
                    let tag = self.builder.build_load(self.i64, tag_ptr, "tag").unwrap().into_int_value();
                    let is_variant0 = self.builder.build_int_compare(inkwell::IntPredicate::EQ, tag, self.i64.const_int(0, false), "is_v0").unwrap();
                    match method.as_str() {
                        "unwrap" => {
                            let data_ptr = unsafe {
                                self.builder.build_gep(self.i64, sv_ptr, &[self.i64.const_int(1, false)], "data_ptr").unwrap()
                            };
                            let data = self.builder.build_load(self.i64, data_ptr, "data").unwrap().into_int_value();
                            return CgValue::Int(data);
                        }
                        "is_some" | "is_ok" => {
                            let zext = self.builder.build_int_z_extend(is_variant0, self.i64, "result").unwrap();
                            return CgValue::Int(zext);
                        }
                        "is_none" | "is_err" => {
                            let not_v0 = self.builder.build_not(is_variant0, "not_v0").unwrap();
                            let zext = self.builder.build_int_z_extend(not_v0, self.i64, "result").unwrap();
                            return CgValue::Int(zext);
                        }
                        _ => {}
                    }
                }
                let mut meta_args: Vec<BasicMetadataValueEnum> = Vec::new();
                match &obj_val {
                    CgValue::Int(v) => meta_args.push((*v).into()),
                    _ => panic!("method call on non-struct"),
                }
                for a in args {
                    // Every parameter is an i64, so pack the argument the same
                    // way the callee unpacks it. Passing a float or a string
                    // through unchanged was rejected outright.
                    let a_val = self.gen_expr(a);
                    meta_args.push(self.value_to_int(&a_val).into());
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
ExprKind::Match { scrutinee, arms } => {
                let sv = self.gen_expr(scrutinee);
                let parent = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let merge = self.context.append_basic_block(parent, "match_merge");
                let sv_int_raw = self.value_to_int(&sv);
                // Whether the scrutinee is a pointer to [tag, payload..] or a
                // bare tag depends on *its* enum. This was a single flag over
                // every enum in the program, so declaring one payload variant
                // anywhere made `match` on an integer dereference it.
                let has_data_variants = match self.infer_enum_type(scrutinee) {
                    Some(en) => self.variants_of(&en).iter().any(|(_, pt)| !pt.is_empty()),
                    None => match &*scrutinee.kind {
                        // A local or parameter that is not a known enum, or a
                        // plain arithmetic value, is its own tag.
                        ExprKind::Ident(n) if self.named.contains_key(n) => false,
                        ExprKind::Int(_) | ExprKind::Binary(..) => false,
                        _ => self.enum_variants.values().flat_map(|v| v.iter()).any(|(_, pt)| !pt.is_empty()),
                    },
                };
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
                let mut arm_shape: Option<CgValue<'ctx>> = None;
                let mut current_check = self.context.append_basic_block(parent, "match_check");
                self.builder.build_unconditional_branch(current_check).unwrap();
                for (i, (patterns, guard, body)) in arms.iter().enumerate() {
                    self.builder.position_at_end(current_check);
                    let arm_bb = self.context.append_basic_block(parent, &format!("arm_{}", i));
                    let is_wildcard = patterns.iter().any(|p| matches!(p, Pattern::Wildcard | Pattern::Variable(_)));
                    // Only a wildcard or a plain binding always matches, and
                    // only when no guard can reject it. Being the last arm does
                    // not make a pattern irrefutable: treating it that way ran
                    // the final body for values nothing matched, so
                    // `match n { 0 => 10, 1 => 20 }` answered 20 for 99.
                    let irrefutable = is_wildcard && guard.is_none();
                    // One block for every way this arm can fail. The pattern test
                    // and the guard each used to append their own, leaving one of
                    // them branched to but never terminated.
                    let next_check = if irrefutable {
                        None
                    } else {
                        Some(self.context.append_basic_block(parent, &format!("match_check_{}", i + 1)))
                    };

                    let mut any_matched_bb = arm_bb;
                    let mut check_bb = current_check;

                    for (pi, pattern) in patterns.iter().enumerate() {
                        self.builder.position_at_end(check_bb);
                        match pattern {
                            Pattern::Literal(e) => {
                                let pv = self.gen_expr(e);
                                let pv_int = self.value_to_int(&pv);
                                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::EQ, sv_tag, pv_int, "match_cmp").unwrap();
                                if pi < patterns.len() - 1 {
                                    let next_pattern = self.context.append_basic_block(parent, &format!("arm_{}_pat_{}", i, pi + 1));
                                    self.builder.build_conditional_branch(cmp, arm_bb, next_pattern).unwrap();
                                    check_bb = next_pattern;
                                } else {
                                    match next_check {
                                        None => { self.builder.build_unconditional_branch(arm_bb).unwrap(); }
                                        Some(nc) => { self.builder.build_conditional_branch(cmp, arm_bb, nc).unwrap(); }
                                    }
                                }
                            }
                            Pattern::EnumVariant { name, inner } => {
                                // Scanning `enum_variants` missed the predeclared
                                // Option/Result entirely, so `Some(v)` bound nothing
                                // and every variant compared against tag 0; and it
                                // never matched a qualified name like `Color::Red`,
                                // which is what the pattern parser produces.
                                let (tag_val, payload_types) = match self.resolve_variant(name) {
                                    Some(v) => (v.tag as u64, v.payload),
                                    None => (0, Vec::new()),
                                };
                                let pv_int = self.i64.const_int(tag_val, false);
                                let cmp = self.builder.build_int_compare(inkwell::IntPredicate::EQ, sv_tag, pv_int, "match_cmp").unwrap();
                                if pi < patterns.len() - 1 {
                                    let next_pattern = self.context.append_basic_block(parent, &format!("arm_{}_pat_{}", i, pi + 1));
                                    self.builder.build_conditional_branch(cmp, arm_bb, next_pattern).unwrap();
                                    check_bb = next_pattern;
                                } else {
                                    match next_check {
                                        None => { self.builder.build_unconditional_branch(arm_bb).unwrap(); }
                                        Some(nc) => { self.builder.build_conditional_branch(cmp, arm_bb, nc).unwrap(); }
                                    }
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
                                            let ptr = self.entry_alloca(self.i64.into(), vname);
                                            self.builder.build_store(ptr, field_val).unwrap();
                                            let pty = payload_types.get(j).cloned().unwrap_or(Type::Int);
                                            self.named.insert(vname.clone(), (ptr, pty.clone()));
                                            self.types.define(vname, pty);
                                        }
                                    }
                                }
                            }
                            Pattern::Wildcard | Pattern::Variable(_) => {
                                self.builder.build_unconditional_branch(arm_bb).unwrap();
                            }
                        }
                    }
                    self.builder.position_at_end(arm_bb);
                    if let Some(first_pat) = patterns.first() {
                        if let Pattern::Variable(name) = first_pat {
                            let ptr = self.entry_alloca(self.i64.into(), name);
                            self.builder.build_store(ptr, sv_tag).unwrap();
                            self.named.insert(name.clone(), (ptr, Type::Int));
                            self.types.define(name, Type::Int);
                        }
                    }
                    // With a guard the body needs its own block, so the guard
                    // can branch past it. Branching to `arm_bb` from inside
                    // `arm_bb` was a self-loop, and the body was never emitted
                    // at all, leaving `merge` with a predecessor that fed the
                    // phi no value.
                    let _body_bb = if let Some(g) = guard {
                        let gv = self.gen_expr(g);
                        let gc = self.to_i1(&gv);
                        let body_bb = self.context.append_basic_block(parent, &format!("arm_{}_body", i));
                        let guard_fail = self.context.append_basic_block(parent, &format!("arm_{}_guard_fail", i));
                        self.builder.build_conditional_branch(gc, body_bb, guard_fail).unwrap();
                        self.builder.position_at_end(guard_fail);
                        match next_check {
                            Some(nc) => { self.builder.build_unconditional_branch(nc).unwrap(); }
                            None => {
                                arm_values.push((self.i64.const_int(0, false), guard_fail));
                                self.builder.build_unconditional_branch(merge).unwrap();
                            }
                        }
                        self.builder.position_at_end(body_bb);
                        body_bb
                    } else {
                        arm_bb
                    };
                    let bv = self.gen_expr(body);
                    let bv_int = self.value_to_int(&bv);
                    if arm_shape.is_none() { arm_shape = Some(bv.clone()); }
                    // An arm body can open blocks of its own — a nested if or
                    // match — so the value reaches the merge from wherever the
                    // body ended, not from the block the arm started in.
                    let from = self.builder.get_insert_block().unwrap();
                    arm_values.push((bv_int, from));
                    self.builder.build_unconditional_branch(merge).unwrap();
                    if let Some(nc) = next_check {
                        current_check = nc;
                    }
                }
                if current_check.get_terminator().is_none() {
                    // No arm matched. There is no exhaustiveness check yet, so
                    // this is only detectable here; the interpreter stops with
                    // the same message.
                    self.builder.position_at_end(current_check);
                    self.gen_abort("no matching pattern", &[]);
                }
                self.builder.position_at_end(merge);
                let phi = self.builder.build_phi(self.i64, "match_result").unwrap();
                for (v, bb) in &arm_values {
                    phi.add_incoming(&[(&*v, *bb)]);
                }
                self.reshape_like(phi.as_basic_value().into_int_value(), arm_shape.as_ref())
            }
            ExprKind::If { cond, then_body, else_body } => {
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
                let then_end = self.builder.get_insert_block().unwrap();
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
                let else_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(self.i64, "if.result").unwrap();
                phi.add_incoming(&[(&then_int, then_end), (&else_int, else_end)]);
                self.reshape_like(phi.as_basic_value().into_int_value(), Some(&then_val))
            }
            ExprKind::While { cond, body } => {
                let fn_val = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let loop_bb = self.context.append_basic_block(fn_val, "while_expr");
                let body_bb = self.context.append_basic_block(fn_val, "while_expr_body");
                let after_bb = self.context.append_basic_block(fn_val, "while_expr_end");

                let result_ptr = self.entry_alloca(self.i64.into(), "while_result");
                self.builder.build_store(result_ptr, self.i64.const_int(0, false)).unwrap();

                self.builder.build_unconditional_branch(loop_bb).unwrap();
                self.builder.position_at_end(loop_bb);
                let cond_val = self.gen_expr(cond);
                let cond_bool = self.to_i1(&cond_val);
                self.builder.build_conditional_branch(cond_bool, body_bb, after_bb).unwrap();

                self.loop_exit.push(after_bb);
                self.loop_continue.push(loop_bb);
                self.loop_result_ptr.push(result_ptr);

                self.builder.position_at_end(body_bb);
                let mut terminated = false;
                for s in body {
                    self.gen_stmt(s, fn_val, &mut terminated);
                    if terminated { break; }
                }
                if !terminated {
                    self.builder.build_unconditional_branch(loop_bb).unwrap();
                }

                self.loop_exit.pop();
                self.loop_continue.pop();
                self.loop_result_ptr.pop();

                self.builder.position_at_end(after_bb);
                let result = self.builder.build_load(self.i64, result_ptr, "while_result").unwrap().into_int_value();
                CgValue::Int(result)
            }
            ExprKind::For { var, iter, body } => {
                let fn_val = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let iter_val = self.gen_expr(iter);
                let iter_int = self.value_to_int(&iter_val);

                let arr_ptr_val = self.entry_alloca(self.i64.into(), "for_arr_ptr");
                let is_array = self.entry_alloca(self.i64.into(), "for_is_array");

                let loop_bb = self.context.append_basic_block(fn_val, "for_loop");
                let body_bb = self.context.append_basic_block(fn_val, "for_body");
                let inc_bb = self.context.append_basic_block(fn_val, "for_inc");
                let after_bb = self.context.append_basic_block(fn_val, "for_end");

                let result_ptr = self.entry_alloca(self.i64.into(), "for_result");
                self.builder.build_store(result_ptr, self.i64.const_int(0, false)).unwrap();
                self.builder.build_store(is_array, self.i64.const_int(0, false)).unwrap();
                self.builder.build_store(arr_ptr_val, self.i64.const_int(0, false)).unwrap();

                let idx_ptr = self.entry_alloca(self.i64.into(), "for_idx");
                self.builder.build_store(idx_ptr, self.i64.const_int(0, false)).unwrap();

                self.builder.build_unconditional_branch(loop_bb).unwrap();
                self.builder.position_at_end(loop_bb);
                let cur_idx = self.builder.build_load(self.i64, idx_ptr, "for_cur_idx").unwrap().into_int_value();
                let is_arr = self.builder.build_load(self.i64, is_array, "for_is_arr").unwrap().into_int_value();
                let is_arr_bool = self.builder.build_int_compare(inkwell::IntPredicate::NE, is_arr, self.i64.const_int(0, false), "is_arr_i1").unwrap();

                let range_check_bb = self.context.append_basic_block(fn_val, "for_range_check");
                self.builder.build_conditional_branch(is_arr_bool, range_check_bb, range_check_bb).unwrap();

                self.builder.position_at_end(range_check_bb);
                let has_more = self.builder.build_int_compare(inkwell::IntPredicate::SLT, cur_idx, iter_int, "has_more").unwrap();
                self.builder.build_conditional_branch(has_more, body_bb, after_bb).unwrap();

                self.loop_exit.push(after_bb);
                self.loop_continue.push(inc_bb);
                self.loop_result_ptr.push(result_ptr);

                self.builder.position_at_end(body_bb);
                let elem = cur_idx;
                let var_ptr = self.entry_alloca(self.i64.into(), var);
                self.builder.build_store(var_ptr, elem).unwrap();
                self.named.insert(var.clone(), (var_ptr, Type::Int));
                self.types.define(var, Type::Int);

                let mut terminated = false;
                for s in body {
                    self.gen_stmt(s, fn_val, &mut terminated);
                    if terminated { break; }
                }
                if !terminated {
                    self.builder.build_unconditional_branch(inc_bb).unwrap();
                }

                self.loop_exit.pop();
                self.loop_continue.pop();
                self.loop_result_ptr.pop();

                self.builder.position_at_end(inc_bb);
                let next_idx = self.builder.build_int_add(cur_idx, self.i64.const_int(1, false), "next_idx").unwrap();
                self.builder.build_store(idx_ptr, next_idx).unwrap();
                self.builder.build_unconditional_branch(loop_bb).unwrap();

                self.builder.position_at_end(after_bb);
                let result = self.builder.build_load(self.i64, result_ptr, "for_result").unwrap().into_int_value();
                CgValue::Int(result)
            }
            ExprKind::Range(_, _) => {
                CgValue::Int(self.i64.const_int(0, false))
            }
            ExprKind::ArrayLit(elems) => {
                let len = elems.len() as u64;
                // Layout is [len, elem0, ..]. This asked malloc for `len + 1`
                // *bytes* and then wrote that many 64-bit words into it.
                let buf = self.scratch_slots(len + 1, "arr_alloc");
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
                    if let Some(ety) = self.type_of(elem) {
                        self.retain_stored(ev_int, &ety, elem);
                    }
                }
                CgValue::Int(buf_i64)
            }
            ExprKind::Index(arr, idx) => {
                let arr_val = self.gen_expr(arr);
                let arr_ptr_val = self.value_to_int(&arr_val);
                let arr_ptr = self.builder.build_int_to_ptr(arr_ptr_val, self.context.ptr_type(AddressSpace::default()), "arr_ptr").unwrap();
                let idx_val = self.gen_expr(idx);
                let idx_int = self.value_to_int(&idx_val);
                let arr_len = self.builder.build_load(self.i64, arr_ptr, "arr_len").unwrap().into_int_value();
                self.gen_bounds_check(idx_int, arr_len, "array");
                let elem_offset = self.builder.build_int_add(idx_int, self.i64.const_int(1, false), "elem_off").unwrap();
                let elem_ptr = unsafe {
                    self.builder.build_gep(self.i64, arr_ptr, &[elem_offset], "elem_ptr").unwrap()
                };
                let raw = self.builder.build_load(self.i64, elem_ptr, "arr_elem").unwrap().into_int_value();
                // Elements keep their declared type, as struct fields do; the
                // load came back as a plain int whatever the array held.
                let elem_ty = match self.type_of(arr) {
                    Some(Type::Array(el)) => (*el).clone(),
                    _ => Type::Int,
                };
                self.typed_value(raw, &elem_ty)
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
        let buf = self.scratch_bytes(total_plus1, "buf");
        self.builder.build_call(memcpy_fn, &[buf.into(), a.into(), len_a.into()], "cp_a").unwrap();
        let offset = unsafe { self.builder.build_gep(self.i8, buf, &[len_a], "offset").unwrap() };
        self.builder.build_call(memcpy_fn, &[offset.into(), b.into(), self.builder.build_int_add(len_b, self.i64.const_int(1, false), "nb1").unwrap().into()], "cp_b").unwrap();
        CgValue::Str(buf)
    }

    fn gen_int_to_str(&self, val: IntValue<'ctx>) -> PointerValue<'ctx> {
        let malloc_type = self.context.ptr_type(AddressSpace::default()).fn_type(&[self.i64.into()], false);
        let malloc_fn = self.module.get_function("malloc").unwrap_or_else(|| self.module.add_function("malloc", malloc_type, None));
        let buf = self.scratch_bytes(self.i64.const_int(32, false), "int_buf");

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

        let idx_ptr = self.entry_alloca(self.i64.into(), "itoa_idx");
        self.builder.build_store(idx_ptr, self.i64.const_int(30, false)).unwrap();
        let val_ptr = self.entry_alloca(self.i64.into(), "itoa_val");
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

        // The digits were built backwards from the end of the buffer. Move them
        // to the front so the string starts where the allocation does —
        // releasing an interior pointer would hand `free` the wrong address.
        let start = unsafe {
            self.builder.build_gep(self.i8, buf_byte_ptr2, &[fd], "digits").unwrap()
        };
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let strlen = self.module.get_function("strlen").unwrap_or_else(|| {
            self.module.add_function("strlen", self.i64.fn_type(&[ptr_ty.into()], false), None)
        });
        let memmove = self.module.get_function("memmove").unwrap_or_else(|| {
            let ty = self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into(), self.i64.into()], false);
            self.module.add_function("memmove", ty, None)
        });
        let n = self.builder.build_call(strlen, &[start.into()], "digits_len").unwrap()
            .try_as_basic_value().left().unwrap().into_int_value();
        let with_nul = self.builder.build_int_add(n, self.i64.const_int(1, false), "digits_n").unwrap();
        self.builder.build_call(memmove, &[buf_byte_ptr2.into(), start.into(), with_nul.into()], "").unwrap();
        buf_byte_ptr2
    }

    /// Format a float the way the interpreter does: the shortest decimal that
    /// reads back as the same double, in plain notation, no exponent.
    ///
    /// This used to be `sprintf("%.6f")`, which both lost precision
    /// (0.3333333333333333 came out as 0.333333) and padded whole numbers
    /// (12 as 12.000000), so the two backends printed different things for the
    /// same program.
    ///
    /// The shortest form is found by asking for one more decimal place until
    /// the result parses back to the original value. A whole number needs a
    /// second pass: at zero decimals `%f` prints the double's exact binary
    /// expansion, which for large magnitudes is far longer than the shortest
    /// form, so its digits are replaced with the correctly rounded ones from
    /// `%e` followed by zeros.
    /// Format a float into frame memory. The result is only valid until the
    /// next call, so anything that keeps it must copy first — see
    /// `gen_float_to_owned`.
    fn gen_float_to_str(&self, val: FloatValue<'ctx>) -> PointerValue<'ctx> {
        const CAP: u64 = 4200; // widest expansion of a subnormal, plus room
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();

        let snprintf = self.module.get_function("snprintf").unwrap_or_else(|| {
            let ty = i32_ty.fn_type(&[ptr_ty.into(), self.i64.into(), ptr_ty.into()], true);
            self.module.add_function("snprintf", ty, None)
        });
        let strtod = self.module.get_function("strtod").unwrap_or_else(|| {
            let ty = self.f64.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
            self.module.add_function("strtod", ty, None)
        });
        let strlen = self.module.get_function("strlen").unwrap_or_else(|| {
            let ty = self.i64.fn_type(&[ptr_ty.into()], false);
            self.module.add_function("strlen", ty, None)
        });

        // Scratch for the formatting itself: it never outlives this call.
        // These were heap and never freed, which cost 4.2 KB per printed float.
        let buf = self.entry_alloca(self.i8.array_type(CAP as u32).into(), "float_buf");
        let ebuf = self.entry_alloca(self.i8.array_type(64).into(), "float_ebuf");
        let cap = self.i64.const_int(CAP, false);
        let null = ptr_ty.const_null();
        let fmt_f = self.builder.build_global_string_ptr("%.*f", "fmt_f").unwrap().as_pointer_value();
        let fmt_g = self.builder.build_global_string_ptr("%.*g", "fmt_g").unwrap().as_pointer_value();
        let fmt_e = self.builder.build_global_string_ptr("%.*e", "fmt_e").unwrap().as_pointer_value();

        let f = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let bb = |n: &str| self.context.append_basic_block(f, n);

        // --- NaN: never compares equal to itself, so the search below would
        // --- run to the cap and then print printf's spelling, "nan" ---------
        let (nan_bb, num_bb) = (bb("float_nan"), bb("float_num"));
        let is_nan = self.builder.build_float_compare(inkwell::FloatPredicate::UNO, val, val, "is_nan").unwrap();
        self.builder.build_conditional_branch(is_nan, nan_bb, num_bb).unwrap();
        self.builder.position_at_end(nan_bb);
        let fmt_s = self.builder.build_global_string_ptr("%s", "fmt_s").unwrap().as_pointer_value();
        let nan_txt = self.builder.build_global_string_ptr("NaN", "nan_txt").unwrap().as_pointer_value();
        self.builder.build_call(snprintf, &[buf.into(), cap.into(), fmt_s.into(), nan_txt.into()], "").unwrap();
        let nan_exit = self.builder.get_insert_block().unwrap();
        self.builder.position_at_end(num_bb);

        // --- shortest number of decimals that round-trips -------------------
        let d_ptr = self.entry_alloca(self.i64.into(), "fd");
        self.builder.build_store(d_ptr, self.i64.const_zero()).unwrap();
        let (d_head, d_next, d_done) = (bb("fd_head"), bb("fd_next"), bb("fd_done"));
        self.builder.build_unconditional_branch(d_head).unwrap();

        self.builder.position_at_end(d_head);
        let d = self.builder.build_load(self.i64, d_ptr, "d").unwrap().into_int_value();
        let d32 = self.builder.build_int_truncate(d, i32_ty, "d32").unwrap();
        self.builder.build_call(snprintf, &[buf.into(), cap.into(), fmt_f.into(), d32.into(), val.into()], "").unwrap();
        let back = self.builder.build_call(strtod, &[buf.into(), null.into()], "back").unwrap()
            .try_as_basic_value().left().unwrap().into_float_value();
        let exact = self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, back, val, "exact").unwrap();
        let capped = self.builder.build_int_compare(inkwell::IntPredicate::SGE, d, self.i64.const_int(1100, false), "capped").unwrap();
        let stop = self.builder.build_or(exact, capped, "stop").unwrap();
        self.builder.build_conditional_branch(stop, d_done, d_next).unwrap();

        self.builder.position_at_end(d_next);
        let d1 = self.builder.build_int_add(d, self.i64.const_int(1, false), "d1").unwrap();
        self.builder.build_store(d_ptr, d1).unwrap();
        self.builder.build_unconditional_branch(d_head).unwrap();

        self.builder.position_at_end(d_done);
        let d = self.builder.build_load(self.i64, d_ptr, "d_final").unwrap().into_int_value();
        let whole = self.builder.build_int_compare(inkwell::IntPredicate::EQ, d, self.i64.const_zero(), "whole").unwrap();
        let pinf = self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, val, self.f64.const_float(f64::INFINITY), "pinf").unwrap();
        let ninf = self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, val, self.f64.const_float(f64::NEG_INFINITY), "ninf").unwrap();
        let infinite = self.builder.build_or(pinf, ninf, "infinite").unwrap();
        let finite = self.builder.build_not(infinite, "finite").unwrap();
        let rewrite = self.builder.build_and(whole, finite, "rewrite").unwrap();
        let (fixup, finish) = (bb("float_fixup"), bb("float_done"));
        self.builder.build_conditional_branch(rewrite, fixup, finish).unwrap();

        // --- whole numbers: shortest significant digits, then zeros ---------
        self.builder.position_at_end(fixup);
        let len = self.builder.build_call(strlen, &[buf.into()], "len").unwrap()
            .try_as_basic_value().left().unwrap().into_int_value();
        let first = self.builder.build_load(self.i8, buf, "first").unwrap().into_int_value();
        let is_neg = self.builder.build_int_compare(inkwell::IntPredicate::EQ, first, self.i8.const_int('-' as u64, false), "is_neg").unwrap();
        let neg = self.builder.build_int_z_extend(is_neg, self.i64, "neg").unwrap();

        let p_ptr = self.entry_alloca(self.i64.into(), "fp");
        self.builder.build_store(p_ptr, self.i64.const_int(1, false)).unwrap();
        let (p_head, p_next, p_done) = (bb("fp_head"), bb("fp_next"), bb("fp_done"));
        self.builder.build_unconditional_branch(p_head).unwrap();

        self.builder.position_at_end(p_head);
        let pv = self.builder.build_load(self.i64, p_ptr, "p").unwrap().into_int_value();
        let p32 = self.builder.build_int_truncate(pv, i32_ty, "p32").unwrap();
        self.builder.build_call(snprintf, &[ebuf.into(), self.i64.const_int(64, false).into(), fmt_g.into(), p32.into(), val.into()], "").unwrap();
        let back2 = self.builder.build_call(strtod, &[ebuf.into(), null.into()], "back2").unwrap()
            .try_as_basic_value().left().unwrap().into_float_value();
        let exact2 = self.builder.build_float_compare(inkwell::FloatPredicate::OEQ, back2, val, "exact2").unwrap();
        let capped2 = self.builder.build_int_compare(inkwell::IntPredicate::SGE, pv, self.i64.const_int(17, false), "capped2").unwrap();
        let stop2 = self.builder.build_or(exact2, capped2, "stop2").unwrap();
        self.builder.build_conditional_branch(stop2, p_done, p_next).unwrap();

        self.builder.position_at_end(p_next);
        let p1 = self.builder.build_int_add(pv, self.i64.const_int(1, false), "p1").unwrap();
        self.builder.build_store(p_ptr, p1).unwrap();
        self.builder.build_unconditional_branch(p_head).unwrap();

        self.builder.position_at_end(p_done);
        let p = self.builder.build_load(self.i64, p_ptr, "p_final").unwrap().into_int_value();
        let pm1 = self.builder.build_int_sub(p, self.i64.const_int(1, false), "pm1").unwrap();
        let pm1_32 = self.builder.build_int_truncate(pm1, i32_ty, "pm1_32").unwrap();
        self.builder.build_call(snprintf, &[ebuf.into(), self.i64.const_int(64, false).into(), fmt_e.into(), pm1_32.into(), val.into()], "").unwrap();

        // copy p digits out of "d.ddde+XX", skipping the point
        let j_ptr = self.entry_alloca(self.i64.into(), "j");
        let k_ptr = self.entry_alloca(self.i64.into(), "k");
        let g_ptr = self.entry_alloca(self.i64.into(), "got");
        self.builder.build_store(j_ptr, neg).unwrap();
        self.builder.build_store(k_ptr, neg).unwrap();
        self.builder.build_store(g_ptr, self.i64.const_zero()).unwrap();
        let (c_head, c_write, c_skip, pad_head, pad_body) =
            (bb("copy_head"), bb("copy_write"), bb("copy_skip"), bb("pad_head"), bb("pad_body"));
        self.builder.build_unconditional_branch(c_head).unwrap();

        self.builder.position_at_end(c_head);
        let got = self.builder.build_load(self.i64, g_ptr, "got_v").unwrap().into_int_value();
        let more = self.builder.build_int_compare(inkwell::IntPredicate::SLT, got, p, "more").unwrap();
        self.builder.build_conditional_branch(more, c_write, pad_head).unwrap();

        self.builder.position_at_end(c_write);
        let k = self.builder.build_load(self.i64, k_ptr, "k_v").unwrap().into_int_value();
        let src = unsafe { self.builder.build_gep(self.i8, ebuf, &[k], "src").unwrap() };
        let ch = self.builder.build_load(self.i8, src, "ch").unwrap().into_int_value();
        let is_dot = self.builder.build_int_compare(inkwell::IntPredicate::EQ, ch, self.i8.const_int('.' as u64, false), "is_dot").unwrap();
        let write_bb = bb("do_write");
        self.builder.build_conditional_branch(is_dot, c_skip, write_bb).unwrap();

        self.builder.position_at_end(write_bb);
        let j = self.builder.build_load(self.i64, j_ptr, "j_v").unwrap().into_int_value();
        let dst = unsafe { self.builder.build_gep(self.i8, buf, &[j], "dst").unwrap() };
        self.builder.build_store(dst, ch).unwrap();
        self.builder.build_store(j_ptr, self.builder.build_int_add(j, self.i64.const_int(1, false), "j1").unwrap()).unwrap();
        self.builder.build_store(g_ptr, self.builder.build_int_add(got, self.i64.const_int(1, false), "got1").unwrap()).unwrap();
        self.builder.build_unconditional_branch(c_skip).unwrap();

        self.builder.position_at_end(c_skip);
        let k2 = self.builder.build_load(self.i64, k_ptr, "k2").unwrap().into_int_value();
        self.builder.build_store(k_ptr, self.builder.build_int_add(k2, self.i64.const_int(1, false), "k1").unwrap()).unwrap();
        self.builder.build_unconditional_branch(c_head).unwrap();

        // the rest of the original digits become zeros; the terminator is already there
        self.builder.position_at_end(pad_head);
        let jp = self.builder.build_load(self.i64, j_ptr, "jp").unwrap().into_int_value();
        let need = self.builder.build_int_compare(inkwell::IntPredicate::SLT, jp, len, "need").unwrap();
        self.builder.build_conditional_branch(need, pad_body, finish).unwrap();

        self.builder.position_at_end(pad_body);
        let zdst = unsafe { self.builder.build_gep(self.i8, buf, &[jp], "zdst").unwrap() };
        self.builder.build_store(zdst, self.i8.const_int('0' as u64, false)).unwrap();
        self.builder.build_store(j_ptr, self.builder.build_int_add(jp, self.i64.const_int(1, false), "jp1").unwrap()).unwrap();
        self.builder.build_unconditional_branch(pad_head).unwrap();

        self.builder.position_at_end(nan_exit);
        self.builder.build_unconditional_branch(finish).unwrap();

        self.builder.position_at_end(finish);
        buf
    }

    /// The same text, in an allocation of exactly the size it needs, for the
    /// cases where it escapes the expression that produced it.
    fn gen_float_to_owned(&self, val: FloatValue<'ctx>) -> PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let scratch = self.gen_float_to_str(val);
        let strlen = self.module.get_function("strlen").unwrap_or_else(|| {
            let ty = self.i64.fn_type(&[ptr_ty.into()], false);
            self.module.add_function("strlen", ty, None)
        });
        let memcpy = self.module.get_function("memcpy").unwrap_or_else(|| {
            let ty = self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into(), self.i64.into()], false);
            self.module.add_function("memcpy", ty, None)
        });
        let len = self.builder.build_call(strlen, &[scratch.into()], "flen").unwrap()
            .try_as_basic_value().left().unwrap().into_int_value();
        let size = self.builder.build_int_add(len, self.i64.const_int(1, false), "fsize").unwrap();
        let owned = self.scratch_bytes(size, "float_owned");
        self.builder.build_call(memcpy, &[owned.into(), scratch.into(), size.into()], "").unwrap();
        owned
    }

    /// Every branch of a match or an if is merged through an i64 phi. Put the
    /// result back into whatever shape the branches produced, or a string arm
    /// comes out as the number its pointer happens to be.
    /// Read a raw word back as the type it was stored as.
    fn typed_value(&self, raw: IntValue<'ctx>, ty: &Type) -> CgValue<'ctx> {
        match ty {
            Type::Float => CgValue::Float(self.builder.build_bit_cast(raw, self.f64, "as_f").unwrap().into_float_value()),
            Type::String => CgValue::Str(
                self.builder.build_int_to_ptr(raw, self.context.ptr_type(AddressSpace::default()), "as_s").unwrap(),
            ),
            _ => CgValue::Int(raw),
        }
    }

    fn reshape_like(&self, merged: IntValue<'ctx>, sample: Option<&CgValue<'ctx>>) -> CgValue<'ctx> {
        match sample {
            Some(CgValue::Str(_)) => CgValue::Str(
                self.builder.build_int_to_ptr(merged, self.context.ptr_type(AddressSpace::default()), "merge_str").unwrap(),
            ),
            Some(CgValue::Float(_)) => CgValue::Float(
                self.builder.build_bit_cast(merged, self.f64, "merge_f").unwrap().into_float_value(),
            ),
            _ => CgValue::Int(merged),
        }
    }

    fn gen_bool_to_str(&self, v: IntValue<'ctx>) -> PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let t = self.string_global("true");
        let f = self.string_global("false");
        let zero = self.i64.const_zero();
        let is_true = self.builder.build_int_compare(inkwell::IntPredicate::NE, v, zero, "is_true").unwrap();
        self.builder.build_select(is_true, t, f, "bool_txt").unwrap().into_pointer_value()
    }

    fn value_to_int(&self, v: &CgValue<'ctx>) -> IntValue<'ctx> {
        match v {
            CgValue::Int(val) => *val,
            CgValue::Float(val) => self.builder.build_bit_cast(*val, self.i64, "f2i").unwrap().into_int_value(),
            CgValue::Str(p) => self.builder.build_ptr_to_int(*p, self.i64, "p2i").unwrap(),
        }
    }

    /// Print one value. Booleans and enums need their text built first; the
    /// caller runs this inside `consumed`, so those buffers live on the frame.
    fn gen_print_arg(&mut self, arg: &Expr) -> CgValue<'ctx> {
        if self.is_bool_expr(arg) {
            let v = self.gen_expr(arg);
            let iv = self.value_to_int(&v);
            let text = self.gen_bool_to_str(iv);
            let fmt = self.string_global("%s\n");
            self.builder.build_call(self.printf, &[fmt.into(), text.into()], "call_printf").unwrap();
        } else if let Some(en) = self.infer_enum_type(arg) {
            let v = self.gen_expr(arg);
            let iv = self.value_to_int(&v);
            let text = self.gen_enum_to_str(iv, &en, 0);
            let fmt = self.string_global("%s\n");
            self.builder.build_call(self.printf, &[fmt.into(), text.into()], "call_printf").unwrap();
        } else {
            let v = self.gen_expr(arg);
            self.gen_print(&v);
        }
        CgValue::Int(self.i64.const_int(0, false))
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
                // Same formatting as to_string, so `print(x)` and
                // `print(to_string(x))` agree, and both match the interpreter.
                // printf consumes it on the spot, so the frame buffer serves.
                let s = self.gen_float_to_str(*v);
                let fmt = self.string_global("%s\n");
                self.builder
                    .build_call(self.printf, &[fmt.into(), s.into()], "call_printf")
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
        self.current_span = s.span;
        match &s.kind {
            StmtKind::Let { name, value, .. } => {
                if let Some(sn) = self.infer_struct_type(value) {
                    self.var_struct_type.insert(name.clone(), sn);
                }
                let declared = self.type_of(value).filter(|t| *t != Type::Inferred);
                let v = self.gen_expr(value);
                // The checker's type decides how the local is held; where it
                // cannot say, fall back to the shape of the value.
                let ty = declared.unwrap_or(match &v {
                    CgValue::Str(_) => Type::String,
                    CgValue::Float(_) => Type::Float,
                    CgValue::Int(_) => Type::Int,
                });
                let borrowed = !self.produces_owned(value);
                self.bind_local(name, &ty, &v);
                if self.is_managed(&ty) {
                    if borrowed {
                        if let Some((p, t)) = self.managed_ptr(name) {
                            let _ = t;
                            self.gen_retain(p);
                        }
                    }
                    self.record_owned(name);
                }
            }
            StmtKind::Assign { name, value } => {
                let v = self.gen_expr(value);
                if let Some((ptr, ty)) = self.named.get(name).map(|(p, t)| (*p, t.clone())) {
                    let managed = self.is_managed(&ty);
                    let old = if managed { self.managed_ptr(name) } else { None };
                    // Convert to the slot's representation; storing the value
                    // raw would put a float's bits into an integer slot.
                    self.store_into(ptr, &ty, &v);
                    if managed {
                        if !self.produces_owned(value) {
                            if let Some((p, _)) = self.managed_ptr(name) {
                                self.gen_retain(p);
                            }
                        }
                        // after the store, so rebinding a name to itself is safe
                        if let Some((p, t)) = old {
                            self.gen_release(p, &t);
                        }
                    }
                } else {
                    panic!("undefined variable for assign: {}", name);
                }
            }
            StmtKind::Expr(e) => {
                self.gen_expr(e);
            }
            StmtKind::Return(e) => {
                match e {
                    Some(x) => {
                        let v = self.gen_expr(x);
                        if let Some(t) = self.type_of(x) {
                            if self.is_managed(&t) && !self.produces_owned(x) {
                                let raw = self.value_to_int(&v);
                                let p = self.builder.build_int_to_ptr(raw, self.context.ptr_type(AddressSpace::default()), "ret_ptr").unwrap();
                                self.gen_retain(p);
                            }
                        }
                        self.release_all_open();
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
            StmtKind::If { cond, then_body, else_body } => {
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
            StmtKind::While { cond, body } => {
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
                self.open_block();
                for s in body {
                    self.gen_stmt(s, fn_val, &mut body_terminated);
                    if body_terminated { break; }
                }
                if !body_terminated { self.close_block(); } else { self.owned.pop(); }
                if !body_terminated {
                    self.builder.build_unconditional_branch(loop_bb).unwrap();
                }

                self.loop_exit.pop();
                self.loop_continue.pop();

                self.builder.position_at_end(after_bb);
            }
            StmtKind::Break(val) => {
                if let Some(exit_bb) = self.loop_exit.last().cloned() {
                    if let Some(expr) = val {
                        let bv = self.gen_expr(expr);
                        let bv_int = self.value_to_int(&bv);
                        if let Some(result_ptr) = self.loop_result_ptr.last().cloned() {
                            self.builder.build_store(result_ptr, bv_int).unwrap();
                        }
                    }
                    self.builder.build_unconditional_branch(exit_bb).unwrap();
                    let dead_bb = self.context.append_basic_block(self.builder.get_insert_block().unwrap().get_parent().unwrap(), "dead");
                    self.builder.position_at_end(dead_bb);
                }
            }
            StmtKind::Continue(_) => {
                if let Some(cont_bb) = self.loop_continue.last().cloned() {
                    self.builder.build_unconditional_branch(cont_bb).unwrap();
                    let dead_bb = self.context.append_basic_block(self.builder.get_insert_block().unwrap().get_parent().unwrap(), "dead");
                    self.builder.position_at_end(dead_bb);
                }
            }
            StmtKind::For { var, iter, body } => {
                if let ExprKind::Range(start_expr, end_expr) = &*iter.kind {
                    let start_val = self.gen_expr(start_expr);
                    let end_val = self.gen_expr(end_expr);
                    let end_int = match end_val { CgValue::Int(v) => v, _ => panic!("for range end must be int") };

                    let entry_bb = self.context.append_basic_block(fn_val, "for_entry");
                    let body_bb = self.context.append_basic_block(fn_val, "for_body");
                    let cont_bb = self.context.append_basic_block(fn_val, "for_cont");
                    let exit_bb = self.context.append_basic_block(fn_val, "for_exit");

                    let counter_ptr = self.entry_alloca(self.i64.into(), var);
                    self.builder.build_store(counter_ptr, start_val.as_basic()).unwrap();

                    self.builder.build_unconditional_branch(entry_bb).unwrap();
                    self.builder.position_at_end(entry_bb);
                    let cur = self.builder.build_load(self.i64, counter_ptr, &format!("{}_cur", var)).unwrap().into_int_value();
                    let cmp = self.builder.build_int_compare(inkwell::IntPredicate::SLT, cur, end_int, "for_cmp").unwrap();
                    self.builder.build_conditional_branch(cmp, body_bb, exit_bb).unwrap();

                    self.named.insert(var.clone(), (counter_ptr, Type::Int));
                    self.types.define(var, Type::Int);
                    self.loop_exit.push(exit_bb);
                    self.loop_continue.push(cont_bb);
                    self.builder.position_at_end(body_bb);
                    let mut body_terminated = false;
                    self.open_block();
                    for s in body {
                        self.gen_stmt(s, fn_val, &mut body_terminated);
                        if body_terminated { break; }
                    }
                    if !body_terminated { self.close_block(); } else { self.owned.pop(); }
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
            StmtKind::Fn { .. } | StmtKind::Struct { .. } | StmtKind::Enum { .. } | StmtKind::Trait { .. }
            | StmtKind::Macro { .. } | StmtKind::ExternFn { .. } | StmtKind::Impl { .. } | StmtKind::Import(_) => {}
        }
    }
}

fn expand_macros_in_expr(expr: &Expr, macros: &HashMap<String, (Vec<String>, Vec<Stmt>)>) -> Expr {
    if let ExprKind::Call(callee, args) = &*expr.kind {
        if let ExprKind::Ident(name) = &*callee.kind {
            if let Some((params, body)) = macros.get(name) {
                let mut arg_map: HashMap<String, Expr> = HashMap::new();
                for (i, p) in params.iter().enumerate() {
                    arg_map.insert(p.clone(), substitute_expr(&args[i], &HashMap::new(), macros));
                }
                if body.len() == 1 {
                    if let StmtKind::Expr(e) = &body[0].kind {
                        return substitute_expr(e, &arg_map, macros);
                    }
                    if let StmtKind::Return(Some(e)) = &body[0].kind {
                        return substitute_expr(e, &arg_map, macros);
                    }
                }
                return expr.clone();
            }
        }
    }
    match &*expr.kind {
        ExprKind::Binary(l, op, r) => Expr::new(ExprKind::Binary(Box::new(expand_macros_in_expr(l, macros)), op.clone(), Box::new(expand_macros_in_expr(r, macros))), expr.span),
        ExprKind::Unary(op, e) => Expr::new(ExprKind::Unary(op.clone(), Box::new(expand_macros_in_expr(e, macros))), expr.span),
        ExprKind::Call(callee, args) => Expr::new(ExprKind::Call(Box::new(expand_macros_in_expr(callee, macros)), args.iter().map(|a| expand_macros_in_expr(a, macros)).collect()), expr.span),
        ExprKind::If { cond, then_body, else_body } => Expr::new(ExprKind::If {
            cond: Box::new(expand_macros_in_expr(cond, macros)),
            then_body: Box::new(expand_macros_in_expr(then_body, macros)),
            else_body: else_body.as_ref().map(|e| Box::new(expand_macros_in_expr(e, macros))),
        }, expr.span),
        ExprKind::FieldAccess(obj, field) => Expr::new(ExprKind::FieldAccess(Box::new(expand_macros_in_expr(obj, macros)), field.clone()), expr.span),
        ExprKind::MethodCall(obj, method, args) => Expr::new(ExprKind::MethodCall(Box::new(expand_macros_in_expr(obj, macros)), method.clone(), args.iter().map(|a| expand_macros_in_expr(a, macros)).collect()), expr.span),
        ExprKind::Index(arr, idx) => Expr::new(ExprKind::Index(Box::new(expand_macros_in_expr(arr, macros)), Box::new(expand_macros_in_expr(idx, macros))), expr.span),
        ExprKind::StructLit { name, fields } => Expr::new(ExprKind::StructLit { name: name.clone(), fields: fields.iter().map(|(n, e)| (n.clone(), expand_macros_in_expr(e, macros))).collect() }, expr.span),
        ExprKind::Match { scrutinee, arms } => Expr::new(ExprKind::Match { scrutinee: Box::new(expand_macros_in_expr(scrutinee, macros)), arms: arms.iter().map(|(pats, guard, e)| (pats.clone(), guard.as_ref().map(|g| expand_macros_in_expr(g, macros)), expand_macros_in_expr(e, macros))).collect() }, expr.span),
        ExprKind::While { cond, body } => Expr::new(ExprKind::While { cond: Box::new(expand_macros_in_expr(cond, macros)), body: body.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect() }, expr.span),
        ExprKind::For { var, iter, body } => Expr::new(ExprKind::For { var: var.clone(), iter: Box::new(expand_macros_in_expr(iter, macros)), body: body.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect() }, expr.span),
        _ => expr.clone(),
    }
}

fn substitute_expr(expr: &Expr, arg_map: &HashMap<String, Expr>, macros: &HashMap<String, (Vec<String>, Vec<Stmt>)>) -> Expr {
    match &*expr.kind {
        ExprKind::Ident(name) => {
            if let Some(val) = arg_map.get(name) {
                val.clone()
            } else {
                Expr::new(ExprKind::Ident(name.clone()), expr.span)
            }
        }
        ExprKind::Binary(l, op, r) => Expr::new(ExprKind::Binary(
            Box::new(substitute_expr(l, arg_map, macros)),
            op.clone(),
            Box::new(substitute_expr(r, arg_map, macros)),
        ), expr.span),
        ExprKind::Unary(op, e) => Expr::new(ExprKind::Unary(op.clone(), Box::new(substitute_expr(e, arg_map, macros))), expr.span),
        ExprKind::Call(callee, args) => {
            let expanded = Expr::new(ExprKind::Call(
                Box::new(substitute_expr(callee, arg_map, macros)),
                args.iter().map(|a| substitute_expr(a, arg_map, macros)).collect(),
            ), expr.span);
            expand_macros_in_expr(&expanded, macros)
        }
        ExprKind::If { cond, then_body, else_body } => Expr::new(ExprKind::If {
            cond: Box::new(substitute_expr(cond, arg_map, macros)),
            then_body: Box::new(substitute_expr(then_body, arg_map, macros)),
            else_body: else_body.as_ref().map(|e| Box::new(substitute_expr(e, arg_map, macros))),
        }, expr.span),
        ExprKind::FieldAccess(obj, field) => Expr::new(ExprKind::FieldAccess(Box::new(substitute_expr(obj, arg_map, macros)), field.clone()), expr.span),
        ExprKind::MethodCall(obj, method, args) => Expr::new(ExprKind::MethodCall(
            Box::new(substitute_expr(obj, arg_map, macros)),
            method.clone(),
            args.iter().map(|a| substitute_expr(a, arg_map, macros)).collect(),
        ), expr.span),
        ExprKind::Index(arr, idx) => Expr::new(ExprKind::Index(
            Box::new(substitute_expr(arr, arg_map, macros)),
            Box::new(substitute_expr(idx, arg_map, macros)),
        ), expr.span),
        ExprKind::StructLit { name, fields } => Expr::new(ExprKind::StructLit {
            name: name.clone(),
            fields: fields.iter().map(|(n, e)| (n.clone(), substitute_expr(e, arg_map, macros))).collect(),
        }, expr.span),
        ExprKind::Match { scrutinee, arms } => Expr::new(ExprKind::Match {
            scrutinee: Box::new(substitute_expr(scrutinee, arg_map, macros)),
            arms: arms.iter().map(|(pats, guard, e)| (pats.clone(), guard.as_ref().map(|g| substitute_expr(g, arg_map, macros)), substitute_expr(e, arg_map, macros))).collect(),
        }, expr.span),
        ExprKind::Range(l, r) => Expr::new(ExprKind::Range(
            Box::new(substitute_expr(l, arg_map, macros)),
            Box::new(substitute_expr(r, arg_map, macros)),
        ), expr.span),
        ExprKind::ArrayLit(elems) => Expr::new(ExprKind::ArrayLit(elems.iter().map(|e| substitute_expr(e, arg_map, macros)).collect()), expr.span),
        _ => expr.clone(),
    }
}

fn expand_macros_program(program: &Program) -> Program {
    let mut macros: HashMap<String, (Vec<String>, Vec<Stmt>)> = HashMap::new();
    for s in &program.stmts {
        if let StmtKind::Macro { name, params, body } = &s.kind {
            macros.insert(name.clone(), (params.clone(), body.clone()));
        }
    }
    if macros.is_empty() {
        return program.clone();
    }
    let mut new_stmts = Vec::new();
    for s in &program.stmts {
        match &s.kind {
            StmtKind::Macro { .. } => {}
            StmtKind::Fn { name, generics, params, ret, body } => {
                let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect();
                new_stmts.push(Stmt::new(StmtKind::Fn { name: name.clone(), generics: generics.clone(), params: params.clone(), ret: ret.clone(), body: new_body }, s.span));
            }
            StmtKind::Expr(e) => { new_stmts.push(Stmt::new(StmtKind::Expr(expand_macros_in_expr(e, &macros)), s.span)); }
            StmtKind::Let { name, ty, value } => { new_stmts.push(Stmt::new(StmtKind::Let { name: name.clone(), ty: ty.clone(), value: expand_macros_in_expr(value, &macros) }, s.span)); }
            StmtKind::Assign { name, value } => { new_stmts.push(Stmt::new(StmtKind::Assign { name: name.clone(), value: expand_macros_in_expr(value, &macros) }, s.span)); }
            StmtKind::Return(e) => { new_stmts.push(Stmt::new(StmtKind::Return(e.as_ref().map(|e| expand_macros_in_expr(e, &macros))), s.span)); }
            StmtKind::While { cond, body } => {
                let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect();
                new_stmts.push(Stmt::new(StmtKind::While { cond: expand_macros_in_expr(cond, &macros), body: new_body }, s.span));
            }
            StmtKind::For { var, iter, body } => {
                let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect();
                new_stmts.push(Stmt::new(StmtKind::For { var: var.clone(), iter: expand_macros_in_expr(iter, &macros), body: new_body }, s.span));
            }
            StmtKind::If { cond, then_body, else_body } => {
                let new_then: Vec<Stmt> = then_body.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect();
                let new_else = else_body.as_ref().map(|v| v.iter().flat_map(|s| expand_macros_in_stmt(s, &macros)).collect());
                new_stmts.push(Stmt::new(StmtKind::If { cond: expand_macros_in_expr(cond, &macros), then_body: new_then, else_body: new_else }, s.span));
            }
            _ => new_stmts.push(s.clone()),
        }
    }
    Program { stmts: new_stmts }
}

fn expand_macros_in_stmt(stmt: &Stmt, macros: &HashMap<String, (Vec<String>, Vec<Stmt>)>) -> Vec<Stmt> {
    match &stmt.kind {
        StmtKind::Expr(e) => {
            if let ExprKind::Call(callee, args) = &*e.kind {
                if let ExprKind::Ident(mname) = &*callee.kind {
                    if let Some((params, body)) = macros.get(mname) {
                        if body.len() >= 2 {
                            return expand_macro_call_inline(params, body, args, None, macros);
                        }
                    }
                }
            }
            vec![Stmt::new(StmtKind::Expr(expand_macros_in_expr(e, macros)), stmt.span)]
        }
        StmtKind::Let { name, ty, value } => {
            if let ExprKind::Call(callee, args) = &*value.kind {
                if let ExprKind::Ident(mname) = &*callee.kind {
                    if let Some((params, body)) = macros.get(mname) {
                        if body.len() >= 2 {
                            return expand_macro_call_inline(params, body, args, Some(name.clone()), macros);
                        }
                    }
                }
            }
            vec![Stmt::new(StmtKind::Let { name: name.clone(), ty: ty.clone(), value: expand_macros_in_expr(value, macros) }, stmt.span)]
        }
        StmtKind::Assign { name, value } => vec![Stmt::new(StmtKind::Assign { name: name.clone(), value: expand_macros_in_expr(value, macros) }, stmt.span)],
        StmtKind::Return(e) => vec![Stmt::new(StmtKind::Return(e.as_ref().map(|e| expand_macros_in_expr(e, macros))), stmt.span)],
        StmtKind::While { cond, body } => {
            let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect();
            vec![Stmt::new(StmtKind::While { cond: expand_macros_in_expr(cond, macros), body: new_body }, stmt.span)]
        }
        StmtKind::For { var, iter, body } => {
            let new_body: Vec<Stmt> = body.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect();
            vec![Stmt::new(StmtKind::For { var: var.clone(), iter: expand_macros_in_expr(iter, macros), body: new_body }, stmt.span)]
        }
        StmtKind::If { cond, then_body, else_body } => {
            let new_then: Vec<Stmt> = then_body.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect();
            let new_else = else_body.as_ref().map(|v| v.iter().flat_map(|s| expand_macros_in_stmt(s, macros)).collect());
            vec![Stmt::new(StmtKind::If { cond: expand_macros_in_expr(cond, macros), then_body: new_then, else_body: new_else }, stmt.span)]
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
        match &s.kind {
            StmtKind::Let { name, ty: _, value } => {
                let substituted_val = substitute_expr(value, &arg_map, macros);
                result.push(Stmt::new(StmtKind::Let { name: name.clone(), ty: Type::Inferred, value: substituted_val }, s.span));
            }
            StmtKind::Return(Some(e)) => {
                let substituted = substitute_expr(e, &arg_map, macros);
                if let Some(var) = &return_var {
                    result.push(Stmt::new(StmtKind::Let { name: var.clone(), ty: Type::Inferred, value: substituted }, s.span));
                } else {
                    result.push(Stmt::new(StmtKind::Return(Some(substituted)), s.span));
                }
            }
            StmtKind::Assign { name, value } => {
                let substituted = substitute_expr(value, &arg_map, macros);
                result.push(Stmt::new(StmtKind::Assign { name: name.clone(), value: substituted }, s.span));
            }
            StmtKind::If { cond, then_body, else_body } => {
                let substituted_cond = substitute_expr(cond, &arg_map, macros);
                let new_then: Vec<Stmt> = then_body.iter().flat_map(|s| expand_macro_call_inline_single(s, &arg_map, return_var.clone(), macros)).collect();
                let new_else = else_body.as_ref().map(|v| v.iter().flat_map(|s| expand_macro_call_inline_single(s, &arg_map, return_var.clone(), macros)).collect());
                result.push(Stmt::new(StmtKind::If { cond: substituted_cond, then_body: new_then, else_body: new_else }, s.span));
            }
            StmtKind::Expr(e) => {
                let substituted = substitute_expr(e, &arg_map, macros);
                result.push(Stmt::new(StmtKind::Expr(substituted), s.span));
            }
            _ => result.push(s.clone()),
        }
    }
    result
}

fn expand_macro_call_inline_single(stmt: &Stmt, arg_map: &HashMap<String, Expr>, return_var: Option<String>, macros: &HashMap<String, (Vec<String>, Vec<Stmt>)>) -> Vec<Stmt> {
    match &stmt.kind {
        StmtKind::Let { name, ty: _, value } => {
            let substituted_val = substitute_expr(value, arg_map, macros);
            vec![Stmt::new(StmtKind::Let { name: name.clone(), ty: Type::Inferred, value: substituted_val }, stmt.span)]
        }
        StmtKind::Return(Some(e)) => {
            let substituted = substitute_expr(e, arg_map, macros);
            if let Some(var) = &return_var {
                vec![Stmt::new(StmtKind::Let { name: var.clone(), ty: Type::Inferred, value: substituted }, stmt.span)]
            } else {
                vec![Stmt::new(StmtKind::Return(Some(substituted)), stmt.span)]
            }
        }
        StmtKind::Assign { name, value } => {
            let substituted = substitute_expr(value, arg_map, macros);
            vec![Stmt::new(StmtKind::Assign { name: name.clone(), value: substituted }, stmt.span)]
        }
        StmtKind::Expr(e) => {
            let substituted = substitute_expr(e, arg_map, macros);
            vec![Stmt::new(StmtKind::Expr(substituted), stmt.span)]
        }
        _ => vec![stmt.clone()],
    }
}

/// Compile a program to a native executable at `out_path` using LLVM + system
/// toolchain (llc / clang).
pub fn compile_to_executable(program: &Program, out_path: &str, src_path: &str, src: &str) {
    let program = expand_macros_program(program);
    let context = Context::create();
    let mut cg = Codegen::new(&context, &program);
    cg.path = src_path.to_string();
    cg.src = src.to_string();

    // Register struct fields and enum variants
    for s in &program.stmts {
        match &s.kind {
            StmtKind::Struct { name, fields, .. } => {
                cg.struct_fields.insert(name.clone(), fields.clone());
            }
            StmtKind::Enum { name, variants } => {
                cg.enum_variants.insert(name.clone(), variants.clone());
            }
            _ => {}
        }
    }

    // Pre-declare all functions (including impl methods)
    for s in &program.stmts {
        match &s.kind {
            StmtKind::Fn { name, params, ret, .. } => {
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
            StmtKind::Impl { methods, .. } => {
                for m in methods {
                    if let StmtKind::Fn { name, params, ret, .. } = &m.kind {
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
        match &s.kind {
            StmtKind::Fn { params, body, ret, .. } => {
                let name = if let StmtKind::Fn { name, .. } = &s.kind { name } else { unreachable!() };
                let func = cg.functions[name];
                let entry = context.append_basic_block(func, "entry");
                cg.builder.position_at_end(entry);
                cg.named.clear();
                cg.var_struct_type.clear();
                // A fresh scope per function: the checker must see this
                // function's locals, not the previous function's.
                cg.types.pop_scope();
                cg.types.push_scope();

                let mut terminated = false;
                cg.open_block();
                for (i, (pname, pty)) in params.iter().enumerate() {
                    // Every parameter arrives as an i64; its declared type says
                    // how to unpack it. Tagging them all "int" meant a `float`
                    // or `string` parameter was read back as an integer.
                    let arg = func.get_nth_param(i as u32).unwrap().into_int_value();
                    cg.bind_local(pname, pty, &CgValue::Int(arg));
                    if let Type::Named(t) = pty {
                        if cg.struct_fields.contains_key(t) {
                            cg.var_struct_type.insert(pname.clone(), t.clone());
                        }
                    }
                }
                for s in body {
                    cg.gen_stmt(s, func, &mut terminated);
                }
                // falling off the end: let go of the locals before returning
                if !terminated { cg.close_block(); } else { cg.owned.pop(); }
                if !terminated {
                    if matches!(ret, Type::Unit) {
                        cg.builder.build_return(None).unwrap();
                    } else {
                        cg.builder.build_return(Some(&cg.i64.const_int(0, false))).unwrap();
                    }
                }
            }
            StmtKind::Impl { methods, type_name, .. } => {
                for m in methods {
                    if let StmtKind::Fn { name, params, body, ret, .. } = &m.kind {
                        let func = cg.functions[name];
                        let entry = context.append_basic_block(func, "entry");
                        cg.builder.position_at_end(entry);
                        cg.named.clear();
                        cg.var_struct_type.clear();
                        // A fresh scope per function: the checker must see this
                        // function's locals, not the previous function's.
                        cg.types.pop_scope();
                        cg.types.push_scope();
                        let mut terminated = false;
                        cg.open_block();
                        for (i, (pname, pty)) in params.iter().enumerate() {
                            // The parser types a `self` parameter as `Self`;
                            // the impl block is what says which type that is.
                            let declared = match pname.as_str() {
                                "self" | "&self" | "&mut" => Type::Named(type_name.clone()),
                                _ => pty.clone(),
                            };
                            let arg = func.get_nth_param(i as u32).unwrap().into_int_value();
                            cg.bind_local(pname, &declared, &CgValue::Int(arg));
                            if let Type::Named(t) = declared {
                                if cg.struct_fields.contains_key(&t) {
                                    cg.var_struct_type.insert(pname.clone(), t);
                                }
                            }
                        }
                        for s in body {
                            cg.gen_stmt(s, func, &mut terminated);
                        }
                        // falling off the end: let go of the locals before returning
                        if !terminated { cg.close_block(); } else { cg.owned.pop(); }
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
    let has_user_main = program.stmts.iter().any(|s| matches!(&s.kind, StmtKind::Fn { name, .. } if name == "main"));

    let main_type = context.i64_type().fn_type(&[], false);
    let main_fn = cg.module.add_function("main", main_type, None);
    let entry = context.append_basic_block(main_fn, "entry");
    cg.builder.position_at_end(entry);
    cg.named.clear();
    cg.var_struct_type.clear();
    // A fresh scope per function: the checker must see this
    // function's locals, not the previous function's.
    cg.types.pop_scope();
    cg.types.push_scope();

    if has_user_main {
        let user_main = cg.module.get_function("_zarrin_main").unwrap();
        cg.builder.build_call(user_main, &[], "").unwrap();
        cg.builder.build_return(Some(&cg.i64.const_int(0, false))).unwrap();
    } else {
        let mut terminated = false;
        for s in &program.stmts {
            match &s.kind {
                StmtKind::Fn { .. } | StmtKind::Struct { .. } | StmtKind::Enum { .. } | StmtKind::Trait { .. }
                | StmtKind::Macro { .. } | StmtKind::ExternFn { .. } | StmtKind::Impl { .. } => {}
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
