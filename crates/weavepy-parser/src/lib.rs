//! Python parser for WeavePy.
//!
//! Consumes tokens produced by [`weavepy_lexer`] and produces an AST
//! that aims to be a faithful representation of CPython's `ast`
//! module. The AST is exposed through the [`ast`] submodule, and a
//! convenience `parse_module` entry point handles tokenization
//! internally for callers that just want "source → AST."
//!
//! # Compatibility level
//!
//! - **Tracks CPython** for grammar productions, operator precedence,
//!   and AST node shape.
//! - **Experimental** for the size of the grammar: see
//!   `docs/rfcs/0001-executable-slice.md` for what's in and out.

pub mod ast;
pub mod error;
mod parser;
pub mod unparse;

pub use ast::{dump_module, Module};
pub use error::ParseError;
pub use parser::{
    expr_children, set_int_literal_max_digits, set_unicode_name_resolver, TypeComments,
    UnicodeNameResolution,
};
pub use weavepy_lexer::EscapeWarning;
pub use weavepy_lexer::{lang_preview, set_lang_preview};

/// Parse a Python source buffer into a [`Module`].
pub fn parse_module(source: &str) -> Result<Module, ParseError> {
    parse_module_with_warnings(source).0
}

/// Like [`parse_module`], but also returns the deferred [`EscapeWarning`]s
/// the tokenizer found in string/bytes literals.
///
/// Warnings are returned on **both** the success and error paths: an
/// invalid escape in an earlier literal must still surface (as a
/// `SyntaxWarning`, or a `SyntaxError` under an `error` filter) even when
/// a later token fails to lex/parse — e.g. `eval("'\\e' $")`. The VM
/// replays the warnings before propagating any parse error.
pub fn parse_module_with_warnings(
    source: &str,
) -> (Result<Module, ParseError>, Vec<EscapeWarning>) {
    parse_module_with_warnings_flags(source, false)
}

/// [`parse_module_with_warnings`] with PEP 401 `barry_as_FLUFL`
/// pre-activated when the caller passed `CO_FUTURE_BARRY_AS_BDFL` to
/// `compile()` (the parser also self-activates on the future import).
pub fn parse_module_with_warnings_flags(
    source: &str,
    flufl: bool,
) -> (Result<Module, ParseError>, Vec<EscapeWarning>) {
    let (result, warnings, _) = parse_module_with_warnings_flags_meta(source, flufl);
    (result, warnings)
}

/// Coarse class of the token the parser was looking at when it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    Newline,
    Dedent,
    Endmarker,
    Other,
}

/// The token the parser stopped on (byte span into the source).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopToken {
    pub kind: StopKind,
    pub start: u32,
    pub end: u32,
    /// Can this token begin an expression? pegen's error pass then
    /// re-parses it as one (`invalid_expression` and friends), which
    /// fetches the token after it.
    pub expr_start: bool,
}

/// Side information from a parse, mirroring the pegen state CPython
/// consults after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParseMeta {
    /// pegen's `last_stmt_location`: the 1-based line and byte column of
    /// the last compound statement the parser completed before failing
    /// (`(0, 0)` if none). CPython 3.14 publishes it as
    /// `SyntaxError._metadata`, which `traceback._find_keyword_typos`
    /// uses to suggest misspelled keywords.
    pub last_stmt: (u32, u32),
    /// The token the parser failed on (`None` on success or when the
    /// tokenizer itself failed). Together with the source this decides
    /// pegen's `_is_end_of_source` (`PyCF_ALLOW_INCOMPLETE_INPUT`).
    pub stop: Option<StopToken>,
    /// The furthest token pegen's second (error-reporting) pass would
    /// have fetched by the time it gives up: [`ParseMeta::stop`] itself,
    /// or the lookahead past it when the stop token gets re-parsed as an
    /// expression. Decides whether CPython's lazy tokenizer had reached
    /// EOF.
    pub furthest: Option<StopToken>,
}

/// [`parse_module_with_warnings_flags`] that also reports
/// [`ParseMeta::last_stmt`].
pub fn parse_module_with_warnings_flags_meta(
    source: &str,
    flufl: bool,
) -> (Result<Module, ParseError>, Vec<EscapeWarning>, (u32, u32)) {
    let (result, warnings, meta) = parse_module_full(source, flufl);
    (result, warnings, meta.last_stmt)
}

/// [`parse_module_with_warnings_flags`] returning the full [`ParseMeta`].
pub fn parse_module_full(
    source: &str,
    flufl: bool,
) -> (Result<Module, ParseError>, Vec<EscapeWarning>, ParseMeta) {
    parse_with_warnings_tracking(source, |src, tokens| {
        parser::parse_with_flufl_tracking(src, tokens, flufl)
    })
}

/// [`parse_eval_with_warnings_flags`] returning the full [`ParseMeta`].
pub fn parse_eval_full(
    source: &str,
    flufl: bool,
) -> (Result<Module, ParseError>, Vec<EscapeWarning>, ParseMeta) {
    let (result, warnings, meta) = parse_with_warnings_tracking(source, |src, tokens| {
        parser::parse_eval_tracking(src, tokens, flufl)
    });
    let result = eval_backslash_at_eof(source, result);
    (result, warnings, meta)
}

/// Like [`parse_module_with_warnings`], but with CPython's `eval` start
/// rule (`expressions NEWLINE* ENDMARKER`): statement syntax is a bare
/// "invalid syntax" at the first token the expression grammar can't
/// accept, never a statement-level diagnostic. Backs `eval(...)` and
/// `compile(..., mode="eval")`.
pub fn parse_eval_with_warnings(source: &str) -> (Result<Module, ParseError>, Vec<EscapeWarning>) {
    parse_eval_with_warnings_flags(source, false)
}

/// [`parse_eval_with_warnings`] with PEP 401 `barry_as_FLUFL`
/// pre-activated.
pub fn parse_eval_with_warnings_flags(
    source: &str,
    flufl: bool,
) -> (Result<Module, ParseError>, Vec<EscapeWarning>) {
    let (result, warnings) = if flufl {
        parse_with_warnings(source, |src, tokens| {
            parser::parse_eval_with_flufl(src, tokens, true)
        })
    } else {
        parse_with_warnings(source, parser::parse_eval)
    };
    (eval_backslash_at_eof(source, result), warnings)
}

/// Parse the tokens the tokenizer managed to produce before it failed
/// (see [`weavepy_lexer::tokenize_partial`]), the way pegen's parser sees
/// them before the lazy tokenizer's error surfaces. The stream is closed
/// with a zero-width NEWLINE and ENDMARKER at `source.len()`, so a
/// [`ParseMeta::stop`] (or [`ParseMeta::furthest`]) starting at
/// `source.len()` means the parser ran into the tokenizer's failure
/// point, while an earlier stop is a genuine parse error in the prefix.
pub fn parse_partial(
    source: &str,
    mut tokens: Vec<weavepy_lexer::Token>,
    flufl: bool,
    eval: bool,
) -> (Result<Module, ParseError>, ParseMeta) {
    use weavepy_lexer::{Span, Token, TokenKind};
    let end = source.len() as u32;
    for kind in [TokenKind::Newline, TokenKind::Endmarker] {
        tokens.push(Token {
            kind,
            span: Span::new(end, end),
        });
    }
    if eval {
        parser::parse_eval_tracking(source, tokens, flufl)
    } else {
        parser::parse_with_flufl_tracking(source, tokens, flufl)
    }
}

/// Re-anchor a line-continuation-at-EOF failure the way CPython's
/// tokenizer reports it when the source is tokenized *as-is* (no appended
/// newline: the `eval` and `single` start rules). Exposed for the VM's
/// `compile(..., "single")`.
pub fn backslash_at_eof_fixup(
    source: &str,
    result: Result<Module, ParseError>,
) -> Result<Module, ParseError> {
    eval_backslash_at_eof(source, result)
}

fn eval_backslash_at_eof(
    source: &str,
    mut result: Result<Module, ParseError>,
) -> Result<Module, ParseError> {
    // Eval-mode line-continuation at hard EOF: `compile()` appends a
    // newline in exec mode (so `"\\"` reads as continuation-then-EOF,
    // "unexpected EOF while parsing"), but the eval grammar tokenizes
    // the source as-is, and CPython reports the stray backslash itself
    // ("unexpected character after line continuation character") — even
    // when a bracket is still open (`eval("(\\")`).
    if let Some(stripped) = source.strip_suffix('\\') {
        let eof_shaped = matches!(
            &result,
            Err(ParseError::Lex(
                weavepy_lexer::LexError::UnexpectedEofParsing { .. }
                    | weavepy_lexer::LexError::BracketNeverClosed { .. }
            ))
        );
        // The backslash only counts if it is *live* syntax (not swallowed
        // by a comment or string). Probe by appending a junk byte: a live
        // continuation-backslash then trips StrayBackslash right there.
        let live_backslash = || {
            let probe = format!("{source}x");
            matches!(
                weavepy_lexer::tokenize(&probe),
                Err(weavepy_lexer::LexError::StrayBackslash { pos })
                    if pos as usize == source.len()
            )
        };
        if eof_shaped && live_backslash() {
            result = Err(ParseError::Lex(weavepy_lexer::LexError::StrayBackslash {
                pos: stripped.len() as u32,
            }));
        }
    }
    result
}

/// Parse with CPython's `func_type_input` start rule — backs
/// `ast.parse(..., mode='func_type')` (PEP 484 signature type comments).
/// Returns the argument-type expressions and the return-type expression
/// of `(t1, t2) -> ret`.
pub fn parse_func_type(source: &str) -> Result<(Vec<ast::Expr>, ast::Expr), ParseError> {
    let tokens = weavepy_lexer::tokenize(source).map_err(ParseError::from)?;
    parser::parse_func_type(source, tokens)
}

/// Parse with PEP 484 type-comment collection — backs
/// `ast.parse(..., type_comments=True)`. Returns the module plus the
/// `# type:` side tables (ignores, per-statement, per-argument).
pub fn parse_module_type_comments(source: &str) -> Result<(Module, TypeComments), ParseError> {
    let tokens = weavepy_lexer::tokenize(source).map_err(ParseError::from)?;
    parser::parse_type_comments(source, tokens)
}

fn parse_with_warnings(
    source: &str,
    parse: fn(&str, Vec<weavepy_lexer::Token>) -> Result<Module, ParseError>,
) -> (Result<Module, ParseError>, Vec<EscapeWarning>) {
    let (result, warnings, _) = parse_with_warnings_tracking(source, |src, tokens| {
        (parse(src, tokens), ParseMeta::default())
    });
    (result, warnings)
}

fn parse_with_warnings_tracking(
    source: &str,
    parse: impl FnOnce(&str, Vec<weavepy_lexer::Token>) -> (Result<Module, ParseError>, ParseMeta),
) -> (Result<Module, ParseError>, Vec<EscapeWarning>, ParseMeta) {
    let (tok_result, warnings) = weavepy_lexer::tokenize_with_escapes(source);
    let mut meta = ParseMeta::default();
    let module = match tok_result {
        Ok(tokens) => {
            let (result, m) = parse(source, tokens);
            meta = m;
            result
        }
        // An f-string field left open at the literal's own terminator:
        // CPython's pegen parses the partial field expression first, so
        // a specialized *inner* error ("Perhaps you forgot a comma?")
        // wins over the generic "f-string: expecting '}'".
        Err(weavepy_lexer::LexError::FstringExpectingBrace {
            pos,
            field_start,
            kind,
        }) if pos > field_start => {
            match parser::partial_fstring_field_error(source, field_start, pos, kind) {
                Some(inner) => Err(inner),
                None => Err(ParseError::from(
                    weavepy_lexer::LexError::FstringExpectingBrace {
                        pos,
                        field_start,
                        kind,
                    },
                )),
            }
        }
        // Same for a field whose *format spec* never closed (`f'{!s:'`):
        // an error pegen would have reported from the already-seen field
        // tokens (e.g. "valid expression required before '!'") wins.
        Err(weavepy_lexer::LexError::FstringExpectingBraceOrSpec {
            pos,
            field_start,
            kind,
        }) if pos > field_start => {
            match parser::partial_fstring_field_error(source, field_start, pos, kind) {
                Some(inner) => Err(inner),
                None => Err(ParseError::from(
                    weavepy_lexer::LexError::FstringExpectingBraceOrSpec {
                        pos,
                        field_start,
                        kind,
                    },
                )),
            }
        }
        // "too many nested f-strings": an error pegen would have found in
        // the tokens before the limit was hit (e.g. `f"{1 1:{f"…`'s comma
        // hint) wins over the nesting diagnostic.
        Err(weavepy_lexer::LexError::FstringTooManyNested {
            pos,
            field_start,
            kind,
        }) if pos > field_start => {
            match parser::partial_fstring_field_error(source, field_start, pos, kind) {
                Some(inner) => Err(inner),
                None => Err(ParseError::from(
                    weavepy_lexer::LexError::FstringTooManyNested {
                        pos,
                        field_start,
                        kind,
                    },
                )),
            }
        }
        Err(e) => Err(ParseError::from(e)),
    };
    (module, warnings, meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_module() {
        let m = parse_module("").expect("empty parses");
        assert!(m.body.is_empty());
    }

    #[test]
    fn parses_simple_expression_statement() {
        let m = parse_module("1 + 2\n").expect("ok");
        assert_eq!(m.body.len(), 1);
    }

    #[test]
    fn parses_function_def_and_call() {
        let src = "def add(a, b):\n    return a + b\nadd(1, 2)\n";
        let m = parse_module(src).expect("ok");
        assert_eq!(m.body.len(), 2);
    }

    #[test]
    fn parses_if_elif_else() {
        let src = "if x:\n    y\nelif z:\n    w\nelse:\n    v\n";
        let _ = parse_module(src).expect("ok");
    }

    #[test]
    fn parses_for_with_range() {
        let src = "for i in range(10):\n    print(i)\n";
        let _ = parse_module(src).expect("ok");
    }

    #[test]
    fn parses_list_comp() {
        let _ = parse_module("[x * x for x in range(10) if x % 2 == 0]\n").expect("ok");
    }

    #[test]
    fn parses_chained_comparison() {
        let _ = parse_module("1 < x < 10\n").expect("ok");
    }

    #[test]
    fn parses_class_with_body() {
        let module = parse_module("class C:\n    pass\n").expect("parse class");
        assert_eq!(module.body.len(), 1);
    }

    #[test]
    fn parses_try_except() {
        let module =
            parse_module("try:\n    pass\nexcept ValueError:\n    pass\n").expect("parse try");
        assert_eq!(module.body.len(), 1);
    }

    #[test]
    fn parses_simple_fstring() {
        let m = parse_module("x = f'hello {name}'\n").expect("parse fstring");
        assert_eq!(m.body.len(), 1);
    }

    #[test]
    fn parses_fstring_with_format_spec() {
        let _ = parse_module("y = f'{val:.2f}'\n").expect("format spec");
    }

    #[test]
    fn parses_fstring_with_conversion_and_spec() {
        let _ = parse_module("z = f'{obj!r:>10}'\n").expect("conv + spec");
    }

    #[test]
    fn parses_fstring_debug_form() {
        let _ = parse_module("print(f'{x = }')\n").expect("debug f-string");
    }

    #[test]
    fn parses_yield_expression() {
        let _ = parse_module("def g():\n    yield 1\n    yield\n").expect("yield");
    }

    #[test]
    fn parses_yield_from() {
        let _ = parse_module("def g():\n    yield from range(10)\n").expect("yield from");
    }

    #[test]
    fn parses_match_with_literal_and_capture() {
        let src = "match x:\n    case 0:\n        pass\n    case y:\n        pass\n";
        let _ = parse_module(src).expect("match basic");
    }

    #[test]
    fn parses_match_with_class_pattern() {
        let src = "match p:\n    case Point(x=0, y=0):\n        pass\n    case _:\n        pass\n";
        let _ = parse_module(src).expect("match class");
    }

    #[test]
    fn parses_match_with_sequence_and_star() {
        let src = "match xs:\n    case [a, b, *rest]:\n        pass\n";
        let _ = parse_module(src).expect("match seq");
    }

    #[test]
    fn parses_match_with_or_and_guard() {
        let src = "match v:\n    case 1 | 2 | 3 if v > 0:\n        pass\n";
        let _ = parse_module(src).expect("match or+guard");
    }

    #[test]
    fn match_is_soft_keyword_when_not_at_statement_start() {
        // `re.match` should still parse as an identifier.
        let _ = parse_module("x = re.match(p, s)\n").expect("match as ident");
    }
}
