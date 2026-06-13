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
    /// Indentation-structure failures — CPython raises these as
    /// `IndentationError` (a `SyntaxError` subclass) rather than plain
    /// `SyntaxError`; see [`ParseError::exception_class`].
    #[error("indentation error at {span:?}: {message}")]
    Indentation { span: Span, message: String },
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
            ParseError::Unexpected { span, .. }
            | ParseError::Indentation { span, .. }
            | ParseError::NotImplemented { span, .. } => span.start.0,
        }
    }

    /// Byte offset one past the end of the offending region, when the
    /// error is anchored to a span wider than a point. Drives
    /// `SyntaxError.end_offset` (the `^^^^` underline width).
    pub fn byte_end_offset(&self) -> u32 {
        match self {
            ParseError::Lex(e) => e.byte_offset(),
            ParseError::Unexpected { span, .. }
            | ParseError::Indentation { span, .. }
            | ParseError::NotImplemented { span, .. } => span.end.0,
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
            ParseError::Unexpected { message, .. } | ParseError::Indentation { message, .. } => {
                message.clone()
            }
            ParseError::NotImplemented { .. } => self.to_string(),
        }
    }

    /// Which exception class CPython raises for this failure:
    /// `"IndentationError"` for indentation-structure errors,
    /// `"TabError"` for inconsistent tab/space mixing, `"SyntaxError"`
    /// otherwise.
    pub fn exception_class(&self) -> &'static str {
        match self {
            ParseError::Indentation { .. } | ParseError::Lex(LexError::UnknownDedent { .. }) => {
                "IndentationError"
            }
            ParseError::Lex(LexError::InconsistentIndent { .. }) => "TabError",
            _ => "SyntaxError",
        }
    }
}
