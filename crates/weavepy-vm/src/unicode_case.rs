//! String-level Unicode case operations — RFC 0050 WS4.
//!
//! Mirrors CPython's `do_upper`/`do_lower`/`do_title`/`do_capitalize`/
//! `do_swapcase`/`do_casefold` (Objects/unicodeobject.c) over the generated
//! UCD 16.0.0 case tables, including the Final_Sigma rule and the full
//! (multi-code-point) SpecialCasing expansions.

use crate::stdlib::ucd;
use std::mem::MaybeUninit;

const CAPITAL_SIGMA: u32 = 0x3A3;
const FINAL_SIGMA: u32 = 0x3C2;
const SMALL_SIGMA: u32 = 0x3C3;

fn push_cps(out: &mut String, map: ucd::CaseMap) {
    for &cp in map.as_slice() {
        out.push(char::from_u32(cp).expect("case mapping is scalar"));
    }
}

/// `handle_capital_sigma`: Σ lowercases to ς at the end of a cased run
/// (Unicode 3.13.2 Final_Sigma; scans the *original* string).
fn capital_sigma_at(cps: &[char], i: usize) -> u32 {
    let mut j = i;
    let before_cased = loop {
        if j == 0 {
            break false;
        }
        j -= 1;
        let c = cps[j] as u32;
        if !ucd::is_case_ignorable(c) {
            break ucd::is_cased(c);
        }
    };
    if !before_cased {
        return SMALL_SIGMA;
    }
    let mut j = i + 1;
    while j < cps.len() {
        let c = cps[j] as u32;
        if !ucd::is_case_ignorable(c) {
            return if ucd::is_cased(c) {
                SMALL_SIGMA
            } else {
                FINAL_SIGMA
            };
        }
        j += 1;
    }
    FINAL_SIGMA
}

/// CPython's `lower_ucs4`: ToLowerFull with the capital-sigma special case.
fn lower_at(cps: &[char], i: usize) -> ucd::CaseMap {
    let c = cps[i] as u32;
    if c == CAPITAL_SIGMA {
        ucd::CaseMap::single(capital_sigma_at(cps, i))
    } else {
        ucd::to_lower_full(c)
    }
}

pub fn lower(s: &str) -> String {
    if s.is_ascii() {
        return ascii_case_map(s, AsciiCase::Lower);
    }
    let cps: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for i in 0..cps.len() {
        push_cps(&mut out, lower_at(&cps, i));
    }
    out
}

pub fn upper(s: &str) -> String {
    if s.is_ascii() {
        return ascii_case_map(s, AsciiCase::Upper);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_cps(&mut out, ucd::to_upper_full(c as u32));
    }
    out
}

/// `str.casefold()` — context-free full fold (no Final_Sigma).
pub fn casefold(s: &str) -> String {
    if s.is_ascii() {
        return ascii_case_map(s, AsciiCase::Lower);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_cps(&mut out, ucd::to_fold_full(c as u32));
    }
    out
}

pub fn title(s: &str) -> String {
    if s.is_ascii() {
        return ascii_case_map(s, AsciiCase::Title);
    }
    let cps: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut previous_is_cased = false;
    for i in 0..cps.len() {
        if previous_is_cased {
            push_cps(&mut out, lower_at(&cps, i));
        } else {
            push_cps(&mut out, ucd::to_title_full(cps[i] as u32));
        }
        previous_is_cased = ucd::is_cased(cps[i] as u32);
    }
    out
}

/// `str.capitalize()` — first char titlecased (3.8+ semantics), rest
/// lowered with the sigma rule.
pub fn capitalize(s: &str) -> String {
    if s.is_ascii() {
        return ascii_case_map(s, AsciiCase::Capitalize);
    }
    let cps: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    if let Some(&first) = cps.first() {
        push_cps(&mut out, ucd::to_title_full(first as u32));
    }
    for i in 1..cps.len() {
        push_cps(&mut out, lower_at(&cps, i));
    }
    out
}

pub fn swapcase(s: &str) -> String {
    if s.is_ascii() {
        return ascii_case_map(s, AsciiCase::Swap);
    }
    let cps: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for i in 0..cps.len() {
        let c = cps[i] as u32;
        let flags = ucd::case_flags(c);
        if flags & ucd::FLAG_UPPER != 0 {
            push_cps(&mut out, lower_at(&cps, i));
        } else if flags & ucd::FLAG_LOWER != 0 {
            push_cps(&mut out, ucd::to_upper_full(c));
        } else {
            out.push(cps[i]);
        }
    }
    out
}

/// The case operations' ASCII forms, which [`ascii_case_into`] applies
/// to a whole buffer at once. (`casefold` of ASCII is `Lower`.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsciiCase {
    Upper,
    Lower,
    Title,
    Capitalize,
    Swap,
}

/// Write the `case` mapping of ASCII `src` into `dst` (of equal length),
/// initializing all of it.
///
/// Each output byte depends only on its input byte and, for `Title`, the
/// one before it (a letter follows a cased letter exactly when the byte
/// before it is alphabetic), so every loop below is branch-free and
/// vectorizes, where the per-character case-table lookups of the general
/// path don't. Each mapping flips the case bit (`0x20`) of selected
/// letters only, so ASCII stays ASCII; a non-ASCII byte is never a letter
/// here and passes through unchanged.
pub fn ascii_case_into(case: AsciiCase, src: &[u8], dst: &mut [MaybeUninit<u8>]) {
    #[inline(always)]
    fn is_lower(b: u8) -> bool {
        b.wrapping_sub(b'a') < 26
    }
    #[inline(always)]
    fn is_upper(b: u8) -> bool {
        b.wrapping_sub(b'A') < 26
    }
    #[inline(always)]
    fn is_alpha(b: u8) -> bool {
        (b | 0x20).wrapping_sub(b'a') < 26
    }
    #[inline(always)]
    fn flip(b: u8, when: bool) -> MaybeUninit<u8> {
        MaybeUninit::new(b ^ (u8::from(when) << 5))
    }
    assert_eq!(src.len(), dst.len());
    let (Some(&first), Some(head)) = (src.first(), dst.first_mut()) else {
        return;
    };
    match case {
        AsciiCase::Upper => {
            for (d, &c) in dst.iter_mut().zip(src) {
                *d = flip(c, is_lower(c));
            }
        }
        AsciiCase::Lower => {
            for (d, &c) in dst.iter_mut().zip(src) {
                *d = flip(c, is_upper(c));
            }
        }
        AsciiCase::Swap => {
            for (d, &c) in dst.iter_mut().zip(src) {
                *d = flip(c, is_alpha(c));
            }
        }
        AsciiCase::Capitalize => {
            *head = flip(first, is_lower(first));
            for (d, &c) in dst[1..].iter_mut().zip(&src[1..]) {
                *d = flip(c, is_upper(c));
            }
        }
        AsciiCase::Title => {
            *head = flip(first, is_lower(first));
            for ((d, &c), &prev) in dst[1..].iter_mut().zip(&src[1..]).zip(src) {
                let cased = is_alpha(prev);
                *d = flip(c, (cased && is_upper(c)) || (!cased && is_lower(c)));
            }
        }
    }
}

/// [`ascii_case_into`] into a new `String`.
fn ascii_case_map(s: &str, case: AsciiCase) -> String {
    let mut out = Vec::with_capacity(s.len());
    ascii_case_into(case, s.as_bytes(), &mut out.spare_capacity_mut()[..s.len()]);
    // SAFETY: `ascii_case_into` initialized all `s.len()` bytes.
    unsafe { out.set_len(s.len()) };
    String::from_utf8(out).expect("ASCII case mapping")
}

/// `Py_UNICODE_ISSPACE` — differs from Rust's `char::is_whitespace` (e.g.
/// U+001C..U+001F are Python whitespace).
pub fn is_space(c: char) -> bool {
    if c.is_ascii() {
        return matches!(c, '\t'..='\r' | '\x1c'..=' ');
    }
    ucd::case_flags(c as u32) & ucd::FLAG_SPACE != 0
}

pub fn is_alpha(c: char) -> bool {
    ucd::case_flags(c as u32) & ucd::FLAG_ALPHA != 0
}

pub fn is_alnum(c: char) -> bool {
    ucd::case_flags(c as u32) & ucd::FLAG_ALNUM != 0
}

pub fn is_printable(c: char) -> bool {
    ucd::case_flags(c as u32) & ucd::FLAG_PRINTABLE != 0
}

pub fn is_xid_start(c: char) -> bool {
    ucd::case_flags(c as u32) & ucd::FLAG_XID_START != 0
}

pub fn is_xid_continue(c: char) -> bool {
    ucd::case_flags(c as u32) & ucd::FLAG_XID_CONTINUE != 0
}

/// `Py_UNICODE_ISTITLE`: category Lt (U+01C5 'ǅ', U+1FFC 'ῼ', …). Rust's
/// `char` API has no Lt predicate, so this is the UCD flag the VM's own
/// `str.istitle` uses.
pub fn is_titlecase(c: char) -> bool {
    ucd::case_flags(c as u32) & ucd::FLAG_TITLE != 0
}

/// `unicode_isupper_impl`: no lower/title chars and at least one upper.
pub fn str_isupper(s: &str) -> bool {
    let mut cased = false;
    for c in s.chars() {
        let f = ucd::case_flags(c as u32);
        if f & (ucd::FLAG_LOWER | ucd::FLAG_TITLE) != 0 {
            return false;
        }
        if f & ucd::FLAG_UPPER != 0 {
            cased = true;
        }
    }
    cased
}

/// `unicode_islower_impl`: no upper/title chars and at least one lower.
pub fn str_islower(s: &str) -> bool {
    let mut cased = false;
    for c in s.chars() {
        let f = ucd::case_flags(c as u32);
        if f & (ucd::FLAG_UPPER | ucd::FLAG_TITLE) != 0 {
            return false;
        }
        if f & ucd::FLAG_LOWER != 0 {
            cased = true;
        }
    }
    cased
}

/// `unicode_istitle_impl`: cased runs start upper/title, and >=1 cased char.
pub fn str_istitle(s: &str) -> bool {
    let mut cased = false;
    let mut previous_is_cased = false;
    for c in s.chars() {
        let f = ucd::case_flags(c as u32);
        if f & (ucd::FLAG_UPPER | ucd::FLAG_TITLE) != 0 {
            if previous_is_cased {
                return false;
            }
            previous_is_cased = true;
            cased = true;
        } else if f & ucd::FLAG_LOWER != 0 {
            if !previous_is_cased {
                return false;
            }
            previous_is_cased = true;
            cased = true;
        } else {
            previous_is_cased = false;
        }
    }
    cased
}
