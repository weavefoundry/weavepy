//! The `_locale` module — RFC 0050 WS5.
//!
//! Backed by the host C library: `setlocale`/`localeconv`/`nl_langinfo`
//! call straight into libc (the process-global C locale, exactly like
//! CPython's `_localemodule.c`), `strcoll`/`strxfrm` use the wide-char
//! collation entry points (`wcscoll`/`wcsxfrm`), and `getencoding`
//! reports the `LC_CTYPE` codeset. Strings coming back from libc are
//! decoded with `mbstowcs` under the current `LC_CTYPE`, mirroring
//! CPython's `PyUnicode_DecodeLocale`.
//!
//! On non-Unix hosts (Windows) the langinfo surface doesn't exist, so
//! we fall back to the pre-RFC-0050 C-locale shim: `setlocale` accepts
//! only `C`/`POSIX`, `localeconv` serves POSIX defaults, and the
//! codeset is always UTF-8.

#[cfg(unix)]
use std::ffi::{CStr, CString};

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

// Wide-char/multibyte libc entry points the `libc` crate doesn't bind.
#[cfg(unix)]
unsafe extern "C" {
    fn mbstowcs(
        dest: *mut libc::wchar_t,
        src: *const libc::c_char,
        n: libc::size_t,
    ) -> libc::size_t;
    fn wcscoll(a: *const libc::wchar_t, b: *const libc::wchar_t) -> libc::c_int;
    fn wcsxfrm(
        dest: *mut libc::wchar_t,
        src: *const libc::wchar_t,
        n: libc::size_t,
    ) -> libc::size_t;
}

// Category constants — the host libc's values (CPython exports these
// verbatim from `locale.h`).
#[cfg(unix)]
pub const LC_ALL: i64 = libc::LC_ALL as i64;
#[cfg(unix)]
pub const LC_CTYPE: i64 = libc::LC_CTYPE as i64;
#[cfg(unix)]
pub const LC_NUMERIC: i64 = libc::LC_NUMERIC as i64;
#[cfg(unix)]
pub const LC_TIME: i64 = libc::LC_TIME as i64;
#[cfg(unix)]
pub const LC_COLLATE: i64 = libc::LC_COLLATE as i64;
#[cfg(unix)]
pub const LC_MONETARY: i64 = libc::LC_MONETARY as i64;
#[cfg(unix)]
pub const LC_MESSAGES: i64 = libc::LC_MESSAGES as i64;

// Non-Unix fallback: the POSIX-ish values the pre-RFC-0050 shim used.
#[cfg(not(unix))]
pub const LC_ALL: i64 = 6;
#[cfg(not(unix))]
pub const LC_CTYPE: i64 = 0;
#[cfg(not(unix))]
pub const LC_NUMERIC: i64 = 1;
#[cfg(not(unix))]
pub const LC_TIME: i64 = 2;
#[cfg(not(unix))]
pub const LC_COLLATE: i64 = 3;
#[cfg(not(unix))]
pub const LC_MONETARY: i64 = 4;
#[cfg(not(unix))]
pub const LC_MESSAGES: i64 = 5;

pub const CHAR_MAX: i64 = 127;

/// `setlocale(LC_CTYPE, "")` — adopt the environment's `LC_CTYPE` locale,
/// as CPython's pre-init does (`_Py_SetLocaleFromEnv`). Called once at
/// interpreter start so `nl_langinfo(CODESET)`/`localeconv` and the
/// `locale` module observe the user's locale rather than plain `"C"`.
#[cfg(unix)]
pub fn init_from_env() {
    let empty = CString::new("").expect("static");
    // SAFETY: `setlocale` with a valid category and NUL-terminated string.
    unsafe {
        libc::setlocale(libc::LC_CTYPE, empty.as_ptr());
    }
}

/// Non-Unix: nothing to adopt — the shim always serves the C locale.
#[cfg(not(unix))]
pub fn init_from_env() {}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
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
        // `nl_langinfo` item constants (langinfo.h). Grouped exactly like
        // CPython's `langinfo_constants` table. Windows has no langinfo.h,
        // matching CPython's `_locale` there.
        #[cfg(unix)]
        for &(name, val) in langinfo_constants() {
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
        // Windows CPython's `_locale` has no `nl_langinfo`; mirror that.
        #[cfg(unix)]
        d.insert(
            DictKey(Object::from_static("nl_langinfo")),
            builtin("nl_langinfo", l_nl_langinfo),
        );
        d.insert(
            DictKey(Object::from_static("getencoding")),
            builtin("getencoding", l_getencoding),
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
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Decode a libc C string under the current `LC_CTYPE` locale, mirroring
/// CPython's `PyUnicode_DecodeLocale`: `mbstowcs` first, with a Latin-1
/// byte-for-byte fallback for undecodable content.
#[cfg(unix)]
fn decode_locale_cstr(ptr: *const libc::c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: libc handed us a NUL-terminated string.
    let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
    if bytes.is_empty() {
        return String::new();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    // SAFETY: `ptr` is NUL-terminated; the first call sizes the buffer.
    unsafe {
        let needed = mbstowcs(std::ptr::null_mut(), ptr, 0);
        if needed != usize::MAX {
            let mut buf = vec![0 as libc::wchar_t; needed + 1];
            let written = mbstowcs(buf.as_mut_ptr(), ptr, needed + 1);
            if written != usize::MAX {
                return buf[..written]
                    .iter()
                    .filter_map(|&w| char::from_u32(w as u32))
                    .collect();
            }
        }
    }
    // Undecodable in the current locale: fall back to Latin-1 so the data
    // is at least preserved byte-for-byte.
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(unix)]
fn arg_category(args: &[Object], fname: &str) -> Result<libc::c_int, RuntimeError> {
    let cat = match args.first() {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => {
            return Err(crate::error::type_error(format!(
                "{fname}: category must be an integer"
            )))
        }
    };
    let lo = LC_ALL.min(LC_CTYPE).min(LC_MESSAGES);
    let hi = LC_ALL
        .max(LC_CTYPE)
        .max(LC_NUMERIC)
        .max(LC_TIME)
        .max(LC_COLLATE)
        .max(LC_MONETARY)
        .max(LC_MESSAGES);
    if !(lo..=hi).contains(&cat) {
        return Err(value_error("invalid locale category"));
    }
    Ok(cat as libc::c_int)
}

#[cfg(unix)]
fn l_setlocale(args: &[Object]) -> Result<Object, RuntimeError> {
    let category = arg_category(args, "setlocale")?;
    match args.get(1) {
        // Query the current locale for the category.
        Some(Object::None) | None => {
            // SAFETY: NULL locale queries without modifying.
            let cur = unsafe { libc::setlocale(category, std::ptr::null()) };
            if cur.is_null() {
                return Err(value_error("locale query failed"));
            }
            Ok(Object::from_str(decode_locale_cstr(cur)))
        }
        Some(Object::Str(s)) => {
            let requested = CString::new(s.as_ref())
                .map_err(|_| value_error("embedded null byte in locale name"))?;
            // SAFETY: valid category + NUL-terminated locale name.
            let res = unsafe { libc::setlocale(category, requested.as_ptr()) };
            if res.is_null() {
                return Err(value_error("unsupported locale setting"));
            }
            Ok(Object::from_str(decode_locale_cstr(res)))
        }
        Some(_) => Err(crate::error::type_error(
            "setlocale: locale must be str or None",
        )),
    }
}

/// CPython's `copy_grouping`: the lconv grouping byte string becomes a list
/// of ints, keeping a trailing `CHAR_MAX` terminator and stopping there.
#[cfg(unix)]
fn copy_grouping(ptr: *const libc::c_char) -> Object {
    if ptr.is_null() {
        return Object::new_list(vec![]);
    }
    // SAFETY: lconv grouping fields are NUL-terminated byte strings.
    let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
    let mut out = Vec::new();
    for &b in bytes {
        out.push(Object::Int(i64::from(b)));
        if i64::from(b) == CHAR_MAX {
            break;
        }
    }
    Object::new_list(out)
}

#[cfg(unix)]
fn l_localeconv(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut d = DictData::default();
    // SAFETY: `localeconv` returns a pointer to static libc storage, valid
    // until the next `localeconv`/`setlocale` call; we copy everything out
    // immediately.
    let lc = unsafe { &*libc::localeconv() };
    let mut ins_str = |k: &'static str, p: *const libc::c_char| {
        d.insert(
            DictKey(Object::from_static(k)),
            Object::from_str(decode_locale_cstr(p)),
        );
    };
    ins_str("decimal_point", lc.decimal_point);
    ins_str("thousands_sep", lc.thousands_sep);
    ins_str("int_curr_symbol", lc.int_curr_symbol);
    ins_str("currency_symbol", lc.currency_symbol);
    ins_str("mon_decimal_point", lc.mon_decimal_point);
    ins_str("mon_thousands_sep", lc.mon_thousands_sep);
    ins_str("positive_sign", lc.positive_sign);
    ins_str("negative_sign", lc.negative_sign);
    let mut ins = |k: &'static str, v: Object| {
        d.insert(DictKey(Object::from_static(k)), v);
    };
    ins("grouping", copy_grouping(lc.grouping));
    ins("mon_grouping", copy_grouping(lc.mon_grouping));
    for (k, v) in [
        ("int_frac_digits", lc.int_frac_digits),
        ("frac_digits", lc.frac_digits),
        ("p_cs_precedes", lc.p_cs_precedes),
        ("p_sep_by_space", lc.p_sep_by_space),
        ("n_cs_precedes", lc.n_cs_precedes),
        ("n_sep_by_space", lc.n_sep_by_space),
        ("p_sign_posn", lc.p_sign_posn),
        ("n_sign_posn", lc.n_sign_posn),
    ] {
        ins(k, Object::Int(i64::from(v)));
    }
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

/// A NUL-terminated wide-char copy of `s` for `wcscoll`/`wcsxfrm`.
#[cfg(unix)]
fn to_wide(s: &str) -> Vec<libc::wchar_t> {
    let mut v: Vec<libc::wchar_t> = s.chars().map(|c| c as u32 as libc::wchar_t).collect();
    v.push(0);
    v
}

fn arg_str(args: &[Object], idx: usize, fname: &str) -> Result<String, RuntimeError> {
    match args.get(idx) {
        Some(Object::Str(s)) => Ok(s.to_string()),
        _ => Err(crate::error::type_error(format!(
            "{fname}() argument must be str"
        ))),
    }
}

#[cfg(unix)]
fn l_strcoll(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = to_wide(&arg_str(args, 0, "strcoll")?);
    let b = to_wide(&arg_str(args, 1, "strcoll")?);
    // SAFETY: both buffers are NUL-terminated wide strings.
    let r = unsafe { wcscoll(a.as_ptr(), b.as_ptr()) };
    Ok(Object::Int(i64::from(r)))
}

#[cfg(unix)]
fn l_strxfrm(args: &[Object]) -> Result<Object, RuntimeError> {
    let s = arg_str(args, 0, "strxfrm")?;
    let src = to_wide(&s);
    // SAFETY: sizing call with a NULL destination, then a copy into a
    // buffer of the reported length (+1 for the terminator).
    unsafe {
        let needed = wcsxfrm(std::ptr::null_mut(), src.as_ptr(), 0);
        if needed == usize::MAX {
            return Err(value_error("invalid string for strxfrm"));
        }
        let mut buf = vec![0 as libc::wchar_t; needed + 1];
        let written = wcsxfrm(buf.as_mut_ptr(), src.as_ptr(), needed + 1);
        if written == usize::MAX || written > needed {
            return Err(value_error("invalid string for strxfrm"));
        }
        Ok(Object::from_str(
            buf[..written]
                .iter()
                .filter_map(|&w| char::from_u32(w as u32))
                .collect::<String>(),
        ))
    }
}

#[cfg(unix)]
fn l_nl_langinfo(args: &[Object]) -> Result<Object, RuntimeError> {
    let item = match args.first() {
        Some(Object::Int(n)) => *n,
        _ => {
            return Err(crate::error::type_error(
                "nl_langinfo() argument must be an integer",
            ))
        }
    };
    if !langinfo_constants().iter().any(|(_, v)| *v == item) {
        return Err(value_error("unsupported langinfo constant"));
    }
    // SAFETY: `item` was validated against the platform's known constants.
    let ptr = unsafe { libc::nl_langinfo(item as libc::nl_item) };
    Ok(Object::from_str(decode_locale_cstr(ptr)))
}

/// The current `LC_CTYPE` codeset (`nl_langinfo(CODESET)`), with the
/// UTF-8 fallback CPython's `_Py_GetLocaleEncoding` applies when the
/// codeset is empty.
#[cfg(unix)]
pub fn current_codeset() -> String {
    // SAFETY: CODESET is a valid langinfo item on all supported hosts.
    let ptr = unsafe { libc::nl_langinfo(libc::CODESET) };
    let name = decode_locale_cstr(ptr);
    if name.is_empty() {
        "UTF-8".to_owned()
    } else {
        name
    }
}

/// Non-Unix: no langinfo — the shim's codeset is always UTF-8.
#[cfg(not(unix))]
pub fn current_codeset() -> String {
    "UTF-8".to_owned()
}

/// `_locale.getencoding()` — the current `LC_CTYPE` codeset.
fn l_getencoding(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(current_codeset()))
}

/// The `nl_langinfo` item constants exported on this platform, mirroring
/// CPython's `langinfo_constants` table in `_localemodule.c`.
#[cfg(unix)]
fn langinfo_constants() -> &'static [(&'static str, i64)] {
    static CONSTANTS: std::sync::OnceLock<Vec<(&'static str, i64)>> = std::sync::OnceLock::new();
    CONSTANTS
        .get_or_init(|| {
            vec![
                ("CODESET", i64::from(libc::CODESET)),
                ("D_T_FMT", i64::from(libc::D_T_FMT)),
                ("D_FMT", i64::from(libc::D_FMT)),
                ("T_FMT", i64::from(libc::T_FMT)),
                ("T_FMT_AMPM", i64::from(libc::T_FMT_AMPM)),
                ("AM_STR", i64::from(libc::AM_STR)),
                ("PM_STR", i64::from(libc::PM_STR)),
                ("DAY_1", i64::from(libc::DAY_1)),
                ("DAY_2", i64::from(libc::DAY_2)),
                ("DAY_3", i64::from(libc::DAY_3)),
                ("DAY_4", i64::from(libc::DAY_4)),
                ("DAY_5", i64::from(libc::DAY_5)),
                ("DAY_6", i64::from(libc::DAY_6)),
                ("DAY_7", i64::from(libc::DAY_7)),
                ("ABDAY_1", i64::from(libc::ABDAY_1)),
                ("ABDAY_2", i64::from(libc::ABDAY_2)),
                ("ABDAY_3", i64::from(libc::ABDAY_3)),
                ("ABDAY_4", i64::from(libc::ABDAY_4)),
                ("ABDAY_5", i64::from(libc::ABDAY_5)),
                ("ABDAY_6", i64::from(libc::ABDAY_6)),
                ("ABDAY_7", i64::from(libc::ABDAY_7)),
                ("MON_1", i64::from(libc::MON_1)),
                ("MON_2", i64::from(libc::MON_2)),
                ("MON_3", i64::from(libc::MON_3)),
                ("MON_4", i64::from(libc::MON_4)),
                ("MON_5", i64::from(libc::MON_5)),
                ("MON_6", i64::from(libc::MON_6)),
                ("MON_7", i64::from(libc::MON_7)),
                ("MON_8", i64::from(libc::MON_8)),
                ("MON_9", i64::from(libc::MON_9)),
                ("MON_10", i64::from(libc::MON_10)),
                ("MON_11", i64::from(libc::MON_11)),
                ("MON_12", i64::from(libc::MON_12)),
                ("ABMON_1", i64::from(libc::ABMON_1)),
                ("ABMON_2", i64::from(libc::ABMON_2)),
                ("ABMON_3", i64::from(libc::ABMON_3)),
                ("ABMON_4", i64::from(libc::ABMON_4)),
                ("ABMON_5", i64::from(libc::ABMON_5)),
                ("ABMON_6", i64::from(libc::ABMON_6)),
                ("ABMON_7", i64::from(libc::ABMON_7)),
                ("ABMON_8", i64::from(libc::ABMON_8)),
                ("ABMON_9", i64::from(libc::ABMON_9)),
                ("ABMON_10", i64::from(libc::ABMON_10)),
                ("ABMON_11", i64::from(libc::ABMON_11)),
                ("ABMON_12", i64::from(libc::ABMON_12)),
                ("RADIXCHAR", i64::from(libc::RADIXCHAR)),
                ("THOUSEP", i64::from(libc::THOUSEP)),
                ("YESEXPR", i64::from(libc::YESEXPR)),
                ("NOEXPR", i64::from(libc::NOEXPR)),
                ("CRNCYSTR", i64::from(libc::CRNCYSTR)),
                ("ERA", i64::from(libc::ERA)),
                ("ERA_D_T_FMT", i64::from(libc::ERA_D_T_FMT)),
                ("ERA_D_FMT", i64::from(libc::ERA_D_FMT)),
                ("ERA_T_FMT", i64::from(libc::ERA_T_FMT)),
                ("ALT_DIGITS", i64::from(libc::ALT_DIGITS)),
            ]
        })
        .as_slice()
}

// ---------------------------------------------------------------------------
// Non-Unix fallbacks — the pre-RFC-0050 C-locale shim, kept so the module
// builds on hosts without POSIX locale APIs (Windows).
// ---------------------------------------------------------------------------

#[cfg(not(unix))]
fn l_setlocale(args: &[Object]) -> Result<Object, RuntimeError> {
    let loc = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        Some(Object::None) | None => "C".to_owned(),
        _ => {
            return Err(crate::error::type_error(
                "setlocale: locale must be str or None",
            ))
        }
    };
    if loc == "C" || loc == "POSIX" || loc.is_empty() {
        return Ok(Object::from_static("C"));
    }
    Err(value_error("unsupported locale setting"))
}

#[cfg(not(unix))]
fn l_localeconv(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut d = DictData::default();
    let mut ins = |k: &'static str, v: Object| {
        d.insert(DictKey(Object::from_static(k)), v);
    };
    for k in [
        "thousands_sep",
        "int_curr_symbol",
        "currency_symbol",
        "mon_decimal_point",
        "mon_thousands_sep",
        "positive_sign",
        "negative_sign",
    ] {
        ins(k, Object::from_static(""));
    }
    ins("decimal_point", Object::from_static("."));
    ins("grouping", Object::new_list(vec![]));
    ins("mon_grouping", Object::new_list(vec![]));
    for k in [
        "int_frac_digits",
        "frac_digits",
        "p_cs_precedes",
        "p_sep_by_space",
        "n_cs_precedes",
        "n_sep_by_space",
        "p_sign_posn",
        "n_sign_posn",
    ] {
        ins(k, Object::Int(CHAR_MAX));
    }
    Ok(Object::Dict(Rc::new(RefCell::new(d))))
}

#[cfg(not(unix))]
fn l_strcoll(args: &[Object]) -> Result<Object, RuntimeError> {
    let a = arg_str(args, 0, "strcoll")?;
    let b = arg_str(args, 1, "strcoll")?;
    use std::cmp::Ordering;
    Ok(Object::Int(match a.cmp(&b) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }))
}

#[cfg(not(unix))]
fn l_strxfrm(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_str(arg_str(args, 0, "strxfrm")?))
}
