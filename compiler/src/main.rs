//! Zarrin compiler CLI.
//!
//! Usage:
//!   zarrinc run <file.zr>        # parse + execute via the built-in interpreter
//!   zarrinc emit-ast <file.zr>   # print the parsed AST
//!   zarrinc check <file.zr>      # parse + type-check placeholder

mod ast;
mod codegen;
mod lexer;
mod parser;
mod typecheck;

#[cfg(feature = "llvm")]
mod codegen_llvm;

use std::fs;
use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: zarrinc <run|emit-ast|check> <file.zr>");
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

    let mut p = parser::Parser::new(&src);
    let program = p.parse_program();

    match cmd {
        "run" => {
            let mut interp = codegen::Interpreter::new(&program);
            interp.run(&program);
        }
        "emit-ast" => {
            println!("{:#?}", program);
        }
        "check" => {
            match typecheck::TypeChecker::check(&program) {
                Ok(()) => println!("OK: all types check out ({} top-level statements)", program.stmts.len()),
                Err(e) => {
                    eprintln!("type error: {}", e);
                    exit(1);
                }
            }
        }
        #[cfg(feature = "llvm")]
        "build" => {
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
