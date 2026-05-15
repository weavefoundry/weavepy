//! WeavePy virtual machine: object model and bytecode interpreter.
//!
//! The VM owns Python's runtime universe: the heap, frame stack, builtin
//! types, and the dispatch loop that walks bytecode produced by
//! [`weavepy_compiler`]. The current scaffold defines a minimal object enum
//! and an [`Interpreter`] entry point that can be invoked by hosts (REPL,
//! CLI, embedders) without commitments to internal representation.

use thiserror::Error;
use weavepy_compiler::CodeObject;

pub mod object;

/// Runtime errors raised by the interpreter.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("interpreter is not yet implemented")]
    NotImplemented,
    #[error("python exception: {0}")]
    PythonException(String),
}

/// Configuration knobs for the interpreter. Will grow over time.
#[derive(Debug, Clone, Default)]
pub struct InterpreterConfig {
    /// Enable verbose tracing of the dispatch loop.
    pub trace: bool,
}

/// The top-level entry point for executing WeavePy bytecode.
#[derive(Debug, Default)]
pub struct Interpreter {
    config: InterpreterConfig,
}

impl Interpreter {
    pub fn new(config: InterpreterConfig) -> Self {
        Self { config }
    }

    /// Run a module-level [`CodeObject`] to completion and return the result
    /// of the implicit module body (typically [`object::Object::None`]).
    pub fn run_module(&mut self, code: &CodeObject) -> Result<object::Object, RuntimeError> {
        if self.config.trace {
            tracing::debug!(name = %code.name, instructions = code.instructions.len(), "run_module");
        }
        // TODO: drive the dispatch loop over `code.instructions`.
        Ok(object::Object::None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_empty_code_object_to_none() {
        let code = CodeObject {
            name: "<module>".to_owned(),
            ..CodeObject::default()
        };
        let mut interp = Interpreter::default();
        let result = interp.run_module(&code).expect("empty module should run");
        assert!(matches!(result, object::Object::None));
    }
}
