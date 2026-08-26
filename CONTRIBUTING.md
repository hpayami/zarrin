# Contributing to Zarrin

Thanks for your interest in Zarrin! This is an early-stage, open-source
programming language project.

## Getting started

1. Install Rust (https://rustup.rs).
2. Build: `cargo build`
3. Run an example: `cargo run --bin zarrinc -- run examples/hello.zr`
4. Run the tests: `cargo test`

The LLVM backend is optional and off by default; see the README if you want to
build native executables.

## Project layout

- `compiler/src/lexer.rs` — tokenizer, tracks line/column
- `compiler/src/parser.rs` — recursive-descent parser -> AST
- `compiler/src/ast.rs` — AST definitions
- `compiler/src/diagnostic.rs` — source spans and error rendering
- `compiler/src/typecheck.rs` — type checker
- `compiler/src/builtins.rs` — signatures of the built-in functions
- `compiler/src/variants.rs` — enum-variant name resolution
- `compiler/src/codegen.rs` — default backend (tree-walk interpreter)
- `compiler/src/codegen_llvm.rs` — LLVM backend (`llvm` feature)
- `compiler/tests/` — integration tests
- `docs/` — language spec and design notes

## Guidelines

- Keep the language design documented in `docs/`.
- Add tests under `compiler/tests/` for new compiler features.
- Open an issue to discuss large changes before implementing.
- Be kind; this is a learning-friendly project.

## Tests

Anything that runs a program end to end belongs in `compiler/tests/`. The
harness in `common/mod.rs` drives the real binary, so tests exercise the same
path a user does.

- `regressions.rs` — one test per fixed bug, stating the behaviour that was
  wrong. Add to it whenever you fix something.
- `typecheck.rs`, `diagnostics.rs` — the checker and error reporting.
- `examples.rs` — golden output for everything in `examples/`. If you change an
  example, re-record with `UPDATE_GOLDEN=1 cargo test`.

A new builtin needs an entry in `compiler/src/builtins.rs` and a call in the
`every_builtin_is_known_to_the_checker` test, which fails otherwise.

Three backends have to agree on the language's semantics — the type checker,
the interpreter and the LLVM backend. When you change behaviour in one, check
the other two.
