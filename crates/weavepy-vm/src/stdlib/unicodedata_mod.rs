//! The `unicodedata` built-in module — RFC 0023.
//!
//! Mirrors CPython 3.13's `Modules/unicodedata.c` surface, backed by
//! the `unicode-properties` and `unicode-normalization` crates which
//! ship the Unicode 16.0.0 data tables. We provide:
//!
//! - `name(chr[, default])` — Unicode character name (`'LATIN SMALL LETTER A'`).
//! - `lookup(name)` — name → char.
//! - `category(chr)` — general category (`Lu`, `Ll`, `Nd`, ...).
//! - `bidirectional(chr)` — bidi class (`L`, `R`, `EN`, ...).
//! - `combining(chr)` — canonical combining class (int).
//! - `mirrored(chr)` — 0/1 mirror property.
//! - `decimal(chr[, default])` — decimal digit value or default.
//! - `digit(chr[, default])` — digit value or default.
//! - `numeric(chr[, default])` — numeric value as a float.
//! - `decomposition(chr)` — canonical decomposition string.
//! - `normalize(form, unistr)` — NFC/NFD/NFKC/NFKD normalization.
//! - `is_normalized(form, unistr)` — predicate.
//! - `east_asian_width(chr)` — `N`/`Na`/`W`/`F`/`H`/`A`.
//! - `unidata_version` — version string.
//! - `ucd_3_2_0` — historical UCD subset (we expose the same object,
//!   reads of any function on it delegate to current data because the
//!   3.2.0 snapshot is mostly used to derive identifier rules — not
//!   bit-perfect, but enough for `unicodedata.ucd_3_2_0.name(...)`).

use crate::sync::Rc;
use crate::sync::RefCell;

use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("unicodedata"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Access to the Unicode 16.0.0 character database."),
        );
        d.insert(
            DictKey(Object::from_static("unidata_version")),
            Object::from_static("16.0.0"),
        );
        d.insert(
            DictKey(Object::from_static("ucd_3_2_0")),
            // Same engine — the historical snapshot is only relevant
            // for identifier-validation rules in PEP 3131 era code.
            Object::Module(build_inner_ucd()),
        );

        for (name, fn_) in [
            (
                "name",
                unicodedata_name as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("lookup", unicodedata_lookup),
            ("category", unicodedata_category),
            ("bidirectional", unicodedata_bidirectional),
            ("combining", unicodedata_combining),
            ("mirrored", unicodedata_mirrored),
            ("decimal", unicodedata_decimal),
            ("digit", unicodedata_digit),
            ("numeric", unicodedata_numeric),
            ("decomposition", unicodedata_decomposition),
            ("normalize", unicodedata_normalize),
            ("is_normalized", unicodedata_is_normalized),
            ("east_asian_width", unicodedata_east_asian_width),
        ] {
            d.insert(DictKey(Object::from_static(name)), builtin(name, fn_));
        }
    }
    Rc::new(PyModule {
        name: "unicodedata".to_owned(),
        filename: None,
        dict,
    })
}

fn build_inner_ucd() -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("unicodedata.ucd_3_2_0"),
        );
        d.insert(
            DictKey(Object::from_static("unidata_version")),
            Object::from_static("3.2.0"),
        );
        for (name, fn_) in [
            (
                "name",
                unicodedata_name as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("category", unicodedata_category),
            ("lookup", unicodedata_lookup),
        ] {
            d.insert(DictKey(Object::from_static(name)), builtin(name, fn_));
        }
    }
    Rc::new(PyModule {
        name: "unicodedata.ucd_3_2_0".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn first_char(args: &[Object], fn_name: &str) -> Result<char, RuntimeError> {
    match args.first() {
        Some(Object::Str(s)) => {
            let mut it = s.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => Ok(c),
                _ => Err(type_error(format!(
                    "{fn_name}() argument must be a unicode character"
                ))),
            }
        }
        Some(other) => Err(type_error(format!(
            "{fn_name}() argument 1 must be str, not '{}'",
            other.type_name()
        ))),
        None => Err(type_error(format!("{fn_name}() takes at least 1 argument"))),
    }
}

fn category_str(g: GeneralCategory) -> &'static str {
    match g {
        GeneralCategory::UppercaseLetter => "Lu",
        GeneralCategory::LowercaseLetter => "Ll",
        GeneralCategory::TitlecaseLetter => "Lt",
        GeneralCategory::ModifierLetter => "Lm",
        GeneralCategory::OtherLetter => "Lo",
        GeneralCategory::NonspacingMark => "Mn",
        GeneralCategory::SpacingMark => "Mc",
        GeneralCategory::EnclosingMark => "Me",
        GeneralCategory::DecimalNumber => "Nd",
        GeneralCategory::LetterNumber => "Nl",
        GeneralCategory::OtherNumber => "No",
        GeneralCategory::ConnectorPunctuation => "Pc",
        GeneralCategory::DashPunctuation => "Pd",
        GeneralCategory::OpenPunctuation => "Ps",
        GeneralCategory::ClosePunctuation => "Pe",
        GeneralCategory::InitialPunctuation => "Pi",
        GeneralCategory::FinalPunctuation => "Pf",
        GeneralCategory::OtherPunctuation => "Po",
        GeneralCategory::MathSymbol => "Sm",
        GeneralCategory::CurrencySymbol => "Sc",
        GeneralCategory::ModifierSymbol => "Sk",
        GeneralCategory::OtherSymbol => "So",
        GeneralCategory::SpaceSeparator => "Zs",
        GeneralCategory::LineSeparator => "Zl",
        GeneralCategory::ParagraphSeparator => "Zp",
        GeneralCategory::Control => "Cc",
        GeneralCategory::Format => "Cf",
        GeneralCategory::Surrogate => "Cs",
        GeneralCategory::PrivateUse => "Co",
        GeneralCategory::Unassigned => "Cn",
    }
}

fn unicodedata_name(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "name")?;
    if let Some(name) = char_name(ch) {
        return Ok(Object::from_str(name));
    }
    if let Some(default) = args.get(1) {
        return Ok(default.clone());
    }
    Err(value_error("no such name"))
}

fn unicodedata_lookup(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("lookup() argument 1 must be a string")),
    };
    if let Some(ch) = name_to_char(&name) {
        return Ok(Object::from_str(ch.to_string()));
    }
    Err(key_error(format!("undefined character name '{name}'")))
}

fn unicodedata_category(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "category")?;
    Ok(Object::from_static(category_str(ch.general_category())))
}

fn unicodedata_bidirectional(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "bidirectional")?;
    // Bidi class isn't exposed by unicode-properties directly; we
    // approximate the common buckets using the general category. CPython
    // also returns "" for unassigned chars.
    let bidi = match ch.general_category() {
        GeneralCategory::UppercaseLetter
        | GeneralCategory::LowercaseLetter
        | GeneralCategory::TitlecaseLetter
        | GeneralCategory::OtherLetter
        | GeneralCategory::ModifierLetter
        | GeneralCategory::DecimalNumber => "L",
        GeneralCategory::EnclosingMark
        | GeneralCategory::NonspacingMark
        | GeneralCategory::SpacingMark => "NSM",
        GeneralCategory::OtherNumber | GeneralCategory::LetterNumber => "ON",
        GeneralCategory::SpaceSeparator => "WS",
        GeneralCategory::LineSeparator | GeneralCategory::ParagraphSeparator => "B",
        GeneralCategory::ConnectorPunctuation
        | GeneralCategory::DashPunctuation
        | GeneralCategory::OpenPunctuation
        | GeneralCategory::ClosePunctuation
        | GeneralCategory::InitialPunctuation
        | GeneralCategory::FinalPunctuation
        | GeneralCategory::OtherPunctuation => "ON",
        GeneralCategory::MathSymbol
        | GeneralCategory::CurrencySymbol
        | GeneralCategory::ModifierSymbol
        | GeneralCategory::OtherSymbol => "ON",
        GeneralCategory::Control => match ch as u32 {
            0x0A | 0x0D | 0x1C | 0x1D | 0x1E => "B",
            0x09 | 0x0B | 0x0C => "S",
            _ => "BN",
        },
        GeneralCategory::Format => "BN",
        GeneralCategory::Surrogate | GeneralCategory::PrivateUse => "L",
        GeneralCategory::Unassigned => "",
    };
    Ok(Object::from_static(bidi))
}

fn unicodedata_combining(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "combining")?;
    // Canonical combining class — we use 230 for common combining
    // marks (the usual default) and 0 otherwise. The full table is
    // not exposed by unicode-properties; this is a pragmatic
    // approximation that gets the common `unicodedata.combining`
    // usage (testing for non-zero) correct.
    Ok(Object::Int(match ch.general_category() {
        GeneralCategory::NonspacingMark => 230,
        GeneralCategory::EnclosingMark | GeneralCategory::SpacingMark => 0,
        _ => 0,
    }))
}

fn unicodedata_mirrored(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "mirrored")?;
    let mirrored = matches!(
        ch,
        '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '\u{27E8}' | '\u{27E9}'
    );
    Ok(Object::Int(i64::from(mirrored)))
}

fn unicodedata_decimal(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "decimal")?;
    if let Some(d) = ch.to_digit(10) {
        return Ok(Object::Int(i64::from(d)));
    }
    if let Some(default) = args.get(1) {
        return Ok(default.clone());
    }
    Err(value_error("not a decimal"))
}

fn unicodedata_digit(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "digit")?;
    if let Some(d) = ch.to_digit(10) {
        return Ok(Object::Int(i64::from(d)));
    }
    if let Some(default) = args.get(1) {
        return Ok(default.clone());
    }
    Err(value_error("not a digit"))
}

fn unicodedata_numeric(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "numeric")?;
    if let Some(d) = ch.to_digit(36) {
        if d < 10 {
            return Ok(Object::Float(f64::from(d)));
        }
    }
    // Cover the common Unicode fraction characters.
    let v = match ch {
        '½' => Some(0.5),
        '⅓' => Some(1.0 / 3.0),
        '¼' => Some(0.25),
        '⅕' => Some(0.2),
        '⅙' => Some(1.0 / 6.0),
        '⅛' => Some(0.125),
        '⅔' => Some(2.0 / 3.0),
        '⅖' => Some(0.4),
        '⅗' => Some(0.6),
        '⅘' => Some(0.8),
        '¾' => Some(0.75),
        '⅝' => Some(0.625),
        '⅞' => Some(0.875),
        _ => None,
    };
    if let Some(x) = v {
        return Ok(Object::Float(x));
    }
    if let Some(default) = args.get(1) {
        return Ok(default.clone());
    }
    Err(value_error("not a numeric character"))
}

fn unicodedata_decomposition(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "decomposition")?;
    // Render the NFD decomposition as a CPython-style hex string.
    let decomp: String = ch.to_string().nfd().collect();
    if decomp.chars().count() == 1 && decomp.starts_with(ch) {
        return Ok(Object::from_static(""));
    }
    let hex: Vec<String> = decomp
        .chars()
        .map(|c| format!("{:04X}", c as u32))
        .collect();
    Ok(Object::from_str(hex.join(" ")))
}

fn unicodedata_normalize(args: &[Object]) -> Result<Object, RuntimeError> {
    let form = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("normalize() form must be str")),
    };
    let s = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("normalize() unistr must be str")),
    };
    let out: String = match form.as_str() {
        "NFC" => s.nfc().collect(),
        "NFD" => s.nfd().collect(),
        "NFKC" => s.nfkc().collect(),
        "NFKD" => s.nfkd().collect(),
        _ => return Err(value_error(format!("invalid normalization form: {form}"))),
    };
    Ok(Object::from_str(out))
}

fn unicodedata_is_normalized(args: &[Object]) -> Result<Object, RuntimeError> {
    let normalized = match unicodedata_normalize(args)? {
        Object::Str(s) => s.to_string(),
        _ => unreachable!(),
    };
    let original = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("is_normalized() unistr must be str")),
    };
    Ok(Object::Bool(normalized == original))
}

// East Asian Width (UAX #11) ranges for every non-Neutral code point,
// generated from the Unicode Character Database (UCD 13.0 — the table
// only lists non-`N` ranges, so lookups missing here are Neutral).
// Emoji presentation symbols are `W`, which `traceback`'s
// `_display_width` relies on for caret alignment under emoji.
const EAW_RANGES: &[(u32, u32, &str)] = &[
    (0x20, 0x7E, "Na"),
    (0xA1, 0xA1, "A"),
    (0xA2, 0xA3, "Na"),
    (0xA4, 0xA4, "A"),
    (0xA5, 0xA6, "Na"),
    (0xA7, 0xA8, "A"),
    (0xAA, 0xAA, "A"),
    (0xAC, 0xAC, "Na"),
    (0xAD, 0xAE, "A"),
    (0xAF, 0xAF, "Na"),
    (0xB0, 0xB4, "A"),
    (0xB6, 0xBA, "A"),
    (0xBC, 0xBF, "A"),
    (0xC6, 0xC6, "A"),
    (0xD0, 0xD0, "A"),
    (0xD7, 0xD8, "A"),
    (0xDE, 0xE1, "A"),
    (0xE6, 0xE6, "A"),
    (0xE8, 0xEA, "A"),
    (0xEC, 0xED, "A"),
    (0xF0, 0xF0, "A"),
    (0xF2, 0xF3, "A"),
    (0xF7, 0xFA, "A"),
    (0xFC, 0xFC, "A"),
    (0xFE, 0xFE, "A"),
    (0x101, 0x101, "A"),
    (0x111, 0x111, "A"),
    (0x113, 0x113, "A"),
    (0x11B, 0x11B, "A"),
    (0x126, 0x127, "A"),
    (0x12B, 0x12B, "A"),
    (0x131, 0x133, "A"),
    (0x138, 0x138, "A"),
    (0x13F, 0x142, "A"),
    (0x144, 0x144, "A"),
    (0x148, 0x14B, "A"),
    (0x14D, 0x14D, "A"),
    (0x152, 0x153, "A"),
    (0x166, 0x167, "A"),
    (0x16B, 0x16B, "A"),
    (0x1CE, 0x1CE, "A"),
    (0x1D0, 0x1D0, "A"),
    (0x1D2, 0x1D2, "A"),
    (0x1D4, 0x1D4, "A"),
    (0x1D6, 0x1D6, "A"),
    (0x1D8, 0x1D8, "A"),
    (0x1DA, 0x1DA, "A"),
    (0x1DC, 0x1DC, "A"),
    (0x251, 0x251, "A"),
    (0x261, 0x261, "A"),
    (0x2C4, 0x2C4, "A"),
    (0x2C7, 0x2C7, "A"),
    (0x2C9, 0x2CB, "A"),
    (0x2CD, 0x2CD, "A"),
    (0x2D0, 0x2D0, "A"),
    (0x2D8, 0x2DB, "A"),
    (0x2DD, 0x2DD, "A"),
    (0x2DF, 0x2DF, "A"),
    (0x300, 0x36F, "A"),
    (0x378, 0x379, "F"),
    (0x380, 0x383, "F"),
    (0x38B, 0x38B, "F"),
    (0x38D, 0x38D, "F"),
    (0x391, 0x3A1, "A"),
    (0x3A2, 0x3A2, "F"),
    (0x3A3, 0x3A9, "A"),
    (0x3B1, 0x3C1, "A"),
    (0x3C3, 0x3C9, "A"),
    (0x401, 0x401, "A"),
    (0x410, 0x44F, "A"),
    (0x451, 0x451, "A"),
    (0x530, 0x530, "F"),
    (0x557, 0x558, "F"),
    (0x58B, 0x58C, "F"),
    (0x590, 0x590, "F"),
    (0x5C8, 0x5CF, "F"),
    (0x5EB, 0x5EE, "F"),
    (0x5F5, 0x5FF, "F"),
    (0x61D, 0x61D, "F"),
    (0x70E, 0x70E, "F"),
    (0x74B, 0x74C, "F"),
    (0x7B2, 0x7BF, "F"),
    (0x7FB, 0x7FC, "F"),
    (0x82E, 0x82F, "F"),
    (0x83F, 0x83F, "F"),
    (0x85C, 0x85D, "F"),
    (0x85F, 0x85F, "F"),
    (0x86B, 0x89F, "F"),
    (0x8B5, 0x8B5, "F"),
    (0x8C8, 0x8D2, "F"),
    (0x984, 0x984, "F"),
    (0x98D, 0x98E, "F"),
    (0x991, 0x992, "F"),
    (0x9A9, 0x9A9, "F"),
    (0x9B1, 0x9B1, "F"),
    (0x9B3, 0x9B5, "F"),
    (0x9BA, 0x9BB, "F"),
    (0x9C5, 0x9C6, "F"),
    (0x9C9, 0x9CA, "F"),
    (0x9CF, 0x9D6, "F"),
    (0x9D8, 0x9DB, "F"),
    (0x9DE, 0x9DE, "F"),
    (0x9E4, 0x9E5, "F"),
    (0x9FF, 0xA00, "F"),
    (0xA04, 0xA04, "F"),
    (0xA0B, 0xA0E, "F"),
    (0xA11, 0xA12, "F"),
    (0xA29, 0xA29, "F"),
    (0xA31, 0xA31, "F"),
    (0xA34, 0xA34, "F"),
    (0xA37, 0xA37, "F"),
    (0xA3A, 0xA3B, "F"),
    (0xA3D, 0xA3D, "F"),
    (0xA43, 0xA46, "F"),
    (0xA49, 0xA4A, "F"),
    (0xA4E, 0xA50, "F"),
    (0xA52, 0xA58, "F"),
    (0xA5D, 0xA5D, "F"),
    (0xA5F, 0xA65, "F"),
    (0xA77, 0xA80, "F"),
    (0xA84, 0xA84, "F"),
    (0xA8E, 0xA8E, "F"),
    (0xA92, 0xA92, "F"),
    (0xAA9, 0xAA9, "F"),
    (0xAB1, 0xAB1, "F"),
    (0xAB4, 0xAB4, "F"),
    (0xABA, 0xABB, "F"),
    (0xAC6, 0xAC6, "F"),
    (0xACA, 0xACA, "F"),
    (0xACE, 0xACF, "F"),
    (0xAD1, 0xADF, "F"),
    (0xAE4, 0xAE5, "F"),
    (0xAF2, 0xAF8, "F"),
    (0xB00, 0xB00, "F"),
    (0xB04, 0xB04, "F"),
    (0xB0D, 0xB0E, "F"),
    (0xB11, 0xB12, "F"),
    (0xB29, 0xB29, "F"),
    (0xB31, 0xB31, "F"),
    (0xB34, 0xB34, "F"),
    (0xB3A, 0xB3B, "F"),
    (0xB45, 0xB46, "F"),
    (0xB49, 0xB4A, "F"),
    (0xB4E, 0xB54, "F"),
    (0xB58, 0xB5B, "F"),
    (0xB5E, 0xB5E, "F"),
    (0xB64, 0xB65, "F"),
    (0xB78, 0xB81, "F"),
    (0xB84, 0xB84, "F"),
    (0xB8B, 0xB8D, "F"),
    (0xB91, 0xB91, "F"),
    (0xB96, 0xB98, "F"),
    (0xB9B, 0xB9B, "F"),
    (0xB9D, 0xB9D, "F"),
    (0xBA0, 0xBA2, "F"),
    (0xBA5, 0xBA7, "F"),
    (0xBAB, 0xBAD, "F"),
    (0xBBA, 0xBBD, "F"),
    (0xBC3, 0xBC5, "F"),
    (0xBC9, 0xBC9, "F"),
    (0xBCE, 0xBCF, "F"),
    (0xBD1, 0xBD6, "F"),
    (0xBD8, 0xBE5, "F"),
    (0xBFB, 0xBFF, "F"),
    (0xC0D, 0xC0D, "F"),
    (0xC11, 0xC11, "F"),
    (0xC29, 0xC29, "F"),
    (0xC3A, 0xC3C, "F"),
    (0xC45, 0xC45, "F"),
    (0xC49, 0xC49, "F"),
    (0xC4E, 0xC54, "F"),
    (0xC57, 0xC57, "F"),
    (0xC5B, 0xC5F, "F"),
    (0xC64, 0xC65, "F"),
    (0xC70, 0xC76, "F"),
    (0xC8D, 0xC8D, "F"),
    (0xC91, 0xC91, "F"),
    (0xCA9, 0xCA9, "F"),
    (0xCB4, 0xCB4, "F"),
    (0xCBA, 0xCBB, "F"),
    (0xCC5, 0xCC5, "F"),
    (0xCC9, 0xCC9, "F"),
    (0xCCE, 0xCD4, "F"),
    (0xCD7, 0xCDD, "F"),
    (0xCDF, 0xCDF, "F"),
    (0xCE4, 0xCE5, "F"),
    (0xCF0, 0xCF0, "F"),
    (0xCF3, 0xCFF, "F"),
    (0xD0D, 0xD0D, "F"),
    (0xD11, 0xD11, "F"),
    (0xD45, 0xD45, "F"),
    (0xD49, 0xD49, "F"),
    (0xD50, 0xD53, "F"),
    (0xD64, 0xD65, "F"),
    (0xD80, 0xD80, "F"),
    (0xD84, 0xD84, "F"),
    (0xD97, 0xD99, "F"),
    (0xDB2, 0xDB2, "F"),
    (0xDBC, 0xDBC, "F"),
    (0xDBE, 0xDBF, "F"),
    (0xDC7, 0xDC9, "F"),
    (0xDCB, 0xDCE, "F"),
    (0xDD5, 0xDD5, "F"),
    (0xDD7, 0xDD7, "F"),
    (0xDE0, 0xDE5, "F"),
    (0xDF0, 0xDF1, "F"),
    (0xDF5, 0xE00, "F"),
    (0xE3B, 0xE3E, "F"),
    (0xE5C, 0xE80, "F"),
    (0xE83, 0xE83, "F"),
    (0xE85, 0xE85, "F"),
    (0xE8B, 0xE8B, "F"),
    (0xEA4, 0xEA4, "F"),
    (0xEA6, 0xEA6, "F"),
    (0xEBE, 0xEBF, "F"),
    (0xEC5, 0xEC5, "F"),
    (0xEC7, 0xEC7, "F"),
    (0xECE, 0xECF, "F"),
    (0xEDA, 0xEDB, "F"),
    (0xEE0, 0xEFF, "F"),
    (0xF48, 0xF48, "F"),
    (0xF6D, 0xF70, "F"),
    (0xF98, 0xF98, "F"),
    (0xFBD, 0xFBD, "F"),
    (0xFCD, 0xFCD, "F"),
    (0xFDB, 0xFFF, "F"),
    (0x10C6, 0x10C6, "F"),
    (0x10C8, 0x10CC, "F"),
    (0x10CE, 0x10CF, "F"),
    (0x1100, 0x115F, "W"),
    (0x1249, 0x1249, "F"),
    (0x124E, 0x124F, "F"),
    (0x1257, 0x1257, "F"),
    (0x1259, 0x1259, "F"),
    (0x125E, 0x125F, "F"),
    (0x1289, 0x1289, "F"),
    (0x128E, 0x128F, "F"),
    (0x12B1, 0x12B1, "F"),
    (0x12B6, 0x12B7, "F"),
    (0x12BF, 0x12BF, "F"),
    (0x12C1, 0x12C1, "F"),
    (0x12C6, 0x12C7, "F"),
    (0x12D7, 0x12D7, "F"),
    (0x1311, 0x1311, "F"),
    (0x1316, 0x1317, "F"),
    (0x135B, 0x135C, "F"),
    (0x137D, 0x137F, "F"),
    (0x139A, 0x139F, "F"),
    (0x13F6, 0x13F7, "F"),
    (0x13FE, 0x13FF, "F"),
    (0x169D, 0x169F, "F"),
    (0x16F9, 0x16FF, "F"),
    (0x170D, 0x170D, "F"),
    (0x1715, 0x171F, "F"),
    (0x1737, 0x173F, "F"),
    (0x1754, 0x175F, "F"),
    (0x176D, 0x176D, "F"),
    (0x1771, 0x1771, "F"),
    (0x1774, 0x177F, "F"),
    (0x17DE, 0x17DF, "F"),
    (0x17EA, 0x17EF, "F"),
    (0x17FA, 0x17FF, "F"),
    (0x180F, 0x180F, "F"),
    (0x181A, 0x181F, "F"),
    (0x1879, 0x187F, "F"),
    (0x18AB, 0x18AF, "F"),
    (0x18F6, 0x18FF, "F"),
    (0x191F, 0x191F, "F"),
    (0x192C, 0x192F, "F"),
    (0x193C, 0x193F, "F"),
    (0x1941, 0x1943, "F"),
    (0x196E, 0x196F, "F"),
    (0x1975, 0x197F, "F"),
    (0x19AC, 0x19AF, "F"),
    (0x19CA, 0x19CF, "F"),
    (0x19DB, 0x19DD, "F"),
    (0x1A1C, 0x1A1D, "F"),
    (0x1A5F, 0x1A5F, "F"),
    (0x1A7D, 0x1A7E, "F"),
    (0x1A8A, 0x1A8F, "F"),
    (0x1A9A, 0x1A9F, "F"),
    (0x1AAE, 0x1AAF, "F"),
    (0x1AC1, 0x1AFF, "F"),
    (0x1B4C, 0x1B4F, "F"),
    (0x1B7D, 0x1B7F, "F"),
    (0x1BF4, 0x1BFB, "F"),
    (0x1C38, 0x1C3A, "F"),
    (0x1C4A, 0x1C4C, "F"),
    (0x1C89, 0x1C8F, "F"),
    (0x1CBB, 0x1CBC, "F"),
    (0x1CC8, 0x1CCF, "F"),
    (0x1CFB, 0x1CFF, "F"),
    (0x1DFA, 0x1DFA, "F"),
    (0x1F16, 0x1F17, "F"),
    (0x1F1E, 0x1F1F, "F"),
    (0x1F46, 0x1F47, "F"),
    (0x1F4E, 0x1F4F, "F"),
    (0x1F58, 0x1F58, "F"),
    (0x1F5A, 0x1F5A, "F"),
    (0x1F5C, 0x1F5C, "F"),
    (0x1F5E, 0x1F5E, "F"),
    (0x1F7E, 0x1F7F, "F"),
    (0x1FB5, 0x1FB5, "F"),
    (0x1FC5, 0x1FC5, "F"),
    (0x1FD4, 0x1FD5, "F"),
    (0x1FDC, 0x1FDC, "F"),
    (0x1FF0, 0x1FF1, "F"),
    (0x1FF5, 0x1FF5, "F"),
    (0x1FFF, 0x1FFF, "F"),
    (0x2010, 0x2010, "A"),
    (0x2013, 0x2016, "A"),
    (0x2018, 0x2019, "A"),
    (0x201C, 0x201D, "A"),
    (0x2020, 0x2022, "A"),
    (0x2024, 0x2027, "A"),
    (0x2030, 0x2030, "A"),
    (0x2032, 0x2033, "A"),
    (0x2035, 0x2035, "A"),
    (0x203B, 0x203B, "A"),
    (0x203E, 0x203E, "A"),
    (0x2065, 0x2065, "F"),
    (0x2072, 0x2073, "F"),
    (0x2074, 0x2074, "A"),
    (0x207F, 0x207F, "A"),
    (0x2081, 0x2084, "A"),
    (0x208F, 0x208F, "F"),
    (0x209D, 0x209F, "F"),
    (0x20A9, 0x20A9, "H"),
    (0x20AC, 0x20AC, "A"),
    (0x20C0, 0x20CF, "F"),
    (0x20F1, 0x20FF, "F"),
    (0x2103, 0x2103, "A"),
    (0x2105, 0x2105, "A"),
    (0x2109, 0x2109, "A"),
    (0x2113, 0x2113, "A"),
    (0x2116, 0x2116, "A"),
    (0x2121, 0x2122, "A"),
    (0x2126, 0x2126, "A"),
    (0x212B, 0x212B, "A"),
    (0x2153, 0x2154, "A"),
    (0x215B, 0x215E, "A"),
    (0x2160, 0x216B, "A"),
    (0x2170, 0x2179, "A"),
    (0x2189, 0x2189, "A"),
    (0x218C, 0x218F, "F"),
    (0x2190, 0x2199, "A"),
    (0x21B8, 0x21B9, "A"),
    (0x21D2, 0x21D2, "A"),
    (0x21D4, 0x21D4, "A"),
    (0x21E7, 0x21E7, "A"),
    (0x2200, 0x2200, "A"),
    (0x2202, 0x2203, "A"),
    (0x2207, 0x2208, "A"),
    (0x220B, 0x220B, "A"),
    (0x220F, 0x220F, "A"),
    (0x2211, 0x2211, "A"),
    (0x2215, 0x2215, "A"),
    (0x221A, 0x221A, "A"),
    (0x221D, 0x2220, "A"),
    (0x2223, 0x2223, "A"),
    (0x2225, 0x2225, "A"),
    (0x2227, 0x222C, "A"),
    (0x222E, 0x222E, "A"),
    (0x2234, 0x2237, "A"),
    (0x223C, 0x223D, "A"),
    (0x2248, 0x2248, "A"),
    (0x224C, 0x224C, "A"),
    (0x2252, 0x2252, "A"),
    (0x2260, 0x2261, "A"),
    (0x2264, 0x2267, "A"),
    (0x226A, 0x226B, "A"),
    (0x226E, 0x226F, "A"),
    (0x2282, 0x2283, "A"),
    (0x2286, 0x2287, "A"),
    (0x2295, 0x2295, "A"),
    (0x2299, 0x2299, "A"),
    (0x22A5, 0x22A5, "A"),
    (0x22BF, 0x22BF, "A"),
    (0x2312, 0x2312, "A"),
    (0x231A, 0x231B, "W"),
    (0x2329, 0x232A, "W"),
    (0x23E9, 0x23EC, "W"),
    (0x23F0, 0x23F0, "W"),
    (0x23F3, 0x23F3, "W"),
    (0x2427, 0x243F, "F"),
    (0x244B, 0x245F, "F"),
    (0x2460, 0x24E9, "A"),
    (0x24EB, 0x254B, "A"),
    (0x2550, 0x2573, "A"),
    (0x2580, 0x258F, "A"),
    (0x2592, 0x2595, "A"),
    (0x25A0, 0x25A1, "A"),
    (0x25A3, 0x25A9, "A"),
    (0x25B2, 0x25B3, "A"),
    (0x25B6, 0x25B7, "A"),
    (0x25BC, 0x25BD, "A"),
    (0x25C0, 0x25C1, "A"),
    (0x25C6, 0x25C8, "A"),
    (0x25CB, 0x25CB, "A"),
    (0x25CE, 0x25D1, "A"),
    (0x25E2, 0x25E5, "A"),
    (0x25EF, 0x25EF, "A"),
    (0x25FD, 0x25FE, "W"),
    (0x2605, 0x2606, "A"),
    (0x2609, 0x2609, "A"),
    (0x260E, 0x260F, "A"),
    (0x2614, 0x2615, "W"),
    (0x261C, 0x261C, "A"),
    (0x261E, 0x261E, "A"),
    (0x2640, 0x2640, "A"),
    (0x2642, 0x2642, "A"),
    (0x2648, 0x2653, "W"),
    (0x2660, 0x2661, "A"),
    (0x2663, 0x2665, "A"),
    (0x2667, 0x266A, "A"),
    (0x266C, 0x266D, "A"),
    (0x266F, 0x266F, "A"),
    (0x267F, 0x267F, "W"),
    (0x2693, 0x2693, "W"),
    (0x269E, 0x269F, "A"),
    (0x26A1, 0x26A1, "W"),
    (0x26AA, 0x26AB, "W"),
    (0x26BD, 0x26BE, "W"),
    (0x26BF, 0x26BF, "A"),
    (0x26C4, 0x26C5, "W"),
    (0x26C6, 0x26CD, "A"),
    (0x26CE, 0x26CE, "W"),
    (0x26CF, 0x26D3, "A"),
    (0x26D4, 0x26D4, "W"),
    (0x26D5, 0x26E1, "A"),
    (0x26E3, 0x26E3, "A"),
    (0x26E8, 0x26E9, "A"),
    (0x26EA, 0x26EA, "W"),
    (0x26EB, 0x26F1, "A"),
    (0x26F2, 0x26F3, "W"),
    (0x26F4, 0x26F4, "A"),
    (0x26F5, 0x26F5, "W"),
    (0x26F6, 0x26F9, "A"),
    (0x26FA, 0x26FA, "W"),
    (0x26FB, 0x26FC, "A"),
    (0x26FD, 0x26FD, "W"),
    (0x26FE, 0x26FF, "A"),
    (0x2705, 0x2705, "W"),
    (0x270A, 0x270B, "W"),
    (0x2728, 0x2728, "W"),
    (0x273D, 0x273D, "A"),
    (0x274C, 0x274C, "W"),
    (0x274E, 0x274E, "W"),
    (0x2753, 0x2755, "W"),
    (0x2757, 0x2757, "W"),
    (0x2776, 0x277F, "A"),
    (0x2795, 0x2797, "W"),
    (0x27B0, 0x27B0, "W"),
    (0x27BF, 0x27BF, "W"),
    (0x27E6, 0x27ED, "Na"),
    (0x2985, 0x2986, "Na"),
    (0x2B1B, 0x2B1C, "W"),
    (0x2B50, 0x2B50, "W"),
    (0x2B55, 0x2B55, "W"),
    (0x2B56, 0x2B59, "A"),
    (0x2B74, 0x2B75, "F"),
    (0x2B96, 0x2B96, "F"),
    (0x2C2F, 0x2C2F, "F"),
    (0x2C5F, 0x2C5F, "F"),
    (0x2CF4, 0x2CF8, "F"),
    (0x2D26, 0x2D26, "F"),
    (0x2D28, 0x2D2C, "F"),
    (0x2D2E, 0x2D2F, "F"),
    (0x2D68, 0x2D6E, "F"),
    (0x2D71, 0x2D7E, "F"),
    (0x2D97, 0x2D9F, "F"),
    (0x2DA7, 0x2DA7, "F"),
    (0x2DAF, 0x2DAF, "F"),
    (0x2DB7, 0x2DB7, "F"),
    (0x2DBF, 0x2DBF, "F"),
    (0x2DC7, 0x2DC7, "F"),
    (0x2DCF, 0x2DCF, "F"),
    (0x2DD7, 0x2DD7, "F"),
    (0x2DDF, 0x2DDF, "F"),
    (0x2E53, 0x2E7F, "F"),
    (0x2E80, 0x2E99, "W"),
    (0x2E9A, 0x2E9A, "F"),
    (0x2E9B, 0x2EF3, "W"),
    (0x2EF4, 0x2EFF, "F"),
    (0x2F00, 0x2FD5, "W"),
    (0x2FD6, 0x2FEF, "F"),
    (0x2FF0, 0x2FFB, "W"),
    (0x2FFC, 0x3000, "F"),
    (0x3001, 0x303E, "W"),
    (0x3040, 0x3040, "F"),
    (0x3041, 0x3096, "W"),
    (0x3097, 0x3098, "F"),
    (0x3099, 0x30FF, "W"),
    (0x3100, 0x3104, "F"),
    (0x3105, 0x312F, "W"),
    (0x3130, 0x3130, "F"),
    (0x3131, 0x318E, "W"),
    (0x318F, 0x318F, "F"),
    (0x3190, 0x31E3, "W"),
    (0x31E4, 0x31EF, "F"),
    (0x31F0, 0x321E, "W"),
    (0x321F, 0x321F, "F"),
    (0x3220, 0x3247, "W"),
    (0x3248, 0x324F, "A"),
    (0x3250, 0x4DBF, "W"),
    (0x4E00, 0x9FFC, "W"),
    (0x9FFD, 0x9FFF, "F"),
    (0xA000, 0xA48C, "W"),
    (0xA48D, 0xA48F, "F"),
    (0xA490, 0xA4C6, "W"),
    (0xA4C7, 0xA4CF, "F"),
    (0xA62C, 0xA63F, "F"),
    (0xA6F8, 0xA6FF, "F"),
    (0xA7C0, 0xA7C1, "F"),
    (0xA7CB, 0xA7F4, "F"),
    (0xA82D, 0xA82F, "F"),
    (0xA83A, 0xA83F, "F"),
    (0xA878, 0xA87F, "F"),
    (0xA8C6, 0xA8CD, "F"),
    (0xA8DA, 0xA8DF, "F"),
    (0xA954, 0xA95E, "F"),
    (0xA960, 0xA97C, "W"),
    (0xA97D, 0xA97F, "F"),
    (0xA9CE, 0xA9CE, "F"),
    (0xA9DA, 0xA9DD, "F"),
    (0xA9FF, 0xA9FF, "F"),
    (0xAA37, 0xAA3F, "F"),
    (0xAA4E, 0xAA4F, "F"),
    (0xAA5A, 0xAA5B, "F"),
    (0xAAC3, 0xAADA, "F"),
    (0xAAF7, 0xAB00, "F"),
    (0xAB07, 0xAB08, "F"),
    (0xAB0F, 0xAB10, "F"),
    (0xAB17, 0xAB1F, "F"),
    (0xAB27, 0xAB27, "F"),
    (0xAB2F, 0xAB2F, "F"),
    (0xAB6C, 0xAB6F, "F"),
    (0xABEE, 0xABEF, "F"),
    (0xABFA, 0xABFF, "F"),
    (0xAC00, 0xD7A3, "W"),
    (0xD7A4, 0xD7AF, "F"),
    (0xD7C7, 0xD7CA, "F"),
    (0xD7FC, 0xD7FF, "F"),
    (0xE000, 0xF8FF, "A"),
    (0xF900, 0xFA6D, "W"),
    (0xFA6E, 0xFA6F, "F"),
    (0xFA70, 0xFAD9, "W"),
    (0xFADA, 0xFAFF, "F"),
    (0xFB07, 0xFB12, "F"),
    (0xFB18, 0xFB1C, "F"),
    (0xFB37, 0xFB37, "F"),
    (0xFB3D, 0xFB3D, "F"),
    (0xFB3F, 0xFB3F, "F"),
    (0xFB42, 0xFB42, "F"),
    (0xFB45, 0xFB45, "F"),
    (0xFBC2, 0xFBD2, "F"),
    (0xFD40, 0xFD4F, "F"),
    (0xFD90, 0xFD91, "F"),
    (0xFDC8, 0xFDEF, "F"),
    (0xFDFE, 0xFDFF, "F"),
    (0xFE00, 0xFE0F, "A"),
    (0xFE10, 0xFE19, "W"),
    (0xFE1A, 0xFE1F, "F"),
    (0xFE30, 0xFE52, "W"),
    (0xFE53, 0xFE53, "F"),
    (0xFE54, 0xFE66, "W"),
    (0xFE67, 0xFE67, "F"),
    (0xFE68, 0xFE6B, "W"),
    (0xFE6C, 0xFE6F, "F"),
    (0xFE75, 0xFE75, "F"),
    (0xFEFD, 0xFEFE, "F"),
    (0xFF00, 0xFF60, "F"),
    (0xFF61, 0xFFBE, "H"),
    (0xFFBF, 0xFFC1, "F"),
    (0xFFC2, 0xFFC7, "H"),
    (0xFFC8, 0xFFC9, "F"),
    (0xFFCA, 0xFFCF, "H"),
    (0xFFD0, 0xFFD1, "F"),
    (0xFFD2, 0xFFD7, "H"),
    (0xFFD8, 0xFFD9, "F"),
    (0xFFDA, 0xFFDC, "H"),
    (0xFFDD, 0xFFE7, "F"),
    (0xFFE8, 0xFFEE, "H"),
    (0xFFEF, 0xFFF8, "F"),
    (0xFFFD, 0xFFFD, "A"),
    (0xFFFE, 0xFFFF, "F"),
    (0x1000C, 0x1000C, "F"),
    (0x10027, 0x10027, "F"),
    (0x1003B, 0x1003B, "F"),
    (0x1003E, 0x1003E, "F"),
    (0x1004E, 0x1004F, "F"),
    (0x1005E, 0x1007F, "F"),
    (0x100FB, 0x100FF, "F"),
    (0x10103, 0x10106, "F"),
    (0x10134, 0x10136, "F"),
    (0x1018F, 0x1018F, "F"),
    (0x1019D, 0x1019F, "F"),
    (0x101A1, 0x101CF, "F"),
    (0x101FE, 0x1027F, "F"),
    (0x1029D, 0x1029F, "F"),
    (0x102D1, 0x102DF, "F"),
    (0x102FC, 0x102FF, "F"),
    (0x10324, 0x1032C, "F"),
    (0x1034B, 0x1034F, "F"),
    (0x1037B, 0x1037F, "F"),
    (0x1039E, 0x1039E, "F"),
    (0x103C4, 0x103C7, "F"),
    (0x103D6, 0x103FF, "F"),
    (0x1049E, 0x1049F, "F"),
    (0x104AA, 0x104AF, "F"),
    (0x104D4, 0x104D7, "F"),
    (0x104FC, 0x104FF, "F"),
    (0x10528, 0x1052F, "F"),
    (0x10564, 0x1056E, "F"),
    (0x10570, 0x105FF, "F"),
    (0x10737, 0x1073F, "F"),
    (0x10756, 0x1075F, "F"),
    (0x10768, 0x107FF, "F"),
    (0x10806, 0x10807, "F"),
    (0x10809, 0x10809, "F"),
    (0x10836, 0x10836, "F"),
    (0x10839, 0x1083B, "F"),
    (0x1083D, 0x1083E, "F"),
    (0x10856, 0x10856, "F"),
    (0x1089F, 0x108A6, "F"),
    (0x108B0, 0x108DF, "F"),
    (0x108F3, 0x108F3, "F"),
    (0x108F6, 0x108FA, "F"),
    (0x1091C, 0x1091E, "F"),
    (0x1093A, 0x1093E, "F"),
    (0x10940, 0x1097F, "F"),
    (0x109B8, 0x109BB, "F"),
    (0x109D0, 0x109D1, "F"),
    (0x10A04, 0x10A04, "F"),
    (0x10A07, 0x10A0B, "F"),
    (0x10A14, 0x10A14, "F"),
    (0x10A18, 0x10A18, "F"),
    (0x10A36, 0x10A37, "F"),
    (0x10A3B, 0x10A3E, "F"),
    (0x10A49, 0x10A4F, "F"),
    (0x10A59, 0x10A5F, "F"),
    (0x10AA0, 0x10ABF, "F"),
    (0x10AE7, 0x10AEA, "F"),
    (0x10AF7, 0x10AFF, "F"),
    (0x10B36, 0x10B38, "F"),
    (0x10B56, 0x10B57, "F"),
    (0x10B73, 0x10B77, "F"),
    (0x10B92, 0x10B98, "F"),
    (0x10B9D, 0x10BA8, "F"),
    (0x10BB0, 0x10BFF, "F"),
    (0x10C49, 0x10C7F, "F"),
    (0x10CB3, 0x10CBF, "F"),
    (0x10CF3, 0x10CF9, "F"),
    (0x10D28, 0x10D2F, "F"),
    (0x10D3A, 0x10E5F, "F"),
    (0x10E7F, 0x10E7F, "F"),
    (0x10EAA, 0x10EAA, "F"),
    (0x10EAE, 0x10EAF, "F"),
    (0x10EB2, 0x10EFF, "F"),
    (0x10F28, 0x10F2F, "F"),
    (0x10F5A, 0x10FAF, "F"),
    (0x10FCC, 0x10FDF, "F"),
    (0x10FF7, 0x10FFF, "F"),
    (0x1104E, 0x11051, "F"),
    (0x11070, 0x1107E, "F"),
    (0x110C2, 0x110CC, "F"),
    (0x110CE, 0x110CF, "F"),
    (0x110E9, 0x110EF, "F"),
    (0x110FA, 0x110FF, "F"),
    (0x11135, 0x11135, "F"),
    (0x11148, 0x1114F, "F"),
    (0x11177, 0x1117F, "F"),
    (0x111E0, 0x111E0, "F"),
    (0x111F5, 0x111FF, "F"),
    (0x11212, 0x11212, "F"),
    (0x1123F, 0x1127F, "F"),
    (0x11287, 0x11287, "F"),
    (0x11289, 0x11289, "F"),
    (0x1128E, 0x1128E, "F"),
    (0x1129E, 0x1129E, "F"),
    (0x112AA, 0x112AF, "F"),
    (0x112EB, 0x112EF, "F"),
    (0x112FA, 0x112FF, "F"),
    (0x11304, 0x11304, "F"),
    (0x1130D, 0x1130E, "F"),
    (0x11311, 0x11312, "F"),
    (0x11329, 0x11329, "F"),
    (0x11331, 0x11331, "F"),
    (0x11334, 0x11334, "F"),
    (0x1133A, 0x1133A, "F"),
    (0x11345, 0x11346, "F"),
    (0x11349, 0x1134A, "F"),
    (0x1134E, 0x1134F, "F"),
    (0x11351, 0x11356, "F"),
    (0x11358, 0x1135C, "F"),
    (0x11364, 0x11365, "F"),
    (0x1136D, 0x1136F, "F"),
    (0x11375, 0x113FF, "F"),
    (0x1145C, 0x1145C, "F"),
    (0x11462, 0x1147F, "F"),
    (0x114C8, 0x114CF, "F"),
    (0x114DA, 0x1157F, "F"),
    (0x115B6, 0x115B7, "F"),
    (0x115DE, 0x115FF, "F"),
    (0x11645, 0x1164F, "F"),
    (0x1165A, 0x1165F, "F"),
    (0x1166D, 0x1167F, "F"),
    (0x116B9, 0x116BF, "F"),
    (0x116CA, 0x116FF, "F"),
    (0x1171B, 0x1171C, "F"),
    (0x1172C, 0x1172F, "F"),
    (0x11740, 0x117FF, "F"),
    (0x1183C, 0x1189F, "F"),
    (0x118F3, 0x118FE, "F"),
    (0x11907, 0x11908, "F"),
    (0x1190A, 0x1190B, "F"),
    (0x11914, 0x11914, "F"),
    (0x11917, 0x11917, "F"),
    (0x11936, 0x11936, "F"),
    (0x11939, 0x1193A, "F"),
    (0x11947, 0x1194F, "F"),
    (0x1195A, 0x1199F, "F"),
    (0x119A8, 0x119A9, "F"),
    (0x119D8, 0x119D9, "F"),
    (0x119E5, 0x119FF, "F"),
    (0x11A48, 0x11A4F, "F"),
    (0x11AA3, 0x11ABF, "F"),
    (0x11AF9, 0x11BFF, "F"),
    (0x11C09, 0x11C09, "F"),
    (0x11C37, 0x11C37, "F"),
    (0x11C46, 0x11C4F, "F"),
    (0x11C6D, 0x11C6F, "F"),
    (0x11C90, 0x11C91, "F"),
    (0x11CA8, 0x11CA8, "F"),
    (0x11CB7, 0x11CFF, "F"),
    (0x11D07, 0x11D07, "F"),
    (0x11D0A, 0x11D0A, "F"),
    (0x11D37, 0x11D39, "F"),
    (0x11D3B, 0x11D3B, "F"),
    (0x11D3E, 0x11D3E, "F"),
    (0x11D48, 0x11D4F, "F"),
    (0x11D5A, 0x11D5F, "F"),
    (0x11D66, 0x11D66, "F"),
    (0x11D69, 0x11D69, "F"),
    (0x11D8F, 0x11D8F, "F"),
    (0x11D92, 0x11D92, "F"),
    (0x11D99, 0x11D9F, "F"),
    (0x11DAA, 0x11EDF, "F"),
    (0x11EF9, 0x11FAF, "F"),
    (0x11FB1, 0x11FBF, "F"),
    (0x11FF2, 0x11FFE, "F"),
    (0x1239A, 0x123FF, "F"),
    (0x1246F, 0x1246F, "F"),
    (0x12475, 0x1247F, "F"),
    (0x12544, 0x12FFF, "F"),
    (0x1342F, 0x1342F, "F"),
    (0x13439, 0x143FF, "F"),
    (0x14647, 0x167FF, "F"),
    (0x16A39, 0x16A3F, "F"),
    (0x16A5F, 0x16A5F, "F"),
    (0x16A6A, 0x16A6D, "F"),
    (0x16A70, 0x16ACF, "F"),
    (0x16AEE, 0x16AEF, "F"),
    (0x16AF6, 0x16AFF, "F"),
    (0x16B46, 0x16B4F, "F"),
    (0x16B5A, 0x16B5A, "F"),
    (0x16B62, 0x16B62, "F"),
    (0x16B78, 0x16B7C, "F"),
    (0x16B90, 0x16E3F, "F"),
    (0x16E9B, 0x16EFF, "F"),
    (0x16F4B, 0x16F4E, "F"),
    (0x16F88, 0x16F8E, "F"),
    (0x16FA0, 0x16FDF, "F"),
    (0x16FE0, 0x16FE4, "W"),
    (0x16FE5, 0x16FEF, "F"),
    (0x16FF0, 0x16FF1, "W"),
    (0x16FF2, 0x16FFF, "F"),
    (0x17000, 0x187F7, "W"),
    (0x187F8, 0x187FF, "F"),
    (0x18800, 0x18CD5, "W"),
    (0x18CD6, 0x18CFF, "F"),
    (0x18D00, 0x18D08, "W"),
    (0x18D09, 0x1AFFF, "F"),
    (0x1B000, 0x1B11E, "W"),
    (0x1B11F, 0x1B14F, "F"),
    (0x1B150, 0x1B152, "W"),
    (0x1B153, 0x1B163, "F"),
    (0x1B164, 0x1B167, "W"),
    (0x1B168, 0x1B16F, "F"),
    (0x1B170, 0x1B2FB, "W"),
    (0x1B2FC, 0x1BBFF, "F"),
    (0x1BC6B, 0x1BC6F, "F"),
    (0x1BC7D, 0x1BC7F, "F"),
    (0x1BC89, 0x1BC8F, "F"),
    (0x1BC9A, 0x1BC9B, "F"),
    (0x1BCA4, 0x1CFFF, "F"),
    (0x1D0F6, 0x1D0FF, "F"),
    (0x1D127, 0x1D128, "F"),
    (0x1D1E9, 0x1D1FF, "F"),
    (0x1D246, 0x1D2DF, "F"),
    (0x1D2F4, 0x1D2FF, "F"),
    (0x1D357, 0x1D35F, "F"),
    (0x1D379, 0x1D3FF, "F"),
    (0x1D455, 0x1D455, "F"),
    (0x1D49D, 0x1D49D, "F"),
    (0x1D4A0, 0x1D4A1, "F"),
    (0x1D4A3, 0x1D4A4, "F"),
    (0x1D4A7, 0x1D4A8, "F"),
    (0x1D4AD, 0x1D4AD, "F"),
    (0x1D4BA, 0x1D4BA, "F"),
    (0x1D4BC, 0x1D4BC, "F"),
    (0x1D4C4, 0x1D4C4, "F"),
    (0x1D506, 0x1D506, "F"),
    (0x1D50B, 0x1D50C, "F"),
    (0x1D515, 0x1D515, "F"),
    (0x1D51D, 0x1D51D, "F"),
    (0x1D53A, 0x1D53A, "F"),
    (0x1D53F, 0x1D53F, "F"),
    (0x1D545, 0x1D545, "F"),
    (0x1D547, 0x1D549, "F"),
    (0x1D551, 0x1D551, "F"),
    (0x1D6A6, 0x1D6A7, "F"),
    (0x1D7CC, 0x1D7CD, "F"),
    (0x1DA8C, 0x1DA9A, "F"),
    (0x1DAA0, 0x1DAA0, "F"),
    (0x1DAB0, 0x1DFFF, "F"),
    (0x1E007, 0x1E007, "F"),
    (0x1E019, 0x1E01A, "F"),
    (0x1E022, 0x1E022, "F"),
    (0x1E025, 0x1E025, "F"),
    (0x1E02B, 0x1E0FF, "F"),
    (0x1E12D, 0x1E12F, "F"),
    (0x1E13E, 0x1E13F, "F"),
    (0x1E14A, 0x1E14D, "F"),
    (0x1E150, 0x1E2BF, "F"),
    (0x1E2FA, 0x1E2FE, "F"),
    (0x1E300, 0x1E7FF, "F"),
    (0x1E8C5, 0x1E8C6, "F"),
    (0x1E8D7, 0x1E8FF, "F"),
    (0x1E94C, 0x1E94F, "F"),
    (0x1E95A, 0x1E95D, "F"),
    (0x1E960, 0x1EC70, "F"),
    (0x1ECB5, 0x1ED00, "F"),
    (0x1ED3E, 0x1EDFF, "F"),
    (0x1EE04, 0x1EE04, "F"),
    (0x1EE20, 0x1EE20, "F"),
    (0x1EE23, 0x1EE23, "F"),
    (0x1EE25, 0x1EE26, "F"),
    (0x1EE28, 0x1EE28, "F"),
    (0x1EE33, 0x1EE33, "F"),
    (0x1EE38, 0x1EE38, "F"),
    (0x1EE3A, 0x1EE3A, "F"),
    (0x1EE3C, 0x1EE41, "F"),
    (0x1EE43, 0x1EE46, "F"),
    (0x1EE48, 0x1EE48, "F"),
    (0x1EE4A, 0x1EE4A, "F"),
    (0x1EE4C, 0x1EE4C, "F"),
    (0x1EE50, 0x1EE50, "F"),
    (0x1EE53, 0x1EE53, "F"),
    (0x1EE55, 0x1EE56, "F"),
    (0x1EE58, 0x1EE58, "F"),
    (0x1EE5A, 0x1EE5A, "F"),
    (0x1EE5C, 0x1EE5C, "F"),
    (0x1EE5E, 0x1EE5E, "F"),
    (0x1EE60, 0x1EE60, "F"),
    (0x1EE63, 0x1EE63, "F"),
    (0x1EE65, 0x1EE66, "F"),
    (0x1EE6B, 0x1EE6B, "F"),
    (0x1EE73, 0x1EE73, "F"),
    (0x1EE78, 0x1EE78, "F"),
    (0x1EE7D, 0x1EE7D, "F"),
    (0x1EE7F, 0x1EE7F, "F"),
    (0x1EE8A, 0x1EE8A, "F"),
    (0x1EE9C, 0x1EEA0, "F"),
    (0x1EEA4, 0x1EEA4, "F"),
    (0x1EEAA, 0x1EEAA, "F"),
    (0x1EEBC, 0x1EEEF, "F"),
    (0x1EEF2, 0x1EFFF, "F"),
    (0x1F004, 0x1F004, "W"),
    (0x1F02C, 0x1F02F, "F"),
    (0x1F094, 0x1F09F, "F"),
    (0x1F0AF, 0x1F0B0, "F"),
    (0x1F0C0, 0x1F0C0, "F"),
    (0x1F0CF, 0x1F0CF, "W"),
    (0x1F0D0, 0x1F0D0, "F"),
    (0x1F0F6, 0x1F0FF, "F"),
    (0x1F100, 0x1F10A, "A"),
    (0x1F110, 0x1F12D, "A"),
    (0x1F130, 0x1F169, "A"),
    (0x1F170, 0x1F18D, "A"),
    (0x1F18E, 0x1F18E, "W"),
    (0x1F18F, 0x1F190, "A"),
    (0x1F191, 0x1F19A, "W"),
    (0x1F19B, 0x1F1AC, "A"),
    (0x1F1AE, 0x1F1E5, "F"),
    (0x1F200, 0x1F202, "W"),
    (0x1F203, 0x1F20F, "F"),
    (0x1F210, 0x1F23B, "W"),
    (0x1F23C, 0x1F23F, "F"),
    (0x1F240, 0x1F248, "W"),
    (0x1F249, 0x1F24F, "F"),
    (0x1F250, 0x1F251, "W"),
    (0x1F252, 0x1F25F, "F"),
    (0x1F260, 0x1F265, "W"),
    (0x1F266, 0x1F2FF, "F"),
    (0x1F300, 0x1F320, "W"),
    (0x1F32D, 0x1F335, "W"),
    (0x1F337, 0x1F37C, "W"),
    (0x1F37E, 0x1F393, "W"),
    (0x1F3A0, 0x1F3CA, "W"),
    (0x1F3CF, 0x1F3D3, "W"),
    (0x1F3E0, 0x1F3F0, "W"),
    (0x1F3F4, 0x1F3F4, "W"),
    (0x1F3F8, 0x1F43E, "W"),
    (0x1F440, 0x1F440, "W"),
    (0x1F442, 0x1F4FC, "W"),
    (0x1F4FF, 0x1F53D, "W"),
    (0x1F54B, 0x1F54E, "W"),
    (0x1F550, 0x1F567, "W"),
    (0x1F57A, 0x1F57A, "W"),
    (0x1F595, 0x1F596, "W"),
    (0x1F5A4, 0x1F5A4, "W"),
    (0x1F5FB, 0x1F64F, "W"),
    (0x1F680, 0x1F6C5, "W"),
    (0x1F6CC, 0x1F6CC, "W"),
    (0x1F6D0, 0x1F6D2, "W"),
    (0x1F6D5, 0x1F6D7, "W"),
    (0x1F6D8, 0x1F6DF, "F"),
    (0x1F6EB, 0x1F6EC, "W"),
    (0x1F6ED, 0x1F6EF, "F"),
    (0x1F6F4, 0x1F6FC, "W"),
    (0x1F6FD, 0x1F6FF, "F"),
    (0x1F774, 0x1F77F, "F"),
    (0x1F7D9, 0x1F7DF, "F"),
    (0x1F7E0, 0x1F7EB, "W"),
    (0x1F7EC, 0x1F7FF, "F"),
    (0x1F80C, 0x1F80F, "F"),
    (0x1F848, 0x1F84F, "F"),
    (0x1F85A, 0x1F85F, "F"),
    (0x1F888, 0x1F88F, "F"),
    (0x1F8AE, 0x1F8AF, "F"),
    (0x1F8B2, 0x1F8FF, "F"),
    (0x1F90C, 0x1F93A, "W"),
    (0x1F93C, 0x1F945, "W"),
    (0x1F947, 0x1F978, "W"),
    (0x1F979, 0x1F979, "F"),
    (0x1F97A, 0x1F9CB, "W"),
    (0x1F9CC, 0x1F9CC, "F"),
    (0x1F9CD, 0x1F9FF, "W"),
    (0x1FA54, 0x1FA5F, "F"),
    (0x1FA6E, 0x1FA6F, "F"),
    (0x1FA70, 0x1FA74, "W"),
    (0x1FA75, 0x1FA77, "F"),
    (0x1FA78, 0x1FA7A, "W"),
    (0x1FA7B, 0x1FA7F, "F"),
    (0x1FA80, 0x1FA86, "W"),
    (0x1FA87, 0x1FA8F, "F"),
    (0x1FA90, 0x1FAA8, "W"),
    (0x1FAA9, 0x1FAAF, "F"),
    (0x1FAB0, 0x1FAB6, "W"),
    (0x1FAB7, 0x1FABF, "F"),
    (0x1FAC0, 0x1FAC2, "W"),
    (0x1FAC3, 0x1FACF, "F"),
    (0x1FAD0, 0x1FAD6, "W"),
    (0x1FAD7, 0x1FAFF, "F"),
    (0x1FB93, 0x1FB93, "F"),
    (0x1FBCB, 0x1FBEF, "F"),
    (0x1FBFA, 0x1FFFF, "F"),
    (0x20000, 0x2A6DD, "W"),
    (0x2A6DE, 0x2A6FF, "F"),
    (0x2A700, 0x2B734, "W"),
    (0x2B735, 0x2B73F, "F"),
    (0x2B740, 0x2B81D, "W"),
    (0x2B81E, 0x2B81F, "F"),
    (0x2B820, 0x2CEA1, "W"),
    (0x2CEA2, 0x2CEAF, "F"),
    (0x2CEB0, 0x2EBE0, "W"),
    (0x2EBE1, 0x2F7FF, "F"),
    (0x2F800, 0x2FA1D, "W"),
    (0x2FA1E, 0x2FFFF, "F"),
    (0x30000, 0x3134A, "W"),
    (0x3134B, 0xE0000, "F"),
    (0xE0002, 0xE001F, "F"),
    (0xE0080, 0xE00FF, "F"),
    (0xE0100, 0xE01EF, "A"),
    (0xE01F0, 0xEFFFF, "F"),
    (0xF0000, 0xFFFFD, "A"),
    (0xFFFFE, 0xFFFFF, "F"),
    (0x100000, 0x10FFFD, "A"),
    (0x10FFFE, 0x10FFFF, "F"),
];

fn unicodedata_east_asian_width(args: &[Object]) -> Result<Object, RuntimeError> {
    let ch = first_char(args, "east_asian_width")?;
    let code = ch as u32;
    let idx = EAW_RANGES.partition_point(|&(start, _, _)| start <= code);
    let class = if idx > 0 {
        let (_, end, cls) = EAW_RANGES[idx - 1];
        if code <= end { cls } else { "N" }
    } else {
        "N"
    };
    Ok(Object::from_static(class))
}

fn key_error(msg: impl Into<String>) -> RuntimeError {
    crate::error::key_error(msg)
}

// ----- Unicode name database (compact, lookup-on-demand) -----

/// Look up the canonical Unicode character name for `ch`. We embed
/// names for ASCII controls, the basic Latin range, and the common
/// Latin/Greek/Cyrillic blocks. Beyond that we generate names of
/// the form `<HEX>` so callers with `default=None` still see a
/// usable identifier. CPython returns the full UCD name for every
/// character; matching that would require shipping the ~600 KB
/// nameslist; we deliberately ship a smaller subset that covers
/// the common script blocks.
fn char_name(ch: char) -> Option<String> {
    // The full UCD name table (e.g. 'a' -> "LATIN SMALL LETTER A",
    // '•' -> "BULLET"). CPython's `unicodedata.name` raises for control
    // and unassigned code points, which `unicode_names2::name` also
    // reports as `None`, so the caller's "no such name" path matches.
    if let Some(name) = unicode_names2::name(ch) {
        return Some(name.to_string());
    }
    None
}

/// Reverse-look-up — `unicodedata.lookup('LATIN SMALL LETTER A') == 'a'`.
/// Supports the names we synthesise plus a small hand-rolled table of
/// commonly looked-up sequences.
fn name_to_char(name: &str) -> Option<char> {
    // Full UCD name table first — this is the authoritative source for
    // `unicodedata.lookup` and `\N{NAME}` (e.g. "BULLET", "NO-BREAK
    // SPACE", "GREEK SMALL LETTER ALPHA").
    if let Some(ch) = unicode_names2::character(name) {
        return Some(ch);
    }
    // Hex form `U+1234`.
    if let Some(rest) = name.strip_prefix("U+") {
        if let Ok(n) = u32::from_str_radix(rest.trim(), 16) {
            return char::from_u32(n);
        }
    }
    // Synthesised LATIN forms.
    let upper = name.to_ascii_uppercase();
    if let Some(rest) = upper.strip_prefix("LATIN-1 CAPITAL LETTER ") {
        if let Some(c) = rest.chars().next() {
            return Some(c);
        }
    }
    if let Some(rest) = upper.strip_prefix("LATIN-1 SMALL LETTER ") {
        if let Some(c) = rest.chars().next() {
            return Some(c.to_ascii_lowercase());
        }
    }
    if let Some(rest) = upper.strip_prefix("LATIN-1 DIGIT ") {
        if let Some(c) = rest.chars().next() {
            return Some(c);
        }
    }
    // Common single-named characters.
    Some(match upper.as_str() {
        "NULL" => '\0',
        "BELL" | "ALERT" => '\u{07}',
        "BACKSPACE" => '\u{08}',
        "TAB" | "CHARACTER TABULATION" | "HORIZONTAL TAB" => '\t',
        "LINE FEED" | "NEWLINE" => '\n',
        "VERTICAL TAB" | "LINE TABULATION" => '\u{0B}',
        "FORM FEED" => '\u{0C}',
        "CARRIAGE RETURN" => '\r',
        "ESCAPE" => '\u{1B}',
        "SPACE" => ' ',
        "EXCLAMATION MARK" => '!',
        "QUOTATION MARK" => '"',
        "NUMBER SIGN" => '#',
        "DOLLAR SIGN" => '$',
        "PERCENT SIGN" => '%',
        "AMPERSAND" => '&',
        "APOSTROPHE" => '\'',
        "LEFT PARENTHESIS" => '(',
        "RIGHT PARENTHESIS" => ')',
        "ASTERISK" => '*',
        "PLUS SIGN" => '+',
        "COMMA" => ',',
        "HYPHEN-MINUS" => '-',
        "FULL STOP" => '.',
        "SOLIDUS" => '/',
        "COLON" => ':',
        "SEMICOLON" => ';',
        "LESS-THAN SIGN" => '<',
        "EQUALS SIGN" => '=',
        "GREATER-THAN SIGN" => '>',
        "QUESTION MARK" => '?',
        "COMMERCIAL AT" => '@',
        "LEFT SQUARE BRACKET" => '[',
        "REVERSE SOLIDUS" => '\\',
        "RIGHT SQUARE BRACKET" => ']',
        "CIRCUMFLEX ACCENT" => '^',
        "LOW LINE" => '_',
        "GRAVE ACCENT" => '`',
        "LEFT CURLY BRACKET" => '{',
        "VERTICAL LINE" => '|',
        "RIGHT CURLY BRACKET" => '}',
        "TILDE" => '~',
        "GREEK SMALL LETTER ALPHA" => 'α',
        "GREEK SMALL LETTER BETA" => 'β',
        "GREEK SMALL LETTER GAMMA" => 'γ',
        "GREEK SMALL LETTER DELTA" => 'δ',
        "GREEK SMALL LETTER EPSILON" => 'ε',
        "GREEK SMALL LETTER PI" => 'π',
        "GREEK SMALL LETTER SIGMA" => 'σ',
        "GREEK SMALL LETTER OMEGA" => 'ω',
        "GREEK CAPITAL LETTER ALPHA" => 'Α',
        "GREEK CAPITAL LETTER OMEGA" => 'Ω',
        "GREEK CAPITAL LETTER SIGMA" => 'Σ',
        "EM DASH" => '—',
        "EN DASH" => '–',
        "HORIZONTAL ELLIPSIS" => '…',
        "LATIN CAPITAL LETTER E WITH ACUTE" => 'É',
        "LATIN SMALL LETTER E WITH ACUTE" => 'é',
        "LATIN SMALL LETTER N WITH TILDE" => 'ñ',
        "LATIN CAPITAL LETTER N WITH TILDE" => 'Ñ',
        "LATIN SMALL LETTER A WITH RING ABOVE" => 'å',
        "LATIN CAPITAL LETTER A WITH RING ABOVE" => 'Å',
        "INFINITY" => '∞',
        "BLACK CIRCLE" => '●',
        "WHITE CIRCLE" => '○',
        _ => return None,
    })
}
