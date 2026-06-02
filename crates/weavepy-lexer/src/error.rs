//! Lexer errors.

use thiserror::Error;

/// Errors produced by [`crate::tokenize`].
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum LexError {
    #[error("unterminated string literal at byte {pos}")]
    UnterminatedString { pos: u32 },
    // PEP 701 f-string diagnostics. CPython distinguishes an unterminated
    // f-string *literal* from an unterminated *replacement field* and uses
    // f-string-specific wording, which several `test_fstring` negative
    // cases assert on verbatim.
    #[error("unterminated f-string literal")]
    UnterminatedFstring { pos: u32 },
    #[error("unterminated triple-quoted f-string literal")]
    UnterminatedTripleFstring { pos: u32 },
    #[error("f-string: expecting '}}'")]
    FstringExpectingBrace { pos: u32 },
    #[error("f-string: expecting '}}', or format specs")]
    FstringExpectingBraceOrSpec { pos: u32 },
    #[error("closing parenthesis '{close}' does not match opening parenthesis '{open}'")]
    FstringParenMismatch { close: char, open: char, pos: u32 },
    #[error("f-string: unmatched '{close}'")]
    FstringUnmatchedParen { close: char, pos: u32 },
    #[error("'{open}' was never closed")]
    BracketNeverClosed { open: char, pos: u32 },
    #[error("f-string: newlines are not allowed in format specifiers for single quoted f-strings")]
    FstringNewlineInSpec { pos: u32 },
    // CPython renders this as `invalid character '€' (U+20AC)` — the
    // glyph in quotes followed by the code point. The byte position is
    // carried separately (see [`LexError::byte_offset`]) and surfaces as
    // the `SyntaxError`'s `lineno`/`offset`, not in the message text.
    #[error("invalid character {ch:?} (U+{codepoint:04X})", codepoint = u32::from(*ch))]
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

impl LexError {
    /// Byte offset into the source where the error was detected. Used to
    /// compute the `SyntaxError` line/column at the raise site.
    pub fn byte_offset(&self) -> u32 {
        match self {
            LexError::UnterminatedString { pos }
            | LexError::UnterminatedFstring { pos }
            | LexError::UnterminatedTripleFstring { pos }
            | LexError::FstringExpectingBrace { pos }
            | LexError::FstringExpectingBraceOrSpec { pos }
            | LexError::FstringParenMismatch { pos, .. }
            | LexError::FstringUnmatchedParen { pos, .. }
            | LexError::BracketNeverClosed { pos, .. }
            | LexError::FstringNewlineInSpec { pos }
            | LexError::InvalidChar { pos, .. }
            | LexError::InconsistentIndent { pos }
            | LexError::UnknownDedent { pos }
            | LexError::InvalidNumber { pos, .. }
            | LexError::InvalidStringPrefix { pos, .. }
            | LexError::StrayBackslash { pos }
            | LexError::UnexpectedEof { pos, .. } => *pos,
        }
    }
}
