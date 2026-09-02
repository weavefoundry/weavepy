//! String-level Unicode case operations — RFC 0050 WS4.
//!
//! Mirrors CPython's `do_upper`/`do_lower`/`do_title`/`do_capitalize`/
//! `do_swapcase`/`do_casefold` (Objects/unicodeobject.c) over the generated
//! UCD 15.1.0 case tables, including the Final_Sigma rule and the full
//! (multi-code-point) SpecialCasing expansions.

use crate::stdlib::ucd;

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
    let cps: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for i in 0..cps.len() {
        push_cps(&mut out, lower_at(&cps, i));
    }
    out
}

pub fn upper(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_cps(&mut out, ucd::to_upper_full(c as u32));
    }
    out
}

/// `str.casefold()` — context-free full fold (no Final_Sigma).
pub fn casefold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_cps(&mut out, ucd::to_fold_full(c as u32));
    }
    out
}

pub fn title(s: &str) -> String {
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

/// `Py_UNICODE_ISSPACE` — differs from Rust's `char::is_whitespace` (e.g.
/// U+001C..U+001F are Python whitespace).
pub fn is_space(c: char) -> bool {
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
