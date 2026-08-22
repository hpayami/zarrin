# Contributing to Zarrin

Thanks for your interest in Zarrin! This is an early-stage, open-source
programming language project.

## Getting started

1. Install Rust (https://rustup.rs).
2. Build: `cargo build`
3. Run an example: `cargo run --bin zarrinc -- run examples/hello.zr`

## Project layout

- `compiler/src/lexer.rs` — tokenizer
- `compiler/src/parser.rs` — recursive-descent parser -> AST
- `compiler/src/ast.rs` — AST definitions
- `compiler/src/codegen.rs` — default backend (tree-walk interpreter)
- `docs/` — language spec and design notes

## Guidelines

- Keep the language design documented in `docs/`.
- Add tests under `tests/` for new compiler features.
- Open an issue to discuss large changes before implementing.
- Be kind; this is a learning-friendly project.
