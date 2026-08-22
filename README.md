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

## License

MIT — see [LICENSE](LICENSE).
