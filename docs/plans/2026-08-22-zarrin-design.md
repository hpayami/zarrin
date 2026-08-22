# Zarrin Language — Design Specification

> Status: Draft v1 (2026-08-22)
> License: MIT
> Implementation language: Rust
> Backend: LLVM (native machine code)

## 1. Vision

`Zarrin` is an open-source, general-purpose, multi-paradigm programming
language. It aims to be:

- **Easy to learn** — clean, readable syntax (Go/Rust-inspired but simpler).
- **Systems-capable** — optional explicit memory control for low-level work.
- **Safe by default** — static typing with type inference, `Result`/`Option`
  error handling instead of exceptions.
- **Fast** — compiles to native machine code via LLVM.

## 2. Project Structure

```
zarrin/
  compiler/      # Rust compiler: lexer, parser, typechecker, LLVM codegen
  std/           # Standard library
  examples/      # Example programs
  docs/          # Language spec + docs
  tests/         # Compiler tests
```

## 3. Language Design

### 3.1 Core types
`int, float, bool, string, array[T], struct, enum, fn`

### 3.2 Features (v1.0)
1. Static typing with type inference
2. Multi-paradigm (imperative / functional / lightweight OOP)
3. `struct` / `enum` / `trait`
4. Pattern matching
5. Generics
6. `Result` / `Option` error handling
7. Modules (`import`)
8. `async` / `await` + channels
9. Macros (metaprogramming)
10. Operator overloading
11. Optional explicit memory control (`raw`)
12. Standard collections

### 3.3 Advanced features
- **FFI** — call C directly (`extern "C"`). In v1.0.
- **Reflection** — runtime type info. In v1.0.
- **Dependent types** — value-dependent types (experimental, post-1.0).

### 3.4 Syntax examples

```zarrin
fn add(a: int, b: int) -> int { return a + b }

let nums = [1, 2, 3]
let doubled = nums.map(|x| x * 2)

struct Point { x: int, y: int }
impl Point {
    fn dist(self) -> float { (self.x*self.x + self.y*self.y).sqrt() }
}

fn divide(a: int, b: int) -> Result<int, string> {
    if b == 0 { return Err("div by zero") }
    return Ok(a / b)
}

match shape {
    Circle(r) => 3.14 * r * r
    Rect(w, h) => w * h
    _ => 0
}

trait Show { fn show(self) -> string }
impl Show for Point { fn show(self) { "(\(self.x),\(self.y))" } }

enum Opt<T> { Some(T), None }

async fn fetch(url: string) -> Result<string, Err> { ... }
let (tx, rx) = channel()
spawn worker(tx)

macro log($expr) { print("\(#expr) = \($expr)") }

extern "C" fn printf(fmt: cstr, ...) -> int
let n = printf("hi\n")

let info = reflect(Point)
for field in info.fields { print(field.name) }
```

## 4. Compiler Architecture (planned)

- **Lexer** — source -> tokens
- **Parser** — tokens -> AST (Pratt / recursive descent)
- **Type checker** — Hindley-Milner-lite inference + trait resolution
- **LLVM codegen** — AST -> LLVM IR -> native object via `inkwell`/`llvm-sys`
- **CLI** — `zarrinc compile`, `--emit=ast|llvm-ir|obj|exe`

## 5. Roadmap

1. Project scaffold + lexer + parser + AST printer (buildable without LLVM).
2. Type checker.
3. LLVM backend (requires system LLVM).
4. Standard library + examples.
5. Advanced features (FFI, reflection, async).
