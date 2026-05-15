//! Tokenizer for Python source code.
//!
//! WeavePy targets full lexical compatibility with CPython's tokenizer, including
//! significant indentation, implicit line continuations inside brackets, f-string
//! tokenization (PEP 701), and the full range of numeric and string literal forms.
//!
//! The current implementation is an early skeleton; see [`tokenize`] for the
//! single entry point we expose so far. Surface-level API is expected to stay
//! stable as the implementation fills in.

use std::fmt;

use thiserror::Error;

/// A position in a source file, measured in bytes from the start of the buffer.
///
/// Byte offsets (rather than `(line, column)` pairs) are the canonical form so
/// that downstream stages can build their own line-mapping tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BytePos(pub u32);

/// Half-open byte range `[start, end)` within a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: BytePos,
    pub end: BytePos,
}

impl Span {
    #[inline]
    pub const fn new(start: u32, end: u32) -> Self {
        Self {
            start: BytePos(start),
            end: BytePos(end),
        }
    }
}

/// The kind of token produced by the lexer.
///
/// This is intentionally a stub. The real enum will mirror CPython's
/// `Lib/token.py` token kinds (NAME, NUMBER, STRING, OP, NEWLINE, INDENT,
/// DEDENT, FSTRING_*, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// End of input.
    Eof,
    /// Logical newline that ends a statement.
    Newline,
    /// Increase in indentation level.
    Indent,
    /// Decrease in indentation level.
    Dedent,
    /// Identifier or keyword (the parser distinguishes keywords contextually).
    Name,
    /// Numeric literal.
    Number,
    /// String literal (including bytes literals; f-strings are handled separately).
    String,
    /// Operator or punctuation.
    Op,
    /// Comment, retained as a trivia token for tooling use.
    Comment,
}

/// A single lexical token together with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Errors produced by the tokenizer.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LexError {
    #[error("unterminated string literal at byte {pos}")]
    UnterminatedString { pos: u32 },
    #[error("invalid character {ch:?} at byte {pos}")]
    InvalidChar { ch: char, pos: u32 },
    #[error("inconsistent indentation at byte {pos}")]
    InconsistentIndent { pos: u32 },
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}@{}..{}",
            self.kind, self.span.start.0, self.span.end.0
        )
    }
}

/// Tokenize a complete Python source buffer.
///
/// This is a stub that currently produces only a single [`TokenKind::Eof`] token.
/// It exists so that downstream crates can depend on a stable API while the
/// implementation is fleshed out.
pub fn tokenize(source: &str) -> Result<Vec<Token>, LexError> {
    let end = u32::try_from(source.len()).unwrap_or(u32::MAX);
    Ok(vec![Token {
        kind: TokenKind::Eof,
        span: Span::new(end, end),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_yields_only_eof() {
        let tokens = tokenize("").expect("empty input should tokenize");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Eof);
        assert_eq!(tokens[0].span, Span::new(0, 0));
    }

    #[test]
    fn span_is_constructed_consistently() {
        let s = Span::new(3, 7);
        assert_eq!(s.start.0, 3);
        assert_eq!(s.end.0, 7);
    }
}
