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

impl ParseError {
    /// Byte offset into the source where the error was detected. Drives
    /// the `SyntaxError` `lineno`/`offset` computed at the raise site.
    pub fn byte_offset(&self) -> u32 {
        match self {
            ParseError::Lex(e) => e.byte_offset(),
            ParseError::Unexpected { span, .. } | ParseError::NotImplemented { span, .. } => {
                span.start.0
            }
        }
    }

    /// The bare message for a CPython-shaped `SyntaxError.msg`, without
    /// the `"lexical error: "` wrapper that the [`Display`] impl adds for
    /// diagnostics. For a lexer error this is exactly CPython's text
    /// (e.g. `invalid character '€' (U+20AC)`).
    ///
    /// [`Display`]: std::fmt::Display
    pub fn syntax_message(&self) -> String {
        match self {
            ParseError::Lex(e) => e.to_string(),
            ParseError::Unexpected { message, .. } => message.clone(),
            ParseError::NotImplemented { .. } => self.to_string(),
        }
    }
}
