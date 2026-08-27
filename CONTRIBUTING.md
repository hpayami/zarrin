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

Arithmetic that can fail is checked on both sides, and the two carry the same
message: `gen_checked` in the native backend uses LLVM's overflow intrinsics,
and the interpreter uses Rust's `checked_*`. Anything added here needs both, or
the backends part company at exactly the point a program goes wrong.

Every value carries the 16-byte header, wherever it lives. A frame allocation
gets one too, with the immortal count, so retain and release are no-ops on it.
The header is not optional: a function compiled once cannot know whether its
caller built the argument on the frame or on the heap, and one that retains a
parameter reads the count 16 bytes before the pointer either way.

A method is registered under its type and its name, `method_key`, not its name
alone — two `impl` blocks may declare the same method and only the type tells
them apart. The call site works out which by asking the checker what the
receiver is.

A block in the native backend is `open_block` / `close_block`: it owns what it
declares and releases it at the end, and the names it binds go back to what
they were. Every construct with a body needs the pair — an `if` branch and a
`match` arm had neither, so a `let` inside one took over the outer variable of
the same name and a string declared there was never released. A path that
leaves early (`return`, `break`) has already released through
`release_all_open`, so it ends its block with `abandon_block` instead.

A string is an address in the native backend, so any question about its
contents goes through `strcmp` — `==`, the ordering operators, and a string
pattern in a `match`. Comparing two addresses asks whether both sides are the
same allocation, which two equal strings built separately never are.

A `for` loop's iterator goes through `for_source`, which answers with bounds,
an array to index if there is one, and whatever the loop itself has to release
afterwards. Both the statement and the expression form of `for` use it, because
they used to disagree — the expression one counted up to the iterator's raw
word. A value built in the loop header belongs to the loop: releasing it is
what keeps `for n in [1, 2, 3]` inside another loop from allocating per turn.

`Type::Named` carries type arguments: `Option<float>` is
`Named("Option", [Float])`. Two named types with the same name are compatible
when one of them has no arguments, which is what lets a signature written
`Option` take any `Option`. The built-in enums are the only ones with type
parameters — `variants.rs` declares them, and their payloads are those
parameters rather than concrete types, so anything reading a payload type has
to substitute first.

Anything printable renders through `gen_to_str`, which takes a raw word and
the type that says how to read it. Adding a new printable type means a case
there, not a new branch in `print` — `print`, `to_string` and interpolation all
go through it, and they used to disagree. Text for the elements of an array is
built one element at a time with the stack unwound in between, because a loop
that allocates per element and never unwinds is how several earlier leaks
started.

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
