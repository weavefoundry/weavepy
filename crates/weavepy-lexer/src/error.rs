//! Lexer errors.

use thiserror::Error;

/// Errors produced by [`crate::tokenize`].
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum LexError {
    #[error("unterminated string literal (detected at line {detected_line})")]
    UnterminatedString { pos: u32, detected_line: u32 },
    /// The string ran to EOL/EOF right after a backslash-escaped copy of
    /// its own quote character (`"blech\"`): CPython appends a hint.
    #[error("unterminated string literal (detected at line {detected_line}); perhaps you escaped the end quote?")]
    UnterminatedStringEscapedQuote { pos: u32, detected_line: u32 },
    #[error("unterminated triple-quoted string literal (detected at line {detected_line})")]
    UnterminatedTripleString { pos: u32, detected_line: u32 },
    /// A `\` line continuation at end of input (bpo-2180). CPython's
    /// tokenizer reports this as an EOF error, anchored one column past
    /// the backslash. `line_had_tokens` mirrors CPython's file
    /// tokenizer, which reports column 0 (suppressing the traceback
    /// caret) when no token preceded the continuation.
    #[error("unexpected EOF while parsing")]
    UnexpectedEofParsing { pos: u32, line_had_tokens: bool },
    // PEP 701 f-string diagnostics. CPython distinguishes an unterminated
    // f-string *literal* from an unterminated *replacement field* and uses
    // f-string-specific wording, which several `test_fstring` negative
    // cases assert on verbatim.
    #[error("unterminated f-string literal")]
    UnterminatedFstring { pos: u32 },
    #[error("unterminated triple-quoted f-string literal")]
    UnterminatedTripleFstring { pos: u32 },
    #[error("f-string: expecting '}}'")]
    FstringExpectingBrace {
        pos: u32,
        /// Byte offset just past the unterminated field's `{`, so the
        /// parser can attempt a partial parse of the field expression
        /// (CPython's pegen surfaces *inner* errors over the missing
        /// brace).
        field_start: u32,
    },
    #[error("f-string: expecting '}}', or format specs")]
    FstringExpectingBraceOrSpec {
        pos: u32,
        /// Byte offset just past the unterminated field's `{` (see
        /// [`LexError::FstringExpectingBrace::field_start`]).
        field_start: u32,
    },
    /// A format spec opened a replacement field at nesting depth > 2
    /// (CPython's tokenizer allows only two levels of spec nesting).
    #[error("f-string: expressions nested too deeply")]
    FstringNestedTooDeeply { pos: u32 },
    /// More than 150 lexically nested f-strings (CPython's
    /// `MAXFSTRINGLEVEL`). `field_start` is the outermost replacement
    /// field's start so the parser can prefer an error pegen would have
    /// reported from the tokens already seen.
    #[error("too many nested f-strings")]
    FstringTooManyNested { pos: u32, field_start: u32 },
    /// `\N` not followed by a complete `{NAME}` group in an f-string
    /// literal part. CPython detects this in the tokenizer (names are
    /// parsed differently inside f-strings), so it wins over an
    /// unterminated-literal diagnostic. `seg_start`/`seg_end` are byte
    /// positions of the escape within the decoded segment, mirroring
    /// CPython's unicodeescape codec error.
    #[error("(unicode error) 'unicodeescape' codec can't decode bytes in position {seg_start}-{seg_end}: malformed \\N character escape")]
    FstringMalformedNamedEscape {
        pos: u32,
        seg_start: u32,
        seg_end: u32,
    },
    #[error("closing parenthesis '{close}' does not match opening parenthesis '{open}'")]
    FstringParenMismatch { close: char, open: char, pos: u32 },
    #[error("f-string: unmatched '{close}'")]
    FstringUnmatchedParen { close: char, pos: u32 },
    #[error("'{open}' was never closed")]
    BracketNeverClosed { open: char, pos: u32 },
    /// A closer with no bracket open at all (`)1 + 2`).
    #[error("unmatched '{close}'")]
    UnmatchedClose { close: char, pos: u32 },
    /// A closer that doesn't pair with the innermost opener. CPython
    /// appends " on line N" when the opener sits on an earlier line.
    #[error("closing parenthesis '{close}' does not match opening parenthesis '{open}'{suffix}", suffix = open_line.map(|l| format!(" on line {l}")).unwrap_or_default())]
    MismatchedClose {
        close: char,
        open: char,
        /// `Some(line)` only when the opener is on a different line.
        open_line: Option<u32>,
        pos: u32,
    },
    #[error("f-string: newlines are not allowed in format specifiers for single quoted f-strings")]
    FstringNewlineInSpec { pos: u32 },
    // CPython renders this as `invalid character '€' (U+20AC)` — the
    // glyph in quotes followed by the code point. The byte position is
    // carried separately (see [`LexError::byte_offset`]) and surfaces as
    // the `SyntaxError`'s `lineno`/`offset`, not in the message text.
    #[error("invalid character {ch:?} (U+{codepoint:04X})", codepoint = u32::from(*ch))]
    InvalidChar { ch: char, pos: u32 },
    /// Non-printable (per `str.isprintable`) characters get a message with
    /// only the code point, no glyph: `invalid non-printable character
    /// U+00A0`.
    #[error("invalid non-printable character U+{codepoint:04X}", codepoint = u32::from(*ch))]
    InvalidNonPrintable { ch: char, pos: u32 },
    /// ASCII junk the tokenizer can't start a token with (`$`, `?`,
    /// `` ` ``) — CPython reports these as a bare "invalid syntax".
    #[error("invalid syntax")]
    InvalidToken { pos: u32 },
    #[error("inconsistent use of tabs and spaces in indentation")]
    InconsistentIndent { pos: u32 },
    #[error("unindent does not match any outer indentation level")]
    UnknownDedent { pos: u32 },
    /// More than `MAXINDENT` (100) nested indentation levels — CPython's
    /// `E_TOODEEP`, an `IndentationError`.
    #[error("too many levels of indentation")]
    TooDeepIndent { pos: u32 },
    /// An INDENT before any content token — a statement can never begin
    /// indented, and CPython's lazy tokenizer surfaces this before any
    /// later lexical error on the same source (test_ast
    /// test_literal_eval_syntax_errors).
    #[error("unexpected indent")]
    UnexpectedIndent { pos: u32 },
    /// Malformed numeric literal. `message` carries CPython's exact
    /// wording ("invalid hexadecimal literal", "invalid digit '9' in
    /// octal literal", "leading zeros in decimal integer literals…");
    /// `pos` points at the tokenizer cursor when the error fired (the
    /// last consumed byte), matching CPython's reported column.
    #[error("{message}")]
    InvalidNumber { pos: u32, message: String },
    #[error("invalid string prefix {prefix:?} at byte {pos}")]
    InvalidStringPrefix { pos: u32, prefix: String },
    #[error("unexpected character after line continuation character")]
    StrayBackslash { pos: u32 },
    #[error("unexpected EOF at byte {pos}: {message}")]
    UnexpectedEof { pos: u32, message: String },
}

impl LexError {
    /// Byte offset into the source where the error was detected. Used to
    /// compute the `SyntaxError` line/column at the raise site.
    pub fn byte_offset(&self) -> u32 {
        match self {
            LexError::UnterminatedString { pos, .. }
            | LexError::UnterminatedStringEscapedQuote { pos, .. }
            | LexError::UnterminatedTripleString { pos, .. }
            | LexError::UnexpectedEofParsing { pos, .. }
            | LexError::UnterminatedFstring { pos }
            | LexError::UnterminatedTripleFstring { pos }
            | LexError::FstringExpectingBrace { pos, .. }
            | LexError::FstringExpectingBraceOrSpec { pos, .. }
            | LexError::FstringNestedTooDeeply { pos }
            | LexError::FstringTooManyNested { pos, .. }
            | LexError::FstringMalformedNamedEscape { pos, .. }
            | LexError::FstringParenMismatch { pos, .. }
            | LexError::FstringUnmatchedParen { pos, .. }
            | LexError::BracketNeverClosed { pos, .. }
            | LexError::UnmatchedClose { pos, .. }
            | LexError::MismatchedClose { pos, .. }
            | LexError::FstringNewlineInSpec { pos }
            | LexError::InvalidChar { pos, .. }
            | LexError::InvalidNonPrintable { pos, .. }
            | LexError::InvalidToken { pos }
            | LexError::InconsistentIndent { pos }
            | LexError::UnknownDedent { pos }
            | LexError::TooDeepIndent { pos }
            | LexError::UnexpectedIndent { pos }
            | LexError::InvalidNumber { pos, .. }
            | LexError::InvalidStringPrefix { pos, .. }
            | LexError::StrayBackslash { pos }
            | LexError::UnexpectedEof { pos, .. } => *pos,
        }
    }
}
