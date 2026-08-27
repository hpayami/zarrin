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
- `compiler/src/monomorphize.rs` — replaces generic functions with concrete copies
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

Heap values in the native backend are reference counted, so two rules matter
when touching allocation there. A stack slot goes through `entry_alloca`, never
`build_alloca` at the point of use — inside a loop the latter grows the stack
once per iteration, which is how several leaks survived being "fixed". And a
pointer handed to `gen_release` must be the start of the allocation, because
the header sits immediately before it; returning an interior pointer, as the
integer formatter once did, hands `free` the wrong address.

Ownership follows one rule: an expression that builds something new hands over
the reference it already has, and one that merely names something existing does
not, so whoever records it takes its own. `produces_owned` answers which.

The LLVM backend erases every value to i64, so it has to know an expression's
type to emit the right code. It asks the type checker: `TypeChecker::type_of`,
against a `TypeEnv` the backend keeps in step with the locals in scope. Do not
add a rule that infers a type from the shape of an expression — that is what
the checker is for, and the two drifted apart every time it was tried.

Generics are gone by the time either backend runs: `monomorphize::expand`
rewrites the program into one with no generic call left in it, so a backend
never has to reason about a type parameter. Two things about that pass are easy
to get wrong. Specialising copies a body, spans and all, so a call site is
identified by the function it sits in *and* its span — a span alone names the
same `id(x)` in every copy of `twice`. And the generic originals stay in the
program: trait `impl` bodies are not walked by the checker, so calls in them are
never rewritten, and removing the originals would break programs that run today.

`every_example_agrees_across_backends` in `llvm_backend.rs` runs every program
in `examples/` through the interpreter and as a native executable and requires
identical output and exit status. Most bugs found in the native backend were
found exactly this way, so an example that exercises a feature is worth more
than one that only demonstrates it. That test needs `--features llvm`; without
it the whole file is skipped.
