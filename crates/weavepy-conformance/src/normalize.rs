//! Canonicalize WeavePy and CPython outputs into comparable forms.
//!
//! Both sides are reduced to a shared shape — `(kind, text)` for tokens,
//! flattened text for AST dumps and `dis` listings — so the diff is
//! independent of representation details (byte offsets vs `(line, col)`,
//! whitespace, internal enum names). As WeavePy's output grows richer,
//! this is the module that decides which fields are eligible for
//! comparison.

use weavepy::lexer::Token;

use crate::oracle::OracleToken;

/// A token in canonical form, used for diffing WeavePy against the oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedToken {
    /// CPython-style symbolic name (e.g. `"NAME"`, `"NUMBER"`, `"OP"`).
    pub kind: String,
    /// Lexeme text exactly as it appeared in the source.
    pub text: String,
}

/// Strip CPython oracle artefacts that WeavePy intentionally doesn't
/// emit, then return the comparable subsequence:
///
/// - **`ENCODING`** is the leading source-encoding marker CPython's
///   `tokenize.tokenize` emits (`(56, 'utf-8', (0, 0), (0, 0), '')`).
///   WeavePy is always UTF-8 and skips the marker; drop it from the
///   oracle side.
/// - **`NL`** is the non-significant newline (blank lines, lines inside
///   brackets). WeavePy emits these too but the parser filters them
///   immediately; the conformance harness compares the *significant*
///   token stream.
/// - **`COMMENT`** is intentionally retained — both sides emit them.
pub fn normalize_oracle_tokens(toks: &[OracleToken]) -> Vec<NormalizedToken> {
    toks.iter()
        .filter(|t| !matches!(t.kind.as_str(), "ENCODING" | "NL"))
        .map(from_oracle)
        .collect()
}

/// Drop the WeavePy artefacts that the oracle doesn't emit, mirroring
/// [`normalize_oracle_tokens`].
pub fn normalize_weavepy_tokens(source: &str, toks: &[Token]) -> Vec<NormalizedToken> {
    use weavepy::lexer::TokenKind;
    toks.iter()
        .filter(|t| !matches!(t.kind, TokenKind::Nl))
        .map(|t| from_weavepy(source, t))
        .collect()
}

/// Convert one WeavePy token into canonical form.
///
/// `source` is the original input — we slice the span out of it rather than
/// trusting the lexer to store text alongside each token.
///
/// The text for `INDENT`/`DEDENT`/`NEWLINE`/`ENDMARKER` is normalised
/// to the empty string — both implementations agree on the *event*
/// but disagree on the captured lexeme (oracle stores the actual
/// whitespace; WeavePy stores nothing). For comparison purposes the
/// text isn't meaningful.
pub fn from_weavepy(source: &str, token: &Token) -> NormalizedToken {
    use weavepy::lexer::TokenKind;
    let start = token.span.start.0 as usize;
    let end = token.span.end.0 as usize;
    let raw = source.get(start..end).unwrap_or("");
    let text = if matches!(
        token.kind,
        TokenKind::Indent | TokenKind::Dedent | TokenKind::Newline | TokenKind::Endmarker
    ) {
        String::new()
    } else {
        raw.to_owned()
    };
    NormalizedToken {
        kind: token.kind.symbolic_name().to_owned(),
        text,
    }
}

/// Convert one CPython oracle token into canonical form. The text
/// for whitespace-only token kinds (`INDENT`, `DEDENT`, `NEWLINE`,
/// `ENDMARKER`) is dropped — see [`from_weavepy`] for the rationale.
pub fn from_oracle(token: &OracleToken) -> NormalizedToken {
    let text = match token.kind.as_str() {
        "INDENT" | "DEDENT" | "NEWLINE" | "ENDMARKER" => String::new(),
        _ => token.string.clone(),
    };
    NormalizedToken {
        kind: token.kind.clone(),
        text,
    }
}

/// Reduce an `ast.dump`-style string to a comparison-friendly form.
///
/// We strip trailing whitespace from each line and collapse runs of
/// blank lines so cosmetic differences (e.g. one side uses
/// `indent=2`, the other none) don't drown the diff. We also drop the
/// `lineno=…, col_offset=…` fields CPython attaches with
/// `include_attributes=True` — WeavePy doesn't surface them yet, plus
/// every keyword-argument field whose value is the default (`is_async=0`,
/// `level=0`, `type_ignores=[]`, `type_comment=None`, etc.) so the two
/// sides agree on field set even when one omits a default.
pub fn canonical_ast(dump: &str) -> String {
    let stripped = strip_lineinfo(dump);
    let stripped = strip_default_kwargs(&stripped);
    let mut out = String::with_capacity(stripped.len());
    for ch in stripped.chars() {
        if !ch.is_whitespace() {
            out.push(ch);
        }
    }
    // Collapse trailing commas: `,)` → `)` and `,]` → `]`.
    let mut collapsed = String::with_capacity(out.len());
    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ',' {
            if matches!(chars.peek(), Some(')') | Some(']')) {
                continue;
            }
        }
        collapsed.push(c);
    }
    collapsed
}

/// Reduce a `dis`-style listing to a comparison-friendly form.
///
/// CPython's `dis.dis` output looks like:
///
/// ```text
///   1           0 RESUME                   0
///
///   2           2 LOAD_CONST               0 (None)
///               4 RETURN_VALUE
/// ```
///
/// WeavePy's `format_dis` emits:
///
/// ```text
///     0              RESUME      0
///     1           LOAD_CONST      0  (None)
///     2          RETURN_VALUE      0
/// ```
///
/// We extract just `(opname, arg_int)` pairs from each line and produce
/// one line per pair, dropping CPython's source-line headers and
/// `dis`'s parenthetical argreprs.
pub fn canonical_dis(dump: &str) -> String {
    let mut out = String::new();
    for line in dump.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip CPython's `Disassembly of <code object foo>:` headers.
        if trimmed.starts_with("Disassembly of") {
            continue;
        }
        let mut toks = trimmed.split_whitespace().peekable();
        let mut opname: Option<&str> = None;
        let mut arg: Option<&str> = None;
        while let Some(tok) = toks.next() {
            let looks_opname = tok.len() >= 2
                && tok
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            if looks_opname && tok.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                opname = Some(tok);
                // The next token (if any) that isn't a parenthesised argrepr is the arg.
                while let Some(next) = toks.next() {
                    if next.starts_with('(') {
                        // skip until balancing ')'
                        if !next.ends_with(')') {
                            for t in toks.by_ref() {
                                if t.ends_with(')') {
                                    break;
                                }
                            }
                        }
                        continue;
                    }
                    arg = Some(next);
                    break;
                }
                break;
            }
        }
        if let Some(name) = opname {
            out.push_str(name);
            if let Some(a) = arg {
                out.push(' ');
                out.push_str(a);
            }
            out.push('\n');
        }
    }
    out
}

/// Drop keyword arguments whose value is the well-known default
/// emitted by CPython but elided by WeavePy (or vice versa). The
/// list is intentionally narrow — we only strip fields where
/// asymmetric defaults are a known source of false-positive
/// mismatches. Operates on the line-stripped dump.
fn strip_default_kwargs(s: &str) -> String {
    const DEFAULTS: &[(&str, &str)] = &[
        ("type_ignores=[]", ""),
        ("type_comment=None", ""),
        ("is_async=0", ""),
        ("level=0", ""),
        ("returns=None", ""),
        ("decorator_list=[]", ""),
        ("simple=1", ""),
        ("kw_defaults=[]", ""),
        ("kwonlyargs=[]", ""),
        ("posonlyargs=[]", ""),
        ("defaults=[]", ""),
        ("annotation=None", ""),
        ("type_params=[]", ""),
    ];
    let mut out = s.to_owned();
    for (needle, replacement) in DEFAULTS {
        loop {
            let before = out.len();
            out = out.replacen(&format!(", {needle}"), replacement, 1);
            out = out.replacen(&format!("{needle}, "), replacement, 1);
            out = out.replacen(needle, replacement, 1);
            if out.len() == before {
                break;
            }
        }
    }
    out
}

fn strip_lineinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ',' {
            let mut lookahead = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }
            for _ in 0..16 {
                match chars.peek() {
                    Some(&p) if p.is_alphanumeric() || p == '_' => {
                        lookahead.push(p);
                        chars.next();
                    }
                    _ => break,
                }
            }
            const STRIP: &[&str] = &[
                "lineno",
                "col_offset",
                "end_lineno",
                "end_col_offset",
                "type_comment",
                "type_ignores",
            ];
            if STRIP.iter().any(|name| lookahead == *name) {
                // Skip the value too. The field ends at either a sibling
                // separator (`,` at our nesting depth) or the closing
                // delimiter of the enclosing call (`)`/`]` at depth 0,
                // which we must NOT consume).
                let mut depth = 0i32;
                while let Some(&p) = chars.peek() {
                    if (p == ')' || p == ']') && depth == 0 {
                        break;
                    }
                    chars.next();
                    if p == '(' || p == '[' {
                        depth += 1;
                    } else if p == ')' || p == ']' {
                        depth -= 1;
                    } else if p == ',' && depth == 0 {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
            out.push_str(&lookahead);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use weavepy::lexer::{Span, TokenKind};

    #[test]
    fn weavepy_eof_normalizes_to_endmarker() {
        let tok = Token {
            kind: TokenKind::Endmarker,
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

    #[test]
    fn canonical_ast_ignores_whitespace() {
        let a = "Module(\n  body=[\n    Pass(),\n  ],\n  type_ignores=[])";
        let b = "Module(body=[Pass()])";
        assert_eq!(canonical_ast(a), canonical_ast(b));
    }

    #[test]
    fn canonical_dis_keeps_opnames() {
        let listing = "  1           LOAD_CONST   0 (None)\n              RETURN_VALUE\n";
        let c = canonical_dis(listing);
        assert!(c.contains("LOAD_CONST"));
        assert!(c.contains("RETURN_VALUE"));
    }
}
