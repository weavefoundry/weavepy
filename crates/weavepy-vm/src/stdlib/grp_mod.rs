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

#[cfg(unix)]
fn grp_getgrgid(args: &[Object]) -> Result<Object, RuntimeError> {
    let gid = match args.first() {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::Long(_)) => -1,
        Some(other) => {
            return Err(type_error(format!(
                "getgrgid(): gid must be a number, not {}",
                other.type_name()
            )))
        }
        None => return Err(type_error("getgrgid() takes exactly 1 argument (0 given)")),
    };
    let g = unsafe { libc::getgrgid(gid as libc::gid_t) };
    if g.is_null() {
        return Err(key_error(format!("getgrgid(): gid not found: {gid}")));
    }
    Ok(unsafe { group_to_object(g) })
}

#[cfg(unix)]
fn grp_getgrnam(args: &[Object]) -> Result<Object, RuntimeError> {
    let name = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(type_error(format!(
                "getgrnam(): argument must be a str, not {}",
                other.type_name()
            )))
        }
        None => return Err(type_error("getgrnam() takes exactly 1 argument (0 given)")),
    };
    let cname = std::ffi::CString::new(name.as_str())
        .map_err(|_| type_error("getgrnam(): embedded null character"))?;
    let g = unsafe { libc::getgrnam(cname.as_ptr()) };
    if g.is_null() {
        return Err(key_error(format!("getgrnam(): name not found: '{name}'")));
    }
    Ok(unsafe { group_to_object(g) })
}

#[cfg(unix)]
fn grp_getgrall(_args: &[Object]) -> Result<Object, RuntimeError> {
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
