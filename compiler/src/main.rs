//! Zarrin compiler CLI.
//!
//! Usage:
//!   zarrinc run <file.zr>        # parse + execute via the built-in interpreter
//!   zarrinc emit-ast <file.zr>   # print the parsed AST
//!   zarrinc check <file.zr>      # parse + type-check placeholder

mod ast;
mod builtins;
mod diagnostic;
mod codegen;
mod lexer;
mod parser;
mod typecheck;
mod variants;

#[cfg(feature = "llvm")]
mod codegen_llvm;

use std::fs;
use std::path::Path;
use std::process::exit;

/// Parse a source file, reporting any syntax error against its text and
/// exiting, rather than unwinding a panic in the user's face.
fn parse_or_exit(path: &Path, src: &str) -> ast::Program {
    match parser::Parser::new(src).and_then(|mut p| p.parse_program()) {
        Ok(program) => program,
        Err(d) => {
            eprint!("{}", d.render(&path.display().to_string(), src));
            exit(1);
        }
    }
}

/// User-facing failures in the interpreter and type checker are still
/// `panic!`s without source positions. Present them as plain errors instead of
/// a Rust panic message; `RUST_BACKTRACE` restores the internal detail.
fn install_error_reporter() {
    if std::env::var_os("RUST_BACKTRACE").is_some() {
        return;
    }
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(|s| s.as_str())
            .or_else(|| info.payload().downcast_ref::<&str>().copied());
        match msg {
            Some(m) => eprintln!("error: {}", m),
            None => default(info),
        }
    }));
}

fn resolve_imports(program: &mut ast::Program, base_dir: &Path, visited: &mut Vec<String>) {
    let mut new_stmts = Vec::new();
    for stmt in &program.stmts {
        if let ast::StmtKind::Import(path) = &stmt.kind {
            if visited.contains(path) {
                continue;
            }
            visited.push(path.clone());
            let full_path = base_dir.join(path);
            let src = match fs::read_to_string(&full_path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error reading import {}: {}", path, e);
                    exit(1);
                }
            };
            let mut imported = parse_or_exit(&full_path, &src);
            let import_dir = full_path.parent().unwrap_or(base_dir);
            resolve_imports(&mut imported, import_dir, visited);
            new_stmts.extend(imported.stmts);
        } else {
            new_stmts.push(stmt.clone());
        }
    }
    program.stmts = new_stmts;
}

/// Type-check before executing or compiling. The checker used to be reachable
/// only through `zarrinc check`, so `run` and `build` happily executed programs
/// it would have rejected.
fn check_or_exit(program: &ast::Program, path: &str, src: &str) {
    if let Err(d) = typecheck::TypeChecker::check(program) {
        eprint!("{}", d.render(path, src));
        exit(1);
    }
}

fn main() {
    install_error_reporter();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: zarrinc <command> <file.zr>");
        eprintln!();
        eprintln!("  run <file.zr>              type-check, then run the interpreter");
        eprintln!("  check <file.zr>            type-check only");
        eprintln!("  emit-ast <file.zr>         print the parsed AST");
        if cfg!(feature = "llvm") {
            eprintln!("  build <file.zr> [-o out]   compile to a native executable");
        } else {
            eprintln!("  build <file.zr> [-o out]   unavailable: rebuild with --features llvm");
        }
        exit(1);
    }
    let cmd = args[1].as_str();
    let file = &args[2];

    let src = match fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {}: {}", file, e);
            exit(1);
        }
    };

    let mut program = parse_or_exit(Path::new(file), &src);

    let base_dir = Path::new(file).parent().unwrap_or(Path::new("."));
    let mut visited = Vec::new();
    resolve_imports(&mut program, base_dir, &mut visited);

    match cmd {
        "run" => {
            check_or_exit(&program, file, &src);
            let mut interp = codegen::Interpreter::new(&program, file, &src);
            interp.run(&program);
        }
        "emit-ast" => {
            println!("{:#?}", program);
        }
        "check" => {
            match typecheck::TypeChecker::check(&program) {
                Ok(()) => println!("OK: all types check out ({} top-level statements)", program.stmts.len()),
                Err(d) => {
                    eprint!("{}", d.render(file, &src));
                    exit(1);
                }
            }
        }
        #[cfg(feature = "llvm")]
        "build" => {
            check_or_exit(&program, file, &src);
            let out = if args.get(3).map(|s| s.as_str()) == Some("-o") {
                args.get(4).cloned().unwrap_or_else(|| "a.out".into())
            } else {
                args.get(3).cloned().unwrap_or_else(|| "a.out".into())
            };
            codegen_llvm::compile_to_executable(&program, &out);
        }
        #[cfg(not(feature = "llvm"))]
        "build" => {
            eprintln!("LLVM backend not compiled. Rebuild with: cargo build --features llvm");
            exit(1);
        }
        other => {
            eprintln!("unknown command: {}", other);
            exit(1);
        }
    }
}
