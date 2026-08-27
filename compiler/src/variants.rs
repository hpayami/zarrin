//! Deterministic mapping from an enum-variant name to the enum that declares it.
//!
//! Each backend used to answer "which enum declares `Foo`?" by scanning a
//! `HashMap` of enums. With two enums sharing a variant name the answer
//! depended on hash order, so the *same* program could type-check on one run
//! and fail on the next. This index is built from the program in declaration
//! order instead, and reports a collision as an ambiguity rather than silently
//! picking a winner.
//!
//! It also resolves qualified names (`Color::Red`), which the pattern parser
//! produces but no backend previously understood.

use crate::ast::{Program, StmtKind, Type};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Variant {
    pub enum_name: String,
    pub name: String,
    /// Position within its enum; used as the runtime tag by the LLVM backend,
    /// which is compiled out unless the `llvm` feature is on.
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    pub tag: usize,
    pub payload: Vec<Type>,
    /// Whether *any* variant of the owning enum carries a payload. The LLVM
    /// backend picks a boxed or an immediate representation based on this.
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    pub enum_has_payload: bool,
}

pub enum Lookup {
    Unknown,
    Unique(Variant),
    /// Declared by more than one enum. Names are in declaration order.
    Ambiguous(Vec<String>),
}

pub struct VariantIndex {
    by_name: HashMap<String, Vec<Variant>>,
}

impl VariantIndex {
    pub fn build(program: &Program) -> Self {
        let mut idx = VariantIndex { by_name: HashMap::new() };
        // Built-ins go in first, so a user enum reusing these names shows up as
        // an ambiguity instead of quietly shadowing them.
        for (name, variants) in builtin_enums() {
            idx.add_enum(&name, &variants);
        }
        for s in &program.stmts {
            if let StmtKind::Enum { name, variants } = &s.kind {
                idx.add_enum(name, variants);
            }
        }
        idx
    }

    fn add_enum(&mut self, enum_name: &str, variants: &[(String, Vec<Type>)]) {
        let enum_has_payload = variants.iter().any(|(_, p)| !p.is_empty());
        for (tag, (vname, payload)) in variants.iter().enumerate() {
            self.by_name.entry(vname.clone()).or_default().push(Variant {
                enum_name: enum_name.to_string(),
                name: vname.clone(),
                tag,
                payload: payload.clone(),
                enum_has_payload,
            });
        }
    }

    /// Every variant of an enum, in declaration order. Collected from the
    /// index and sorted by tag, because the map's own order is not stable.
    pub fn variants_of(&self, enum_name: &str) -> Vec<Variant> {
        let mut found: Vec<Variant> = self
            .by_name
            .values()
            .flatten()
            .filter(|v| v.enum_name == enum_name)
            .cloned()
            .collect();
        found.sort_by_key(|v| v.tag);
        found
    }

    pub fn is_enum(&self, name: &str) -> bool {
        self.by_name.values().flatten().any(|v| v.enum_name == name)
    }

    /// Accepts both a bare name (`Red`) and a qualified one (`Color::Red`).
    pub fn lookup(&self, name: &str) -> Lookup {
        if let Some((enum_name, vname)) = name.split_once("::") {
            return match self.by_name.get(vname).and_then(|vs| vs.iter().find(|v| v.enum_name == enum_name)) {
                Some(v) => Lookup::Unique(v.clone()),
                None => Lookup::Unknown,
            };
        }
        match self.by_name.get(name) {
            None => Lookup::Unknown,
            Some(vs) if vs.len() == 1 => Lookup::Unique(vs[0].clone()),
            Some(vs) => Lookup::Ambiguous(vs.iter().map(|v| v.enum_name.clone()).collect()),
        }
    }
}

/// `Option` and `Result` are predeclared. Single source of truth: the
/// interpreter and the type checker previously disagreed on their payloads.
pub fn builtin_enums() -> Vec<(String, Vec<(String, Vec<Type>)>)> {
    vec![
        ("Option".to_string(), vec![
            ("Some".to_string(), vec![Type::Inferred]),
            ("None".to_string(), vec![]),
        ]),
        ("Result".to_string(), vec![
            ("Ok".to_string(), vec![Type::Inferred]),
            ("Err".to_string(), vec![Type::Inferred]),
        ]),
    ]
}
