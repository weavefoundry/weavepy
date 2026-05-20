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

/// Convert one WeavePy token into canonical form.
///
/// `source` is the original input — we slice the span out of it rather than
/// trusting the lexer to store text alongside each token.
pub fn from_weavepy(source: &str, token: &Token) -> NormalizedToken {
    let start = token.span.start.0 as usize;
    let end = token.span.end.0 as usize;
    let text = source.get(start..end).unwrap_or("").to_owned();
    NormalizedToken {
        kind: token.kind.symbolic_name().to_owned(),
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

/// Reduce an `ast.dump`-style string to a comparison-friendly form.
///
/// We strip trailing whitespace from each line and collapse runs of
/// blank lines so cosmetic differences (e.g. one side uses
/// `indent=2`, the other none) don't drown the diff. We also drop the
/// `lineno=…, col_offset=…` fields CPython attaches with
/// `include_attributes=True` — WeavePy doesn't surface them yet.
pub fn canonical_ast(dump: &str) -> String {
    let stripped = strip_lineinfo(dump);
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

/// Reduce a `dis`-style listing to a comparison-friendly form: opcode
/// names and their args, one per line, in order.
pub fn canonical_dis(dump: &str) -> String {
    let mut out = String::new();
    for line in dump.lines() {
        let mut toks = line.split_whitespace();
        // CPython prefixes with line number / offset / opname / argrepr.
        // We want just (opname, argrepr) — the first all-uppercase token
        // and whatever follows it on the line.
        let opname = loop {
            match toks.next() {
                Some(t) if t.chars().all(|c| c.is_ascii_uppercase() || c == '_') => break t,
                Some(_) => continue,
                None => break "",
            }
        };
        if opname.is_empty() {
            continue;
        }
        out.push_str(opname);
        for rest in toks {
            // Skip CPython's parenthesised argreprs — they're advisory.
            if rest.starts_with('(') {
                continue;
            }
            out.push(' ');
            out.push_str(rest);
        }
        out.push('\n');
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
