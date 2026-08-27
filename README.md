# Zarrin

A new open-source, general-purpose, multi-paradigm programming language.

The goals, from the [design notes](docs/plans/2026-08-22-zarrin-design.md):

- **Easy to learn** — clean, readable syntax.
- **Systems-capable** — optional explicit memory control.
- **Safe by default** — static typing with inference, `Result`/`Option`.
- **Fast** — compiles to native machine code via LLVM.

Those are goals, not a description of the current compiler. What exists today:

| | |
|---|---|
| Lexer and parser | syntax errors report file, line and column |
| Type checker | runs before `run` and `build`, not just under `check` |
| Interpreter | the default backend, a tree-walk evaluator |
| LLVM backend | behind the `llvm` Cargo feature, see below |

The language has functions, `let` bindings with inference, structs, enums with
payloads, traits and `impl`, `match` with guards and multi-patterns, macros,
`extern` functions, arrays, string interpolation, and the usual control flow.

Not yet: generics (parsed, then ignored), escape sequences in string literals,
and source positions on type and runtime errors — only syntax errors carry
them so far.

## Build

```sh
cargo build
```

## Run an example

```sh
cargo run --bin zarrinc -- run examples/hello.zr
```

`zarrinc` takes a command and a file:

| Command | |
|---|---|
| `run <file.zr>` | type-check, then execute with the interpreter |
| `check <file.zr>` | type-check only |
| `emit-ast <file.zr>` | print the parsed AST |
| `build <file.zr> [-o out]` | compile to a native executable (needs the `llvm` feature) |

## Native compilation

The LLVM backend is optional so the project builds without LLVM installed. It
needs LLVM 18, with `LLVM_SYS_180_PREFIX` pointing at the install prefix —
`llc` is taken from there, and `cc` from `PATH` does the linking.

```sh
brew install llvm@18
LLVM_SYS_180_PREFIX=$(brew --prefix llvm@18) cargo build --features llvm
LLVM_SYS_180_PREFIX=$(brew --prefix llvm@18) \
  cargo run --features llvm --bin zarrinc -- build examples/hello.zr -o hello
```

`compiler/tests/llvm_backend.rs` covers this backend, requiring each compiled
program to print exactly what the interpreter prints. Those tests are skipped
unless the feature is on, and on macOS they re-run each executable under Guard
Malloc, which faults on a heap overrun instead of letting it pass unnoticed.

All six programs in `examples/` produce identical output under both backends.

## Tests

```sh
cargo test
```

`compiler/tests/regressions.rs` holds one test per bug that has been found and
fixed, each stating the behaviour that was wrong. `typecheck.rs` and
`diagnostics.rs` cover the checker and error reporting. `examples.rs` runs
every program in `examples/` and compares its output against a recorded golden
file. When an example legitimately changes, re-record with:

```sh
UPDATE_GOLDEN=1 cargo test
```

## License

MIT — see [LICENSE](LICENSE).
