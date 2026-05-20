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
use crate::token::{Keyword, Span, StringPrefix, Token, TokenKind};

/// Tokenize a complete Python source buffer.
pub fn tokenize(source: &str) -> Result<Vec<Token>, LexError> {
    let mut scanner = Scanner::new(source);
    let mut out = Vec::new();
    loop {
        match scanner.next_token()? {
            Some(tok) => {
                let is_endmarker = matches!(tok.kind, TokenKind::Endmarker);
                out.push(tok);
                if is_endmarker {
                    return Ok(out);
                }
            }
            None => continue,
        }
    }
}

struct Scanner<'src> {
    src: &'src [u8],
    pos: usize,
    /// Bracket depth — when > 0, NEWLINE is suppressed and DEDENT
    /// tracking pauses (CPython's "implicit line continuation").
    paren_depth: u32,
    /// Indent stack: column counts of each open block. Always
    /// starts with 0.
    indents: Vec<u32>,
    /// True when at start of a logical line; controls indentation
    /// emission. Reset whenever we emit a non-trivia token on a line.
    at_line_start: bool,
    /// Set when the next call must emit pending DEDENTs before any
    /// real token.
    pending_dedents: u32,
    /// Set when the next call must emit a pending INDENT.
    pending_indent: bool,
    /// True after we emitted ENDMARKER; further calls return None.
    finished: bool,
}

impl<'src> Scanner<'src> {
    fn new(source: &'src str) -> Self {
        Self {
            src: source.as_bytes(),
            pos: 0,
            paren_depth: 0,
            indents: vec![0],
            at_line_start: true,
            pending_dedents: 0,
            pending_indent: false,
            finished: false,
        }
    }

    /// Produce the next token, or `Ok(None)` if the scanner consumed
    /// whitespace / a comment with no token to emit at this point.
    fn next_token(&mut self) -> Result<Option<Token>, LexError> {
        if self.finished {
            return Ok(None);
        }

        // Drain any indent/dedent tokens queued from the previous
        // newline-handling pass before doing anything else.
        if self.pending_indent {
            self.pending_indent = false;
            return Ok(Some(self.token(TokenKind::Indent, self.pos, self.pos)));
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
            if self.pending_indent || self.pending_dedents > 0 {
                return self.next_token();
            }
        }

        // Skip non-newline horizontal whitespace.
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }

        let Some(b) = self.peek() else {
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
            self.pending_indent = true;
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
                return Err(LexError::UnknownDedent {
                    pos: line_start as u32,
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
            // `.` followed by non-digit and at end of identifier
            // could be attribute access (e.g. `1.real`). CPython
            // disallows that — `1.real` is a syntax error in
            // tokenizing — but to be safe we only treat `.` as
            // float if a digit, exponent, or end-of-token follows.
            let after_dot = self.peek_at(1);
            let can_be_float = match after_dot {
                None => true,
                Some(c) => c.is_ascii_digit() || c == b'e' || c == b'E' || c == b'j' || c == b'J',
            };
            if can_be_float {
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
        if triple {
            self.pos += 3;
            self.scan_triple_string(start, quote, prefix)
        } else {
            self.pos += 1;
            self.scan_single_line_string(start, quote, prefix)
        }
    }

    fn scan_single_line_string(
        &mut self,
        start: usize,
        quote: u8,
        prefix: StringPrefix,
    ) -> Result<Token, LexError> {
        let raw = prefix.raw;
        while let Some(b) = self.peek() {
            if b == b'\n' || b == b'\r' {
                return Err(LexError::UnterminatedString { pos: start as u32 });
            }
            if b == b'\\' && !raw {
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
        loop {
            let Some(b) = self.peek() else {
                return Err(LexError::UnterminatedString { pos: start as u32 });
            };
            if b == b'\\' && !raw {
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
                TokenKind::LPar
            }
            b')' => {
                self.paren_depth = self.paren_depth.saturating_sub(1);
                TokenKind::RPar
            }
            b'[' => {
                self.paren_depth += 1;
                TokenKind::LSqb
            }
            b']' => {
                self.paren_depth = self.paren_depth.saturating_sub(1);
                TokenKind::RSqb
            }
            b'{' => {
                self.paren_depth += 1;
                TokenKind::LBrace
            }
            b'}' => {
                self.paren_depth = self.paren_depth.saturating_sub(1);
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
