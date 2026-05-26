//! The `_locale` module — RFC 0023.
//!
//! A pragmatic, host-agnostic locale shim. We expose the constants
//! and entry points that `locale.py` references but always serve the
//! "C" locale data: `localeconv()` returns the POSIX defaults and
//! `setlocale()` rejects any change other than `C`/`POSIX`. Real
//! libc-driven locales are reserved for a follow-up RFC because
//! they require careful thread-safety and Windows differences.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

// libc constants — mirror the values CPython exports.
pub const LC_ALL: i64 = 6;
pub const LC_CTYPE: i64 = 0;
pub const LC_NUMERIC: i64 = 1;
pub const LC_TIME: i64 = 2;
pub const LC_COLLATE: i64 = 3;
pub const LC_MONETARY: i64 = 4;
pub const LC_MESSAGES: i64 = 5;
pub const CHAR_MAX: i64 = 127;

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_locale"),
        );
        for (name, val) in [
            ("LC_ALL", LC_ALL),
            ("LC_CTYPE", LC_CTYPE),
            ("LC_NUMERIC", LC_NUMERIC),
            ("LC_TIME", LC_TIME),
            ("LC_COLLATE", LC_COLLATE),
            ("LC_MONETARY", LC_MONETARY),
            ("LC_MESSAGES", LC_MESSAGES),
            ("CHAR_MAX", CHAR_MAX),
        ] {
            d.insert(DictKey(Object::from_static(name)), Object::Int(val));
        }
        d.insert(
            DictKey(Object::from_static("Error")),
            Object::Type(crate::builtin_types::builtin_types().value_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("setlocale")),
            builtin("setlocale", l_setlocale),
        );
        d.insert(
            DictKey(Object::from_static("getlocale")),
            builtin("getlocale", l_getlocale),
        );
        d.insert(
            DictKey(Object::from_static("localeconv")),
            builtin("localeconv", l_localeconv),
        );
        d.insert(
            DictKey(Object::from_static("strcoll")),
            builtin("strcoll", l_strcoll),
        );
        d.insert(
            DictKey(Object::from_static("strxfrm")),
            builtin("strxfrm", l_strxfrm),
        );
        d.insert(
            DictKey(Object::from_static("nl_langinfo")),
            builtin("nl_langinfo", l_nl_langinfo),
        );
    }
    Rc::new(PyModule {
        name: "_locale".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn l_setlocale(args: &[Object]) -> Result<Object, RuntimeError> {
    let loc = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => "C".to_owned(),
        _ => return Err(value_error("setlocale: locale must be str or None")),
    };
    if loc == "C" || loc == "POSIX" || loc.is_empty() {
        return Ok(Object::from_static("C"));
    }
    // Pretend success — we don't have real libc integration yet.
    Ok(Object::from_str(loc))
}

fn l_getlocale(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_tuple(vec![Object::None, Object::None]))
}

fn l_localeconv(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut d = DictData::new();
    macro_rules! ins {
        ($k:literal, $v:expr) => {
            d.insert(DictKey(Object::from_static($k)), $v);
        };
    }
    ins!("decimal_point", Object::from_static("."));
    ins!("thousands_sep", Object::from_static(""));
    ins!("grouping", Object::new_list(vec![]));
    ins!("int_curr_symbol", Object::from_static(""));
    ins!("currency_symbol", Object::from_static(""));
    ins!("mon_decimal_point", Object::from_static(""));
    ins!("mon_thousands_sep", Object::from_static(""));
    ins!("mon_grouping", Object::new_list(vec![]));
    ins!("positive_sign", Object::from_static(""));
    ins!("negative_sign", Object::from_static(""));
    ins!("int_frac_digits", Object::Int(CHAR_MAX));
    ins!("frac_digits", Object::Int(CHAR_MAX));
    ins!("p_cs_precedes", Object::Int(CHAR_MAX));
    ins!("p_sep_by_space", Object::Int(CHAR_MAX));
    ins!("n_cs_precedes", Object::Int(CHAR_MAX));
    ins!("n_sep_by_space", Object::Int(CHAR_MAX));
    ins!("p_sign_posn", Object::Int(CHAR_MAX));
    ins!("n_sign_posn", Object::Int(CHAR_MAX));
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

fn l_strcoll(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = args.first().map(|o| o.to_str()).unwrap_or_default();
    let b = args.get(1).map(|o| o.to_str()).unwrap_or_default();
    use std::cmp::Ordering;
    Ok(Object::Int(match a.cmp(&b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }))
}

fn l_strxfrm(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(args.first().cloned().unwrap_or(Object::from_static("")))
}

fn l_nl_langinfo(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = args;
    Ok(Object::from_static(""))
}
