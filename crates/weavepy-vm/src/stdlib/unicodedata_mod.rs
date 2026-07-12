//! The `unicodedata` built-in module — RFC 0023, rebuilt on generated
//! UCD 15.1.0 tables in RFC 0050 WS4.
//!
//! Mirrors CPython 3.13's `Modules/unicodedata.c` surface, backed by
//! `stdlib/ucd` — packed tables produced by `tools/gen_ucd_tables.py`
//! probing host CPython 3.13 (whose database *is* UCD 15.1.0), including
//! the real `ucd_3_2_0` snapshot. Every property answer, name, and
//! normalization comes from the same data CPython ships:
//!
//! - `name(chr[, default])` / `lookup(name)` — full name database plus the
//!   algorithmic Hangul-syllable and ideograph ranges.
//! - `category`, `bidirectional`, `combining`, `mirrored`,
//!   `east_asian_width` — record properties.
//! - `decimal`/`digit`/`numeric` — numeric properties with CPython's
//!   ValueError defaults.
//! - `decomposition(chr)` — raw UCD decomposition text (with `<tag>`).
//! - `normalize`/`is_normalized` — NFC/NFD/NFKC/NFKD from the same tables
//!   (canonical composition pairs probed with exclusions applied).
//! - `unidata_version = "15.1.0"`, `ucd_3_2_0` — the genuine 3.2.0 delta.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::stdlib::ucd;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("unicodedata"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "This module provides access to the Unicode Character Database which \
                 defines character properties for all Unicode characters.",
            ),
        );
        d.insert(
            DictKey(Object::from_static("unidata_version")),
            Object::from_static("15.1.0"),
        );
        d.insert(
            DictKey(Object::from_static("ucd_3_2_0")),
            Object::Module(build_inner_ucd()),
        );

        for (name, fn_) in [
            (
                "name",
                nd_name as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("lookup", nd_lookup),
            ("category", nd_category),
            ("bidirectional", nd_bidirectional),
            ("combining", nd_combining),
            ("mirrored", nd_mirrored),
            ("decimal", nd_decimal),
            ("digit", nd_digit),
            ("numeric", nd_numeric),
            ("decomposition", nd_decomposition),
            ("normalize", nd_normalize),
            ("is_normalized", nd_is_normalized),
            ("east_asian_width", nd_east_asian_width),
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
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        // The genuine 3.2.0 snapshot: property records probed from CPython's
        // own `unicodedata.ucd_3_2_0` (change-record deltas applied), names
        // filtered by the 3.2.0 assigned set. `stringprep`/`encodings.idna`
        // run IDNA nameprep on exactly CPython's answers.
        for (name, fn_) in [
            (
                "name",
                nd_name_32 as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("lookup", nd_lookup_32),
            ("category", nd_category_32),
            ("bidirectional", nd_bidirectional_32),
            ("combining", nd_combining_32),
            ("mirrored", nd_mirrored_32),
            ("decimal", nd_decimal_32),
            ("digit", nd_digit_32),
            ("numeric", nd_numeric_32),
            ("decomposition", nd_decomposition_32),
            ("normalize", nd_normalize_32),
            ("is_normalized", nd_is_normalized_32),
            ("east_asian_width", nd_east_asian_width_32),
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

/// The single code point of a one-character string argument, accepting both
/// plain `str` (`Object::Str`) and surrogate-bearing `str` (`Object::WStr`,
/// e.g. `chr(0xD800)`). CPython's `unicodedata` functions accept lone
/// surrogates — `test_urlparse`'s `test_urlsplit_normalization` sweeps
/// `map(chr, range(0x21, 0x10000))`, which includes the surrogate range — so
/// we hand back the raw `u32` code point.
fn first_codepoint(args: &[Object], fn_name: &str) -> Result<u32, RuntimeError> {
    match args.first() {
        Some(obj) if obj.is_str() => {
            let cps = obj.str_codepoints().unwrap_or_default();
            match cps.as_slice() {
                [cp] => Ok(*cp),
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

// ---------------------------------------------------------------------------
// property functions (shared impls; thin current/3.2.0 wrappers below)
// ---------------------------------------------------------------------------

/// In the 3.2.0 snapshot, code points that were unassigned in 3.2.0 must
/// answer as unassigned even though the shared record pool is current.
/// CPython gates this on the change-record "category changed to Cn" delta,
/// which the generator bakes directly into the 3.2.0 index — so `record()`
/// already answers correctly; only `name()` needs the assigned-set filter.
fn impl_name(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "name")?;
    let known = if old && !ucd::assigned_in_32(cp) {
        None
    } else {
        ucd::name(cp)
    };
    if let Some(name) = known {
        return Ok(Object::from_str(name));
    }
    if let Some(default) = args.get(1) {
        return Ok(default.clone());
    }
    Err(value_error("no such name"))
}

fn impl_lookup(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("lookup() argument 1 must be a string")),
    };
    // Aliases and named sequences resolve only against the *current*
    // database (CPython's `getcode(..., with_alias_and_seq=1)` is passed 0
    // for the `ucd_3_2_0` snapshot).
    let resolved = if old {
        ucd::lookup(&name)
    } else {
        ucd::lookup_with_aliases(&name)
    };
    if let Some(cp) = resolved {
        if !old || ucd::assigned_in_32(cp) {
            return Ok(Object::str_from_codepoints(vec![cp]));
        }
    }
    if !old {
        if let Some(cps) = ucd::lookup_named_sequence(&name) {
            return Ok(Object::str_from_codepoints(cps.to_vec()));
        }
    }
    Err(key_error(format!("undefined character name '{name}'")))
}

fn impl_category(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "category")?;
    Ok(Object::from_static(ucd::record(cp, old).category()))
}

fn impl_bidirectional(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "bidirectional")?;
    Ok(Object::from_static(ucd::record(cp, old).bidirectional()))
}

fn impl_combining(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "combining")?;
    Ok(Object::Int(i64::from(ucd::record(cp, old).combining())))
}

fn impl_mirrored(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "mirrored")?;
    Ok(Object::Int(i64::from(ucd::record(cp, old).mirrored())))
}

fn impl_decimal(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "decimal")?;
    if let Some(d) = ucd::record(cp, old).decimal() {
        return Ok(Object::Int(i64::from(d)));
    }
    if let Some(default) = args.get(1) {
        return Ok(default.clone());
    }
    Err(value_error("not a decimal"))
}

fn impl_digit(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "digit")?;
    if let Some(d) = ucd::record(cp, old).digit() {
        return Ok(Object::Int(i64::from(d)));
    }
    if let Some(default) = args.get(1) {
        return Ok(default.clone());
    }
    Err(value_error("not a digit"))
}

fn impl_numeric(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "numeric")?;
    if let Some(v) = ucd::record(cp, old).numeric() {
        return Ok(Object::Float(v));
    }
    if let Some(default) = args.get(1) {
        return Ok(default.clone());
    }
    Err(value_error("not a numeric character"))
}

fn impl_decomposition(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "decomposition")?;
    Ok(Object::from_str(
        ucd::record(cp, old).decomposition_string(),
    ))
}

fn impl_east_asian_width(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let cp = first_codepoint(args, "east_asian_width")?;
    Ok(Object::from_static(ucd::record(cp, old).east_asian_width()))
}

fn arg_unistr_codepoints(args: &[Object], fname: &str) -> Result<Vec<u32>, RuntimeError> {
    match args.get(1) {
        Some(Object::Str(s)) => Ok(s.chars().map(|c| c as u32).collect()),
        Some(Object::WStr(cps)) => Ok(cps.to_vec()),
        _ => Err(type_error(format!("{fname}() unistr must be str"))),
    }
}

fn arg_form<'a>(args: &'a [Object], fname: &str) -> Result<&'a str, RuntimeError> {
    match args.first() {
        Some(Object::Str(s)) => Ok(s),
        _ => Err(type_error(format!("{fname}() form must be str"))),
    }
}

// The versioned-UCD behavior is baked into the generated 3.2.0 records:
// code points unassigned in 3.2.0 probe with no decomposition and ccc 0
// (CPython's `nfd_nfkd` passes them through per code point), while
// composition always runs on the current pair table (CPython's `nfc_nfkc`
// has no version gate — `'\U00011935\U00011930'` composes even under
// `ucd_3_2_0`, exercised by `test_normalization`).

fn impl_normalize(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let form = arg_form(args, "normalize")?;
    if !matches!(form, "NFC" | "NFD" | "NFKC" | "NFKD") {
        return Err(value_error("invalid normalization form"));
    }
    let cps = arg_unistr_codepoints(args, "normalize")?;
    let out = ucd::normalize(form, &cps, old);
    Ok(Object::str_from_codepoints(out))
}

fn impl_is_normalized(args: &[Object], old: bool) -> Result<Object, RuntimeError> {
    let form = arg_form(args, "is_normalized")?;
    if !matches!(form, "NFC" | "NFD" | "NFKC" | "NFKD") {
        return Err(value_error("invalid normalization form"));
    }
    let cps = arg_unistr_codepoints(args, "is_normalized")?;
    Ok(Object::Bool(ucd::normalize(form, &cps, old) == cps))
}

macro_rules! nd_pair {
    ($cur:ident, $old:ident, $impl_:ident) => {
        fn $cur(args: &[Object]) -> Result<Object, RuntimeError> {
            $impl_(args, false)
        }
        fn $old(args: &[Object]) -> Result<Object, RuntimeError> {
            $impl_(args, true)
        }
    };
}

nd_pair!(nd_name, nd_name_32, impl_name);
nd_pair!(nd_lookup, nd_lookup_32, impl_lookup);
nd_pair!(nd_category, nd_category_32, impl_category);
nd_pair!(nd_bidirectional, nd_bidirectional_32, impl_bidirectional);
nd_pair!(nd_combining, nd_combining_32, impl_combining);
nd_pair!(nd_mirrored, nd_mirrored_32, impl_mirrored);
nd_pair!(nd_decimal, nd_decimal_32, impl_decimal);
nd_pair!(nd_digit, nd_digit_32, impl_digit);
nd_pair!(nd_numeric, nd_numeric_32, impl_numeric);
nd_pair!(nd_decomposition, nd_decomposition_32, impl_decomposition);
nd_pair!(nd_normalize, nd_normalize_32, impl_normalize);
nd_pair!(nd_is_normalized, nd_is_normalized_32, impl_is_normalized);
nd_pair!(
    nd_east_asian_width,
    nd_east_asian_width_32,
    impl_east_asian_width
);

fn key_error(msg: impl Into<String>) -> RuntimeError {
    crate::error::key_error(msg)
}

// ---------------------------------------------------------------------------
// crate-internal name helpers (codecs `namereplace` + `\N{...}` decode)
// ---------------------------------------------------------------------------

/// Canonical UCD 15.1.0 name for `ch` (CPython raises for controls and
/// unassigned code points; those return `None` here).
pub(crate) fn char_name(ch: char) -> Option<String> {
    ucd::name(ch as u32)
}

/// Reverse lookup for `\N{NAME}` escapes and `unicodedata.lookup`.
pub(crate) fn name_to_char(name: &str) -> Option<char> {
    ucd::lookup(name).and_then(char::from_u32)
}
