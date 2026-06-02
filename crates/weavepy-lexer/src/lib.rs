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
pub use scanner::{tokenize, tokenize_with_escapes};
pub use token::{BytePos, EscapeWarning, Keyword, Span, StringPrefix, Token, TokenKind};

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

    fn lex_err_msg(src: &str) -> String {
        tokenize(src)
            .expect_err("source should fail to tokenize")
            .to_string()
    }

    // PEP 701 — the lexer must reproduce CPython's f-string diagnostics
    // verbatim; `test_fstring.py` asserts on these exact strings.
    #[test]
    fn fstring_unterminated_literal_messages() {
        // test_not_closing_quotes: bare `f"` / `f'`.
        assert_eq!(lex_err_msg("f\""), "unterminated f-string literal");
        assert_eq!(lex_err_msg("f'"), "unterminated f-string literal");
        // A single-line f-string may not span a newline in its literal part.
        assert_eq!(lex_err_msg("f'abc\n"), "unterminated f-string literal");
    }

    #[test]
    fn fstring_unterminated_triple_messages() {
        // test_not_closing_quotes: `f"""` / `f'''`.
        assert_eq!(
            lex_err_msg("f\"\"\""),
            "unterminated triple-quoted f-string literal"
        );
        assert_eq!(
            lex_err_msg("f'''"),
            "unterminated triple-quoted f-string literal"
        );
    }

    #[test]
    fn fstring_unterminated_field_is_expecting_brace() {
        // An open replacement *expression* that runs off the end is
        // "f-string: expecting '}'" — including when a same-quote that
        // can't find its pair was really the f-string terminator (`f'{3'`).
        assert_eq!(lex_err_msg("f'{3'"), "f-string: expecting '}'");
        assert_eq!(lex_err_msg("f'{3!'"), "f-string: expecting '}'");
        assert_eq!(lex_err_msg("f'{3!s'"), "f-string: expecting '}'");
        assert_eq!(lex_err_msg("f'{(3)'"), "f-string: expecting '}'");
        // `{{` is a brace escape; the trailing `{` then opens an (empty)
        // field that hits raw EOF.
        assert_eq!(lex_err_msg("f'{{{'"), "f-string: expecting '}'");
    }

    #[test]
    fn fstring_unterminated_spec_names_format_specs() {
        // An open *format spec* gets CPython's spec-specific wording. The
        // outer quote inside a single-quoted spec is the terminator (a
        // fill-char must use the other quote), so this also triggers it.
        assert_eq!(
            lex_err_msg("f'{3:'"),
            "f-string: expecting '}', or format specs"
        );
        assert_eq!(
            lex_err_msg("f'{x:>'"),
            "f-string: expecting '}', or format specs"
        );
    }

    #[test]
    fn fstring_same_quote_reuse_is_valid() {
        // PEP 701 quote reuse: a same-quote that *does* find its pair is a
        // genuine nested string, not the terminator.
        assert_eq!(kinds("f'{3 + 'a'}'")[0], TokenKind::String);
        assert_eq!(kinds("f'{3''}'")[0], TokenKind::String); // empty nested str
        // The other quote is literal inside a format spec.
        assert_eq!(kinds("f\"{x:'>10}\"")[0], TokenKind::String);
    }

    #[test]
    fn fstring_newline_in_single_line_spec() {
        // test_newlines_in_format_specifiers: a newline in the format spec
        // of a single-line f-string is rejected (CPython's full wording
        // ends "...for single quoted f-strings")...
        assert_eq!(
            lex_err_msg("f'{1:d\n}'"),
            "f-string: newlines are not allowed in format specifiers for single quoted f-strings"
        );
        // ...but is perfectly legal inside a triple-quoted f-string.
        assert_eq!(kinds("f'''{1:d\n}'''")[0], TokenKind::String);
    }

    #[test]
    fn fstring_bracket_mismatch_messages() {
        // A close that doesn't match the innermost opener names both, like
        // CPython (test_mismatched_parens).
        assert_eq!(
            lex_err_msg("f'{((}'"),
            "closing parenthesis '}' does not match opening parenthesis '('"
        );
        assert_eq!(
            lex_err_msg("f'{a[4}'"),
            "closing parenthesis '}' does not match opening parenthesis '['"
        );
        assert_eq!(
            lex_err_msg("f'{a(4}'"),
            "closing parenthesis '}' does not match opening parenthesis '('"
        );
    }

    #[test]
    fn fstring_unmatched_and_never_closed() {
        // A `)` with nothing open.
        assert_eq!(lex_err_msg("f'{)}'"), "f-string: unmatched ')'");
        assert_eq!(lex_err_msg("f'{)#}'"), "f-string: unmatched ')'");
        // A `#` comment that eats the rest to EOF leaves the innermost
        // bracket "never closed" (the field `{`, or a nested opener).
        assert_eq!(lex_err_msg("f'{1#}'"), "'{' was never closed");
        assert_eq!(lex_err_msg("f'{#}'"), "'{' was never closed");
        assert_eq!(lex_err_msg("f'{(1#}'"), "'(' was never closed");
        // A comment terminated by a newline is *not* "never closed".
        assert_eq!(lex_err_msg("f'{1#}\n'"), "f-string: expecting '}'");
    }

    #[test]
    fn fstring_nested_dict_and_calls_still_valid() {
        // The stack-based scanner must keep accepting balanced nesting.
        assert_eq!(kinds("f'{ {1:2} }'")[0], TokenKind::String);
        assert_eq!(kinds("f'{d[\"k\"]}'")[0], TokenKind::String);
        assert_eq!(kinds("f'{f(a, b)}'")[0], TokenKind::String);
        assert_eq!(kinds("f'{x:{y}}'")[0], TokenKind::String);
    }

    #[test]
    fn fstring_unterminated_nested_string_stays_string_error() {
        // test_unterminated_string: a *different*-quoted nested string is
        // what's unterminated, so CPython keeps the generic wording
        // ("unterminated string literal", which our Display extends with a
        // byte offset — still a regex match for the test).
        assert!(lex_err_msg("f'{\"x'").starts_with("unterminated string literal"));
        assert!(lex_err_msg("f'{(\"x'").starts_with("unterminated string literal"));
    }
}
