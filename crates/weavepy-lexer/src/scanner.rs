//! Hand-written Python tokenizer.
//!
//! Operates on a `&str` and produces a `Vec<Token>` with byte spans
//! into the original source. Significant indentation is handled via
//! an explicit indent stack; implicit line continuation tracks
//! bracket depth (`paren_depth`); explicit `\` continuation is
//! handled in the main loop.
//!
//! The scanner is intentionally a single struct with a switch-style
//! dispatch in [`Scanner::next_token`]. Performance work
//! (computed-goto-style dispatch, etc.) is roadmap territory; the
//! goal here is correctness against CPython 3.13.

use crate::error::LexError;
use crate::token::{EscapeWarning, Keyword, Span, StringPrefix, Token, TokenKind};

/// Tokenize a complete Python source buffer.
pub fn tokenize(source: &str) -> Result<Vec<Token>, LexError> {
    tokenize_with_escapes(source).0
}

/// Tokenize, also returning the invalid-escape [`EscapeWarning`]s found
/// while scanning string/bytes literals.
///
/// The warnings are returned **even when tokenizing fails**: CPython
/// detects invalid escapes as the tokenizer walks each string, so a
/// `SyntaxWarning` from an earlier literal must still fire before a
/// later hard error (e.g. `eval("'\\e' $")` warns once *and* raises a
/// `SyntaxError` for the stray `$`). Collecting them on the scanner and
/// handing them back regardless of the result preserves that ordering.
pub fn tokenize_with_escapes(source: &str) -> (Result<Vec<Token>, LexError>, Vec<EscapeWarning>) {
    let mut scanner = Scanner::new(source);
    let mut out = Vec::new();
    let result = loop {
        match scanner.next_token() {
            Ok(Some(tok)) => {
                let is_endmarker = matches!(tok.kind, TokenKind::Endmarker);
                // Track whether the most recent token leaves a logical line
                // "open" (i.e. needs a NEWLINE to terminate it). The EOF
                // branch of `next_token` consults this to synthesize the
                // implicit final NEWLINE CPython emits for source lacking a
                // trailing newline. Structural/trivia tokens don't open a
                // logical line.
                scanner.last_was_content = !matches!(
                    tok.kind,
                    TokenKind::Newline
                        | TokenKind::Nl
                        | TokenKind::Indent
                        | TokenKind::Dedent
                        | TokenKind::Endmarker
                );
                out.push(tok);
                if is_endmarker {
                    break Ok(out);
                }
            }
            Ok(None) => continue,
            Err(e) => break Err(e),
        }
    };
    (result, scanner.escape_warnings)
}

struct Scanner<'src> {
    src: &'src [u8],
    pos: usize,
    /// Bracket depth — when > 0, NEWLINE is suppressed and DEDENT
    /// tracking pauses (CPython's "implicit line continuation").
    paren_depth: u32,
    /// Byte offsets of currently-open brackets, innermost last.
    /// Drives CPython's `'(' was never closed` error at EOF.
    open_brackets: Vec<(u8, usize)>,
    /// Indent stack: column counts of each open block. Always
    /// starts with 0.
    indents: Vec<u32>,
    /// True when at start of a logical line; controls indentation
    /// emission. Reset whenever we emit a non-trivia token on a line.
    at_line_start: bool,
    /// Set when the next call must emit pending DEDENTs before any
    /// real token.
    pending_dedents: u32,
    /// Set when the next call must emit a pending INDENT; holds the
    /// byte offset of the line start so the token's span covers the
    /// whitespace run (CPython anchors "unexpected indent" at col 1).
    pending_indent: Option<usize>,
    /// True after we emitted ENDMARKER; further calls return None.
    finished: bool,
    /// True when the most recently emitted token leaves a logical line
    /// "open" — any token other than NEWLINE/NL/INDENT/DEDENT/ENDMARKER.
    /// Drives the implicit final-NEWLINE synthesis in `next_token`'s EOF
    /// branch (CPython terminates an unterminated last line this way).
    last_was_content: bool,
    /// Invalid-escape `SyntaxWarning`s gathered while scanning string and
    /// bytes literals, in source order (the first invalid escape *per
    /// literal*, matching CPython's `first_invalid_escape` tracking).
    escape_warnings: Vec<EscapeWarning>,
}

impl<'src> Scanner<'src> {
    fn new(source: &'src str) -> Self {
        Self {
            src: source.as_bytes(),
            pos: 0,
            paren_depth: 0,
            open_brackets: Vec::new(),
            indents: vec![0],
            at_line_start: true,
            pending_dedents: 0,
            pending_indent: None,
            finished: false,
            last_was_content: false,
            escape_warnings: Vec::new(),
        }
    }

    /// Inspect the escape that begins at the backslash at absolute offset
    /// `bs` (in a non-raw string/bytes body) and, if it is one CPython
    /// would flag, record a [`EscapeWarning`]. Returns `true` when a
    /// warning was recorded so the caller can stop after the *first*
    /// invalid escape in a literal (CPython warns once per literal).
    ///
    /// `is_bytes` selects the bytes alphabet, which has no `\N`/`\u`/`\U`
    /// named/Unicode escapes — those letters are invalid escapes there.
    /// Valid escapes (and the incomplete `\x`/`\u`/`\U`/`\N` forms, which
    /// the parser turns into hard `SyntaxError`s at decode time) are left
    /// alone here.
    fn note_invalid_escape(&mut self, bs: usize, is_bytes: bool) -> bool {
        let Some(&esc) = self.src.get(bs + 1) else {
            return false;
        };
        // Octal escape: warn when the written value exceeds `\377`.
        if (b'0'..=b'7').contains(&esc) {
            let mut val = (esc - b'0') as u32;
            let mut digits = String::new();
            digits.push(esc as char);
            let mut k = bs + 2;
            for _ in 0..2 {
                match self.src.get(k) {
                    Some(&d) if (b'0'..=b'7').contains(&d) => {
                        val = val * 8 + (d - b'0') as u32;
                        digits.push(d as char);
                        k += 1;
                    }
                    _ => break,
                }
            }
            if val > 0o377 {
                self.escape_warnings.push(EscapeWarning {
                    offset: bs as u32,
                    message: format!("invalid octal escape sequence '\\{digits}'"),
                });
                return true;
            }
            return false;
        }
        // Recognised single-character / sized escapes. `x`/`u`/`U`/`N`
        // are accepted here (a malformed one is a decode-time error, not
        // a warning); bytes literals have no `u`/`U`/`N`.
        let recognised = matches!(
            esc,
            b'\n' | b'\r'
                | b'\\'
                | b'\''
                | b'"'
                | b'a'
                | b'b'
                | b'f'
                | b'n'
                | b'r'
                | b't'
                | b'v'
                | b'x'
        ) || (!is_bytes && matches!(esc, b'u' | b'U' | b'N'));
        if recognised {
            return false;
        }
        // Unknown escape — render the *character* (decoding UTF-8 so a
        // non-ASCII escape like `\€` shows the glyph, not a stray byte).
        let esc_char = std::str::from_utf8(&self.src[bs + 1..])
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or(esc as char);
        self.escape_warnings.push(EscapeWarning {
            offset: bs as u32,
            message: format!("invalid escape sequence '\\{esc_char}'"),
        });
        true
    }

    /// Produce the next token, or `Ok(None)` if the scanner consumed
    /// whitespace / a comment with no token to emit at this point.
    fn next_token(&mut self) -> Result<Option<Token>, LexError> {
        if self.finished {
            return Ok(None);
        }

        // Drain any indent/dedent tokens queued from the previous
        // newline-handling pass before doing anything else.
        if let Some(ws_start) = self.pending_indent.take() {
            return Ok(Some(self.token(TokenKind::Indent, ws_start, self.pos)));
        }
        if self.pending_dedents > 0 {
            self.pending_dedents -= 1;
            return Ok(Some(self.token(TokenKind::Dedent, self.pos, self.pos)));
        }

        if self.at_line_start && self.paren_depth == 0 {
            // Process leading whitespace as indentation.
            self.handle_line_start()?;
            // If line-start handling queued INDENT or DEDENT tokens,
            // drain one before consuming any real source content. The
            // recursion is bounded because the queued flags are set
            // synchronously and consumed on the next call.
            if self.pending_indent.is_some() || self.pending_dedents > 0 {
                return self.next_token();
            }
        }

        // Skip non-newline horizontal whitespace (incl. form feed,
        // which CPython tolerates anywhere it allows a space).
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == 0x0C {
                self.pos += 1;
            } else {
                break;
            }
        }

        let Some(b) = self.peek() else {
            // CPython's tokenizer implicitly terminates a final logical line
            // that lacks a trailing newline with a NEWLINE token *before*
            // emitting the closing DEDENTs. Without it, source whose last
            // line sits inside an indented block — e.g.
            // `compile("def f():\n return (x,)", ...)`, exactly the shape
            // `dataclasses`/`namedtuple`/`functools` codegen produces via
            // `exec` — fails to parse, because the parser never sees the
            // NEWLINE that closes the statement and the suite. We mirror
            // CPython here. `last_was_content` is the reliable signal —
            // `at_line_start` is cleared by `handle_line_start` at EOF even
            // for newline-terminated input, which would double-emit.
            if self.paren_depth == 0 && self.last_was_content {
                self.last_was_content = false;
                return Ok(Some(self.token(TokenKind::Newline, self.pos, self.pos)));
            }
            // EOF with an unclosed bracket: CPython's tokenizer reports
            // the *outermost* unclosed bracket (`parenstack[0]`),
            // anchored at its opening position (`'(' was never closed`).
            if let Some(&(bracket, pos)) = self.open_brackets.first() {
                return Err(LexError::BracketNeverClosed {
                    open: bracket as char,
                    pos: pos as u32,
                });
            }
            return Ok(Some(self.emit_endmarker()));
        };

        // Comment: lex up to (but not including) newline.
        if b == b'#' {
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c == b'\n' {
                    break;
                }
                self.pos += 1;
            }
            return Ok(Some(self.token(TokenKind::Comment, start, self.pos)));
        }

        // Newline handling.
        if b == b'\n' {
            return Ok(Some(self.handle_newline()));
        }
        if b == b'\r' {
            // Treat \r and \r\n as a single newline.
            self.pos += 1;
            if matches!(self.peek(), Some(b'\n')) {
                self.pos += 1;
            }
            // Replay newline logic without reading the byte again.
            return Ok(Some(self.handle_newline_at(self.pos)));
        }

        // Explicit line continuation.
        if b == b'\\' {
            let bs_pos = self.pos;
            self.pos += 1;
            // Tolerate \r before \n.
            if matches!(self.peek(), Some(b'\r')) {
                self.pos += 1;
            }
            if matches!(self.peek(), Some(b'\n')) {
                self.pos += 1;
                // Skip the newline; do not start a new logical line.
                return Ok(None);
            }
            return Err(LexError::StrayBackslash { pos: bs_pos as u32 });
        }

        // Strings (possibly prefixed: r, b, rb, br, f, u, with case variants).
        if b == b'"' || b == b'\'' {
            return self
                .scan_string(self.pos, StringPrefix::default())
                .map(Some);
        }

        // Identifiers (and prefix-then-string-quote case).
        if is_ident_start(b) {
            return self.scan_ident_or_prefixed_string().map(Some);
        }
        // PEP 3131: non-ASCII identifier start (e.g. `π`, `名前`, `Δt`).
        // The ASCII fast path above is the common case; here we decode a
        // single UTF-8 scalar and admit it when it's an `XID_Start`
        // character. The continuation loop in
        // `scan_ident_or_prefixed_string` already consumes `XID_Continue`,
        // so the rest of the identifier falls out uniformly. (NFKC
        // normalization of the resulting name is a documented follow-up;
        // we currently key identifiers on their source spelling.)
        if b >= 0x80 {
            if let Some((ch, _)) = decode_utf8(&self.src[self.pos..]) {
                if unicode_ident::is_xid_start(ch) {
                    return self.scan_ident_or_prefixed_string().map(Some);
                }
            }
        }

        // Numbers.
        if b.is_ascii_digit() || (b == b'.' && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()))
        {
            return self.scan_number().map(Some);
        }

        // Punctuation / operators (longest-match).
        self.scan_punct().map(Some)
    }

    // ---------- line start / indentation ----------

    fn handle_line_start(&mut self) -> Result<(), LexError> {
        let mut indent = 0u32;
        let line_start = self.pos;
        let mut saw_tab = false;
        let mut saw_space = false;
        while let Some(b) = self.peek() {
            match b {
                b' ' => {
                    indent += 1;
                    saw_space = true;
                    self.pos += 1;
                }
                b'\t' => {
                    // CPython aligns tabs to the next multiple of 8.
                    indent = (indent / 8 + 1) * 8;
                    saw_tab = true;
                    self.pos += 1;
                }
                // Form feed at line start: CPython's tokenizer resets
                // the column to 0 and keeps scanning — it neither
                // contributes to indentation nor counts as tab/space
                // mixing.
                0x0C => {
                    indent = 0;
                    saw_space = false;
                    saw_tab = false;
                    self.pos += 1;
                }
                _ => break,
            }
        }

        // Blank line or comment-only line: don't emit INDENT/DEDENT,
        // and don't reset `at_line_start`.
        let Some(b) = self.peek() else {
            self.at_line_start = false;
            return Ok(());
        };
        if b == b'\n' || b == b'\r' || b == b'#' {
            // Trivia line; fall through to main loop without
            // changing indent state.
            self.at_line_start = false;
            return Ok(());
        }

        if saw_tab && saw_space {
            // CPython treats mixed tab/space at the same level as
            // an error in `python -tt` mode. We follow that strict
            // interpretation.
            return Err(LexError::InconsistentIndent {
                pos: line_start as u32,
            });
        }

        let current = *self.indents.last().expect("indent stack non-empty");
        if indent > current {
            self.indents.push(indent);
            self.pending_indent = Some(line_start);
            self.at_line_start = false;
            return Ok(());
        }
        if indent < current {
            let mut dedents = 0u32;
            while *self.indents.last().expect("indent stack non-empty") > indent {
                self.indents.pop();
                dedents += 1;
            }
            if *self.indents.last().expect("indent stack non-empty") != indent {
                // CPython's tokenizer reports this error with the
                // column past the end of the offending line (its
                // buffer cursor sits at line end), so the traceback
                // caret lands after the last character.
                let mut line_end = self.pos;
                while line_end < self.src.len() && self.src[line_end] != b'\n' {
                    line_end += 1;
                }
                return Err(LexError::UnknownDedent {
                    pos: line_end as u32,
                });
            }
            self.pending_dedents = dedents;
            self.at_line_start = false;
            return Ok(());
        }

        self.at_line_start = false;
        Ok(())
    }

    fn handle_newline(&mut self) -> Token {
        let start = self.pos;
        self.pos += 1;
        self.handle_newline_at(start)
    }

    fn handle_newline_at(&mut self, start: usize) -> Token {
        // Inside brackets: emit NL (trivia), don't reset line state.
        if self.paren_depth > 0 {
            return self.token(TokenKind::Nl, start, self.pos);
        }
        self.at_line_start = true;
        self.token(TokenKind::Newline, start, self.pos)
    }

    fn emit_endmarker(&mut self) -> Token {
        // On EOF, close any open indent blocks with DEDENTs before
        // ENDMARKER. We bias toward correctness over compactness:
        // a final NEWLINE is emitted by the standard handling.
        if self.indents.len() > 1 {
            self.pending_dedents = (self.indents.len() - 1) as u32;
            self.indents.truncate(1);
            self.pending_dedents -= 1; // we'll emit one now and let the rest drain
            self.at_line_start = false;
            return self.token(TokenKind::Dedent, self.pos, self.pos);
        }
        self.finished = true;
        self.token(TokenKind::Endmarker, self.pos, self.pos)
    }

    // ---------- identifiers / keywords ----------

    fn scan_ident_or_prefixed_string(&mut self) -> Result<Token, LexError> {
        let start = self.pos;
        // First char is ASCII ident-start; consume identifier chars.
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else if b >= 0x80 {
                // Possibly a unicode identifier continuation.
                let rest = &self.src[self.pos..];
                let Some((ch, len)) = decode_utf8(rest) else {
                    return Err(LexError::InvalidChar {
                        ch: '\u{FFFD}',
                        pos: self.pos as u32,
                    });
                };
                if unicode_ident::is_xid_continue(ch) {
                    self.pos += len;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        let lexeme = &self.src[start..self.pos];

        // Quote following the identifier? It might be a string prefix.
        if let Some(b) = self.peek() {
            if b == b'"' || b == b'\'' {
                let prefix_str = std::str::from_utf8(lexeme).unwrap_or("");
                if let Some(prefix) = StringPrefix::parse(prefix_str) {
                    return self.scan_string(start, prefix);
                }
                // Otherwise fall through to treat as identifier.
            }
        }

        let lexeme_str = std::str::from_utf8(lexeme).map_err(|_| LexError::InvalidChar {
            ch: '\u{FFFD}',
            pos: start as u32,
        })?;
        let kind = match Keyword::from_ident(lexeme_str) {
            Some(kw) => TokenKind::Keyword(kw),
            None => TokenKind::Name,
        };
        Ok(self.token(kind, start, self.pos))
    }

    // ---------- numbers ----------

    fn scan_number(&mut self) -> Result<Token, LexError> {
        let start = self.pos;
        let first = self.peek().expect("scan_number with no input");
        // Hex / octal / binary
        if first == b'0'
            && matches!(
                self.peek_at(1),
                Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
            )
        {
            let radix_char = self.peek_at(1).expect("checked above");
            self.pos += 2;
            let valid: fn(u8) -> bool = match radix_char {
                b'x' | b'X' => |b: u8| b.is_ascii_hexdigit(),
                b'o' | b'O' => |b: u8| (b'0'..=b'7').contains(&b),
                _ => |b: u8| b == b'0' || b == b'1',
            };
            let body_start = self.pos;
            while let Some(b) = self.peek() {
                if valid(b) || b == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos == body_start {
                return Err(LexError::InvalidNumber {
                    pos: start as u32,
                    message: "missing digits".to_owned(),
                });
            }
            return Ok(self.token(TokenKind::Number, start, self.pos));
        }

        // Decimal integer or float — consume digits.
        let mut saw_digit_before_dot = false;
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || b == b'_' {
                if b.is_ascii_digit() {
                    saw_digit_before_dot = true;
                }
                self.pos += 1;
            } else {
                break;
            }
        }

        let mut is_float = false;
        if matches!(self.peek(), Some(b'.')) {
            // A `.` immediately after the integer part is always part of
            // the float in Python: `1.`, `2.+3.`, `[1.]`, `1.e3` are all
            // valid. The dot binds to the number, never to attribute
            // access — `1.real` tokenizes as `1.` then `real` and is a
            // `SyntaxError` (you must write `(1).real` or `1 .real`),
            // exactly as in CPython's tokenizer.
            is_float = true;
            self.pos += 1;
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() || b == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() || b == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos == exp_start {
                return Err(LexError::InvalidNumber {
                    pos: start as u32,
                    message: "exponent has no digits".to_owned(),
                });
            }
        }

        // Allow the imaginary suffix `j`/`J`; we tokenize it as
        // part of a number (semantics handled later).
        if matches!(self.peek(), Some(b'j' | b'J')) {
            is_float = true;
            self.pos += 1;
        }

        let _ = (is_float, saw_digit_before_dot);
        Ok(self.token(TokenKind::Number, start, self.pos))
    }

    // ---------- strings ----------

    fn scan_string(&mut self, start: usize, prefix: StringPrefix) -> Result<Token, LexError> {
        let quote = self.peek().expect("scan_string at quote");
        debug_assert!(quote == b'"' || quote == b'\'');
        let triple = self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote);
        // PEP 701 — f-strings need a structure-aware scan so that quotes,
        // braces, backslashes, comments, newlines, and even nested
        // f-strings *inside* replacement fields don't prematurely
        // terminate the literal. We still emit a single `String` token
        // (the parser re-scans the interior); this just finds the true
        // extent. Non-f strings keep the simple fast paths below.
        if prefix.fstring {
            if triple {
                self.pos += 3;
            } else {
                self.pos += 1;
            }
            self.scan_fstring_extent(start, quote, triple, prefix.raw)?;
            return Ok(self.token(TokenKind::String, start, self.pos));
        }
        if triple {
            self.pos += 3;
            self.scan_triple_string(start, quote, prefix)
        } else {
            self.pos += 1;
            self.scan_single_line_string(start, quote, prefix)
        }
    }

    /// PEP 701 — scan the literal part of a (possibly nested) f-string,
    /// recursing through `{ ... }` replacement fields. On entry `self.pos`
    /// is just past the opening quote(s); on success it ends just past
    /// the matching closing quote(s).
    fn scan_fstring_extent(
        &mut self,
        start: usize,
        quote: u8,
        triple: bool,
        _raw: bool,
    ) -> Result<(), LexError> {
        loop {
            let Some(b) = self.peek() else {
                // Ran off the end with the literal still open: CPython
                // names the quote style ("unterminated f-string literal"
                // vs "...triple-quoted f-string literal").
                return Err(if triple {
                    LexError::UnterminatedTripleFstring { pos: start as u32 }
                } else {
                    LexError::UnterminatedFstring { pos: start as u32 }
                });
            };
            if b == quote {
                if triple {
                    if self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote) {
                        self.pos += 3;
                        return Ok(());
                    }
                    self.pos += 1;
                    continue;
                }
                self.pos += 1;
                return Ok(());
            }
            match b {
                // A single-line f-string's *literal* text can't span
                // lines; newlines are only legal inside `{ }`.
                b'\n' | b'\r' if !triple => {
                    return Err(LexError::UnterminatedFstring { pos: start as u32 });
                }
                // Escape in the literal part — consume the backslash and
                // the byte it escapes (full validation happens at decode
                // time). This applies in raw f-strings too: the backslash
                // stays literal, but per CPython a `\<quote>` still does
                // not terminate the string (e.g. `fr'\'\"'`), so we must
                // consume both bytes here rather than letting the quote
                // close the literal early. Exception: `{`/`}` are always
                // structural in an f-string (escaped only as `{{`/`}}`,
                // never by a backslash), so a backslash never swallows
                // them — `fr'\{{'` is a literal backslash followed by the
                // brace escape.
                b'\\' => {
                    self.pos += 1;
                    if matches!(self.peek(), Some(n) if n != b'{' && n != b'}') {
                        self.pos += 1;
                    }
                }
                b'{' => {
                    if self.peek_at(1) == Some(b'{') {
                        self.pos += 2; // `{{` literal-brace escape
                    } else {
                        self.pos += 1;
                        self.scan_fstring_field_extent(start, quote, triple)?;
                    }
                }
                b'}' => {
                    // `}}` is a literal-brace escape; a lone `}` is
                    // invalid, but we defer that diagnostic to the parser,
                    // which carries span context for a good message.
                    if self.peek_at(1) == Some(b'}') {
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                    }
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Scan a replacement field's *expression* part from just past its
    /// opening `{`. Tracks `()[]{}` nesting and skips nested strings
    /// (including nested f-strings) and comments so their contents can't
    /// close the field early. A top-level `:` hands off to the
    /// format-spec scan; a top-level `}` ends the field.
    fn scan_fstring_field_extent(
        &mut self,
        start: usize,
        outer_quote: u8,
        outer_triple: bool,
    ) -> Result<(), LexError> {
        // Explicit bracket stack (mirroring the parser) so we reproduce
        // CPython's precise PEP 701 diagnostics rather than masking a
        // mismatch behind a generic "expecting '}'". `in_comment` records a
        // `#` comment that ran to EOF, which CPython reports as the innermost
        // open bracket having "never closed" (distinct from a plain
        // unterminated field).
        let mut stack: Vec<u8> = Vec::new();
        let mut in_comment = false;
        loop {
            let Some(b) = self.peek() else {
                if in_comment {
                    let open = stack.last().copied().unwrap_or(b'{');
                    return Err(LexError::BracketNeverClosed {
                        open: open as char,
                        pos: start as u32,
                    });
                }
                return Err(LexError::FstringExpectingBrace { pos: start as u32 });
            };
            match b {
                b'}' if stack.is_empty() => {
                    self.pos += 1;
                    return Ok(());
                }
                // Top-level `:` begins the format spec, where `#`, quotes
                // and `:` are literal and only `{ }` nest replacement
                // fields (e.g. `{x:#06x}`, `{x:.{prec}f}`).
                b':' if stack.is_empty() => {
                    self.pos += 1;
                    return self.scan_fstring_format_spec_extent(start, outer_quote, outer_triple);
                }
                b'(' | b'[' | b'{' => {
                    stack.push(b);
                    self.pos += 1;
                }
                b')' | b']' | b'}' => {
                    let want = match b {
                        b')' => b'(',
                        b']' => b'[',
                        _ => b'{',
                    };
                    match stack.last() {
                        Some(&open) if open == want => {
                            stack.pop();
                            self.pos += 1;
                        }
                        // A close that doesn't match the innermost opener
                        // ("closing parenthesis 'X' does not match opening
                        // parenthesis 'Y'").
                        Some(&open) => {
                            return Err(LexError::FstringParenMismatch {
                                close: b as char,
                                open: open as char,
                                pos: self.pos as u32,
                            })
                        }
                        // A `)`/`]` with nothing open ("f-string: unmatched
                        // 'X'"). A top-level `}` was the field terminator,
                        // already handled above.
                        None => {
                            return Err(LexError::FstringUnmatchedParen {
                                close: b as char,
                                pos: self.pos as u32,
                            })
                        }
                    }
                }
                b'"' | b'\'' => self.scan_fstring_nested_string(outer_quote)?,
                // In the *expression* part, `#` starts a comment to end
                // of line (only meaningful in multiline fields). A comment
                // terminated by a newline resumes normal scanning; one that
                // reaches EOF leaves the innermost bracket "never closed".
                b'#' => {
                    in_comment = true;
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            in_comment = false;
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Scan a format spec from just past the field's top-level `:` to the
    /// closing `}`. The spec is literal text except that `{` opens a
    /// nested replacement field (its own expression) — so `#`, quotes and
    /// `:` here are *not* special.
    fn scan_fstring_format_spec_extent(
        &mut self,
        start: usize,
        outer_quote: u8,
        outer_triple: bool,
    ) -> Result<(), LexError> {
        loop {
            let Some(b) = self.peek() else {
                // Spec ran to EOF with the field still open. CPython's spec
                // diagnostic names the spec too: "expecting '}', or format
                // specs" (vs the plain "expecting '}'" for the expr part).
                return Err(LexError::FstringExpectingBraceOrSpec { pos: start as u32 });
            };
            match b {
                b'}' => {
                    self.pos += 1;
                    return Ok(());
                }
                b'{' => {
                    self.pos += 1;
                    self.scan_fstring_field_extent(start, outer_quote, outer_triple)?;
                }
                // The spec is literal text, so the *outer* quote here is the
                // f-string's own terminator (a quote-as-fill must use the
                // other quote, e.g. `f"{x:'>10}"`). Reaching it means the
                // field never closed: "expecting '}', or format specs".
                _ if b == outer_quote => {
                    return Err(LexError::FstringExpectingBraceOrSpec { pos: self.pos as u32 });
                }
                // A literal newline in the spec is only legal inside a
                // triple-quoted f-string; in a single-line one CPython
                // raises the "newlines are not allowed in format
                // specifiers..." error. (Newlines reached *inside* a nested
                // `{...}` field are consumed by the recursion above.)
                b'\n' | b'\r' if !outer_triple => {
                    return Err(LexError::FstringNewlineInSpec { pos: self.pos as u32 });
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Skip a nested string literal that appears inside a replacement
    /// field. Detects an immediately-preceding string prefix so a nested
    /// f-string recurses (its own fields may reuse the outer quote).
    fn scan_fstring_nested_string(&mut self, outer_quote: u8) -> Result<(), LexError> {
        let quote = self.peek().expect("nested string at quote");
        let triple = self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote);
        // When a lone quote *matching the enclosing f-string's* quote can't
        // form a complete string (runs to EOF unpaired), it was never a
        // nested string — it's the f-string's own terminator, and the field
        // is what's unterminated. CPython surfaces "f-string: expecting '}'",
        // not "unterminated string literal". (`f'{3'` vs the valid `f'{3''}'`
        // empty string, or `f'{3 + 'a'}'` which finds its pair.)
        let unterminated = |pos: u32| {
            if quote == outer_quote {
                LexError::FstringExpectingBrace { pos }
            } else {
                LexError::UnterminatedString { pos }
            }
        };
        // Walk back over the immediately-preceding ASCII-letter run to
        // recover any prefix (`f`, `r`, `rb`, ...). It's a real prefix
        // only when not glued to a longer identifier.
        let mut s = self.pos;
        while s > 0 && self.src[s - 1].is_ascii_alphabetic() {
            s -= 1;
        }
        let glued_to_ident =
            s > 0 && (self.src[s - 1] == b'_' || self.src[s - 1].is_ascii_digit());
        let prefix = if !glued_to_ident && s < self.pos {
            std::str::from_utf8(&self.src[s..self.pos])
                .ok()
                .and_then(StringPrefix::parse)
                .unwrap_or_default()
        } else {
            StringPrefix::default()
        };
        if triple {
            self.pos += 3;
        } else {
            self.pos += 1;
        }
        if prefix.fstring {
            return self.scan_fstring_extent(self.pos, quote, triple, prefix.raw);
        }
        let _ = prefix.raw;
        loop {
            let Some(b) = self.peek() else {
                return Err(unterminated(self.pos as u32));
            };
            if b == b'\\' {
                // A backslash escapes the next byte for tokenizing in raw
                // and non-raw strings alike (raw-ness only changes decode).
                self.pos += 1;
                if self.peek().is_some() {
                    self.pos += 1;
                }
                continue;
            }
            if b == quote {
                if triple {
                    if self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote) {
                        self.pos += 3;
                        return Ok(());
                    }
                    self.pos += 1;
                    continue;
                }
                self.pos += 1;
                return Ok(());
            }
            if (b == b'\n' || b == b'\r') && !triple {
                return Err(unterminated(self.pos as u32));
            }
            self.pos += 1;
        }
    }

    fn scan_single_line_string(
        &mut self,
        start: usize,
        quote: u8,
        prefix: StringPrefix,
    ) -> Result<Token, LexError> {
        let raw = prefix.raw;
        let mut warned = false;
        while let Some(b) = self.peek() {
            if b == b'\n' || b == b'\r' {
                return Err(LexError::UnterminatedString { pos: start as u32 });
            }
            if b == b'\\' && !raw {
                if !warned {
                    warned = self.note_invalid_escape(self.pos, prefix.bytes);
                }
                // Skip the backslash and one following byte (the escape).
                self.pos += 1;
                if let Some(next) = self.peek() {
                    if next == b'\n' {
                        self.pos += 1;
                    } else if next == b'\r' {
                        self.pos += 1;
                        if matches!(self.peek(), Some(b'\n')) {
                            self.pos += 1;
                        }
                    } else {
                        // Consume one byte; full escape validation
                        // happens at decode time.
                        self.pos += 1;
                    }
                }
                continue;
            }
            if b == b'\\' && raw {
                // In raw strings, a `\` followed by anything is
                // taken literally — but if it's the closing quote,
                // CPython still treats the backslash-quote as not
                // closing the string. Track that.
                self.pos += 1;
                if self.peek().is_some() {
                    self.pos += 1;
                }
                continue;
            }
            if b == quote {
                self.pos += 1;
                let _ = prefix;
                return Ok(self.token(TokenKind::String, start, self.pos));
            }
            self.pos += 1;
        }
        Err(LexError::UnterminatedString { pos: start as u32 })
    }

    fn scan_triple_string(
        &mut self,
        start: usize,
        quote: u8,
        prefix: StringPrefix,
    ) -> Result<Token, LexError> {
        let raw = prefix.raw;
        let mut warned = false;
        loop {
            let Some(b) = self.peek() else {
                return Err(LexError::UnterminatedString { pos: start as u32 });
            };
            if b == b'\\' {
                // Backslash escapes the next byte for tokenizing in raw
                // and non-raw triple strings alike (a raw `\"""` therefore
                // does not close the literal); decode handles raw-ness.
                if !raw && !warned {
                    warned = self.note_invalid_escape(self.pos, prefix.bytes);
                }
                self.pos += 1;
                if self.peek().is_some() {
                    self.pos += 1;
                }
                continue;
            }
            if b == quote && self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote) {
                self.pos += 3;
                return Ok(self.token(TokenKind::String, start, self.pos));
            }
            self.pos += 1;
        }
    }

    // ---------- punctuation / operators ----------

    fn scan_punct(&mut self) -> Result<Token, LexError> {
        let start = self.pos;
        let b = self.peek().expect("scan_punct with no input");
        let b1 = self.peek_at(1);
        let b2 = self.peek_at(2);

        // Three-character operators first.
        if b == b'.' && b1 == Some(b'.') && b2 == Some(b'.') {
            self.pos += 3;
            return Ok(self.token(TokenKind::Ellipsis, start, self.pos));
        }
        if b == b'*' && b1 == Some(b'*') && b2 == Some(b'=') {
            self.pos += 3;
            return Ok(self.token(TokenKind::DoubleStarEqual, start, self.pos));
        }
        if b == b'/' && b1 == Some(b'/') && b2 == Some(b'=') {
            self.pos += 3;
            return Ok(self.token(TokenKind::DoubleSlashEqual, start, self.pos));
        }
        if b == b'<' && b1 == Some(b'<') && b2 == Some(b'=') {
            self.pos += 3;
            return Ok(self.token(TokenKind::LeftShiftEqual, start, self.pos));
        }
        if b == b'>' && b1 == Some(b'>') && b2 == Some(b'=') {
            self.pos += 3;
            return Ok(self.token(TokenKind::RightShiftEqual, start, self.pos));
        }

        // Two-character operators.
        if let Some(c1) = b1 {
            let two = (b, c1);
            let kind2 = match two {
                (b'*', b'*') => Some(TokenKind::DoubleStar),
                (b'/', b'/') => Some(TokenKind::DoubleSlash),
                (b'<', b'<') => Some(TokenKind::LeftShift),
                (b'>', b'>') => Some(TokenKind::RightShift),
                (b'=', b'=') => Some(TokenKind::EqEqual),
                (b'!', b'=') => Some(TokenKind::NotEqual),
                (b'<', b'=') => Some(TokenKind::LessEqual),
                (b'>', b'=') => Some(TokenKind::GreaterEqual),
                (b'+', b'=') => Some(TokenKind::PlusEqual),
                (b'-', b'=') => Some(TokenKind::MinusEqual),
                (b'*', b'=') => Some(TokenKind::StarEqual),
                (b'/', b'=') => Some(TokenKind::SlashEqual),
                (b'%', b'=') => Some(TokenKind::PercentEqual),
                (b'&', b'=') => Some(TokenKind::AmperEqual),
                (b'|', b'=') => Some(TokenKind::VbarEqual),
                (b'^', b'=') => Some(TokenKind::CaretEqual),
                (b'@', b'=') => Some(TokenKind::AtEqual),
                (b':', b'=') => Some(TokenKind::ColonEqual),
                (b'-', b'>') => Some(TokenKind::RArrow),
                _ => None,
            };
            if let Some(k) = kind2 {
                self.pos += 2;
                return Ok(self.token(k, start, self.pos));
            }
        }

        // Single-character punctuation.
        let kind = match b {
            b'(' => {
                self.paren_depth += 1;
                self.open_brackets.push((b'(', start));
                TokenKind::LPar
            }
            b')' => {
                self.paren_depth = self.paren_depth.saturating_sub(1);
                self.open_brackets.pop();
                TokenKind::RPar
            }
            b'[' => {
                self.paren_depth += 1;
                self.open_brackets.push((b'[', start));
                TokenKind::LSqb
            }
            b']' => {
                self.paren_depth = self.paren_depth.saturating_sub(1);
                self.open_brackets.pop();
                TokenKind::RSqb
            }
            b'{' => {
                self.paren_depth += 1;
                self.open_brackets.push((b'{', start));
                TokenKind::LBrace
            }
            b'}' => {
                self.paren_depth = self.paren_depth.saturating_sub(1);
                self.open_brackets.pop();
                TokenKind::RBrace
            }
            b',' => TokenKind::Comma,
            b':' => TokenKind::Colon,
            b';' => TokenKind::Semi,
            b'.' => TokenKind::Dot,
            b'@' => TokenKind::At,
            b'+' => TokenKind::Plus,
            b'-' => TokenKind::Minus,
            b'*' => TokenKind::Star,
            b'/' => TokenKind::Slash,
            b'%' => TokenKind::Percent,
            b'&' => TokenKind::Amper,
            b'|' => TokenKind::Vbar,
            b'^' => TokenKind::Caret,
            b'~' => TokenKind::Tilde,
            b'<' => TokenKind::Less,
            b'>' => TokenKind::Greater,
            b'=' => TokenKind::Equal,
            _ => {
                let pos = self.pos as u32;
                // Try to report a meaningful char from the byte
                // stream (which may be multi-byte UTF-8).
                let ch = decode_utf8(&self.src[self.pos..])
                    .map(|(c, _)| c)
                    .unwrap_or('\u{FFFD}');
                // CPython wording: ASCII junk (`$`, `?`, `` ` ``) is a
                // plain "invalid syntax"; only non-ASCII gets the
                // `invalid character '€' (U+20AC)` message.
                if ch.is_ascii() {
                    return Err(LexError::InvalidToken { pos });
                }
                return Err(LexError::InvalidChar { ch, pos });
            }
        };
        self.pos += 1;
        Ok(self.token(kind, start, self.pos))
    }

    // ---------- helpers ----------

    fn token(&self, kind: TokenKind, start: usize, end: usize) -> Token {
        Token {
            kind,
            span: Span::new(start as u32, end as u32),
        }
    }

    #[inline]
    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    #[inline]
    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }
}

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

/// Decode one UTF-8 code point at the start of `bytes`. Returns the
/// character and the number of bytes consumed. Falls back to `None`
/// if the leading byte isn't a valid start of a UTF-8 sequence.
fn decode_utf8(bytes: &[u8]) -> Option<(char, usize)> {
    let first = *bytes.first()?;
    if first.is_ascii() {
        return Some((first as char, 1));
    }
    let width = match first {
        b if b & 0b1110_0000 == 0b1100_0000 => 2,
        b if b & 0b1111_0000 == 0b1110_0000 => 3,
        b if b & 0b1111_1000 == 0b1111_0000 => 4,
        _ => return None,
    };
    if bytes.len() < width {
        return None;
    }
    let s = std::str::from_utf8(&bytes[..width]).ok()?;
    let ch = s.chars().next()?;
    Some((ch, width))
}
