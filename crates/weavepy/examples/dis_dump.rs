//! Tiny example: dump compiled bytecode for an arbitrary source file.
//!
//! Usage: `cargo run -p weavepy --example dis_dump -- path/to/file.py`

use std::{env, fs};

fn dump(code: &weavepy::compiler::CodeObject, depth: usize) {
    let pad = "  ".repeat(depth);
    println!(
        "{pad}<code {}> varnames={:?} cellvars={:?} freevars={:?}",
        code.name, code.varnames, code.cellvars, code.freevars,
    );
    for (off, ins) in code.instructions.iter().enumerate() {
        println!("{pad}  {off:>4} {:<22?} {}", ins.op, ins.arg);
    }
    for c in &code.constants {
        if let weavepy::compiler::Constant::Code(inner) = c {
            dump(inner, depth + 1);
        }
    }
}

fn main() {
    let path = env::args().nth(1).expect("usage: dis_dump <file>");
    let src = fs::read_to_string(&path).expect("read");
    let module = weavepy::parser::parse_module(&src).expect("parse");
    let code = weavepy::compiler::compile_module_with_filename(&module, &path).expect("compile");
    dump(&code, 0);
}
