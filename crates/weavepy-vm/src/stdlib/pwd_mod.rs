#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

//! The `pwd` built-in module (RFC 0075 WS9).
//!
//! CPython's `Modules/pwdmodule.c`: password-database access over libc
//! `getpwuid`/`getpwnam`/`getpwent`. gunicorn imports it unconditionally
//! at module level (`gunicorn/util.py`) for its user-switching surface,
//! so the `-k gevent` capstone cannot even reach the master process
//! without it. POSIX-only, like CPython (Windows builds have no `pwd`).

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{key_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// `pwd.struct_passwd` field names (CPython's `struct_pwd_type_fields`).
const PASSWD_FIELDS: [&str; 7] = [
    "pw_name",
    "pw_passwd",
    "pw_uid",
    "pw_gid",
    "pw_gecos",
    "pw_dir",
    "pw_shell",
];

fn struct_passwd_type() -> Rc<crate::types::TypeObject> {
    super::os::struct_seq_type("struct_passwd", "pwd", &PASSWD_FIELDS)
}

/// # Safety
/// `p` must point to a live `libc::passwd` returned by libc.
#[cfg(unix)]
unsafe fn passwd_to_object(p: *const libc::passwd) -> Object {
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
    let values = unsafe {
        vec![
            cstr((*p).pw_name),
            cstr((*p).pw_passwd),
            Object::Int(i64::from((*p).pw_uid)),
            Object::Int(i64::from((*p).pw_gid)),
            cstr((*p).pw_gecos),
            cstr((*p).pw_dir),
            cstr((*p).pw_shell),
        ]
    };
    super::os::struct_seq_instance(struct_passwd_type(), &PASSWD_FIELDS, values)
}

fn uid_from_obj(obj: &Object) -> Result<i64, RuntimeError> {
    match obj {
        Object::Int(n) => Ok(*n),
        Object::Bool(b) => Ok(i64::from(*b)),
        // CPython accepts anything `_Py_Uid_Converter` can digest; a huge
        // int is simply "not found" rather than an OverflowError here.
        Object::Long(_) => Ok(-1),
        _ => Err(type_error(format!(
            "getpwuid(): uid must be a number, not {}",
            obj.type_name()
        ))),
    }
}

#[cfg(unix)]
fn pwd_getpwuid(args: &[Object]) -> Result<Object, RuntimeError> {
    let arg = args
        .first()
        .ok_or_else(|| type_error("getpwuid() takes exactly 1 argument (0 given)"))?;
    let uid = uid_from_obj(arg)?;
    let p = unsafe { libc::getpwuid(uid as libc::uid_t) };
    if p.is_null() {
        return Err(key_error(format!("getpwuid(): uid not found: {uid}")));
    }
    Ok(unsafe { passwd_to_object(p) })
}

#[cfg(unix)]
fn pwd_getpwnam(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "getpwnam(): argument must be a str, not {}",
                other.type_name()
            )))
        }
        None => return Err(type_error("getpwnam() takes exactly 1 argument (0 given)")),
    };
    let cname = std::ffi::CString::new(name.as_str())
        .map_err(|_| type_error("getpwnam(): embedded null character"))?;
    let p = unsafe { libc::getpwnam(cname.as_ptr()) };
    if p.is_null() {
        return Err(key_error(format!("getpwnam(): name not found: '{name}'")));
    }
    Ok(unsafe { passwd_to_object(p) })
}

#[cfg(unix)]
fn pwd_getpwall(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut entries = Vec::new();
    unsafe {
        libc::setpwent();
        loop {
            let p = libc::getpwent();
            if p.is_null() {
                break;
            }
            entries.push(passwd_to_object(p));
        }
        libc::endpwent();
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
            Object::from_static("pwd"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("This module provides access to the Unix password database."),
        );
        d.insert(
            DictKey(Object::from_static("struct_passwd")),
            Object::Type(struct_passwd_type()),
        );
        #[cfg(unix)]
        {
            d.insert(
                DictKey(Object::from_static("getpwuid")),
                builtin("getpwuid", pwd_getpwuid),
            );
            d.insert(
                DictKey(Object::from_static("getpwnam")),
                builtin("getpwnam", pwd_getpwnam),
            );
            d.insert(
                DictKey(Object::from_static("getpwall")),
                builtin("getpwall", pwd_getpwall),
            );
        }
    }
    Rc::new(PyModule {
        name: "pwd".to_owned(),
        filename: None,
        dict,
    })
}
