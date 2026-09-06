//! Unicode numeric-classification predicates for `str.isdecimal` /
//! `str.isdigit` / `str.isnumeric`, mirroring CPython exactly.
//!
//! Backed by the generated `stdlib/ucd` case-record flags
//! (`tools/gen_ucd_tables.py`, probed from host CPython's `str` methods), so
//! these answers track the bundled UCD version (16.0.0, matching CPython
//! 3.14) instead of a separately maintained range table. The three predicates
//! nest (decimal ⊂ digit ⊂ numeric).

use crate::stdlib::ucd;

/// `str.isdecimal()` predicate for a single character (Unicode
/// `Numeric_Type=Decimal`, equivalently General_Category `Nd`).
pub fn is_decimal_char(c: char) -> bool {
    ucd::case_flags(c as u32) & ucd::FLAG_DECIMAL != 0
}

/// `str.isdigit()` predicate (Unicode `Numeric_Type` in {Decimal, Digit}):
/// superscripts `²³` count, but fractions `¼` do not.
pub fn is_digit_char(c: char) -> bool {
    ucd::case_flags(c as u32) & ucd::FLAG_DIGIT != 0
}

/// `str.isnumeric()` predicate (Unicode `Numeric_Type` in {Decimal, Digit,
/// Numeric}): includes fractions `¼`, Roman numerals, and CJK letter-numerals
/// like `一`.
pub fn is_numeric_char(c: char) -> bool {
    ucd::case_flags(c as u32) & ucd::FLAG_NUMERIC != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_and_samples() {
        assert!(is_decimal_char('7') && is_digit_char('7') && is_numeric_char('7'));
        assert!(!is_decimal_char('²') && is_digit_char('²') && is_numeric_char('²'));
        assert!(!is_decimal_char('¼') && !is_digit_char('¼') && is_numeric_char('¼'));
        assert!(is_numeric_char('一') && !is_digit_char('一'));
        assert!(!is_numeric_char('a'));
        // Garay digits, new in UCD 16.0.0.
        assert!(is_decimal_char('\u{10D40}'));
    }
}
