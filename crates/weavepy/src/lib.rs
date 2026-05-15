//! WeavePy: a high-performance, CPython-compatible Python interpreter written in Rust.
//!
//! This crate is the public Rust entry point for embedding WeavePy. It
//! re-exports the stable surface of the underlying pipeline crates and
//! provides convenience wrappers for the common case of "run this Python
//! source string."
//!
//! # Example
//!
//! ```no_run
//! use weavepy::run_source;
//!
//! run_source("print('hello, weavepy')").unwrap();
//! ```

use thiserror::Error;

pub use weavepy_compiler as compiler;
pub use weavepy_lexer as lexer;
pub use weavepy_parser as parser;
pub use weavepy_vm as vm;

/// Errors that can surface from the high-level [`run_source`] entry point.
#[derive(Debug, Error)]
pub enum Error {
    #[error("parse error: {0}")]
    Parse(#[from] parser::ParseError),
    #[error("compile error: {0}")]
    Compile(#[from] compiler::CompileError),
    #[error("runtime error: {0}")]
    Runtime(#[from] vm::RuntimeError),
}

/// Convenience: parse, compile, and execute a Python source string under a
/// fresh interpreter, discarding the resulting module-level value.
///
/// This is intended for embedding scenarios that only need "run this script."
/// For more control, drive the [`parser`], [`compiler`], and [`vm`] crates
/// directly.
pub fn run_source(source: &str) -> Result<(), Error> {
    let module = parser::parse_module(source)?;
    let code = compiler::compile_module(&module)?;
    let mut interpreter = vm::Interpreter::default();
    let _ = interpreter.run_module(&code)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_empty_source_succeeds() {
        run_source("").expect("empty source should run");
    }
}
