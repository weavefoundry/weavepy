//! Parser errors.

use thiserror::Error;
use weavepy_lexer::{LexError, Span};

/// Errors produced by [`crate::parse_module`].
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("lexical error: {0}")]
    Lex(#[from] LexError),
    #[error("unexpected token at {span:?}: {message}")]
    Unexpected { span: Span, message: String },
    #[error("`{feature}` is not implemented in the executable slice (tracked in {rfc})")]
    NotImplemented {
        span: Span,
        feature: &'static str,
        rfc: &'static str,
    },
}
