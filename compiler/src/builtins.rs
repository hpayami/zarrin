//! Functions and methods the backends implement directly.
//!
//! These used to be spelled out as `if name == "..."` chains in the
//! interpreter, the LLVM backend and the type checker independently. The three
//! lists drifted: the checker knew six of them, so `len("x")` — which both
//! backends run happily — was rejected as an undefined function. Anything the
//! backends can execute must be listed here.

use crate::ast::Type;

/// Arity and result type of a free-standing builtin, or `None` if `name` is
/// not one. `Type::Inferred` in a result means "not known statically".
pub fn signature(name: &str) -> Option<(usize, Type)> {
    let sig = match name {
        "print" => (1, Type::Unit),
        "panic" => (1, Type::Unit),
        "len" => (1, Type::Int),
        "to_string" => (1, Type::String),
        "int_to_str" => (1, Type::String),
        "substring" => (3, Type::String),
        "contains" => (2, Type::Bool),
        "split" => (2, Type::Array(Box::new(Type::String))),
        "trim" => (1, Type::String),
        "char_at" => (2, Type::String),
        "array_len" => (1, Type::Int),
        // Arrays are untyped at runtime in the LLVM backend, so the element
        // type is not recoverable here. Indexing with `a[i]` is checked
        // properly; these two are the older accessors.
        "array_get" => (2, Type::Inferred),
        "array_set" => (3, Type::Inferred),
        _ => return None,
    };
    Some(sig)
}

/// Result type of a builtin method on `Option`/`Result`, or `None` if the
/// method is not one of them.
/// `type_args` are the enum's, so `unwrap` on an `Option<float>` is a float.
/// On a bare `Option` it is still unknown — nothing said what it holds.
pub fn method_signature(type_name: &str, type_args: &[Type], method: &str) -> Option<(usize, Type)> {
    if type_name != "Option" && type_name != "Result" {
        return None;
    }
    match method {
        "unwrap" => Some((0, type_args.first().cloned().unwrap_or(Type::Inferred))),
        "is_some" | "is_none" | "is_ok" | "is_err" => Some((0, Type::Bool)),
        _ => None,
    }
}

/// Names every backend must handle. Mirrored by `BUILTIN_NAMES` in
/// `compiler/tests/typecheck.rs`, which asserts each one is exercised.
#[allow(dead_code)]
pub const NAMES: &[&str] = &[
    "print", "panic", "len", "to_string", "int_to_str", "substring", "contains",
    "split", "trim", "char_at", "array_len", "array_get", "array_set",
];
