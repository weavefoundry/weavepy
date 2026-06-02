//! Token types and span utilities.
//!
//! `TokenKind` carries one variant per Python lexical category. Unlike
//! CPython's user-facing `tokenize` module (which collapses every
//! operator into a single `OP` kind), WeavePy keeps operators distinct
//! so the parser can dispatch on them directly. The conformance
//! normalizer maps the operator variants back to `"OP"` before
//! diffing against CPython.

use std::fmt;

/// A position in a source file, measured in bytes from the start of
/// the buffer. Byte offsets (rather than `(line, column)` pairs) are
/// the canonical form so each downstream phase can build its own
/// line-mapping table.
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

    #[inline]
    pub fn merge(self, other: Self) -> Self {
        Self {
            start: BytePos(self.start.0.min(other.start.0)),
            end: BytePos(self.end.0.max(other.end.0)),
        }
    }
}

/// A deferred compile-time diagnostic discovered while scanning a string
/// or bytes literal: CPython's invalid-escape and oversized-octal-escape
/// `SyntaxWarning`s (e.g. `invalid escape sequence '\z'`).
///
/// The tokenizer detects these (matching CPython, which warns from the
/// tokenizer/parser) but cannot emit them — that needs the runtime
/// `warnings` machinery. They are surfaced to the compile path, which
/// replays them through `warnings.warn_explicit`; an active `error`
/// filter then turns them into `SyntaxError`s. `offset` is the absolute
/// byte offset of the offending backslash within the source buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscapeWarning {
    pub offset: u32,
    pub message: String,
}

/// The lexical category of a token.
///
/// Operator and punctuation variants are distinct so parser dispatch
/// can avoid string comparisons. The conformance harness collapses
/// them under `"OP"` to match CPython's `tokenize` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Structural
    /// End of input. Maps to CPython's `ENDMARKER`.
    Endmarker,
    /// Logical newline that ends a statement.
    Newline,
    /// Trivia newline (blank line, line inside brackets). Maps to CPython's `NL`.
    Nl,
    /// Increase in indentation level.
    Indent,
    /// Decrease in indentation level.
    Dedent,
    /// Comment. Retained as trivia for tooling and conformance checks.
    Comment,

    // Atoms
    /// Identifier — non-keyword.
    Name,
    /// Reserved word (`if`, `for`, `True`, …). Carries which one.
    Keyword(Keyword),
    /// Numeric literal (int or float).
    Number,
    /// String / bytes literal, possibly with a prefix and triple quotes.
    /// f-strings tokenize as a single `String` for now (interior
    /// tokenization is RFC 0005).
    String,

    // Delimiters
    LPar,
    RPar,
    LSqb,
    RSqb,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Semi,
    Dot,
    Ellipsis,
    RArrow,
    At,
    AtEqual,

    // Operators — arithmetic
    Plus,
    Minus,
    Star,
    DoubleStar,
    Slash,
    DoubleSlash,
    Percent,

    // Operators — bitwise
    Amper,
    Vbar,
    Caret,
    Tilde,
    LeftShift,
    RightShift,

    // Operators — comparison
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    EqEqual,
    NotEqual,

    // Operators — assignment / augmented
    Equal,
    PlusEqual,
    MinusEqual,
    StarEqual,
    SlashEqual,
    DoubleSlashEqual,
    PercentEqual,
    AmperEqual,
    VbarEqual,
    CaretEqual,
    LeftShiftEqual,
    RightShiftEqual,
    DoubleStarEqual,
    ColonEqual,
}

impl TokenKind {
    /// Symbolic name used in conformance reports and diagnostics. Matches
    /// CPython's `tokenize.tok_name` where the kinds align; operators
    /// collapse to `"OP"`.
    pub fn symbolic_name(&self) -> &'static str {
        use TokenKind::{
            Amper, AmperEqual, At, AtEqual, Caret, CaretEqual, Colon, ColonEqual, Comma, Comment,
            Dedent, Dot, DoubleSlash, DoubleSlashEqual, DoubleStar, DoubleStarEqual, Ellipsis,
            Endmarker, EqEqual, Equal, Greater, GreaterEqual, Indent, Keyword, LBrace, LPar, LSqb,
            LeftShift, LeftShiftEqual, Less, LessEqual, Minus, MinusEqual, Name, Newline, Nl,
            NotEqual, Number, Percent, PercentEqual, Plus, PlusEqual, RArrow, RBrace, RPar, RSqb,
            RightShift, RightShiftEqual, Semi, Slash, SlashEqual, Star, StarEqual, String, Tilde,
            Vbar, VbarEqual,
        };
        match self {
            Endmarker => "ENDMARKER",
            Newline => "NEWLINE",
            Nl => "NL",
            Indent => "INDENT",
            Dedent => "DEDENT",
            Comment => "COMMENT",
            Name => "NAME",
            Keyword(_) => "NAME",
            Number => "NUMBER",
            String => "STRING",
            LPar | RPar | LSqb | RSqb | LBrace | RBrace | Comma | Colon | Semi | Dot | Ellipsis
            | RArrow | At | AtEqual | Plus | Minus | Star | DoubleStar | Slash | DoubleSlash
            | Percent | Amper | Vbar | Caret | Tilde | LeftShift | RightShift | Less | Greater
            | LessEqual | GreaterEqual | EqEqual | NotEqual | Equal | PlusEqual | MinusEqual
            | StarEqual | SlashEqual | DoubleSlashEqual | PercentEqual | AmperEqual | VbarEqual
            | CaretEqual | LeftShiftEqual | RightShiftEqual | DoubleStarEqual | ColonEqual => "OP",
        }
    }
}

/// Recognised Python keywords. The order is informational only —
/// classification happens via [`Keyword::from_ident`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum Keyword {
    False,
    None,
    True,
    And,
    As,
    Assert,
    Async,
    Await,
    Break,
    Class,
    Continue,
    Def,
    Del,
    Elif,
    Else,
    Except,
    Finally,
    For,
    From,
    Global,
    If,
    Import,
    In,
    Is,
    Lambda,
    Nonlocal,
    Not,
    Or,
    Pass,
    Raise,
    Return,
    Try,
    While,
    With,
    Yield,
}

impl Keyword {
    /// Recognise an identifier as a keyword.
    pub fn from_ident(s: &str) -> Option<Self> {
        Some(match s {
            "False" => Self::False,
            "None" => Self::None,
            "True" => Self::True,
            "and" => Self::And,
            "as" => Self::As,
            "assert" => Self::Assert,
            "async" => Self::Async,
            "await" => Self::Await,
            "break" => Self::Break,
            "class" => Self::Class,
            "continue" => Self::Continue,
            "def" => Self::Def,
            "del" => Self::Del,
            "elif" => Self::Elif,
            "else" => Self::Else,
            "except" => Self::Except,
            "finally" => Self::Finally,
            "for" => Self::For,
            "from" => Self::From,
            "global" => Self::Global,
            "if" => Self::If,
            "import" => Self::Import,
            "in" => Self::In,
            "is" => Self::Is,
            "lambda" => Self::Lambda,
            "nonlocal" => Self::Nonlocal,
            "not" => Self::Not,
            "or" => Self::Or,
            "pass" => Self::Pass,
            "raise" => Self::Raise,
            "return" => Self::Return,
            "try" => Self::Try,
            "while" => Self::While,
            "with" => Self::With,
            "yield" => Self::Yield,
            _ => return None,
        })
    }

    /// Lexeme as it appears in source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::False => "False",
            Self::None => "None",
            Self::True => "True",
            Self::And => "and",
            Self::As => "as",
            Self::Assert => "assert",
            Self::Async => "async",
            Self::Await => "await",
            Self::Break => "break",
            Self::Class => "class",
            Self::Continue => "continue",
            Self::Def => "def",
            Self::Del => "del",
            Self::Elif => "elif",
            Self::Else => "else",
            Self::Except => "except",
            Self::Finally => "finally",
            Self::For => "for",
            Self::From => "from",
            Self::Global => "global",
            Self::If => "if",
            Self::Import => "import",
            Self::In => "in",
            Self::Is => "is",
            Self::Lambda => "lambda",
            Self::Nonlocal => "nonlocal",
            Self::Not => "not",
            Self::Or => "or",
            Self::Pass => "pass",
            Self::Raise => "raise",
            Self::Return => "return",
            Self::Try => "try",
            Self::While => "while",
            Self::With => "with",
            Self::Yield => "yield",
        }
    }
}

/// Prefix on a string literal. Parsed lazily — the lexer just notes
/// which prefix appeared so the parser/compiler can decode the body
/// correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StringPrefix {
    pub raw: bool,
    pub bytes: bool,
    pub fstring: bool,
    pub unicode: bool,
}

impl StringPrefix {
    /// Parse a prefix string like `rb`, `Rb`, `f`, or empty.
    pub fn parse(prefix: &str) -> Option<Self> {
        let mut p = Self::default();
        for c in prefix.chars() {
            match c.to_ascii_lowercase() {
                'r' if !p.raw => p.raw = true,
                'b' if !p.bytes => p.bytes = true,
                'f' if !p.fstring => p.fstring = true,
                'u' if !p.unicode => p.unicode = true,
                _ => return None,
            }
        }
        // CPython rejects every combination of the `u` prefix with another
        // marker (`ur`, `ru`, `bu`, `fu`) and of bytes with `f`. The `u`
        // prefix is only valid standing alone (kept for Py2 source compat).
        if (p.bytes && p.unicode)
            || (p.bytes && p.fstring)
            || (p.fstring && p.unicode)
            || (p.raw && p.unicode)
        {
            return None;
        }
        Some(p)
    }
}

/// A single lexical token together with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{}..{}",
            self.kind.symbolic_name(),
            self.span.start.0,
            self.span.end.0
        )
    }
}
