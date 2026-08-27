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
| Lexer and parser | errors report file, line and column |
| Type checker | runs before `run` and `build`, not just under `check` |
| Interpreter | the default backend, a tree-walk evaluator |
| LLVM backend | behind the `llvm` Cargo feature, see below |

The language has functions, `let` bindings with inference, structs, enums with
payloads, traits and `impl`, `match` with guards and multi-patterns, macros,
`extern` functions, arrays, ranges, string interpolation, and the usual control
flow.

A `match` must cover its scrutinee: every variant of an enum, both booleans, or
a `_` arm. An arm carrying a guard covers nothing, since the guard can turn it
down.

A struct literal names its fields, and may list them in any order. Leaving one
out, giving one twice, or naming one the struct does not have is a type error
that says which field it means.

`print` and `to_string` render a value the way it is written: an array as
`[1, 2, 3]`, a struct as `Point { x: 1, y: 2 }` with its fields in declaration
order, an enum as its variant name and payload.

`+` joins two strings; a number becomes text through `to_string` or
interpolation. A `fn` type can be written in a signature, but a function cannot
yet be passed as a value or called through one, and the checker says so.

Strings compare by their characters: `==` and `!=`, and `<` through `>=` in the
order they sort. A string pattern in a `match` is matched the same way.

A range is a value of its own type, `range`: it can be bound, passed, returned,
printed as `1..4`, and walked later. A `for` walks a range, an array, or an
integer — counting from zero — and the loop variable takes the element's type.
Anything else is a type error rather than something each backend decides for
itself.

`Option` and `Result` carry the type of what they hold: `Some(1.5)` is an
`Option<float>`, and that is what makes it print as `Some(1.5)` rather than as
the bits of a float. Write the argument where a signature needs it —
`Option<float>`, `Result<int, string>` — or leave it off, and a plain `Option`
accepts any payload but cannot say what it holds. `unwrap` and a `match` arm
both hand back the payload at that type.

Integer arithmetic is checked. Overflow stops the program with a diagnostic
rather than wrapping, the same way an index out of range or a zero divisor
does. Dividing an integer by zero stops it too. Float arithmetic follows IEEE:
`1.0 / 0.0` is `inf`, and nothing about it is checked.

`panic` is a failure like any other: the message, the position, stderr, exit
status 1. An `extern fn` may be declared but not yet called — no backend
implements one, and calling it is a type error rather than a surprise later.

Errors — syntax, type, and run time — are reported against the source with a
line, a column and the offending line quoted. A type error points at the
subexpression that caused it, down to the individual argument. Run-time
failures are reported against the statement, which is as far as the value being
blamed can be traced.

Strings are counted and indexed in characters, not bytes: `len("héllo")` is 5,
`char_at(s, 1)` is `é`, and `substring(s, 0, len(s))` is `s` whatever is in it.
An index past the last character is an error, as it is for an array.

A float may be written with an exponent: `1.0e3`, `2.5e-2`, `5e+1`.

String literals take `\n`, `\t`, `\r`, `\"`, `\\`, and `\{` / `\}` for a
literal brace — an unescaped `{` starts an interpolation. There is no `\0`: the
native backend uses NUL-terminated strings, so an embedded NUL would behave
differently there.

Generic functions — `fn id<T>(x: T) -> T` — work on both backends. The checker
works out what each type parameter stands for at every call, and a pass before
code generation gives each set of type arguments its own copy of the function,
so neither backend ever meets a `T`. A function that recurses at a *growing*
type has no finite set of copies and is rejected with a diagnostic rather than
expanded until memory runs out. Generic structs parse but are not supported.

Memory in the native backend is reference counted. A value that cannot outlive
the expression building it goes on the frame instead and is released when the
statement ends; everything else carries a count in the 16 bytes ahead of it,
gains an owner when a name or an aggregate takes it, and is freed — along with
whatever it owns — when the last one lets go. Reference cycles are not
constructible in Zarrin, so nothing is left behind by construction. String
constants carry a count that never moves.

Not covered: a value produced and then discarded without ever being named, and
one returned into an expression that drops it. Both leak.

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
