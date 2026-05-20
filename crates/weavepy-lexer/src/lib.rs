//! Tokenizer for Python source code.
//!
//! WeavePy targets lexical compatibility with CPython 3.13. The tokenizer
//! handles significant indentation, implicit line continuation inside
//! brackets, all integer/float/string literal forms, and the full operator
//! and keyword set documented in `Lib/token.py` and `Parser/tokenizer.c`.
//!
//! The lexer is hand-written: scanning is done by [`scanner::Scanner`]
//! over a `&[u8]` byte buffer, with UTF-8 decoded lazily for identifier
//! continuation. Tokens carry byte spans (not `(line, col)` pairs) so
//! downstream phases can build their own line-mapping tables when
//! reporting errors.
//!
//! # Compatibility level
//!
//! - **Tracks CPython** for token kinds, lexeme shapes, and indent stack
//!   semantics.
//! - **Experimental** for the exact `TokenKind` variants — we intentionally
//!   carry more variants than CPython exposes via its `tokenize` module
//!   (e.g. distinct `Plus` and `Star` rather than a single `Op`), which
//!   makes the parser's job easier. The conformance harness normalizes
//!   them back to CPython's `OP` umbrella before diffing.
//! - **Experimental** for the f-string story: PEP 701 interior
//!   tokenization is deferred to RFC 0005. F-strings tokenize as a single
//!   `String` lexeme today.

pub mod error;
pub mod scanner;
pub mod token;

pub use error::LexError;
pub use scanner::tokenize;
pub use token::{BytePos, Keyword, Span, StringPrefix, Token, TokenKind};

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src)
            .expect("tokenize should succeed")
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn empty_source_emits_only_endmarker() {
        let toks = tokenize("").expect("empty source tokenizes");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::Endmarker);
    }

    #[test]
    fn identifier_and_eof() {
        let k = kinds("foo\n");
        assert_eq!(
            k,
            vec![TokenKind::Name, TokenKind::Newline, TokenKind::Endmarker]
        );
    }

    #[test]
    fn integer_kinds() {
        assert_eq!(kinds("42")[0], TokenKind::Number);
        assert_eq!(kinds("0x1f")[0], TokenKind::Number);
        assert_eq!(kinds("0o17")[0], TokenKind::Number);
        assert_eq!(kinds("0b1010")[0], TokenKind::Number);
        assert_eq!(kinds("1_000_000")[0], TokenKind::Number);
    }

    #[test]
    fn float_kinds() {
        assert_eq!(kinds("1.5")[0], TokenKind::Number);
        assert_eq!(kinds(".5")[0], TokenKind::Number);
        assert_eq!(kinds("1.")[0], TokenKind::Number);
        assert_eq!(kinds("1e10")[0], TokenKind::Number);
        assert_eq!(kinds("1.5e-3")[0], TokenKind::Number);
    }

    #[test]
    fn string_kinds() {
        assert_eq!(kinds("'hello'")[0], TokenKind::String);
        assert_eq!(kinds("\"world\"")[0], TokenKind::String);
        assert_eq!(kinds("r\"raw\"")[0], TokenKind::String);
        assert_eq!(kinds("b\"bytes\"")[0], TokenKind::String);
        assert_eq!(kinds("\"\"\"triple\"\"\"")[0], TokenKind::String);
    }

    #[test]
    fn operators_have_distinct_kinds() {
        assert_eq!(kinds("+")[0], TokenKind::Plus);
        assert_eq!(kinds("-")[0], TokenKind::Minus);
        assert_eq!(kinds("**")[0], TokenKind::DoubleStar);
        assert_eq!(kinds("<<")[0], TokenKind::LeftShift);
        assert_eq!(kinds("//")[0], TokenKind::DoubleSlash);
        assert_eq!(kinds("==")[0], TokenKind::EqEqual);
        assert_eq!(kinds("!=")[0], TokenKind::NotEqual);
        assert_eq!(kinds(":=")[0], TokenKind::ColonEqual);
        assert_eq!(kinds("->")[0], TokenKind::RArrow);
        assert_eq!(kinds("...")[0], TokenKind::Ellipsis);
    }

    #[test]
    fn keywords_classified() {
        let toks = tokenize("if True else None").expect("ok");
        assert!(matches!(toks[0].kind, TokenKind::Keyword(Keyword::If)));
        assert!(matches!(toks[1].kind, TokenKind::Keyword(Keyword::True)));
        assert!(matches!(toks[2].kind, TokenKind::Keyword(Keyword::Else)));
        assert!(matches!(toks[3].kind, TokenKind::Keyword(Keyword::None)));
    }

    #[test]
    fn simple_indent_dedent() {
        let src = "if x:\n    y\n";
        let k = kinds(src);
        assert!(k.contains(&TokenKind::Indent));
        assert!(k.contains(&TokenKind::Dedent));
    }

    #[test]
    fn implicit_continuation_inside_brackets() {
        let src = "(1,\n 2,\n 3)\n";
        let k = kinds(src);
        // No NEWLINE between bracketed elements — only at the end.
        let newlines = k.iter().filter(|t| **t == TokenKind::Newline).count();
        assert_eq!(newlines, 1);
    }

    #[test]
    fn comments_are_emitted_as_trivia_then_nl() {
        let toks = tokenize("# hi\nx\n").expect("ok");
        // Comment + NL + Name + Newline + Endmarker
        assert!(toks.iter().any(|t| t.kind == TokenKind::Comment));
    }

    #[test]
    fn backslash_line_continuation() {
        let src = "1 + \\\n 2\n";
        let k = kinds(src);
        let newlines = k.iter().filter(|t| **t == TokenKind::Newline).count();
        assert_eq!(newlines, 1);
    }
}
