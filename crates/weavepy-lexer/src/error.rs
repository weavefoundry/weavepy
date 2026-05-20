//! Lexer errors.

use thiserror::Error;

/// Errors produced by [`crate::tokenize`].
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum LexError {
    #[error("unterminated string literal at byte {pos}")]
    UnterminatedString { pos: u32 },
    #[error("invalid character {ch:?} at byte {pos}")]
    InvalidChar { ch: char, pos: u32 },
    #[error("inconsistent indentation at byte {pos}")]
    InconsistentIndent { pos: u32 },
    #[error("indentation does not match any outer level at byte {pos}")]
    UnknownDedent { pos: u32 },
    #[error("invalid numeric literal at byte {pos}: {message}")]
    InvalidNumber { pos: u32, message: String },
    #[error("invalid string prefix {prefix:?} at byte {pos}")]
    InvalidStringPrefix { pos: u32, prefix: String },
    #[error("line continuation `\\` must be followed by a newline at byte {pos}")]
    StrayBackslash { pos: u32 },
    #[error("unexpected EOF at byte {pos}: {message}")]
    UnexpectedEof { pos: u32, message: String },
}
