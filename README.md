# Zarrin

A new open-source, general-purpose, multi-paradigm programming language.

- **Easy to learn** — clean, readable syntax.
- **Systems-capable** — optional explicit memory control.
- **Safe by default** — static typing with inference, `Result`/`Option`.
- **Fast** — compiles to native machine code via LLVM.

> Status: early scaffold. The compiler currently has a lexer, parser,
> and a built-in tree-walk interpreter. The LLVM backend is planned
> (behind the `llvm` Cargo feature, requires system LLVM).

## Build

```sh
cargo build
```

## Run an example

```sh
cargo run --bin zarrinc -- run examples/hello.zr
cargo run -- bin zarrinc -- emit-ast examples/hello.zr
```

## Tests

```sh
cargo test
```

`compiler/tests/regressions.rs` holds one test per bug that has been found and
fixed, each stating the behaviour that was wrong. `compiler/tests/examples.rs`
runs every program in `examples/` and compares its output against a recorded
golden file. When an example legitimately changes, re-record with:

```sh
UPDATE_GOLDEN=1 cargo test
```

## License

MIT — see [LICENSE](LICENSE).
