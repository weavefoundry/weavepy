//! Canonicalize WeavePy and CPython outputs into comparable forms.
//!
//! Both sides are reduced to a shared shape — currently `(kind, text)` for
//! tokens — so the diff is independent of representation details (byte
//! offsets vs `(line, col)`, internal enum names, etc.). As WeavePy's
//! output grows richer, this is the module that decides which fields are
//! eligible for comparison.

use weavepy::lexer::{Token, TokenKind};

use crate::oracle::OracleToken;

/// A token in canonical form, used for diffing WeavePy against the oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedToken {
    /// CPython-style symbolic name (e.g. `"NAME"`, `"NUMBER"`, `"OP"`).
    pub kind: String,
    /// Lexeme text exactly as it appeared in the source.
    pub text: String,
}

/// Convert one WeavePy token into canonical form.
///
/// `source` is the original input — we slice the span out of it rather than
/// trusting the lexer to store text alongside each token.
pub fn from_weavepy(source: &str, token: &Token) -> NormalizedToken {
    let start = token.span.start.0 as usize;
    let end = token.span.end.0 as usize;
    let text = source.get(start..end).unwrap_or("").to_owned();
    NormalizedToken {
        kind: weavepy_kind_name(&token.kind).to_owned(),
        text,
    }
}

/// Convert one CPython oracle token into canonical form.
pub fn from_oracle(token: &OracleToken) -> NormalizedToken {
    NormalizedToken {
        kind: token.kind.clone(),
        text: token.string.clone(),
    }
}

/// Map a WeavePy [`TokenKind`] to its CPython `tok_name` equivalent.
///
/// WeavePy's enum is intentionally a small superset/subset of CPython's
/// for now; the mapping below is the canonical contract between the two
/// for diffing purposes. New WeavePy variants must be added here so the
/// harness can compare them.
pub fn weavepy_kind_name(kind: &TokenKind) -> &'static str {
    match kind {
        TokenKind::Eof => "ENDMARKER",
        TokenKind::Newline => "NEWLINE",
        TokenKind::Indent => "INDENT",
        TokenKind::Dedent => "DEDENT",
        TokenKind::Name => "NAME",
        TokenKind::Number => "NUMBER",
        TokenKind::String => "STRING",
        TokenKind::Op => "OP",
        TokenKind::Comment => "COMMENT",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weavepy::lexer::Span;

    #[test]
    fn weavepy_eof_normalizes_to_endmarker() {
        let tok = Token {
            kind: TokenKind::Eof,
            span: Span::new(0, 0),
        };
        let norm = from_weavepy("", &tok);
        assert_eq!(norm.kind, "ENDMARKER");
        assert_eq!(norm.text, "");
    }

    #[test]
    fn oracle_token_round_trips() {
        let t = OracleToken {
            kind: "NAME".to_owned(),
            string: "foo".to_owned(),
        };
        let norm = from_oracle(&t);
        assert_eq!(norm.kind, "NAME");
        assert_eq!(norm.text, "foo");
    }
}
