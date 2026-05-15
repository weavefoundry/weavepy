//! AST-to-bytecode compiler for WeavePy.
//!
//! The compiler walks a [`weavepy_parser::ast::Module`] and produces a
//! [`CodeObject`] containing the bytecode and constant/name pools to be
//! executed by [`weavepy_vm`]. The instruction set deliberately starts as a
//! near-clone of CPython's so that compatibility work can proceed in parallel
//! with experiments in instruction redesign.

use thiserror::Error;
use weavepy_parser::ast::Module;

pub mod bytecode;

/// Errors produced during compilation.
#[derive(Debug, Error)]
pub enum CompileError {
    #[error("compiler is not yet implemented")]
    NotImplemented,
}

/// A compiled Python code object.
///
/// The fields here mirror the subset of `PyCodeObject` we need to emulate. They
/// will grow to include line tables, exception tables, free/cell variable
/// metadata, and so on.
#[derive(Debug, Clone, Default)]
pub struct CodeObject {
    pub name: String,
    pub instructions: Vec<bytecode::Instruction>,
    pub constants: Vec<Constant>,
    pub names: Vec<String>,
}

/// Constants embedded in a [`CodeObject`].
#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    None,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
}

/// Compile a parsed module into a top-level [`CodeObject`].
///
/// Stub implementation: returns an empty code object whose name is `"<module>"`.
pub fn compile_module(_module: &Module) -> Result<CodeObject, CompileError> {
    Ok(CodeObject {
        name: "<module>".to_owned(),
        ..CodeObject::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_empty_module_to_empty_code_object() {
        let module = Module::default();
        let code = compile_module(&module).expect("empty module should compile");
        assert_eq!(code.name, "<module>");
        assert!(code.instructions.is_empty());
        assert!(code.constants.is_empty());
        assert!(code.names.is_empty());
    }
}
