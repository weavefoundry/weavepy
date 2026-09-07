#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

//! The `grp` built-in module (RFC 0075 WS9).
//!
//! CPython's `Modules/grpmodule.c`: group-database access over libc
//! `getgrgid`/`getgrnam`/`getgrent`. gunicorn's user-switching surface
//! (`gunicorn/util.py`) imports it alongside `pwd`. POSIX-only, like
//! CPython.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{key_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// `grp.struct_group` field names (CPython's `struct_group_type_fields`).
const GROUP_FIELDS: [&str; 4] = ["gr_name", "gr_passwd", "gr_gid", "gr_mem"];

fn struct_group_type() -> Rc<crate::types::TypeObject> {
    super::os::struct_seq_type("struct_group", "grp", &GROUP_FIELDS)
}

/// # Safety
/// `g` must point to a live `libc::group` returned by libc.
#[cfg(unix)]
unsafe fn group_to_object(g: *const libc::group) -> Object {
    unsafe fn cstr(p: *const libc::c_char) -> Object {
        if p.is_null() {
            Object::from_static("")
        } else {
            Object::from_str(
                unsafe { std::ffi::CStr::from_ptr(p) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }
    let mut members = Vec::new();
    unsafe {
        let mut mem = (*g).gr_mem;
        while !mem.is_null() && !(*mem).is_null() {
            members.push(cstr(*mem));
            mem = mem.add(1);
        }
    }
    let values = unsafe {
        vec![
            cstr((*g).gr_name),
            cstr((*g).gr_passwd),
            Object::Int(i64::from((*g).gr_gid)),
            Object::new_list(members),
        ]
    };
    super::os::struct_seq_instance(struct_group_type(), &GROUP_FIELDS, values)
}

/// `grp.getgrgid(id)` — arity and `_Py_Gid_Converter` semantics
/// (`test_grp.test_errors`): exactly one argument, `int` only (`-1` wraps
/// to `(gid_t)-1`), and anything outside `gid_t` is an `OverflowError`.
#[cfg(unix)]
fn grp_getgrgid(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() > 1 {
        return Err(type_error(format!(
            "getgrgid() takes at most 1 argument ({} given)",
            args.len()
        )));
    }
    let gid = match args.first() {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::Long(n)) => {
            return Err(crate::error::overflow_error(
                if n.sign() == num_bigint::Sign::Minus {
                    "gid is less than minimum"
                } else {
                    "gid is greater than maximum"
                },
            ))
        }
        Some(other) => {
            return Err(type_error(format!(
                "gid should be integer, not {}",
                other.type_name()
            )))
        }
        None => {
            return Err(type_error(
                "getgrgid() missing required argument 'id' (pos 1)",
            ))
        }
    };
    let raw = if gid == -1 {
        libc::gid_t::MAX
    } else {
        libc::gid_t::try_from(gid).map_err(|_| {
            crate::error::overflow_error(if gid < 0 {
                "gid is less than minimum"
            } else {
                "gid is greater than maximum"
            })
        })?
    };
    let g = unsafe { libc::getgrgid(raw) };
    if g.is_null() {
        return Err(key_error(format!("getgrgid(): gid not found: {gid}")));
    }
    Ok(unsafe { group_to_object(g) })
}

/// Convert a `str` name argument to a C string the way CPython's
/// `PyUnicode_FSConverter` does: a surrogate-bearing `str` goes through
/// `utf-8`/`surrogateescape` (an unencodable one is a `UnicodeEncodeError`),
/// and an embedded NUL is a `ValueError`.
#[cfg(unix)]
pub(super) fn name_arg_to_cstring(
    args: &[Object],
    func: &str,
) -> Result<(String, std::ffi::CString), RuntimeError> {
    if args.len() > 1 {
        return Err(type_error(format!(
            "{func}() takes at most 1 argument ({} given)",
            args.len()
        )));
    }
    let (name, bytes) = match args.first() {
        Some(Object::Str(s)) => (s.to_string(), s.as_bytes().to_vec()),
        Some(Object::WStr(cps)) => {
            let bytes =
                crate::stdlib::codecs_mod::encode_codepoints(cps, "utf-8", "surrogateescape")?;
            (String::from_utf8_lossy(&bytes).into_owned(), bytes)
        }
        Some(other) => {
            return Err(type_error(format!(
                "{func}() argument 'name' must be str, not {}",
                other.type_name()
            )))
        }
        None => {
            return Err(type_error(format!(
                "{func}() missing required argument 'name' (pos 1)"
            )))
        }
    };
    let cname = std::ffi::CString::new(bytes)
        .map_err(|_| crate::error::value_error("embedded null byte"))?;
    Ok((name, cname))
}

#[cfg(unix)]
fn grp_getgrnam(args: &[Object]) -> Result<Object, RuntimeError> {
    let (name, cname) = name_arg_to_cstring(args, "getgrnam")?;
    let g = unsafe { libc::getgrnam(cname.as_ptr()) };
    if g.is_null() {
        return Err(key_error(format!("getgrnam(): name not found: '{name}'")));
    }
    Ok(unsafe { group_to_object(g) })
}

#[cfg(unix)]
fn grp_getgrall(args: &[Object]) -> Result<Object, RuntimeError> {
    if !args.is_empty() {
        return Err(type_error(format!(
            "grp.getgrall() takes no arguments ({} given)",
            args.len()
        )));
    }
    let mut entries = Vec::new();
    unsafe {
        libc::setgrent();
        loop {
            let g = libc::getgrent();
            if g.is_null() {
                break;
            }
            entries.push(group_to_object(g));
        }
        libc::endgrent();
    }
    Ok(Object::new_list(entries))
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("grp"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Access to the Unix group database."),
        );
        d.insert(
            DictKey(Object::from_static("struct_group")),
            Object::Type(struct_group_type()),
        );
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("getgrgid")),
                builtin("getgrgid", grp_getgrgid),
            );
            d.insert(
                DictKey(Object::from_static("getgrnam")),
                builtin("getgrnam", grp_getgrnam),
            );
            d.insert(
                DictKey(Object::from_static("getgrall")),
                builtin("getgrall", grp_getgrall),
            );
        }
    }
    Rc::new(PyModule {
        name: "grp".to_owned(),
        filename: None,
        dict,
    })
}
