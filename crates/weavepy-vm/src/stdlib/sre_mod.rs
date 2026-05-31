//! The native `_sre` module: WeavePy's faithful port of CPython's
//! secret-labs regular-expression engine (RFC 0035).
//!
//! This is a direct, line-for-line translation of the backtracking
//! matcher in CPython 3.13's `Modules/_sre/sre_lib.h` (the `SRE(match)`
//! / `SRE(count)` / `SRE(charset)` / `SRE(search)` templated
//! functions). It consumes the exact same compiled int-code emitted by
//! the frozen Python `re._compiler`, so behaviour — including
//! lookaround, backreferences, atomic groups, possessive quantifiers,
//! conditional groups and the precise greedy/lazy backtracking order —
//! matches CPython.
//!
//! The public Python surface (`Pattern` / `Match` objects, `sub`,
//! `split`, `finditer`, …) lives in the frozen `re` package; this
//! module only exposes the primitive matching core plus the
//! case-folding helpers the compiler needs.
//!
//! Strings are matched over code-point arrays, so every position
//! returned (group spans, `pos`, `endpos`) is a Python code-point
//! index, exactly like CPython. Byte patterns are matched over the raw
//! byte values (each byte widened to a `u32`).

use crate::error::{runtime_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::Rc;
use crate::sync::RefCell;

// ---------------------------------------------------------------------------
// Constants (mirrors re/_constants.py and sre_constants.h)
// ---------------------------------------------------------------------------

pub const MAGIC: i64 = 20_230_612;
pub const CODESIZE: i64 = 4;
/// `SRE_MAXREPEAT` — the "unlimited" sentinel for `{m,}` style repeats.
const MAXREPEAT: u32 = 4_294_967_295;
const MAXREPEAT_I64: i64 = 4_294_967_295;
/// `SRE_MAXGROUPS`.
const MAXGROUPS: i64 = 2_147_483_647 / 2;

// Opcodes — indices into re/_constants.py OPCODES (after trimming
// MIN_REPEAT / MAX_REPEAT, which never reach the compiled code).
const OP_FAILURE: u32 = 0;
const OP_SUCCESS: u32 = 1;
const OP_ANY: u32 = 2;
const OP_ANY_ALL: u32 = 3;
const OP_ASSERT: u32 = 4;
const OP_ASSERT_NOT: u32 = 5;
const OP_AT: u32 = 6;
const OP_BRANCH: u32 = 7;
const OP_CATEGORY: u32 = 8;
const OP_CHARSET: u32 = 9;
const OP_BIGCHARSET: u32 = 10;
const OP_GROUPREF: u32 = 11;
const OP_GROUPREF_EXISTS: u32 = 12;
const OP_IN: u32 = 13;
const OP_INFO: u32 = 14;
const OP_JUMP: u32 = 15;
const OP_LITERAL: u32 = 16;
const OP_MARK: u32 = 17;
const OP_MAX_UNTIL: u32 = 18;
const OP_MIN_UNTIL: u32 = 19;
const OP_NOT_LITERAL: u32 = 20;
const OP_NEGATE: u32 = 21;
const OP_RANGE: u32 = 22;
const OP_REPEAT: u32 = 23;
const OP_REPEAT_ONE: u32 = 24;
#[allow(dead_code)] // appears only in parser output, never in compiled code
const OP_SUBPATTERN: u32 = 25;
const OP_MIN_REPEAT_ONE: u32 = 26;
const OP_ATOMIC_GROUP: u32 = 27;
const OP_POSSESSIVE_REPEAT: u32 = 28;
const OP_POSSESSIVE_REPEAT_ONE: u32 = 29;
const OP_GROUPREF_IGNORE: u32 = 30;
const OP_IN_IGNORE: u32 = 31;
const OP_LITERAL_IGNORE: u32 = 32;
const OP_NOT_LITERAL_IGNORE: u32 = 33;
const OP_GROUPREF_LOC_IGNORE: u32 = 34;
const OP_IN_LOC_IGNORE: u32 = 35;
const OP_LITERAL_LOC_IGNORE: u32 = 36;
const OP_NOT_LITERAL_LOC_IGNORE: u32 = 37;
const OP_GROUPREF_UNI_IGNORE: u32 = 38;
const OP_IN_UNI_IGNORE: u32 = 39;
const OP_LITERAL_UNI_IGNORE: u32 = 40;
const OP_NOT_LITERAL_UNI_IGNORE: u32 = 41;
const OP_RANGE_UNI_IGNORE: u32 = 42;

// AT codes.
const AT_BEGINNING: u32 = 0;
const AT_BEGINNING_LINE: u32 = 1;
const AT_BEGINNING_STRING: u32 = 2;
const AT_BOUNDARY: u32 = 3;
const AT_NON_BOUNDARY: u32 = 4;
const AT_END: u32 = 5;
const AT_END_LINE: u32 = 6;
const AT_END_STRING: u32 = 7;
const AT_LOC_BOUNDARY: u32 = 8;
const AT_LOC_NON_BOUNDARY: u32 = 9;
const AT_UNI_BOUNDARY: u32 = 10;
const AT_UNI_NON_BOUNDARY: u32 = 11;

// Category codes.
const CAT_DIGIT: u32 = 0;
const CAT_NOT_DIGIT: u32 = 1;
const CAT_SPACE: u32 = 2;
const CAT_NOT_SPACE: u32 = 3;
const CAT_WORD: u32 = 4;
const CAT_NOT_WORD: u32 = 5;
const CAT_LINEBREAK: u32 = 6;
const CAT_NOT_LINEBREAK: u32 = 7;
const CAT_LOC_WORD: u32 = 8;
const CAT_LOC_NOT_WORD: u32 = 9;
const CAT_UNI_DIGIT: u32 = 10;
const CAT_UNI_NOT_DIGIT: u32 = 11;
const CAT_UNI_SPACE: u32 = 12;
const CAT_UNI_NOT_SPACE: u32 = 13;
const CAT_UNI_WORD: u32 = 14;
const CAT_UNI_NOT_WORD: u32 = 15;
const CAT_UNI_LINEBREAK: u32 = 16;
const CAT_UNI_NOT_LINEBREAK: u32 = 17;

/// Guards against unbounded native recursion on pathological patterns
/// (CPython uses a heap-allocated context stack; we recurse on the
/// Rust stack and bail out with an error rather than crash).
const MAX_DEPTH: u32 = 10_000;

// ---------------------------------------------------------------------------
// Compiled-pattern registry
// ---------------------------------------------------------------------------

struct CompiledCode {
    code: Vec<u32>,
    groups: usize,
}

thread_local! {
    static REGISTRY: RefCell<Vec<Rc<CompiledCode>>> = const { RefCell::new(Vec::new()) };
}

// ---------------------------------------------------------------------------
// Case-folding helpers
// ---------------------------------------------------------------------------

#[inline]
fn lower_ascii(ch: u32) -> u32 {
    if (u32::from(b'A')..=u32::from(b'Z')).contains(&ch) {
        ch + 32
    } else {
        ch
    }
}

#[inline]
fn upper_ascii(ch: u32) -> u32 {
    if (u32::from(b'a')..=u32::from(b'z')).contains(&ch) {
        ch - 32
    } else {
        ch
    }
}

fn lower_unicode(ch: u32) -> u32 {
    match char::from_u32(ch) {
        Some(c) => c.to_lowercase().next().map_or(ch, |c| c as u32),
        None => ch,
    }
}

fn upper_unicode(ch: u32) -> u32 {
    match char::from_u32(ch) {
        Some(c) => c.to_uppercase().next().map_or(ch, |c| c as u32),
        None => ch,
    }
}

// We approximate locale case folding with ASCII (CPython's behaviour is
// locale-dependent and LOCALE tests are largely skipped).
#[inline]
fn lower_locale(ch: u32) -> u32 {
    lower_ascii(ch)
}
#[inline]
fn upper_locale(ch: u32) -> u32 {
    upper_ascii(ch)
}

#[inline]
fn char_loc_ignore(pat: u32, ch: u32) -> bool {
    ch == pat || lower_locale(ch) == pat || upper_locale(ch) == pat
}

fn unicode_iscased(ch: u32) -> bool {
    let lo = lower_unicode(ch);
    let up = upper_unicode(ch);
    ch != lo || ch != up
}

fn ascii_iscased(ch: u32) -> bool {
    (u32::from(b'a')..=u32::from(b'z')).contains(&ch)
        || (u32::from(b'A')..=u32::from(b'Z')).contains(&ch)
}

// ---------------------------------------------------------------------------
// Character classification (mirrors the SRE_IS_* / SRE_UNI_IS_* macros)
// ---------------------------------------------------------------------------

#[inline]
fn is_linebreak(ch: u32) -> bool {
    ch == u32::from(b'\n')
}

#[inline]
fn ascii_digit(ch: u32) -> bool {
    ch < 128 && (u32::from(b'0')..=u32::from(b'9')).contains(&ch)
}

#[inline]
fn ascii_space(ch: u32) -> bool {
    // ' ', \t, \n, \r, \v, \f
    ch < 128 && matches!(ch, 0x20 | 0x09 | 0x0a | 0x0b | 0x0c | 0x0d)
}

#[inline]
fn ascii_word(ch: u32) -> bool {
    ch < 128
        && (ascii_digit(ch)
            || (u32::from(b'a')..=u32::from(b'z')).contains(&ch)
            || (u32::from(b'A')..=u32::from(b'Z')).contains(&ch)
            || ch == u32::from(b'_'))
}

#[inline]
fn loc_word(ch: u32) -> bool {
    // Latin-1 alphanumeric or underscore.
    if ch == u32::from(b'_') {
        return true;
    }
    match char::from_u32(ch) {
        Some(c) => ch < 256 && (c.is_alphanumeric()),
        None => false,
    }
}

fn uni_digit(ch: u32) -> bool {
    match char::from_u32(ch) {
        // Py_UNICODE_ISDECIMAL — decimal digits: ASCII `0`-`9` plus the
        // Unicode Decimal_Number (Nd) category for non-ASCII scripts.
        Some(c) => c.is_ascii_digit() || nd_digit(c),
        None => false,
    }
}

/// Best-effort Unicode decimal-digit (general category Nd) test for the
/// common non-ASCII blocks, so `\d` matches like CPython without a full
/// Unicode database.
fn nd_digit(c: char) -> bool {
    let v = c as u32;
    matches!(v,
        0x0660..=0x0669 // Arabic-Indic
        | 0x06F0..=0x06F9 // Extended Arabic-Indic
        | 0x07C0..=0x07C9 // NKo
        | 0x0966..=0x096F // Devanagari
        | 0x09E6..=0x09EF // Bengali
        | 0x0A66..=0x0A6F // Gurmukhi
        | 0x0AE6..=0x0AEF // Gujarati
        | 0x0B66..=0x0B6F // Oriya
        | 0x0BE6..=0x0BEF // Tamil
        | 0x0C66..=0x0C6F // Telugu
        | 0x0CE6..=0x0CEF // Kannada
        | 0x0D66..=0x0D6F // Malayalam
        | 0x0E50..=0x0E59 // Thai
        | 0x0ED0..=0x0ED9 // Lao
        | 0x0F20..=0x0F29 // Tibetan
        | 0xFF10..=0xFF19 // Fullwidth
    )
}

fn uni_space(ch: u32) -> bool {
    match char::from_u32(ch) {
        Some(c) => c.is_whitespace(),
        None => false,
    }
}

fn uni_word(ch: u32) -> bool {
    if ch == u32::from(b'_') {
        return true;
    }
    match char::from_u32(ch) {
        Some(c) => c.is_alphanumeric(),
        None => false,
    }
}

fn uni_linebreak(ch: u32) -> bool {
    matches!(
        ch,
        0x0a | 0x0b | 0x0c | 0x0d | 0x1c | 0x1d | 0x1e | 0x85 | 0x2028 | 0x2029
    )
}

fn category(chcode: u32, ch: u32) -> bool {
    match chcode {
        CAT_DIGIT => ascii_digit(ch),
        CAT_NOT_DIGIT => !ascii_digit(ch),
        CAT_SPACE => ascii_space(ch),
        CAT_NOT_SPACE => !ascii_space(ch),
        CAT_WORD => ascii_word(ch),
        CAT_NOT_WORD => !ascii_word(ch),
        CAT_LINEBREAK => is_linebreak(ch),
        CAT_NOT_LINEBREAK => !is_linebreak(ch),
        CAT_LOC_WORD => loc_word(ch),
        CAT_LOC_NOT_WORD => !loc_word(ch),
        CAT_UNI_DIGIT => uni_digit(ch),
        CAT_UNI_NOT_DIGIT => !uni_digit(ch),
        CAT_UNI_SPACE => uni_space(ch),
        CAT_UNI_NOT_SPACE => !uni_space(ch),
        CAT_UNI_WORD => uni_word(ch),
        CAT_UNI_NOT_WORD => !uni_word(ch),
        CAT_UNI_LINEBREAK => uni_linebreak(ch),
        CAT_UNI_NOT_LINEBREAK => !uni_linebreak(ch),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// The matcher
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MarkSnapshot {
    marks: Vec<isize>,
    lastmark: isize,
    lastindex: isize,
}

struct RepeatCtx {
    count: isize,
    /// Index in `code` of the REPEAT op's first argument (skip slot).
    pattern: usize,
    last_ptr: isize,
    prev: Option<usize>,
}

struct Matcher<'a> {
    s: &'a [u32],
    code: &'a [u32],
    beginning: usize,
    start: usize,
    end: usize,
    ptr: usize,
    marks: Vec<isize>,
    lastmark: isize,
    lastindex: isize,
    must_advance: bool,
    match_all: bool,
    repeats: Vec<RepeatCtx>,
    cur_repeat: Option<usize>,
    depth: u32,
}

impl<'a> Matcher<'a> {
    fn new(s: &'a [u32], code: &'a [u32], groups: usize) -> Self {
        Matcher {
            s,
            code,
            beginning: 0,
            start: 0,
            end: s.len(),
            ptr: 0,
            marks: vec![-1; groups * 2],
            lastmark: -1,
            lastindex: -1,
            must_advance: false,
            match_all: false,
            repeats: Vec::new(),
            cur_repeat: None,
            depth: 0,
        }
    }

    fn reset_capture(&mut self) {
        for m in self.marks.iter_mut() {
            *m = -1;
        }
        self.lastmark = -1;
        self.lastindex = -1;
    }

    #[inline]
    fn snapshot(&self) -> MarkSnapshot {
        MarkSnapshot {
            marks: self.marks.clone(),
            lastmark: self.lastmark,
            lastindex: self.lastindex,
        }
    }

    #[inline]
    fn restore(&mut self, snap: &MarkSnapshot) {
        self.marks.clone_from(&snap.marks);
        self.lastmark = snap.lastmark;
        self.lastindex = snap.lastindex;
    }

    fn at(&self, ptr: usize, atcode: u32) -> bool {
        let s = self.s;
        match atcode {
            AT_BEGINNING | AT_BEGINNING_STRING => ptr == self.beginning,
            AT_BEGINNING_LINE => ptr == self.beginning || is_linebreak(s[ptr - 1]),
            AT_END => (self.end - ptr == 1 && is_linebreak(s[ptr])) || ptr == self.end,
            AT_END_LINE => ptr == self.end || is_linebreak(s[ptr]),
            AT_END_STRING => ptr == self.end,
            AT_BOUNDARY => self.word_boundary(ptr, ascii_word),
            AT_NON_BOUNDARY => !self.word_boundary(ptr, ascii_word),
            AT_LOC_BOUNDARY => self.word_boundary(ptr, loc_word),
            AT_LOC_NON_BOUNDARY => !self.word_boundary(ptr, loc_word),
            AT_UNI_BOUNDARY => self.word_boundary(ptr, uni_word),
            AT_UNI_NON_BOUNDARY => !self.word_boundary(ptr, uni_word),
            _ => false,
        }
    }

    #[inline]
    fn word_boundary(&self, ptr: usize, is_word: fn(u32) -> bool) -> bool {
        if self.beginning == self.end {
            return false;
        }
        let thatp = ptr > self.beginning && is_word(self.s[ptr - 1]);
        let thisp = ptr < self.end && is_word(self.s[ptr]);
        thisp != thatp
    }

    /// `SRE(charset)` — is `ch` a member of the set starting at `set`?
    fn charset(&self, mut set: usize, ch: u32) -> bool {
        let code = self.code;
        let mut ok = true;
        loop {
            let op = code[set];
            set += 1;
            match op {
                OP_FAILURE => return !ok,
                OP_LITERAL => {
                    if ch == code[set] {
                        return ok;
                    }
                    set += 1;
                }
                OP_CATEGORY => {
                    if category(code[set], ch) {
                        return ok;
                    }
                    set += 1;
                }
                OP_CHARSET => {
                    // <CHARSET> <bitmap: 8 words>
                    if ch < 256 && (code[set + (ch / 32) as usize] & (1u32 << (ch & 31))) != 0 {
                        return ok;
                    }
                    set += 8;
                }
                OP_RANGE => {
                    if code[set] <= ch && ch <= code[set + 1] {
                        return ok;
                    }
                    set += 2;
                }
                OP_RANGE_UNI_IGNORE => {
                    if code[set] <= ch && ch <= code[set + 1] {
                        return ok;
                    }
                    let uch = upper_unicode(ch);
                    if code[set] <= uch && uch <= code[set + 1] {
                        return ok;
                    }
                    set += 2;
                }
                OP_NEGATE => ok = !ok,
                OP_BIGCHARSET => {
                    // <BIGCHARSET> <blockcount> <256 block-indices as bytes
                    //   packed into 64 words> <blocks: blockcount * 8 words>
                    let count = code[set] as usize;
                    set += 1;
                    let block: i64 = if ch < 0x10000 {
                        // 256 indices stored as bytes, little/native order
                        // inside u32 words.
                        let byte_index = (ch >> 8) as usize;
                        let word = code[set + byte_index / 4];
                        i64::from((word >> ((byte_index % 4) * 8)) & 0xff)
                    } else {
                        -1
                    };
                    set += 64;
                    if block >= 0 {
                        let block = block as usize;
                        let bit = (block * 256 + (ch as usize & 255)) as u32;
                        if (code[set + (bit / 32) as usize] & (1u32 << (bit & 31))) != 0 {
                            return ok;
                        }
                    }
                    set += count * 8;
                }
                _ => return false,
            }
        }
    }

    fn charset_loc_ignore(&self, set: usize, ch: u32) -> bool {
        let lo = lower_locale(ch);
        if self.charset(set, lo) {
            return true;
        }
        let up = upper_locale(ch);
        up != lo && self.charset(set, up)
    }

    /// `SRE(count)` — count repeated single-character matches of the
    /// item at `pat`, starting at `self.ptr`, up to `maxcount`.
    fn count(&mut self, pat: usize, maxcount: u32) -> Result<usize, RuntimeError> {
        let code = self.code;
        let ptr = self.ptr;
        let mut end = self.end;
        if maxcount != MAXREPEAT && (maxcount as usize) < end - ptr {
            end = ptr + maxcount as usize;
        }
        let s = self.s;
        let op = code[pat];
        let counted = match op {
            OP_IN => {
                let mut p = ptr;
                while p < end && self.charset(pat + 2, s[p]) {
                    p += 1;
                }
                p - ptr
            }
            OP_ANY => {
                let mut p = ptr;
                while p < end && !is_linebreak(s[p]) {
                    p += 1;
                }
                p - ptr
            }
            OP_ANY_ALL => end - ptr,
            OP_LITERAL => {
                let chr = code[pat + 1];
                let mut p = ptr;
                while p < end && s[p] == chr {
                    p += 1;
                }
                p - ptr
            }
            OP_NOT_LITERAL => {
                let chr = code[pat + 1];
                let mut p = ptr;
                while p < end && s[p] != chr {
                    p += 1;
                }
                p - ptr
            }
            OP_LITERAL_IGNORE => {
                let chr = code[pat + 1];
                let mut p = ptr;
                while p < end && lower_ascii(s[p]) == chr {
                    p += 1;
                }
                p - ptr
            }
            OP_LITERAL_UNI_IGNORE => {
                let chr = code[pat + 1];
                let mut p = ptr;
                while p < end && lower_unicode(s[p]) == chr {
                    p += 1;
                }
                p - ptr
            }
            OP_LITERAL_LOC_IGNORE => {
                let chr = code[pat + 1];
                let mut p = ptr;
                while p < end && char_loc_ignore(chr, s[p]) {
                    p += 1;
                }
                p - ptr
            }
            OP_NOT_LITERAL_IGNORE => {
                let chr = code[pat + 1];
                let mut p = ptr;
                while p < end && lower_ascii(s[p]) != chr {
                    p += 1;
                }
                p - ptr
            }
            OP_NOT_LITERAL_UNI_IGNORE => {
                let chr = code[pat + 1];
                let mut p = ptr;
                while p < end && lower_unicode(s[p]) != chr {
                    p += 1;
                }
                p - ptr
            }
            OP_NOT_LITERAL_LOC_IGNORE => {
                let chr = code[pat + 1];
                let mut p = ptr;
                while p < end && !char_loc_ignore(chr, s[p]) {
                    p += 1;
                }
                p - ptr
            }
            _ => {
                // General case: repeatedly match the subpattern.
                self.ptr = ptr;
                while self.ptr < end {
                    let matched = self.do_match(pat, false)?;
                    if !matched {
                        break;
                    }
                }
                let n = self.ptr - ptr;
                self.ptr = ptr;
                return Ok(n);
            }
        };
        self.ptr = ptr;
        Ok(counted)
    }

    /// `SRE(match)` — try to match the pattern at `pat` against the
    /// string starting at `self.ptr`. Returns whether it matched; on
    /// success `self.ptr` holds the end position.
    fn do_match(&mut self, pat: usize, toplevel: bool) -> Result<bool, RuntimeError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(runtime_error(
                "internal: regular expression recursion limit exceeded",
            ));
        }
        let r = self.do_match_inner(pat, toplevel);
        self.depth -= 1;
        r
    }

    fn do_match_inner(&mut self, mut pat: usize, toplevel: bool) -> Result<bool, RuntimeError> {
        let code = self.code;
        let mut ptr = self.ptr;
        let end = self.end;

        // Optimization info block at the head of the (sub)pattern.
        if code[pat] == OP_INFO {
            let min = code[pat + 3] as usize;
            if min != 0 && end - ptr < min {
                return Ok(false);
            }
            pat += code[pat + 1] as usize + 1;
        }

        loop {
            let op = code[pat];
            pat += 1;
            match op {
                OP_MARK => {
                    let i = code[pat] as usize;
                    let ii = i as isize;
                    if i & 1 != 0 {
                        self.lastindex = (i / 2 + 1) as isize;
                    }
                    if ii > self.lastmark {
                        let mut j = self.lastmark + 1;
                        while j < ii {
                            self.marks[j as usize] = -1;
                            j += 1;
                        }
                        self.lastmark = ii;
                    }
                    self.marks[i] = ptr as isize;
                    pat += 1;
                }
                OP_LITERAL => {
                    if ptr >= end || self.s[ptr] != code[pat] {
                        return Ok(false);
                    }
                    pat += 1;
                    ptr += 1;
                }
                OP_NOT_LITERAL => {
                    if ptr >= end || self.s[ptr] == code[pat] {
                        return Ok(false);
                    }
                    pat += 1;
                    ptr += 1;
                }
                OP_SUCCESS => {
                    if toplevel
                        && ((self.match_all && ptr != self.end)
                            || (self.must_advance && ptr == self.start))
                    {
                        return Ok(false);
                    }
                    self.ptr = ptr;
                    return Ok(true);
                }
                OP_AT => {
                    if !self.at(ptr, code[pat]) {
                        return Ok(false);
                    }
                    pat += 1;
                }
                OP_CATEGORY => {
                    if ptr >= end || !category(code[pat], self.s[ptr]) {
                        return Ok(false);
                    }
                    pat += 1;
                    ptr += 1;
                }
                OP_ANY => {
                    if ptr >= end || is_linebreak(self.s[ptr]) {
                        return Ok(false);
                    }
                    ptr += 1;
                }
                OP_ANY_ALL => {
                    if ptr >= end {
                        return Ok(false);
                    }
                    ptr += 1;
                }
                OP_IN => {
                    if ptr >= end || !self.charset(pat + 1, self.s[ptr]) {
                        return Ok(false);
                    }
                    pat += code[pat] as usize;
                    ptr += 1;
                }
                OP_LITERAL_IGNORE => {
                    if ptr >= end || lower_ascii(self.s[ptr]) != code[pat] {
                        return Ok(false);
                    }
                    pat += 1;
                    ptr += 1;
                }
                OP_LITERAL_UNI_IGNORE => {
                    if ptr >= end || lower_unicode(self.s[ptr]) != code[pat] {
                        return Ok(false);
                    }
                    pat += 1;
                    ptr += 1;
                }
                OP_LITERAL_LOC_IGNORE => {
                    if ptr >= end || !char_loc_ignore(code[pat], self.s[ptr]) {
                        return Ok(false);
                    }
                    pat += 1;
                    ptr += 1;
                }
                OP_NOT_LITERAL_IGNORE => {
                    if ptr >= end || lower_ascii(self.s[ptr]) == code[pat] {
                        return Ok(false);
                    }
                    pat += 1;
                    ptr += 1;
                }
                OP_NOT_LITERAL_UNI_IGNORE => {
                    if ptr >= end || lower_unicode(self.s[ptr]) == code[pat] {
                        return Ok(false);
                    }
                    pat += 1;
                    ptr += 1;
                }
                OP_NOT_LITERAL_LOC_IGNORE => {
                    if ptr >= end || char_loc_ignore(code[pat], self.s[ptr]) {
                        return Ok(false);
                    }
                    pat += 1;
                    ptr += 1;
                }
                OP_IN_IGNORE => {
                    if ptr >= end || !self.charset(pat + 1, lower_ascii(self.s[ptr])) {
                        return Ok(false);
                    }
                    pat += code[pat] as usize;
                    ptr += 1;
                }
                OP_IN_UNI_IGNORE => {
                    if ptr >= end || !self.charset(pat + 1, lower_unicode(self.s[ptr])) {
                        return Ok(false);
                    }
                    pat += code[pat] as usize;
                    ptr += 1;
                }
                OP_IN_LOC_IGNORE => {
                    if ptr >= end || !self.charset_loc_ignore(pat + 1, self.s[ptr]) {
                        return Ok(false);
                    }
                    pat += code[pat] as usize;
                    ptr += 1;
                }
                OP_JUMP | OP_INFO => {
                    pat += code[pat] as usize;
                }
                OP_BRANCH => {
                    let save = self.snapshot();
                    while code[pat] != 0 {
                        // Fast skip when the branch can't possibly match.
                        if code[pat + 1] == OP_LITERAL
                            && (ptr >= end || self.s[ptr] != code[pat + 2])
                        {
                            pat += code[pat] as usize;
                            continue;
                        }
                        if code[pat + 1] == OP_IN
                            && (ptr >= end || !self.charset(pat + 3, self.s[ptr]))
                        {
                            pat += code[pat] as usize;
                            continue;
                        }
                        self.ptr = ptr;
                        // Each alternative flows through its trailing JUMP to
                        // the BRANCH tail and on to the final SUCCESS, so it
                        // inherits `toplevel` (CPython `DO_JUMP`) — otherwise
                        // the `must_advance`/`match_all` guards are skipped for
                        // top-level alternations (e.g. `fullmatch('a|ab','ab')`).
                        if self.do_match(pat + 1, toplevel)? {
                            return Ok(true);
                        }
                        self.restore(&save);
                        pat += code[pat] as usize;
                    }
                    return Ok(false);
                }
                OP_REPEAT_ONE => {
                    let skip = code[pat] as usize;
                    let pmin = code[pat + 1] as usize;
                    let pmax = code[pat + 2];
                    if pmin > end - ptr {
                        return Ok(false);
                    }
                    self.ptr = ptr;
                    let cnt0 = self.count(pat + 3, pmax)?;
                    let mut cnt = cnt0 as isize;
                    if (cnt as usize) < pmin {
                        return Ok(false);
                    }
                    let tail = pat + skip;
                    let after = ptr + cnt as usize;
                    if code[tail] == OP_SUCCESS
                        && after == self.end
                        && !(toplevel && self.must_advance && after == self.start)
                    {
                        self.ptr = after;
                        return Ok(true);
                    }
                    let save = self.snapshot();
                    let orig = ptr;
                    let pmin_i = pmin as isize;
                    if code[tail] == OP_LITERAL {
                        let chr = code[tail + 1];
                        loop {
                            while cnt >= pmin_i && {
                                let pos = orig + cnt as usize;
                                pos >= end || self.s[pos] != chr
                            } {
                                cnt -= 1;
                            }
                            if cnt < pmin_i {
                                break;
                            }
                            let pos = orig + cnt as usize;
                            self.ptr = pos;
                            // The tail is the continuation of *this* match,
                            // so it inherits `toplevel` (CPython `DO_JUMP`)
                            // — otherwise the trailing SUCCESS would skip the
                            // empty-match / `must_advance` guard and the
                            // scanner could loop on a zero-width match.
                            if self.do_match(tail, toplevel)? {
                                return Ok(true);
                            }
                            self.restore(&save);
                            cnt -= 1;
                        }
                    } else {
                        while cnt >= pmin_i {
                            let pos = orig + cnt as usize;
                            self.ptr = pos;
                            if self.do_match(tail, toplevel)? {
                                return Ok(true);
                            }
                            self.restore(&save);
                            cnt -= 1;
                        }
                    }
                    return Ok(false);
                }
                OP_MIN_REPEAT_ONE => {
                    let skip = code[pat] as usize;
                    let pmin = code[pat + 1] as usize;
                    let pmax = code[pat + 2];
                    if pmin > end - ptr {
                        return Ok(false);
                    }
                    self.ptr = ptr;
                    let mut cnt: isize = 0;
                    if pmin != 0 {
                        let r = self.count(pat + 3, code[pat + 1])?;
                        if r < pmin {
                            return Ok(false);
                        }
                        cnt = r as isize;
                        ptr += cnt as usize;
                    }
                    let tail = pat + skip;
                    if code[tail] == OP_SUCCESS
                        && !(toplevel
                            && ((self.match_all && ptr != self.end)
                                || (self.must_advance && ptr == self.start)))
                    {
                        self.ptr = ptr;
                        return Ok(true);
                    }
                    let save = self.snapshot();
                    loop {
                        if !(pmax == MAXREPEAT || (cnt as u32) <= pmax) {
                            break;
                        }
                        self.ptr = ptr;
                        if self.do_match(tail, toplevel)? {
                            return Ok(true);
                        }
                        self.restore(&save);
                        self.ptr = ptr;
                        let r = self.count(pat + 3, 1)?;
                        if r == 0 {
                            break;
                        }
                        ptr += 1;
                        cnt += 1;
                    }
                    return Ok(false);
                }
                OP_POSSESSIVE_REPEAT_ONE => {
                    let skip = code[pat] as usize;
                    let pmin = code[pat + 1] as usize;
                    let pmax = code[pat + 2];
                    if ptr + pmin > end {
                        return Ok(false);
                    }
                    self.ptr = ptr;
                    let cnt = self.count(pat + 3, pmax)?;
                    ptr += cnt;
                    if cnt < pmin {
                        return Ok(false);
                    }
                    pat += skip;
                    if code[pat] == OP_SUCCESS
                        && ptr == self.end
                        && !(toplevel && self.must_advance && ptr == self.start)
                    {
                        self.ptr = ptr;
                        return Ok(true);
                    }
                    // Evaluate the tail in this same frame.
                }
                OP_REPEAT => {
                    let skip = code[pat] as usize;
                    let rep = RepeatCtx {
                        count: -1,
                        pattern: pat,
                        last_ptr: -1,
                        prev: self.cur_repeat,
                    };
                    let idx = self.repeats.len();
                    self.repeats.push(rep);
                    self.cur_repeat = Some(idx);
                    self.ptr = ptr;
                    // The MAX_UNTIL/MIN_UNTIL operator (reached via `pat+skip`)
                    // ultimately continues to the pattern tail and SUCCESS, so
                    // it inherits `toplevel` (CPython `DO_JUMP`). Forcing it to
                    // `false` would skip the `must_advance` guard and let the
                    // scanner loop forever on a zero-width repeat such as
                    // `(a)*` over an empty match.
                    let r = self.do_match(pat + skip, toplevel);
                    self.cur_repeat = self.repeats[idx].prev;
                    self.repeats.truncate(idx);
                    return r;
                }
                OP_MAX_UNTIL => {
                    let idx = self
                        .cur_repeat
                        .ok_or_else(|| runtime_error("internal: MAX_UNTIL without REPEAT"))?;
                    self.ptr = ptr;
                    let count = self.repeats[idx].count + 1;
                    let rpat = self.repeats[idx].pattern;
                    let rmin = code[rpat + 1] as isize;
                    let rmax = code[rpat + 2];
                    let item = rpat + 3;
                    if count < rmin {
                        self.repeats[idx].count = count;
                        self.ptr = ptr;
                        // Repeated-item matches inherit `toplevel` (CPython
                        // `DO_JUMP` for JUMP_MAX_UNTIL_1/_2): when the item can
                        // match empty (e.g. `(a?)*`), the recursion bottoms out
                        // at the tail SUCCESS, which must still see the
                        // `must_advance`/`match_all` guards.
                        if self.do_match(item, toplevel)? {
                            return Ok(true);
                        }
                        self.repeats[idx].count = count - 1;
                        self.ptr = ptr;
                        return Ok(false);
                    }
                    if (count < rmax as isize || rmax == MAXREPEAT)
                        && (ptr as isize) != self.repeats[idx].last_ptr
                    {
                        self.repeats[idx].count = count;
                        let save = self.snapshot();
                        let saved_last = self.repeats[idx].last_ptr;
                        self.repeats[idx].last_ptr = ptr as isize;
                        self.ptr = ptr;
                        if self.do_match(item, toplevel)? {
                            return Ok(true);
                        }
                        self.repeats[idx].last_ptr = saved_last;
                        self.restore(&save);
                        self.repeats[idx].count = count - 1;
                        self.ptr = ptr;
                    }
                    let prev = self.repeats[idx].prev;
                    self.cur_repeat = prev;
                    self.ptr = ptr;
                    // Tail continuation inherits `toplevel` (CPython
                    // `DO_JUMP`) so the trailing SUCCESS still honours the
                    // `must_advance`/`match_all` guards.
                    let r = self.do_match(pat, toplevel)?;
                    self.cur_repeat = Some(idx);
                    if r {
                        return Ok(true);
                    }
                    self.ptr = ptr;
                    return Ok(false);
                }
                OP_MIN_UNTIL => {
                    let idx = self
                        .cur_repeat
                        .ok_or_else(|| runtime_error("internal: MIN_UNTIL without REPEAT"))?;
                    self.ptr = ptr;
                    let count = self.repeats[idx].count + 1;
                    let rpat = self.repeats[idx].pattern;
                    let rmin = code[rpat + 1] as isize;
                    let rmax = code[rpat + 2];
                    let item = rpat + 3;
                    if count < rmin {
                        self.repeats[idx].count = count;
                        self.ptr = ptr;
                        // Inherit `toplevel` (CPython `DO_JUMP` JUMP_MIN_UNTIL_1).
                        if self.do_match(item, toplevel)? {
                            return Ok(true);
                        }
                        self.repeats[idx].count = count - 1;
                        self.ptr = ptr;
                        return Ok(false);
                    }
                    let prev = self.repeats[idx].prev;
                    let save = self.snapshot();
                    self.cur_repeat = prev;
                    self.ptr = ptr;
                    let r = self.do_match(pat, toplevel)?;
                    self.cur_repeat = Some(idx);
                    if r {
                        return Ok(true);
                    }
                    self.restore(&save);
                    self.ptr = ptr;
                    if (count >= rmax as isize && rmax != MAXREPEAT)
                        || (ptr as isize) == self.repeats[idx].last_ptr
                    {
                        return Ok(false);
                    }
                    self.repeats[idx].count = count;
                    let saved_last = self.repeats[idx].last_ptr;
                    self.repeats[idx].last_ptr = ptr as isize;
                    self.ptr = ptr;
                    // Inherit `toplevel` (CPython `DO_JUMP` JUMP_MIN_UNTIL_3).
                    if self.do_match(item, toplevel)? {
                        return Ok(true);
                    }
                    self.repeats[idx].last_ptr = saved_last;
                    self.repeats[idx].count = count - 1;
                    self.ptr = ptr;
                    return Ok(false);
                }
                OP_POSSESSIVE_REPEAT => {
                    let skip = code[pat] as usize;
                    let pmin = code[pat + 1] as usize;
                    let pmax = code[pat + 2];
                    self.ptr = ptr;
                    let rep = RepeatCtx {
                        count: -1,
                        pattern: usize::MAX,
                        last_ptr: -1,
                        prev: self.cur_repeat,
                    };
                    let idx = self.repeats.len();
                    self.repeats.push(rep);
                    self.cur_repeat = Some(idx);
                    let body = pat + 3;
                    let mut cnt: usize = 0;
                    let mut failed = false;
                    while cnt < pmin {
                        if self.do_match(body, false)? {
                            cnt += 1;
                        } else {
                            failed = true;
                            break;
                        }
                    }
                    if failed {
                        self.ptr = ptr;
                        self.cur_repeat = self.repeats[idx].prev;
                        self.repeats.truncate(idx);
                        return Ok(false);
                    }
                    let mut prev_ptr: Option<usize> = None;
                    loop {
                        let can_more = (pmax == MAXREPEAT || (cnt as u32) < pmax)
                            && Some(self.ptr) != prev_ptr;
                        if !can_more {
                            break;
                        }
                        let save = self.snapshot();
                        prev_ptr = Some(self.ptr);
                        if self.do_match(body, false)? {
                            cnt += 1;
                        } else {
                            self.restore(&save);
                            self.ptr = prev_ptr.unwrap();
                            break;
                        }
                    }
                    self.cur_repeat = self.repeats[idx].prev;
                    self.repeats.truncate(idx);
                    pat += skip + 1;
                    ptr = self.ptr;
                    continue;
                }
                OP_ATOMIC_GROUP => {
                    let skip = code[pat] as usize;
                    self.ptr = ptr;
                    if self.do_match(pat + 1, false)? {
                        pat += skip;
                        ptr = self.ptr;
                    } else {
                        self.ptr = ptr;
                        return Ok(false);
                    }
                }
                OP_GROUPREF => {
                    if !self.groupref_match(pat, GroupRefKind::Exact, end, ptr, &mut ptr) {
                        return Ok(false);
                    }
                    pat += 1;
                }
                OP_GROUPREF_IGNORE => {
                    if !self.groupref_match(pat, GroupRefKind::Ascii, end, ptr, &mut ptr) {
                        return Ok(false);
                    }
                    pat += 1;
                }
                OP_GROUPREF_UNI_IGNORE => {
                    if !self.groupref_match(pat, GroupRefKind::Unicode, end, ptr, &mut ptr) {
                        return Ok(false);
                    }
                    pat += 1;
                }
                OP_GROUPREF_LOC_IGNORE => {
                    if !self.groupref_match(pat, GroupRefKind::Locale, end, ptr, &mut ptr) {
                        return Ok(false);
                    }
                    pat += 1;
                }
                OP_GROUPREF_EXISTS => {
                    let g = code[pat] as usize;
                    let skip = code[pat + 1] as usize;
                    let groupref = (g * 2) as isize;
                    let set = if groupref >= self.lastmark {
                        false
                    } else {
                        let p = self.marks[groupref as usize];
                        let e = self.marks[groupref as usize + 1];
                        !(p < 0 || e < 0 || e < p)
                    };
                    if set {
                        pat += 2;
                    } else {
                        pat += skip;
                    }
                }
                OP_ASSERT => {
                    let skip = code[pat] as usize;
                    let back = code[pat + 1] as usize;
                    if ptr - self.beginning < back {
                        return Ok(false);
                    }
                    self.ptr = ptr - back;
                    if !self.do_match(pat + 2, false)? {
                        return Ok(false);
                    }
                    pat += skip;
                }
                OP_ASSERT_NOT => {
                    let skip = code[pat] as usize;
                    let back = code[pat + 1] as usize;
                    if ptr - self.beginning >= back {
                        self.ptr = ptr - back;
                        let save = self.snapshot();
                        let matched = self.do_match(pat + 2, false)?;
                        self.restore(&save);
                        if matched {
                            return Ok(false);
                        }
                    }
                    pat += skip;
                }
                OP_FAILURE => return Ok(false),
                _ => {
                    return Err(value_error(format!(
                        "internal: unsupported sre opcode {op}"
                    )));
                }
            }
        }
    }

    fn groupref_match(
        &self,
        pat: usize,
        kind: GroupRefKind,
        end: usize,
        start_ptr: usize,
        ptr_out: &mut usize,
    ) -> bool {
        let g = self.code[pat] as usize;
        let groupref = (g * 2) as isize;
        if groupref >= self.lastmark {
            return false;
        }
        let p0 = self.marks[groupref as usize];
        let e0 = self.marks[groupref as usize + 1];
        if p0 < 0 || e0 < 0 || e0 < p0 {
            return false;
        }
        let mut p = p0 as usize;
        let e = e0 as usize;
        let mut ptr = start_ptr;
        while p < e {
            if ptr >= end {
                return false;
            }
            let a = self.s[ptr];
            let b = self.s[p];
            let eq = match kind {
                GroupRefKind::Exact => a == b,
                GroupRefKind::Ascii => lower_ascii(a) == lower_ascii(b),
                GroupRefKind::Unicode => lower_unicode(a) == lower_unicode(b),
                GroupRefKind::Locale => lower_locale(a) == lower_locale(b),
            };
            if !eq {
                return false;
            }
            p += 1;
            ptr += 1;
        }
        *ptr_out = ptr;
        true
    }

    /// `SRE(search)` — scan for the leftmost match at or after
    /// `self.start`. Returns the start position of the match on success.
    fn search(&mut self) -> Result<Option<usize>, RuntimeError> {
        // Determine where the real pattern starts (after any INFO block)
        // for the anchored-pattern fast reject.
        let mut p = 0usize;
        let mut min = 0usize;
        if self.code[0] == OP_INFO {
            min = self.code[3] as usize;
            p = 1 + self.code[1] as usize;
        }
        let anchored = self.code.get(p) == Some(&OP_AT)
            && matches!(
                self.code.get(p + 1).copied(),
                Some(AT_BEGINNING) | Some(AT_BEGINNING_STRING)
            );

        let mut ptr = self.start;
        let mut first = true;
        loop {
            if min != 0 && self.end.saturating_sub(ptr) < min {
                return Ok(None);
            }
            self.start = ptr;
            self.ptr = ptr;
            self.reset_capture();
            let matched = self.do_match(0, true)?;
            if first {
                self.must_advance = false;
                first = false;
            }
            if matched {
                return Ok(Some(ptr));
            }
            if anchored {
                return Ok(None);
            }
            if ptr >= self.end {
                return Ok(None);
            }
            ptr += 1;
        }
    }
}

#[derive(Clone, Copy)]
enum GroupRefKind {
    Exact,
    Ascii,
    Unicode,
    Locale,
}

// ---------------------------------------------------------------------------
// Module functions
// ---------------------------------------------------------------------------

fn arg_i64(args: &[Object], i: usize, name: &str) -> Result<i64, RuntimeError> {
    args.get(i)
        .and_then(|o| o.as_i64())
        .ok_or_else(|| type_error(format!("_sre: expected int for {name}")))
}

/// Read a Python sequence of small ints into a `Vec<u32>`.
fn codeseq_to_vec(obj: &Object) -> Result<Vec<u32>, RuntimeError> {
    let collect = |items: &[Object]| -> Result<Vec<u32>, RuntimeError> {
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            let v = it
                .as_i64()
                .ok_or_else(|| type_error("_sre.compile: code must be a sequence of ints"))?;
            if !(0..=i64::from(u32::MAX)).contains(&v) {
                return Err(value_error("_sre.compile: code value out of range"));
            }
            out.push(v as u32);
        }
        Ok(out)
    };
    match obj {
        Object::List(l) => collect(&l.borrow()),
        Object::Tuple(t) => collect(t),
        _ => Err(type_error("_sre.compile: code must be a list or tuple")),
    }
}

/// Decode the subject into code points (str) or byte values (bytes).
fn subject_to_vec(obj: &Object) -> Result<Vec<u32>, RuntimeError> {
    match obj {
        Object::Str(s) => Ok(s.chars().map(|c| c as u32).collect()),
        Object::Bytes(b) => Ok(b.iter().map(|&x| u32::from(x)).collect()),
        Object::ByteArray(b) => Ok(b.borrow().iter().map(|&x| u32::from(x)).collect()),
        _ => Err(type_error("expected string or bytes-like object")),
    }
}

/// `_sre.compile(code, groups)` → an integer handle into the registry.
fn sre_compile(args: &[Object]) -> Result<Object, RuntimeError> {
    let code = codeseq_to_vec(
        args.first()
            .ok_or_else(|| type_error("_sre.compile: code"))?,
    )?;
    let groups = arg_i64(args, 1, "groups")?.max(0) as usize;
    let handle = REGISTRY.with(|reg| {
        let mut reg = reg.borrow_mut();
        reg.push(Rc::new(CompiledCode { code, groups }));
        reg.len() - 1
    });
    Ok(Object::Int(handle as i64))
}

/// `_sre.exec(handle, string, pos, endpos, mode, must_advance)`.
///
/// Returns `None` on no match, otherwise a tuple
/// `(start, end, lastindex, marks)` where `marks` is a tuple of
/// `2 * groups` code-point indices (`-1` for an unset group).
fn sre_exec(args: &[Object]) -> Result<Object, RuntimeError> {
    let handle = arg_i64(args, 0, "handle")? as usize;
    let cc = REGISTRY.with(|reg| reg.borrow().get(handle).cloned());
    let cc = cc.ok_or_else(|| value_error("_sre.exec: invalid pattern handle"))?;
    let subject = subject_to_vec(args.get(1).ok_or_else(|| type_error("_sre.exec: string"))?)?;
    let slen = subject.len() as i64;
    let pos = arg_i64(args, 2, "pos")?.clamp(0, slen) as usize;
    let endpos = arg_i64(args, 3, "endpos")?.clamp(0, slen) as usize;
    let mode = arg_i64(args, 4, "mode")?;
    let must_advance = args
        .get(5)
        .map(|o| o.as_i64().unwrap_or(0) != 0)
        .unwrap_or(false);

    if pos > endpos {
        return Ok(Object::None);
    }

    let mut m = Matcher::new(&subject, &cc.code, cc.groups);
    m.end = endpos;
    m.start = pos;
    m.ptr = pos;
    m.must_advance = must_advance;

    let (mstart, ok) = match mode {
        // 1 = match (anchored at pos)
        1 => {
            let r = m.do_match(0, true)?;
            (pos, r)
        }
        // 2 = fullmatch (anchored + must reach endpos)
        2 => {
            m.match_all = true;
            let r = m.do_match(0, true)?;
            (pos, r)
        }
        // 0 = search
        _ => match m.search()? {
            Some(s) => (s, true),
            None => (0, false),
        },
    };

    if !ok {
        return Ok(Object::None);
    }

    let mend = m.ptr;
    let mut marks_out: Vec<Object> = Vec::with_capacity(cc.groups * 2);
    for i in 0..cc.groups * 2 {
        let v = if (i as isize) <= m.lastmark {
            m.marks[i]
        } else {
            -1
        };
        marks_out.push(Object::Int(v as i64));
    }
    Ok(Object::new_tuple(vec![
        Object::Int(mstart as i64),
        Object::Int(mend as i64),
        Object::Int(m.lastindex as i64),
        Object::new_tuple(marks_out),
    ]))
}

fn sre_ascii_tolower(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(lower_ascii(
        arg_i64(args, 0, "ch")? as u32
    ))))
}
fn sre_ascii_iscased(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(ascii_iscased(arg_i64(args, 0, "ch")? as u32)))
}
fn sre_unicode_tolower(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(lower_unicode(
        arg_i64(args, 0, "ch")? as u32,
    ))))
}
fn sre_unicode_iscased(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(
        unicode_iscased(arg_i64(args, 0, "ch")? as u32),
    ))
}
fn sre_getcodesize(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(CODESIZE))
}
/// `_sre.getlower(ch, flags)`.
fn sre_getlower(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = arg_i64(args, 0, "ch")? as u32;
    let flags = arg_i64(args, 1, "flags").unwrap_or(0);
    // SRE_FLAG_LOCALE = 4, SRE_FLAG_UNICODE = 32
    let lowered = if flags & 4 != 0 {
        lower_locale(ch)
    } else if flags & 32 != 0 {
        lower_unicode(ch)
    } else {
        lower_ascii(ch)
    };
    Ok(Object::Int(i64::from(lowered)))
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_sre"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("WeavePy native SRE regular-expression core (RFC 0035)."),
        );
        d.insert(DictKey(Object::from_static("MAGIC")), Object::Int(MAGIC));
        d.insert(
            DictKey(Object::from_static("CODESIZE")),
            Object::Int(CODESIZE),
        );
        d.insert(
            DictKey(Object::from_static("MAXREPEAT")),
            Object::Int(MAXREPEAT_I64),
        );
        d.insert(
            DictKey(Object::from_static("MAXGROUPS")),
            Object::Int(MAXGROUPS),
        );
        d.insert(
            DictKey(Object::from_static("compile")),
            b("compile", sre_compile),
        );
        d.insert(DictKey(Object::from_static("exec")), b("exec", sre_exec));
        d.insert(
            DictKey(Object::from_static("ascii_tolower")),
            b("ascii_tolower", sre_ascii_tolower),
        );
        d.insert(
            DictKey(Object::from_static("ascii_iscased")),
            b("ascii_iscased", sre_ascii_iscased),
        );
        d.insert(
            DictKey(Object::from_static("unicode_tolower")),
            b("unicode_tolower", sre_unicode_tolower),
        );
        d.insert(
            DictKey(Object::from_static("unicode_iscased")),
            b("unicode_iscased", sre_unicode_iscased),
        );
        d.insert(
            DictKey(Object::from_static("getcodesize")),
            b("getcodesize", sre_getcodesize),
        );
        d.insert(
            DictKey(Object::from_static("getlower")),
            b("getlower", sre_getlower),
        );
    }
    Rc::new(PyModule {
        name: "_sre".to_owned(),
        filename: None,
        dict,
    })
}
