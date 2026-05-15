//! Python parser for WeavePy.
//!
//! Consumes tokens produced by [`weavepy_lexer`] and produces an AST that aims
//! to be a faithful representation of CPython's `ast` module. The AST is
//! exposed through the [`ast`] submodule.

use thiserror::Error;
use weavepy_lexer::{LexError, Span};

pub mod ast;

/// Errors produced while parsing.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("lexical error: {0}")]
    Lex(#[from] LexError),
    #[error("unexpected token at {span:?}: {message}")]
    Unexpected { span: Span, message: String },
    #[error("parser is not yet implemented")]
    NotImplemented,
}

/// Parse a Python source buffer into a [`ast::Module`].
///
/// Stub implementation: tokenizes the input and returns an empty module.
pub fn parse_module(source: &str) -> Result<ast::Module, ParseError> {
    let _tokens = weavepy_lexer::tokenize(source)?;
    Ok(ast::Module { body: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_module() {
        let module = parse_module("").expect("empty source should parse");
        assert!(module.body.is_empty());
    }
}
