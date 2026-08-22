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
            println!("parsed OK: {} top-level statements", program.stmts.len());
        }
        other => {
            eprintln!("unknown command: {}", other);
            exit(1);
        }
    }
}
