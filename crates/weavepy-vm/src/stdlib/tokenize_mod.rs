//! `_tokenize_core` — the native tokenizer behind the frozen
//! `_tokenize` module (RFC 0052).
//!
//! CPython 3.13's `tokenize.py` is a thin wrapper over the C
//! `_tokenize.TokenizerIter`, which drives the *readline* flavour of the
//! pegen tokenizer (`Parser/lexer/lexer.c` +
//! `Parser/tokenizer/readline_tokenizer.c`) and post-processes each raw
//! token into the classic 5-tuple (`Python/Python-tokenize.c`). This
//! module is a line-by-line port of that C code: the same buffer
//! discipline (the buffer resets between tokens and *accumulates* across
//! lines while a multi-line token — triple-quoted string, open bracket,
//! f-string — is in flight), the same indentation/dedent stack with the
//! alternate-tabsize consistency check, the same PEP 701 f-string mode
//! stack producing `FSTRING_START`/`FSTRING_MIDDLE`/`FSTRING_END`
//! triples, and the same `E_*` done-code → exception mapping
//! (`SyntaxError` / `IndentationError` / `TabError` with CPython's exact
//! messages and locations).
//!
//! The single entry point is `tokens(lines, extra_tokens)` where `lines`
//! is the list of source lines a `readline` callable produced (the
//! frozen `_tokenize.TokenizerIter` slurps them — WeavePy builtins keep
//! the readline dispatch in Python). It returns `(token_tuples, error)`
//! where `error` is `None` or a structured descriptor the shim re-raises
//! faithfully.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

// ---- token types (Grammar/Tokens, pycore_token.h) ----
const ENDMARKER: i32 = 0;
const NAME: i32 = 1;
const NUMBER: i32 = 2;
const STRING: i32 = 3;
const NEWLINE: i32 = 4;
const INDENT: i32 = 5;
const DEDENT: i32 = 6;
const LPAR: i32 = 7;
const RPAR: i32 = 8;
const LSQB: i32 = 9;
const RSQB: i32 = 10;
const COLON: i32 = 11;
const COMMA: i32 = 12;
const SEMI: i32 = 13;
const PLUS: i32 = 14;
const MINUS: i32 = 15;
const STAR: i32 = 16;
const SLASH: i32 = 17;
const VBAR: i32 = 18;
const AMPER: i32 = 19;
const LESS: i32 = 20;
const GREATER: i32 = 21;
const EQUAL: i32 = 22;
const DOT: i32 = 23;
const PERCENT: i32 = 24;
const LBRACE: i32 = 25;
const RBRACE: i32 = 26;
const EQEQUAL: i32 = 27;
const NOTEQUAL: i32 = 28;
const LESSEQUAL: i32 = 29;
const GREATEREQUAL: i32 = 30;
const TILDE: i32 = 31;
const CIRCUMFLEX: i32 = 32;
const LEFTSHIFT: i32 = 33;
const RIGHTSHIFT: i32 = 34;
const DOUBLESTAR: i32 = 35;
const PLUSEQUAL: i32 = 36;
const MINEQUAL: i32 = 37;
const STAREQUAL: i32 = 38;
const SLASHEQUAL: i32 = 39;
const PERCENTEQUAL: i32 = 40;
const AMPEREQUAL: i32 = 41;
const VBAREQUAL: i32 = 42;
const CIRCUMFLEXEQUAL: i32 = 43;
const LEFTSHIFTEQUAL: i32 = 44;
const RIGHTSHIFTEQUAL: i32 = 45;
const DOUBLESTAREQUAL: i32 = 46;
const DOUBLESLASH: i32 = 47;
const DOUBLESLASHEQUAL: i32 = 48;
const AT: i32 = 49;
const ATEQUAL: i32 = 50;
const RARROW: i32 = 51;
const ELLIPSIS: i32 = 52;
const COLONEQUAL: i32 = 53;
const EXCLAMATION: i32 = 54;
const OP: i32 = 55;
const FSTRING_START: i32 = 59;
const FSTRING_MIDDLE: i32 = 60;
const FSTRING_END: i32 = 61;
const COMMENT: i32 = 62;
const NL: i32 = 63;
const ERRORTOKEN: i32 = 64;

const EOF: i32 = -1;

const MAXINDENT: usize = 100; // Max indentation level
const MAXLEVEL: usize = 200; // Max parentheses level
const MAXFSTRINGLEVEL: usize = 150; // Max f-string nesting level
const MAX_EXPR_NESTING: i32 = 3;
const TABSIZE: i64 = 8;
const ALTTABSIZE: i64 = 1;

fn is_potential_identifier_start(c: i32) -> bool {
    (c >= 'a' as i32 && c <= 'z' as i32)
        || (c >= 'A' as i32 && c <= 'Z' as i32)
        || c == '_' as i32
        || c >= 128
}

fn is_potential_identifier_char(c: i32) -> bool {
    (c >= 'a' as i32 && c <= 'z' as i32)
        || (c >= 'A' as i32 && c <= 'Z' as i32)
        || (c >= '0' as i32 && c <= '9' as i32)
        || c == '_' as i32
        || c >= 128
}

fn is_digit(c: i32) -> bool {
    c >= '0' as i32 && c <= '9' as i32
}

fn is_xdigit(c: i32) -> bool {
    is_digit(c) || (c >= 'a' as i32 && c <= 'f' as i32) || (c >= 'A' as i32 && c <= 'F' as i32)
}

/// `_PyToken_OneChar` (Parser/token.c).
fn one_char_token(c: i32) -> i32 {
    match c as u8 as char {
        '!' => EXCLAMATION,
        '%' => PERCENT,
        '&' => AMPER,
        '(' => LPAR,
        ')' => RPAR,
        '*' => STAR,
        '+' => PLUS,
        ',' => COMMA,
        '-' => MINUS,
        '.' => DOT,
        '/' => SLASH,
        ':' => COLON,
        ';' => SEMI,
        '<' => LESS,
        '=' => EQUAL,
        '>' => GREATER,
        '@' => AT,
        '[' => LSQB,
        ']' => RSQB,
        '^' => CIRCUMFLEX,
        '{' => LBRACE,
        '|' => VBAR,
        '}' => RBRACE,
        '~' => TILDE,
        _ => OP,
    }
}

/// `_PyToken_TwoChars`.
fn two_chars_token(c1: i32, c2: i32) -> i32 {
    match (c1 as u8 as char, c2 as u8 as char) {
        ('!', '=') => NOTEQUAL,
        ('%', '=') => PERCENTEQUAL,
        ('&', '=') => AMPEREQUAL,
        ('*', '*') => DOUBLESTAR,
        ('*', '=') => STAREQUAL,
        ('+', '=') => PLUSEQUAL,
        ('-', '=') => MINEQUAL,
        ('-', '>') => RARROW,
        ('/', '/') => DOUBLESLASH,
        ('/', '=') => SLASHEQUAL,
        (':', '=') => COLONEQUAL,
        ('<', '<') => LEFTSHIFT,
        ('<', '=') => LESSEQUAL,
        ('<', '>') => NOTEQUAL,
        ('=', '=') => EQEQUAL,
        ('>', '=') => GREATEREQUAL,
        ('>', '>') => RIGHTSHIFT,
        ('@', '=') => ATEQUAL,
        ('^', '=') => CIRCUMFLEXEQUAL,
        ('|', '=') => VBAREQUAL,
        _ => OP,
    }
}

/// `_PyToken_ThreeChars`.
fn three_chars_token(c1: i32, c2: i32, c3: i32) -> i32 {
    match (c1 as u8 as char, c2 as u8 as char, c3 as u8 as char) {
        ('*', '*', '=') => DOUBLESTAREQUAL,
        ('.', '.', '.') => ELLIPSIS,
        ('/', '/', '=') => DOUBLESLASHEQUAL,
        ('<', '<', '=') => LEFTSHIFTEQUAL,
        ('>', '>', '=') => RIGHTSHIFTEQUAL,
        _ => OP,
    }
}

// ---- tok_state.done codes we distinguish (errcode.h subset) ----
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Done {
    Ok,
    /// E_EOF
    Eof,
    /// E_ERROR — a pending error descriptor is set.
    Error,
    /// E_DEDENT
    Dedent,
    /// E_TABSPACE
    TabSpace,
    /// E_TOODEEP
    TooDeep,
    /// E_LINECONT
    LineCont,
    /// E_EOLS / E_EOFS (string-EOF refinements; reported like Error).
    Eols,
    Eofs,
}

/// The exception the shim should raise, mirroring what
/// `Python-tokenize.c` leaves in `PyErr`.
struct PendingError {
    /// "syntax" | "indent" | "tab"
    kind: &'static str,
    msg: String,
    lineno: i64,
    /// character offset (already converted from bytes)
    offset: i64,
    /// `None` only for the bare-location E_EOF flavour.
    text: Option<String>,
    end_lineno: Option<i64>,
    end_offset: Option<i64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModeKind {
    Regular,
    Fstring,
}

/// `tokenizer_mode` (Parser/lexer/state.h). The `last_expr_*` metadata
/// buffer is omitted: it only feeds `token->metadata` (the f-string
/// debug-text the *parser* consumes), which the tokenize tuples never
/// expose.
struct Mode {
    kind: ModeKind,
    curly_bracket_depth: i32,
    curly_bracket_expr_start_depth: i32,
    quote: i32,
    quote_size: i32,
    raw: bool,
    /// buffer index of the f-string opening prefix (`f_string_start`).
    start: usize,
    /// buffer index of the line the f-string started on.
    multi_line_start: usize,
    /// line number the f-string started on.
    line_start: i64,
    in_format_spec: bool,
    debug: bool,
}

impl Mode {
    fn regular_top() -> Self {
        // state.c zero-initializes the bottom stack slot, so
        // `curly_bracket_expr_start_depth` is 0 (not -1) at top level.
        Mode {
            kind: ModeKind::Regular,
            curly_bracket_depth: 0,
            curly_bracket_expr_start_depth: 0,
            quote: 0,
            quote_size: 0,
            raw: false,
            start: 0,
            multi_line_start: 0,
            line_start: 0,
            in_format_spec: false,
            debug: false,
        }
    }
}

/// `struct tok_state` for the readline flavour, with C pointers replaced
/// by indices into `buf`.
struct Tok {
    buf: Vec<u8>,
    /// index of the next unread byte.
    cur: usize,
    /// end of buffered data.
    inp: usize,
    /// start of the current token (None between tokens).
    start: Option<usize>,
    done: Done,
    /// remaining input lines (the shim's slurped readline output).
    lines: Vec<String>,
    next_line: usize,
    indent: usize,
    indstack: [i64; MAXINDENT],
    altindstack: [i64; MAXINDENT],
    atbol: bool,
    pendin: i32,
    lineno: i64,
    first_lineno: i64,
    starting_col_offset: i64,
    col_offset: i64,
    level: usize,
    parenstack: [(u8, i64, i64); MAXLEVEL],
    cont_line: bool,
    /// buffer index of the current line start.
    line_start: usize,
    /// buffer index of the first line of a multi-line string token.
    multi_line_start: usize,
    extra_tokens: bool,
    comment_newline: bool,
    implicit_newline: bool,
    modes: Vec<Mode>,
    /// the C `PyErr` slot: set by `syntaxerror()`.
    err: Option<PendingError>,
}

impl Tok {
    fn new(lines: Vec<String>, extra_tokens: bool) -> Self {
        Tok {
            buf: Vec::new(),
            cur: 0,
            inp: 0,
            start: None,
            done: Done::Ok,
            lines,
            next_line: 0,
            indent: 0,
            indstack: [0; MAXINDENT],
            altindstack: [0; MAXINDENT],
            atbol: true,
            pendin: 0,
            lineno: 0,
            first_lineno: 0,
            starting_col_offset: -1,
            col_offset: -1,
            level: 0,
            parenstack: [(0, 0, 0); MAXLEVEL],
            cont_line: false,
            line_start: 0,
            multi_line_start: 0,
            extra_tokens,
            comment_newline: false,
            implicit_newline: false,
            modes: vec![Mode::regular_top()],
            err: None,
        }
    }

    fn inside_fstring(&self) -> bool {
        self.modes.len() > 1
    }

    // ---- error construction (Parser/tokenizer/helpers.c) ----

    /// `_syntaxerror_range`: full-location SyntaxError. `col_offset` /
    /// `end_col_offset` of -1 mean "at tok->cur".
    fn syntaxerror_range(&mut self, msg: String, col_offset: i64, end_col_offset: i64) -> i32 {
        if self.err.is_some() {
            return ERRORTOKEN;
        }
        let upto_cur = String::from_utf8_lossy(
            &self.buf[self.line_start.min(self.cur)..self.cur.min(self.inp).max(self.line_start)],
        )
        .into_owned();
        let col = if col_offset == -1 {
            upto_cur.chars().count() as i64
        } else {
            col_offset
        };
        let end_col = if end_col_offset == -1 {
            col
        } else {
            end_col_offset
        };
        // strcspn(line_start, "\n"): the full physical line for display.
        let ls = self.line_start.min(self.inp);
        let line_len = self.buf[ls..self.inp]
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(self.inp - ls);
        let errtext = if line_len != self.cur.saturating_sub(ls) {
            String::from_utf8_lossy(&self.buf[ls..ls + line_len]).into_owned()
        } else {
            upto_cur
        };
        self.err = Some(PendingError {
            kind: "syntax",
            msg,
            lineno: self.lineno,
            offset: col,
            text: Some(errtext),
            end_lineno: Some(self.lineno),
            end_offset: Some(end_col),
        });
        self.done = Done::Error;
        ERRORTOKEN
    }

    fn syntaxerror(&mut self, msg: impl Into<String>) -> i32 {
        self.syntaxerror_range(msg.into(), -1, -1)
    }

    /// `_PyTokenizer_indenterror`.
    fn indenterror(&mut self) -> i32 {
        self.done = Done::TabSpace;
        self.cur = self.inp;
        ERRORTOKEN
    }

    // ---- character stream (lexer.c tok_nextc / tok_backup) ----

    fn nextc(&mut self) -> i32 {
        loop {
            if self.cur != self.inp {
                self.col_offset += 1;
                let c = self.buf[self.cur];
                self.cur += 1;
                return i32::from(c);
            }
            if self.done != Done::Ok {
                return EOF;
            }
            if !self.underflow() {
                self.cur = self.inp;
                return EOF;
            }
            self.line_start = self.cur;
            if self.buf[self.line_start..self.inp].contains(&0) {
                self.syntaxerror("source code cannot contain null bytes");
                self.cur = self.inp;
                return EOF;
            }
        }
    }

    fn backup(&mut self, c: i32) {
        if c != EOF {
            debug_assert!(self.cur > 0);
            self.cur -= 1;
            debug_assert_eq!(i32::from(self.buf[self.cur]), c);
            self.col_offset -= 1;
        }
    }

    /// `tok_underflow_readline`: pull the next slurped line into the
    /// buffer, resetting it first unless a token (or f-string) is in
    /// flight.
    fn underflow(&mut self) -> bool {
        if self.start.is_none() && !self.inside_fstring() {
            self.buf.clear();
            self.cur = 0;
            self.inp = 0;
        }
        let line = match self.lines.get(self.next_line) {
            Some(l) => {
                self.next_line += 1;
                l.clone()
            }
            None => String::new(),
        };
        self.buf.extend_from_slice(line.as_bytes());
        self.inp = self.buf.len();
        // tok_readline_string resets line_start even when nothing was
        // read (so EOF doesn't leave it dangling past a buffer reset).
        self.line_start = self.cur;
        if self.inp == self.cur {
            self.done = Done::Eof;
            return false;
        }
        self.implicit_newline = false;
        if self.buf[self.inp - 1] != b'\n' {
            // Last line does not end in \n, fake one.
            self.buf.push(b'\n');
            self.inp += 1;
            self.implicit_newline = true;
        }
        // ADVANCE_LINENO()
        self.lineno += 1;
        self.col_offset = 0;
        true
    }

    /// `tok_continuation_line`.
    fn continuation_line(&mut self) -> i32 {
        let mut c = self.nextc();
        if c == '\r' as i32 {
            c = self.nextc();
        }
        if c != '\n' as i32 {
            self.done = Done::LineCont;
            return -1;
        }
        c = self.nextc();
        if c == EOF {
            self.done = Done::Eof;
            self.cur = self.inp;
            return -1;
        }
        self.backup(c);
        c
    }

    // ---- number helpers ----

    /// `tok_decimal_tail`: 0 signals an error was raised.
    fn decimal_tail(&mut self) -> i32 {
        loop {
            let mut c;
            loop {
                c = self.nextc();
                if !is_digit(c) {
                    break;
                }
            }
            if c != '_' as i32 {
                return c;
            }
            c = self.nextc();
            if !is_digit(c) {
                self.backup(c);
                self.syntaxerror("invalid decimal literal");
                return 0;
            }
        }
    }

    /// `lookahead`: does the identifier-ish suffix `test` (followed by a
    /// non-identifier char) come next? Restores the stream either way.
    fn lookahead(&mut self, test: &str) -> bool {
        let pat = test.as_bytes();
        let mut matched: Vec<i32> = Vec::new();
        let res;
        loop {
            let c = self.nextc();
            if matched.len() == pat.len() {
                res = !is_potential_identifier_char(c);
                self.backup(c);
                break;
            }
            if c == i32::from(pat[matched.len()]) {
                matched.push(c);
                continue;
            }
            self.backup(c);
            res = false;
            break;
        }
        for &c in matched.iter().rev() {
            self.backup(c);
        }
        res
    }

    /// `verify_end_of_number`. WeavePy departure: the SyntaxWarning for
    /// keyword-adjacent literals (`0in x`) is not *emitted* (no warnings
    /// machinery here), but the control flow — including the char
    /// consumption — matches the warn-succeeded path.
    fn verify_end_of_number(&mut self, c: i32, kind: &str) -> bool {
        if self.extra_tokens {
            return true;
        }
        let mut r = false;
        if c == 'a' as i32 {
            r = self.lookahead("nd");
        } else if c == 'e' as i32 {
            r = self.lookahead("lse");
        } else if c == 'f' as i32 {
            r = self.lookahead("or");
        } else if c == 'i' as i32 {
            let c2 = self.nextc();
            if c2 == 'f' as i32 || c2 == 'n' as i32 || c2 == 's' as i32 {
                r = true;
            }
            self.backup(c2);
        } else if c == 'o' as i32 {
            r = self.lookahead("r");
        } else if c == 'n' as i32 {
            r = self.lookahead("ot");
        }
        if r {
            self.backup(c);
            // parser_warn(SyntaxWarning, "invalid %s literal") — warning
            // suppressed; on the non-raising path the char is re-consumed.
            self.nextc();
        } else if c < 128 && is_potential_identifier_char(c) {
            self.backup(c);
            self.syntaxerror(format!("invalid {kind} literal"));
            return false;
        }
        true
    }

    /// `verify_identifier` — PEP 3131 validation of a non-ASCII name.
    fn verify_identifier(&mut self) -> bool {
        if self.extra_tokens {
            return true;
        }
        let start = self.start.unwrap_or(self.cur);
        let text = match std::str::from_utf8(&self.buf[start..self.cur]) {
            Ok(t) => t.to_owned(),
            Err(_) => {
                // Unreachable with str input; treated as E_DECODE in C.
                self.syntaxerror("invalid decode");
                return false;
            }
        };
        let chars: Vec<char> = text.chars().collect();
        debug_assert!(!chars.is_empty());
        let mut invalid = chars.len();
        for (i, &ch) in chars.iter().enumerate() {
            let ok = if i == 0 {
                ch == '_' || crate::unicode_case::is_xid_start(ch)
            } else {
                crate::unicode_case::is_xid_continue(ch)
            };
            if !ok {
                invalid = i;
                break;
            }
        }
        if invalid < chars.len() {
            let ch = chars[invalid];
            if invalid + 1 < chars.len() {
                // Shift tok->cur to just past the offending character so
                // the caret lands on it.
                let byte_len: usize = chars[..=invalid].iter().map(|c| c.len_utf8()).sum();
                self.cur = start + byte_len;
            }
            if crate::object::char_is_printable(ch) {
                self.syntaxerror(format!("invalid character '{ch}' (U+{:04X})", ch as u32));
            } else {
                self.syntaxerror(format!(
                    "invalid non-printable character U+{:04X}",
                    ch as u32
                ));
            }
            return false;
        }
        true
    }
}

/// Result of one `tok_get`: the token type plus the `p_start`/`p_end`
/// buffer indices (None ↔ C NULL).
struct RawToken {
    ty: i32,
    start: Option<usize>,
    end: Option<usize>,
}

impl Tok {
    /// `_PyTokenizer_Get` / `tok_get`.
    fn get(&mut self) -> RawToken {
        if self.modes.last().unwrap().kind == ModeKind::Regular {
            self.get_normal_mode()
        } else {
            self.get_fstring_mode()
        }
    }

    fn make(&self, ty: i32, start: Option<usize>, end: Option<usize>) -> RawToken {
        RawToken { ty, start, end }
    }

    /// `tok_get_normal_mode`.
    fn get_normal_mode(&mut self) -> RawToken {
        let mut blankline;

        'nextline: loop {
            self.start = None;
            self.starting_col_offset = -1;
            blankline = false;

            // Get indentation level.
            if self.atbol {
                let mut col: i64 = 0;
                let mut altcol: i64 = 0;
                self.atbol = false;
                let mut cont_line_col: i64 = 0;
                let mut c;
                loop {
                    c = self.nextc();
                    if c == ' ' as i32 {
                        col += 1;
                        altcol += 1;
                    } else if c == '\t' as i32 {
                        col = (col / TABSIZE + 1) * TABSIZE;
                        altcol = (altcol / ALTTABSIZE + 1) * ALTTABSIZE;
                    } else if c == 0x0C {
                        // Control-L (formfeed): for Emacs users.
                        col = 0;
                        altcol = 0;
                    } else if c == '\\' as i32 {
                        // Indentation cannot be split over multiple
                        // physical lines with backslashes: the first
                        // backslash's column wins.
                        cont_line_col = if cont_line_col != 0 {
                            cont_line_col
                        } else {
                            col
                        };
                        c = self.continuation_line();
                        if c == -1 {
                            return self.make(ERRORTOKEN, self.start, Some(self.cur));
                        }
                    } else if c == EOF && self.err.is_some() {
                        return self.make(ERRORTOKEN, self.start, Some(self.cur));
                    } else {
                        break;
                    }
                }
                self.backup(c);
                if c == '#' as i32 || c == '\n' as i32 || c == '\r' as i32 {
                    // Whitespace/comment-only lines don't affect
                    // indentation (no interactive prompt here).
                    blankline = true;
                }
                if !blankline && self.level == 0 {
                    let col = if cont_line_col != 0 {
                        cont_line_col
                    } else {
                        col
                    };
                    let altcol = if cont_line_col != 0 {
                        cont_line_col
                    } else {
                        altcol
                    };
                    if col == self.indstack[self.indent] {
                        // No change.
                        if altcol != self.altindstack[self.indent] {
                            let t = self.indenterror();
                            return self.make(t, self.start, Some(self.cur));
                        }
                    } else if col > self.indstack[self.indent] {
                        // Indent — always one.
                        if self.indent + 1 >= MAXINDENT {
                            self.done = Done::TooDeep;
                            self.cur = self.inp;
                            return self.make(ERRORTOKEN, self.start, Some(self.cur));
                        }
                        if altcol <= self.altindstack[self.indent] {
                            let t = self.indenterror();
                            return self.make(t, self.start, Some(self.cur));
                        }
                        self.pendin += 1;
                        self.indent += 1;
                        self.indstack[self.indent] = col;
                        self.altindstack[self.indent] = altcol;
                    } else {
                        // Dedent — any number, must be consistent.
                        while self.indent > 0 && col < self.indstack[self.indent] {
                            self.pendin -= 1;
                            self.indent -= 1;
                        }
                        if col != self.indstack[self.indent] {
                            self.done = Done::Dedent;
                            self.cur = self.inp;
                            return self.make(ERRORTOKEN, self.start, Some(self.cur));
                        }
                        if altcol != self.altindstack[self.indent] {
                            let t = self.indenterror();
                            return self.make(t, self.start, Some(self.cur));
                        }
                    }
                }
            }

            self.start = Some(self.cur);
            self.starting_col_offset = self.col_offset;

            // Return pending indents/dedents.
            if self.pendin != 0 {
                if self.pendin < 0 {
                    self.pendin += 1;
                    let (s, e) = if self.extra_tokens {
                        (Some(self.cur), Some(self.cur))
                    } else {
                        (None, None)
                    };
                    return self.make(DEDENT, s, e);
                }
                self.pendin -= 1;
                let (s, e) = if self.extra_tokens {
                    (Some(0), Some(self.cur))
                } else {
                    (None, None)
                };
                return self.make(INDENT, s, e);
            }

            // Peek ahead at the next character.
            let c = self.nextc();
            self.backup(c);

            'again: loop {
                self.start = None;
                // Skip spaces.
                let mut c;
                loop {
                    c = self.nextc();
                    if !(c == ' ' as i32 || c == '\t' as i32 || c == 0x0C) {
                        break;
                    }
                }

                // Set start of current token.
                self.start = if self.cur == 0 {
                    None
                } else {
                    Some(self.cur - 1)
                };
                self.starting_col_offset = self.col_offset - 1;

                // Skip comment (type comments are not requested here).
                if c == '#' as i32 {
                    while c != EOF && c != '\n' as i32 && c != '\r' as i32 {
                        c = self.nextc();
                    }
                    if self.extra_tokens {
                        self.backup(c); // don't eat the newline or EOF
                        let p = self.start;
                        self.comment_newline = blankline;
                        return self.make(COMMENT, p, Some(self.cur));
                    }
                }

                // Check for EOF and errors now.
                if c == EOF {
                    if self.level > 0 {
                        return self.make(ERRORTOKEN, self.start, Some(self.cur));
                    }
                    let ty = if self.done == Done::Eof {
                        ENDMARKER
                    } else {
                        ERRORTOKEN
                    };
                    return self.make(ty, self.start, Some(self.cur));
                }

                // Identifier (most frequent token!).
                let mut nonascii = false;
                if is_potential_identifier_start(c) {
                    // Process the legal combinations of b"", r"", u"", f"".
                    let (mut saw_b, mut saw_r, mut saw_u, mut saw_f) = (false, false, false, false);
                    loop {
                        if !(saw_b || saw_u || saw_f) && (c == 'b' as i32 || c == 'B' as i32) {
                            saw_b = true;
                        } else if !(saw_b || saw_u || saw_r || saw_f)
                            && (c == 'u' as i32 || c == 'U' as i32)
                        {
                            saw_u = true;
                        } else if !(saw_r || saw_u) && (c == 'r' as i32 || c == 'R' as i32) {
                            saw_r = true;
                        } else if !(saw_f || saw_b || saw_u) && (c == 'f' as i32 || c == 'F' as i32)
                        {
                            saw_f = true;
                        } else {
                            break;
                        }
                        c = self.nextc();
                        if c == '"' as i32 || c == '\'' as i32 {
                            if saw_f {
                                return self.f_string_quote(c);
                            }
                            return self.letter_quote(c);
                        }
                    }
                    while is_potential_identifier_char(c) {
                        if c >= 128 {
                            nonascii = true;
                        }
                        c = self.nextc();
                    }
                    self.backup(c);
                    if nonascii && !self.verify_identifier() {
                        return self.make(ERRORTOKEN, self.start, Some(self.cur));
                    }
                    return self.make(NAME, self.start, Some(self.cur));
                }

                if c == '\r' as i32 {
                    c = self.nextc();
                }

                // Newline.
                if c == '\n' as i32 {
                    self.atbol = true;
                    if blankline || self.level > 0 {
                        if self.extra_tokens {
                            if self.comment_newline {
                                self.comment_newline = false;
                            }
                            return self.make(NL, self.start, Some(self.cur));
                        }
                        continue 'nextline;
                    }
                    if self.comment_newline && self.extra_tokens {
                        self.comment_newline = false;
                        return self.make(NL, self.start, Some(self.cur));
                    }
                    // Leave '\n' out of the string.
                    let t = self.make(NEWLINE, self.start, Some(self.cur - 1));
                    self.cont_line = false;
                    return t;
                }

                // Period or number starting with period?
                if c == '.' as i32 {
                    c = self.nextc();
                    if is_digit(c) {
                        return self.number_fraction(c);
                    } else if c == '.' as i32 {
                        c = self.nextc();
                        if c == '.' as i32 {
                            return self.make(ELLIPSIS, self.start, Some(self.cur));
                        }
                        self.backup(c);
                        self.backup('.' as i32);
                    } else {
                        self.backup(c);
                    }
                    return self.make(DOT, self.start, Some(self.cur));
                }

                // Number.
                if is_digit(c) {
                    return self.number(c);
                }

                if c == '\'' as i32 || c == '"' as i32 {
                    return self.letter_quote(c);
                }

                // Line continuation.
                if c == '\\' as i32 {
                    if self.continuation_line() == -1 {
                        return self.make(ERRORTOKEN, self.start, Some(self.cur));
                    }
                    self.cont_line = true;
                    continue 'again; // Read next line.
                }

                // Punctuation inside an f-string expression part.
                let is_punctuation =
                    c == ':' as i32 || c == '}' as i32 || c == '!' as i32 || c == '{' as i32;
                if is_punctuation && self.inside_fstring() {
                    let (expr_start_depth, depth, in_format_spec, debug) = {
                        let m = self.modes.last().unwrap();
                        (
                            m.curly_bracket_expr_start_depth,
                            m.curly_bracket_depth,
                            m.in_format_spec,
                            m.debug,
                        )
                    };
                    if expr_start_depth >= 0 {
                        // Runs before `{` increments the depth, so adjust
                        // to test "at the 0th level".
                        let cursor = depth - i32::from(c != '{' as i32);
                        let cursor_in_format_with_debug = cursor == 1 && (debug || in_format_spec);
                        let cursor_valid = cursor == 0 || cursor_in_format_with_debug;
                        // (update_fstring_expr / set_fstring_expr only feed
                        // the parser-side metadata — skipped.)
                        let _ = cursor_valid;
                        if c == ':' as i32 && cursor == expr_start_depth {
                            let m = self.modes.last_mut().unwrap();
                            m.kind = ModeKind::Fstring;
                            m.in_format_spec = true;
                            return self.make(one_char_token(c), self.start, Some(self.cur));
                        }
                    }
                }

                // Check for two-character token.
                {
                    let c2 = self.nextc();
                    let tok2 = two_chars_token(c, c2);
                    if tok2 != OP {
                        let c3 = self.nextc();
                        let tok3 = three_chars_token(c, c2, c3);
                        let current = if tok3 != OP {
                            tok3
                        } else {
                            self.backup(c3);
                            tok2
                        };
                        return self.make(current, self.start, Some(self.cur));
                    }
                    self.backup(c2);
                }

                // Keep track of parentheses nesting level.
                if c == '(' as i32 || c == '[' as i32 || c == '{' as i32 {
                    if self.level >= MAXLEVEL {
                        let t = self.syntaxerror("too many nested parentheses");
                        return self.make(t, self.start, Some(self.cur));
                    }
                    self.parenstack[self.level] = (
                        c as u8,
                        self.lineno,
                        (self.start.unwrap_or(self.cur) as i64) - (self.line_start as i64),
                    );
                    self.level += 1;
                    if self.inside_fstring() {
                        self.modes.last_mut().unwrap().curly_bracket_depth += 1;
                    }
                } else if c == ')' as i32 || c == ']' as i32 || c == '}' as i32 {
                    if self.inside_fstring()
                        && self.modes.last().unwrap().curly_bracket_depth == 0
                        && c == '}' as i32
                    {
                        let t = self.syntaxerror("f-string: single '}' is not allowed");
                        return self.make(t, self.start, Some(self.cur));
                    }
                    if !self.extra_tokens && self.level == 0 {
                        let t = self.syntaxerror(format!("unmatched '{}'", c as u8 as char));
                        return self.make(t, self.start, Some(self.cur));
                    }
                    if self.level > 0 {
                        self.level -= 1;
                        let (opening, open_lineno, _opencol) = self.parenstack[self.level];
                        let matches = (opening == b'(' && c == ')' as i32)
                            || (opening == b'[' && c == ']' as i32)
                            || (opening == b'{' && c == '}' as i32);
                        if !self.extra_tokens && !matches {
                            // An f-string expression's `{` closed by some
                            // other bracket reports as unmatched.
                            if self.inside_fstring() && opening == b'{' {
                                let m = self.modes.last().unwrap();
                                let previous_bracket = m.curly_bracket_depth - 1;
                                if previous_bracket == m.curly_bracket_expr_start_depth {
                                    let t = self.syntaxerror(format!(
                                        "f-string: unmatched '{}'",
                                        c as u8 as char
                                    ));
                                    return self.make(t, self.start, Some(self.cur));
                                }
                            }
                            let t = if open_lineno != self.lineno {
                                self.syntaxerror(format!(
                                    "closing parenthesis '{}' does not match opening parenthesis '{}' on line {}",
                                    c as u8 as char, opening as char, open_lineno
                                ))
                            } else {
                                self.syntaxerror(format!(
                                    "closing parenthesis '{}' does not match opening parenthesis '{}'",
                                    c as u8 as char, opening as char
                                ))
                            };
                            return self.make(t, self.start, Some(self.cur));
                        }
                    }
                    if self.inside_fstring() {
                        let m = self.modes.last_mut().unwrap();
                        m.curly_bracket_depth -= 1;
                        if m.curly_bracket_depth < 0 {
                            let t = self
                                .syntaxerror(format!("f-string: unmatched '{}'", c as u8 as char));
                            return self.make(t, self.start, Some(self.cur));
                        }
                        if c == '}' as i32
                            && m.curly_bracket_depth == m.curly_bracket_expr_start_depth
                        {
                            m.curly_bracket_expr_start_depth -= 1;
                            m.kind = ModeKind::Fstring;
                            m.in_format_spec = false;
                            m.debug = false;
                        }
                    }
                }

                // ASCII control chars (bytes ≥ 128 never reach here — they
                // take the identifier path above).
                if !(0x20..0x7F).contains(&c) {
                    let t = self.syntaxerror(format!("invalid non-printable character U+{c:04X}"));
                    return self.make(t, self.start, Some(self.cur));
                }

                if c == '=' as i32 && self.inside_fstring() {
                    let m = self.modes.last_mut().unwrap();
                    if m.curly_bracket_depth - m.curly_bracket_expr_start_depth == 1 {
                        m.debug = true;
                    }
                }

                // Punctuation character.
                return self.make(one_char_token(c), self.start, Some(self.cur));
            }
        }
    }

    /// The `f_string_quote:` label — start of an f-string prefix.
    fn f_string_quote(&mut self, c: i32) -> RawToken {
        let start = self.start.unwrap();
        let first = (self.buf[start] as char).to_ascii_lowercase();
        if !((first == 'f' || first == 'r') && (c == '\'' as i32 || c == '"' as i32)) {
            return self.letter_quote(c);
        }
        let quote = c;
        let mut quote_size = 1;

        self.first_lineno = self.lineno;
        self.multi_line_start = self.line_start;

        // Find the quote size and start of string.
        let after_quote = self.nextc();
        if after_quote == quote {
            let after_after_quote = self.nextc();
            if after_after_quote == quote {
                quote_size = 3;
            } else {
                self.backup(after_after_quote);
                self.backup(after_quote);
            }
        }
        if after_quote != quote {
            self.backup(after_quote);
        }

        if self.modes.len() + 1 >= MAXFSTRINGLEVEL {
            let t = self.syntaxerror("too many nested f-strings");
            return self.make(t, self.start, Some(self.cur));
        }
        let raw = match first {
            'f' => self.buf[start + 1].eq_ignore_ascii_case(&b'r'),
            'r' => true,
            _ => unreachable!(),
        };
        self.modes.push(Mode {
            kind: ModeKind::Fstring,
            curly_bracket_depth: 0,
            curly_bracket_expr_start_depth: -1,
            quote,
            quote_size,
            raw,
            start,
            multi_line_start: self.line_start,
            line_start: self.lineno,
            in_format_spec: false,
            debug: false,
        });
        self.make(FSTRING_START, self.start, Some(self.cur))
    }

    /// The `letter_quote:` label — an ordinary string literal.
    fn letter_quote(&mut self, c: i32) -> RawToken {
        if !(c == '\'' as i32 || c == '"' as i32) {
            // Fall through in C reaches the operator logic; in practice
            // the callers only jump here on a quote.
            return self.make(one_char_token(c), self.start, Some(self.cur));
        }
        let quote = c;
        let mut quote_size = 1;
        let mut end_quote_size = 0;
        let mut has_escaped_quote = false;

        self.first_lineno = self.lineno;
        self.multi_line_start = self.line_start;

        // Find the quote size and start of string.
        let mut c = self.nextc();
        if c == quote {
            c = self.nextc();
            if c == quote {
                quote_size = 3;
            } else {
                end_quote_size = 1; // empty string found
            }
        }
        if c != quote {
            self.backup(c);
        }

        // Get rest of string.
        while end_quote_size != quote_size {
            c = self.nextc();
            if self.done == Done::Error {
                return self.make(ERRORTOKEN, self.start, Some(self.cur));
            }
            if c == EOF || (quote_size == 1 && c == '\n' as i32) {
                // Shift the location to the start of the string and
                // report from the initial quote character.
                self.cur = self.start.unwrap() + 1;
                self.line_start = self.multi_line_start;
                let start_line = self.lineno;
                self.lineno = self.first_lineno;

                if self.inside_fstring() {
                    let m = self.modes.last().unwrap();
                    if m.quote == quote && m.quote_size == quote_size {
                        let t = self.syntaxerror("f-string: expecting '}'");
                        return self.make(t, self.start, Some(self.cur));
                    }
                }

                if quote_size == 3 {
                    self.syntaxerror(format!(
                        "unterminated triple-quoted string literal (detected at line {start_line})"
                    ));
                    if c != '\n' as i32 {
                        self.done = Done::Eofs;
                    }
                    return self.make(ERRORTOKEN, self.start, Some(self.cur));
                }
                if has_escaped_quote {
                    self.syntaxerror(format!(
                        "unterminated string literal (detected at line {start_line}); perhaps you escaped the end quote?"
                    ));
                } else {
                    self.syntaxerror(format!(
                        "unterminated string literal (detected at line {start_line})"
                    ));
                }
                if c != '\n' as i32 {
                    self.done = Done::Eols;
                }
                return self.make(ERRORTOKEN, self.start, Some(self.cur));
            }
            if c == quote {
                end_quote_size += 1;
            } else {
                end_quote_size = 0;
                if c == '\\' as i32 {
                    c = self.nextc(); // skip escaped char
                    if c == quote {
                        has_escaped_quote = true;
                    }
                    if c == '\r' as i32 {
                        self.nextc();
                    }
                }
            }
        }

        self.make(STRING, self.start, Some(self.cur))
    }

    /// `number()`: entered from the main loop with the first digit.
    fn number(&mut self, c0: i32) -> RawToken {
        let mut c = c0;
        if c == '0' as i32 {
            // Hex, octal or binary — maybe.
            c = self.nextc();
            if c == 'x' as i32 || c == 'X' as i32 {
                // Hex.
                c = self.nextc();
                loop {
                    if c == '_' as i32 {
                        c = self.nextc();
                    }
                    if !is_xdigit(c) {
                        self.backup(c);
                        let t = self.syntaxerror("invalid hexadecimal literal");
                        return self.make(t, self.start, Some(self.cur));
                    }
                    loop {
                        c = self.nextc();
                        if !is_xdigit(c) {
                            break;
                        }
                    }
                    if c != '_' as i32 {
                        break;
                    }
                }
                if !self.verify_end_of_number(c, "hexadecimal") {
                    return self.make(ERRORTOKEN, self.start, Some(self.cur));
                }
            } else if c == 'o' as i32 || c == 'O' as i32 {
                // Octal.
                c = self.nextc();
                loop {
                    if c == '_' as i32 {
                        c = self.nextc();
                    }
                    if !('0' as i32..'8' as i32).contains(&c) {
                        if is_digit(c) {
                            let t = self.syntaxerror(format!(
                                "invalid digit '{}' in octal literal",
                                c as u8 as char
                            ));
                            return self.make(t, self.start, Some(self.cur));
                        }
                        self.backup(c);
                        let t = self.syntaxerror("invalid octal literal");
                        return self.make(t, self.start, Some(self.cur));
                    }
                    loop {
                        c = self.nextc();
                        if !('0' as i32..'8' as i32).contains(&c) {
                            break;
                        }
                    }
                    if c != '_' as i32 {
                        break;
                    }
                }
                if is_digit(c) {
                    let t = self.syntaxerror(format!(
                        "invalid digit '{}' in octal literal",
                        c as u8 as char
                    ));
                    return self.make(t, self.start, Some(self.cur));
                }
                if !self.verify_end_of_number(c, "octal") {
                    return self.make(ERRORTOKEN, self.start, Some(self.cur));
                }
            } else if c == 'b' as i32 || c == 'B' as i32 {
                // Binary.
                c = self.nextc();
                loop {
                    if c == '_' as i32 {
                        c = self.nextc();
                    }
                    if c != '0' as i32 && c != '1' as i32 {
                        if is_digit(c) {
                            let t = self.syntaxerror(format!(
                                "invalid digit '{}' in binary literal",
                                c as u8 as char
                            ));
                            return self.make(t, self.start, Some(self.cur));
                        }
                        self.backup(c);
                        let t = self.syntaxerror("invalid binary literal");
                        return self.make(t, self.start, Some(self.cur));
                    }
                    loop {
                        c = self.nextc();
                        if c != '0' as i32 && c != '1' as i32 {
                            break;
                        }
                    }
                    if c != '_' as i32 {
                        break;
                    }
                }
                if is_digit(c) {
                    let t = self.syntaxerror(format!(
                        "invalid digit '{}' in binary literal",
                        c as u8 as char
                    ));
                    return self.make(t, self.start, Some(self.cur));
                }
                if !self.verify_end_of_number(c, "binary") {
                    return self.make(ERRORTOKEN, self.start, Some(self.cur));
                }
            } else {
                // Maybe old-style octal; in any case allow '0' itself.
                let mut nonzero = false;
                loop {
                    if c == '_' as i32 {
                        c = self.nextc();
                        if !is_digit(c) {
                            self.backup(c);
                            let t = self.syntaxerror("invalid decimal literal");
                            return self.make(t, self.start, Some(self.cur));
                        }
                    }
                    if c != '0' as i32 {
                        break;
                    }
                    c = self.nextc();
                }
                let zeros_end = self.cur;
                if is_digit(c) {
                    nonzero = true;
                    c = self.decimal_tail();
                    if c == 0 {
                        return self.make(ERRORTOKEN, self.start, Some(self.cur));
                    }
                }
                if c == '.' as i32 {
                    c = self.nextc();
                    return self.number_fraction(c);
                } else if c == 'e' as i32 || c == 'E' as i32 {
                    return self.number_exponent(c);
                } else if c == 'j' as i32 || c == 'J' as i32 {
                    return self.number_imaginary();
                } else if nonzero && !self.extra_tokens {
                    // Old-style octal: now disallowed.
                    self.backup(c);
                    let col = (self.start.unwrap() as i64 + 1) - self.line_start as i64;
                    let end_col = zeros_end as i64 - self.line_start as i64;
                    let t = self.syntaxerror_range(
                        "leading zeros in decimal integer literals are not permitted; \
                         use an 0o prefix for octal integers"
                            .to_owned(),
                        col,
                        end_col,
                    );
                    return self.make(t, self.start, Some(self.cur));
                }
                if !self.verify_end_of_number(c, "decimal") {
                    return self.make(ERRORTOKEN, self.start, Some(self.cur));
                }
            }
        } else {
            // Decimal.
            c = self.decimal_tail();
            if c == 0 {
                return self.make(ERRORTOKEN, self.start, Some(self.cur));
            }
            // Accept floating-point numbers.
            if c == '.' as i32 {
                c = self.nextc();
                return self.number_fraction(c);
            }
            if c == 'e' as i32 || c == 'E' as i32 {
                return self.number_exponent(c);
            }
            if c == 'j' as i32 || c == 'J' as i32 {
                return self.number_imaginary();
            }
            if !self.verify_end_of_number(c, "decimal") {
                return self.make(ERRORTOKEN, self.start, Some(self.cur));
            }
        }
        self.backup(c);
        self.make(NUMBER, self.start, Some(self.cur))
    }

    /// The `fraction:` label — c is the char after the '.'.
    fn number_fraction(&mut self, mut c: i32) -> RawToken {
        if is_digit(c) {
            c = self.decimal_tail();
            if c == 0 {
                return self.make(ERRORTOKEN, self.start, Some(self.cur));
            }
        }
        if c == 'e' as i32 || c == 'E' as i32 {
            return self.number_exponent(c);
        }
        if c == 'j' as i32 || c == 'J' as i32 {
            return self.number_imaginary();
        }
        if !self.verify_end_of_number(c, "decimal") {
            return self.make(ERRORTOKEN, self.start, Some(self.cur));
        }
        self.backup(c);
        self.make(NUMBER, self.start, Some(self.cur))
    }

    /// The `exponent:` label — e is the 'e'/'E' just consumed.
    fn number_exponent(&mut self, e: i32) -> RawToken {
        let mut c = self.nextc();
        if c == '+' as i32 || c == '-' as i32 {
            c = self.nextc();
            if !is_digit(c) {
                self.backup(c);
                let t = self.syntaxerror("invalid decimal literal");
                return self.make(t, self.start, Some(self.cur));
            }
        } else if !is_digit(c) {
            self.backup(c);
            if !self.verify_end_of_number(e, "decimal") {
                return self.make(ERRORTOKEN, self.start, Some(self.cur));
            }
            self.backup(e);
            return self.make(NUMBER, self.start, Some(self.cur));
        }
        c = self.decimal_tail();
        if c == 0 {
            return self.make(ERRORTOKEN, self.start, Some(self.cur));
        }
        if c == 'j' as i32 || c == 'J' as i32 {
            return self.number_imaginary();
        }
        if !self.verify_end_of_number(c, "decimal") {
            return self.make(ERRORTOKEN, self.start, Some(self.cur));
        }
        self.backup(c);
        self.make(NUMBER, self.start, Some(self.cur))
    }

    /// The `imaginary:` label — 'j'/'J' just consumed.
    fn number_imaginary(&mut self) -> RawToken {
        let c = self.nextc();
        if !self.verify_end_of_number(c, "imaginary") {
            return self.make(ERRORTOKEN, self.start, Some(self.cur));
        }
        self.backup(c);
        self.make(NUMBER, self.start, Some(self.cur))
    }

    /// `tok_get_fstring_mode`.
    fn get_fstring_mode(&mut self) -> RawToken {
        let mut end_quote_size = 0;
        let mut unicode_escape = false;

        self.start = Some(self.cur);
        self.first_lineno = self.lineno;
        self.starting_col_offset = self.col_offset;

        let (quote, quote_size, raw) = {
            let m = self.modes.last().unwrap();
            (m.quote, m.quote_size, m.raw)
        };

        // If we start with a bracket, defer to normal mode: nothing to
        // tokenize before it.
        let start_char = self.nextc();
        if start_char == '{' as i32 {
            let peek1 = self.nextc();
            self.backup(peek1);
            self.backup(start_char);
            if peek1 != '{' as i32 {
                {
                    let m = self.modes.last_mut().unwrap();
                    m.curly_bracket_expr_start_depth += 1;
                    if m.curly_bracket_expr_start_depth >= MAX_EXPR_NESTING {
                        let t = self.syntaxerror("f-string: expressions nested too deeply");
                        return self.make(t, self.start, Some(self.cur));
                    }
                    m.kind = ModeKind::Regular;
                }
                return self.get_normal_mode();
            }
        } else {
            self.backup(start_char);
        }

        // Check if we are at the end of the string. On a mismatch C
        // backs up only the mismatching char — matched quote chars stay
        // consumed and become part of the FSTRING_MIDDLE below.
        let mut at_end = true;
        for _ in 0..quote_size {
            let q = self.nextc();
            if q != quote {
                self.backup(q);
                at_end = false;
                break;
            }
        }
        if at_end {
            self.modes.pop();
            return self.make(FSTRING_END, self.start, Some(self.cur));
        }

        self.multi_line_start = self.line_start;
        while end_quote_size != quote_size {
            let c = self.nextc();
            if self.done == Done::Error {
                return self.make(ERRORTOKEN, self.start, Some(self.cur));
            }
            let in_format_spec = {
                let m = self.modes.last().unwrap();
                m.in_format_spec && m.curly_bracket_expr_start_depth >= 0
            };

            if c == EOF || (quote_size == 1 && c == '\n' as i32) {
                // A newline ends a format spec for single-quoted
                // f-strings (multi-line specs are only legal in
                // triple-quoted ones).
                if in_format_spec && c == '\n' as i32 {
                    if quote_size == 1 {
                        let t = self.syntaxerror(
                            "f-string: newlines are not allowed in format specifiers for single quoted f-strings",
                        );
                        return self.make(t, self.start, Some(self.cur));
                    }
                    self.backup(c);
                    let m = self.modes.last_mut().unwrap();
                    m.kind = ModeKind::Regular;
                    m.in_format_spec = false;
                    return self.make(FSTRING_MIDDLE, self.start, Some(self.cur));
                }

                // Report the error from the initial quote character.
                let (fs_start, fs_mls, fs_ls) = {
                    let m = self.modes.last().unwrap();
                    (m.start, m.multi_line_start, m.line_start)
                };
                self.cur = fs_start + 1;
                self.line_start = fs_mls;
                let start_line = self.lineno;
                self.lineno = fs_ls;

                if quote_size == 3 {
                    self.syntaxerror(format!(
                        "unterminated triple-quoted f-string literal (detected at line {start_line})"
                    ));
                    if c != '\n' as i32 {
                        self.done = Done::Eofs;
                    }
                    return self.make(ERRORTOKEN, self.start, Some(self.cur));
                }
                let t = self.syntaxerror(format!(
                    "unterminated f-string literal (detected at line {start_line})"
                ));
                return self.make(t, self.start, Some(self.cur));
            }

            if c == quote {
                end_quote_size += 1;
                continue;
            }
            end_quote_size = 0;

            if c == '{' as i32 {
                let peek = self.nextc();
                if peek != '{' as i32 || in_format_spec {
                    self.backup(peek);
                    self.backup(c);
                    {
                        let m = self.modes.last_mut().unwrap();
                        m.curly_bracket_expr_start_depth += 1;
                        if m.curly_bracket_expr_start_depth >= MAX_EXPR_NESTING {
                            let t = self.syntaxerror("f-string: expressions nested too deeply");
                            return self.make(t, self.start, Some(self.cur));
                        }
                        m.kind = ModeKind::Regular;
                        m.in_format_spec = false;
                    }
                    return self.make(FSTRING_MIDDLE, self.start, Some(self.cur));
                }
                return self.make(FSTRING_MIDDLE, self.start, Some(self.cur - 1));
            } else if c == '}' as i32 {
                if unicode_escape {
                    return self.make(FSTRING_MIDDLE, self.start, Some(self.cur));
                }
                let peek = self.nextc();
                // Format specs can't legally use double brackets, so `}}`
                // at bracket-depth 0 outside a spec is a literal brace.
                let cursor = self.modes.last().unwrap().curly_bracket_depth;
                if peek == '}' as i32 && !in_format_spec && cursor == 0 {
                    return self.make(FSTRING_MIDDLE, self.start, Some(self.cur - 1));
                }
                self.backup(peek);
                self.backup(c);
                {
                    let m = self.modes.last_mut().unwrap();
                    m.kind = ModeKind::Regular;
                    m.in_format_spec = false;
                }
                return self.make(FSTRING_MIDDLE, self.start, Some(self.cur));
            } else if c == '\\' as i32 {
                let mut peek = self.nextc();
                #[allow(unused_assignments)]
                if peek == '\r' as i32 {
                    peek = self.nextc();
                }
                // A backslash right before a curly brace: restore and
                // let the loop handle the brace itself. (The invalid
                // escape SyntaxWarning is suppressed here, like the
                // number-literal one.)
                if peek == '{' as i32 || peek == '}' as i32 {
                    self.backup(peek);
                    continue;
                }
                if !raw {
                    if peek == 'N' as i32 {
                        // Handle named unicode escapes (\N{BULLET}).
                        peek = self.nextc();
                        if peek == '{' as i32 {
                            unicode_escape = true;
                        } else {
                            self.backup(peek);
                        }
                    }
                }
                // else: skip the escaped character.
            }
        }

        // Backup the quotes: emit a final FSTRING_MIDDLE; the quotes
        // become the FSTRING_END on the next iteration.
        for _ in 0..quote_size {
            self.backup(quote);
        }
        self.make(FSTRING_MIDDLE, self.start, Some(self.cur))
    }
}

fn is_string_lit(ty: i32) -> bool {
    ty == STRING || ty == FSTRING_MIDDLE
}

fn chars_of(bytes: &[u8]) -> i64 {
    String::from_utf8_lossy(bytes).chars().count() as i64
}

/// The driver: `tokenizeriter_next` in a loop, producing the finished
/// 5-tuples plus an optional structured error.
fn run(lines: Vec<String>, extra_tokens: bool) -> (Vec<Object>, Option<Object>) {
    let mut tok = Tok::new(lines, extra_tokens);
    let mut out: Vec<Object> = Vec::new();

    loop {
        let raw = tok.get();
        let mut ty = raw.ty;
        if ty == ERRORTOKEN {
            let err = tok.err.take().unwrap_or_else(|| tokenizer_error(&tok));
            return (out, Some(error_object(err)));
        }

        let mut string: String = match (raw.start, raw.end) {
            (Some(s), Some(e)) if s <= e && e <= tok.buf.len() => {
                String::from_utf8_lossy(&tok.buf[s..e]).into_owned()
            }
            _ => String::new(),
        };

        let is_trailing_token = ty == ENDMARKER || (ty == DEDENT && tok.done == Done::Eof);

        let line_start = if is_string_lit(ty) {
            tok.multi_line_start
        } else {
            tok.line_start
        };
        let line: String = if tok.extra_tokens && is_trailing_token {
            String::new()
        } else {
            let ls = line_start.min(tok.inp);
            let mut size = tok.inp - ls;
            if size >= 1 && tok.implicit_newline {
                size -= 1;
            }
            String::from_utf8_lossy(&tok.buf[ls..ls + size]).into_owned()
        };

        let mut lineno = if is_string_lit(ty) {
            tok.first_lineno
        } else {
            tok.lineno
        };
        let mut end_lineno = tok.lineno;
        let mut col_offset: i64 = -1;
        let mut end_col_offset: i64 = -1;
        if let Some(s) = raw.start {
            if s >= line_start {
                col_offset = chars_of(&tok.buf[line_start..s]);
            }
        }
        if let Some(e) = raw.end {
            if e >= tok.line_start {
                if lineno == end_lineno {
                    // Same line: chars from the (string-lit adjusted)
                    // line start.
                    end_col_offset = col_offset.max(0)
                        + chars_of(&tok.buf[raw.start.unwrap_or(e).max(line_start)..e]);
                } else {
                    end_col_offset = chars_of(&tok.buf[tok.line_start..e]);
                }
            }
        }

        if tok.extra_tokens {
            if is_trailing_token {
                lineno += 1;
                end_lineno = lineno;
                col_offset = 0;
                end_col_offset = 0;
            }
            // Match the original Python tokenize implementation.
            if ty > DEDENT && ty < OP {
                ty = OP;
            } else if ty == NEWLINE {
                string = if tok.implicit_newline {
                    String::new()
                } else if raw.start.is_some_and(|s| tok.buf.get(s) == Some(&b'\r')) {
                    "\r\n".to_owned()
                } else {
                    "\n".to_owned()
                };
                end_col_offset += 1;
            } else if ty == NL && tok.implicit_newline {
                string = String::new();
            }
        }

        out.push(Object::new_tuple(vec![
            Object::Int(i64::from(ty)),
            Object::from_str(string),
            Object::new_tuple(vec![Object::Int(lineno), Object::Int(col_offset)]),
            Object::new_tuple(vec![Object::Int(end_lineno), Object::Int(end_col_offset)]),
            Object::from_str(line),
        ]));

        if ty == ENDMARKER {
            return (out, None);
        }
    }
}

/// `_tokenizer_error` — build the exception from `tok->done` when the
/// lexer returned ERRORTOKEN without setting one itself.
fn tokenizer_error(tok: &Tok) -> PendingError {
    let (kind, msg): (&'static str, &str) = match tok.done {
        Done::Eof => {
            // PyErr_SyntaxLocationObject flavour: bare location, no text.
            return PendingError {
                kind: "syntax",
                msg: "unexpected EOF in multi-line statement".to_owned(),
                lineno: tok.lineno,
                offset: tok.inp as i64,
                text: None,
                end_lineno: None,
                end_offset: None,
            };
        }
        Done::Dedent => (
            "indent",
            "unindent does not match any outer indentation level",
        ),
        Done::TabSpace => ("tab", "inconsistent use of tabs and spaces in indentation"),
        Done::TooDeep => ("indent", "too many levels of indentation"),
        Done::LineCont => (
            "syntax",
            "unexpected character after line continuation character",
        ),
        _ => ("syntax", "unknown tokenization error"),
    };
    // error_line = the whole buffer minus its trailing newline; offset =
    // char offset of tok->inp (one past the end — the C conversion reads
    // the NUL terminator as one extra char).
    let size = tok.inp.saturating_sub(1);
    let error_line = String::from_utf8_lossy(&tok.buf[..size]).into_owned();
    let offset = error_line.chars().count() as i64 + 1;
    PendingError {
        kind,
        msg: msg.to_owned(),
        lineno: tok.lineno,
        offset,
        text: Some(error_line),
        end_lineno: None,
        end_offset: None,
    }
}

fn error_object(e: PendingError) -> Object {
    Object::new_tuple(vec![
        Object::from_static(match e.kind {
            "indent" => "indent",
            "tab" => "tab",
            _ => "syntax",
        }),
        Object::from_str(e.msg),
        Object::Int(e.lineno),
        Object::Int(e.offset),
        match e.text {
            Some(t) => Object::from_str(t),
            None => Object::None,
        },
        match e.end_lineno {
            Some(l) => Object::Int(l),
            None => Object::None,
        },
        match e.end_offset {
            Some(o) => Object::Int(o),
            None => Object::None,
        },
    ])
}

/// `_tokenize_core.tokens(lines, extra_tokens)` →
/// `(list[token-5-tuple], error-descriptor | None)`.
fn tokens_fn(args: &[Object]) -> Result<Object, RuntimeError> {
    let lines_obj = args
        .first()
        .ok_or_else(|| type_error("tokens() missing required argument 'lines'"))?;
    let extra_tokens = matches!(args.get(1), Some(Object::Bool(true)) | Some(Object::Int(1)));
    let mut lines: Vec<String> = Vec::new();
    match lines_obj {
        Object::List(l) => {
            for item in l.borrow().iter() {
                match item {
                    Object::Str(s) => lines.push(s.to_string()),
                    _ => return Err(type_error("tokens() lines must be a list of str")),
                }
            }
        }
        _ => return Err(type_error("tokens() lines must be a list of str")),
    }
    let (toks, err) = run(lines, extra_tokens);
    Ok(Object::new_tuple(vec![
        Object::new_list(toks),
        err.unwrap_or(Object::None),
    ]))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_tokenize_core"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "WeavePy native tokenizer core (RFC 0052) — CPython 3.13 lexer port.",
            ),
        );
        let f = Object::Builtin(Rc::new(BuiltinFn::new("tokens", tokens_fn)));
        crate::descr_registry::register_module(&f, "_tokenize_core");
        d.insert(DictKey(Object::from_static("tokens")), f);
    }
    Rc::new(PyModule {
        name: "_tokenize_core".to_owned(),
        filename: None,
        dict,
    })
}
