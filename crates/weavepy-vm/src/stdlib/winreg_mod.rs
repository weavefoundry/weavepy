//! The `winreg` built-in module (RFC 0063 WS3) — the Windows registry
//! surface, a faithful transcription of CPython's `PC/winreg.c`.
//!
//! Three layers, mirroring the C module's structure:
//!
//!   1. **The `PyHKEY` handle type** — a context-manager wrapper around
//!      a raw `HKEY` with `Close()`/`Detach()`, truthiness (`False`
//!      once closed), and int conversion. Every function that takes a
//!      key accepts *either* a `PyHKEY` or a plain int (CPython's
//!      `PyHKEY_AsHKEY` contract), and every function that opens a key
//!      returns a `PyHKEY`, so keys close deterministically under
//!      `with` and leak-close at GC time otherwise.
//!   2. **Value marshalling** — [`reg_to_py`]/[`py_to_reg`] transcribe
//!      `Reg2Py`/`Py2Reg`: `REG_SZ`/`REG_EXPAND_SZ` ↔ `str` (raw
//!      UTF-16, so PEP-383 lone surrogates round-trip through the
//!      WStr arc), `REG_MULTI_SZ` ↔ `list[str]` (double-NUL block),
//!      `REG_DWORD`/`REG_QWORD` ↔ unsigned ints, and everything else
//!      (`REG_BINARY` included) ↔ `bytes` (`None` when empty).
//!   3. **The function surface** — the full CPython 3.13 inventory
//!      from `OpenKey` to `QueryReflectionKey`, plus the `HKEY_*` /
//!      `KEY_*` / `REG_*` constant families.
//!
//! Error model: the `Reg*` APIs return the Win32 error code directly
//! (an `LSTATUS`, no `GetLastError` round-trip), so every nonzero
//! status feeds [`nt_support::win32_error_to_py`] verbatim — the
//! resulting `OSError` carries `.winerror`, the errmap-translated
//! `.errno` (`ERROR_FILE_NOT_FOUND` → `ENOENT` → `FileNotFoundError`),
//! and the `FormatMessageW` text, exactly like
//! `PyErr_SetFromWindowsErrWithFunction`.
//!
//! Every registry call runs with the GIL released
//! (`Py_BEGIN_ALLOW_THREADS` in CPython): against a remote registry
//! (`ConnectRegistry`) or a hive on slow storage these are real
//! blocking I/O.

use crate::sync::Rc;
use crate::sync::RefCell;

use num_traits::ToPrimitive;
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_INVALID_DATA, ERROR_MORE_DATA, ERROR_SUCCESS, FILETIME,
};
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::System::Registry as reg;

use crate::error::{overflow_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::stdlib::nt_support;
use crate::stdlib::os::{builtin, builtin_kw};
use crate::types::{PyInstance, TypeObject};

// ---------------------------------------------------------------------------
// Constants windows-sys does not export (winnt.h composites).
// ---------------------------------------------------------------------------

/// `winnt.h REG_LEGAL_CHANGE_FILTER` — the OR of every
/// `REG_NOTIFY_CHANGE_*` bit plus `REG_NOTIFY_THREAD_AGNOSTIC`
/// (0x1000_0000), which is what modern SDKs (and hence CPython's
/// compiled constant) include.
const REG_LEGAL_CHANGE_FILTER: u32 = 0x1000_000F;

/// `winnt.h REG_LEGAL_OPTION` — the OR of every `REG_OPTION_*` bit
/// including `REG_OPTION_DONT_VIRTUALIZE` (0x10) per modern SDKs.
const REG_LEGAL_OPTION: u32 = 0x1F;

/// `winnt.h` hive-load flags (`RegRestoreKey`/`RegReplaceKey` family).
const REG_NO_LAZY_FLUSH: u32 = 0x4;
const REG_REFRESH_HIVE: u32 = 0x2;

/// `winnt.h MAXIMUM_ALLOWED`. `PC/winreg.c`'s `CreateKey` uses the
/// legacy `RegCreateKeyW`, whose documented `RegCreateKeyExW`
/// equivalent requests this access mask — the returned handle must be
/// usable for both writing values and enumerating, which no single
/// `KEY_*` composite grants.
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

// ---------------------------------------------------------------------------
// Module construction
// ---------------------------------------------------------------------------

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("winreg"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("This module provides access to the Windows registry API."),
        );

        // `winreg.error` is OSError (PC/winreg.c inserts PyExc_OSError).
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(crate::builtin_types::builtin_types().os_error.clone()),
        );
        // The handle type is exposed for isinstance checks
        // (`winreg.HKEYType`, like CPython's PyHKEY_Type insertion).
        d.insert(
            DictKey(Object::from_static("HKEYType")),
            Object::Type(hkey_type()),
        );

        // The predefined root keys. The SDK's HKEY_* macros are
        // sign-extended pseudo-handles on 64-bit
        // (0xFFFFFFFF_80000001, …); CPython documents and tests the
        // *unsigned 32-bit* face (HKEY_CURRENT_USER == 0x80000001 ==
        // 2147483649), so truncate back to that before publishing.
        // [`hkey_from_i128`] re-extends on the way into the API.
        for (name, v) in [
            ("HKEY_CLASSES_ROOT", reg::HKEY_CLASSES_ROOT as usize as u32),
            ("HKEY_CURRENT_USER", reg::HKEY_CURRENT_USER as usize as u32),
            (
                "HKEY_LOCAL_MACHINE",
                reg::HKEY_LOCAL_MACHINE as usize as u32,
            ),
            ("HKEY_USERS", reg::HKEY_USERS as usize as u32),
            (
                "HKEY_PERFORMANCE_DATA",
                reg::HKEY_PERFORMANCE_DATA as usize as u32,
            ),
            (
                "HKEY_CURRENT_CONFIG",
                reg::HKEY_CURRENT_CONFIG as usize as u32,
            ),
            ("HKEY_DYN_DATA", reg::HKEY_DYN_DATA as usize as u32),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Int(i64::from(v)),
            );
        }

        // Access rights, value types, and the option/notify/hive-load
        // families — PC/winreg.c's ADD_INT inventory, values straight
        // from windows-sys (= the SDK) so the masks round-trip through
        // the real API.
        for (name, v) in [
            // KEY_* access rights.
            ("KEY_ALL_ACCESS", reg::KEY_ALL_ACCESS),
            ("KEY_WRITE", reg::KEY_WRITE),
            ("KEY_READ", reg::KEY_READ),
            ("KEY_EXECUTE", reg::KEY_EXECUTE),
            ("KEY_QUERY_VALUE", reg::KEY_QUERY_VALUE),
            ("KEY_SET_VALUE", reg::KEY_SET_VALUE),
            ("KEY_CREATE_SUB_KEY", reg::KEY_CREATE_SUB_KEY),
            ("KEY_ENUMERATE_SUB_KEYS", reg::KEY_ENUMERATE_SUB_KEYS),
            ("KEY_NOTIFY", reg::KEY_NOTIFY),
            ("KEY_CREATE_LINK", reg::KEY_CREATE_LINK),
            ("KEY_WOW64_64KEY", reg::KEY_WOW64_64KEY),
            ("KEY_WOW64_32KEY", reg::KEY_WOW64_32KEY),
            // REG_* value types.
            ("REG_NONE", reg::REG_NONE),
            ("REG_SZ", reg::REG_SZ),
            ("REG_EXPAND_SZ", reg::REG_EXPAND_SZ),
            ("REG_BINARY", reg::REG_BINARY),
            ("REG_DWORD", reg::REG_DWORD),
            ("REG_DWORD_LITTLE_ENDIAN", reg::REG_DWORD_LITTLE_ENDIAN),
            ("REG_DWORD_BIG_ENDIAN", reg::REG_DWORD_BIG_ENDIAN),
            ("REG_LINK", reg::REG_LINK),
            ("REG_MULTI_SZ", reg::REG_MULTI_SZ),
            ("REG_RESOURCE_LIST", reg::REG_RESOURCE_LIST),
            (
                "REG_FULL_RESOURCE_DESCRIPTOR",
                reg::REG_FULL_RESOURCE_DESCRIPTOR,
            ),
            (
                "REG_RESOURCE_REQUIREMENTS_LIST",
                reg::REG_RESOURCE_REQUIREMENTS_LIST,
            ),
            ("REG_QWORD", reg::REG_QWORD),
            ("REG_QWORD_LITTLE_ENDIAN", reg::REG_QWORD_LITTLE_ENDIAN),
            // CreateKeyEx dispositions.
            ("REG_CREATED_NEW_KEY", reg::REG_CREATED_NEW_KEY),
            ("REG_OPENED_EXISTING_KEY", reg::REG_OPENED_EXISTING_KEY),
            // Notify filters.
            ("REG_NOTIFY_CHANGE_NAME", reg::REG_NOTIFY_CHANGE_NAME),
            (
                "REG_NOTIFY_CHANGE_ATTRIBUTES",
                reg::REG_NOTIFY_CHANGE_ATTRIBUTES,
            ),
            (
                "REG_NOTIFY_CHANGE_LAST_SET",
                reg::REG_NOTIFY_CHANGE_LAST_SET,
            ),
            (
                "REG_NOTIFY_CHANGE_SECURITY",
                reg::REG_NOTIFY_CHANGE_SECURITY,
            ),
            ("REG_LEGAL_CHANGE_FILTER", REG_LEGAL_CHANGE_FILTER),
            // Open/create options.
            ("REG_OPTION_RESERVED", reg::REG_OPTION_RESERVED),
            ("REG_OPTION_NON_VOLATILE", reg::REG_OPTION_NON_VOLATILE),
            ("REG_OPTION_VOLATILE", reg::REG_OPTION_VOLATILE),
            ("REG_OPTION_CREATE_LINK", reg::REG_OPTION_CREATE_LINK),
            ("REG_OPTION_BACKUP_RESTORE", reg::REG_OPTION_BACKUP_RESTORE),
            ("REG_OPTION_OPEN_LINK", reg::REG_OPTION_OPEN_LINK),
            ("REG_LEGAL_OPTION", REG_LEGAL_OPTION),
            // Hive-load flags.
            ("REG_NO_LAZY_FLUSH", REG_NO_LAZY_FLUSH),
            ("REG_REFRESH_HIVE", REG_REFRESH_HIVE),
            (
                "REG_WHOLE_HIVE_VOLATILE",
                reg::REG_WHOLE_HIVE_VOLATILE as u32,
            ),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Int(i64::from(v)),
            );
        }

        for (name, f) in [
            ("CloseKey", winreg_close_key as fn(&[Object]) -> _),
            ("ConnectRegistry", winreg_connect_registry),
            ("CreateKey", winreg_create_key),
            ("DeleteKey", winreg_delete_key),
            ("DeleteValue", winreg_delete_value),
            ("DisableReflectionKey", winreg_disable_reflection_key),
            ("EnableReflectionKey", winreg_enable_reflection_key),
            ("QueryReflectionKey", winreg_query_reflection_key),
            ("EnumKey", winreg_enum_key),
            ("EnumValue", winreg_enum_value),
            (
                "ExpandEnvironmentStrings",
                winreg_expand_environment_strings,
            ),
            ("FlushKey", winreg_flush_key),
            ("LoadKey", winreg_load_key),
            ("QueryInfoKey", winreg_query_info_key),
            ("QueryValue", winreg_query_value),
            ("QueryValueEx", winreg_query_value_ex),
            ("SaveKey", winreg_save_key),
            ("SetValue", winreg_set_value),
            ("SetValueEx", winreg_set_value_ex),
        ] {
            d.insert(DictKey(Object::from_static(name)), builtin(name, f));
        }
        // The keyword-accepting quartet (argument clinic exposes
        // `reserved=`/`access=` by name on exactly these four).
        for (name, f) in [
            (
                "CreateKeyEx",
                winreg_create_key_ex as fn(&[Object], &[(String, Object)]) -> _,
            ),
            ("DeleteKeyEx", winreg_delete_key_ex),
            ("OpenKey", winreg_open_key),
            ("OpenKeyEx", winreg_open_key),
        ] {
            d.insert(DictKey(Object::from_static(name)), builtin_kw(name, f));
        }
    }
    Rc::new(PyModule {
        name: "winreg".to_owned(),
        filename: None,
        dict,
    })
}

// ---------------------------------------------------------------------------
// Small shared plumbing
// ---------------------------------------------------------------------------

/// A bound method for the `PyHKEY` type dict (poll-object pattern).
fn method(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Run one registry call with the GIL released — CPython brackets
/// every `Reg*` call in `Py_BEGIN_ALLOW_THREADS` because a remote
/// registry or a hive flush is real blocking I/O.
fn reg_call(f: impl FnOnce() -> u32) -> u32 {
    crate::gil::allow_threads_then(f)
}

/// Raise for a nonzero `Reg*` status. The `LSTATUS` *is* the Win32
/// error (no `GetLastError`), so it feeds the error bridge verbatim —
/// `PyErr_SetFromWindowsErrWithFunction(rc, …)` in CPython.
fn check(rc: u32) -> Result<(), RuntimeError> {
    if rc == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(nt_support::win32_error_to_py(rc as i32, None))
    }
}

/// Integer view of an int-like object, wide enough for both i64 and
/// the unsigned 64-bit face of a `Detach()`ed handle.
fn as_int_i128(o: &Object) -> Option<i128> {
    match o {
        Object::Bool(b) => Some(i128::from(*b)),
        Object::Int(i) => Some(i128::from(*i)),
        Object::Long(b) => b.to_i128(),
        _ => None,
    }
}

/// A required positional argument, with CPython's missing-argument
/// `TypeError` shape.
fn required_arg<'a>(
    args: &'a [Object],
    idx: usize,
    func: &str,
    param: &str,
) -> Result<&'a Object, RuntimeError> {
    args.get(idx).ok_or_else(|| {
        type_error(format!(
            "{func}() missing required argument '{param}' (pos {})",
            idx + 1
        ))
    })
}

/// Resolve a positional-or-keyword parameter (the clinic quartet:
/// `OpenKey`/`OpenKeyEx`/`CreateKeyEx`/`DeleteKeyEx`).
fn arg_or_kw<'a>(
    args: &'a [Object],
    kwargs: &'a [(String, Object)],
    idx: usize,
    name: &str,
) -> Option<&'a Object> {
    args.get(idx)
        .or_else(|| kwargs.iter().find(|(k, _)| k == name).map(|(_, v)| v))
}

/// Argument-clinic `int` conversion for `reserved`/`access`/`index`
/// parameters. Accepts the full unsigned 32-bit range because
/// `access` is the raw `REGSAM` mask (`KEY_WOW64_64KEY | KEY_READ`
/// style compositions are documented usage).
fn u32_arg(o: &Object, func: &str, param: &str) -> Result<u32, RuntimeError> {
    let v = as_int_i128(o).ok_or_else(|| {
        type_error(format!(
            "{func}() argument '{param}' must be int, not {}",
            o.type_name()
        ))
    })?;
    if !(i128::from(i32::MIN)..=i128::from(u32::MAX)).contains(&v) {
        return Err(overflow_error("Python int too large to convert to C int"));
    }
    Ok(v as u32)
}

// ---------------------------------------------------------------------------
// UTF-16 string plumbing
// ---------------------------------------------------------------------------

/// UTF-16 code units (no terminator) for a string object. The WStr
/// arc matters here: registry names/values are raw UTF-16 with no
/// well-formedness guarantee, and CPython round-trips lone surrogates
/// through `PyUnicode_AsWideCharString` untouched.
fn utf16_units(o: &Object) -> Option<Vec<u16>> {
    match o {
        Object::Str(s) => Some(s.encode_utf16().collect()),
        Object::WStr(cps) => {
            let mut out = Vec::with_capacity(cps.len());
            for &cp in cps.iter() {
                if cp < 0x1_0000 {
                    // BMP scalar or lone surrogate: one raw unit.
                    out.push(cp as u16);
                } else {
                    let v = cp - 0x1_0000;
                    out.push((0xD800 + (v >> 10)) as u16);
                    out.push((0xDC00 + (v & 0x3FF)) as u16);
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// Decode raw UTF-16 units to a `str`, pairing surrogates where they
/// pair and keeping lone ones (the WStr arc) — the inverse of
/// [`utf16_units`], so registry round-trips are byte-faithful.
fn str_from_utf16(units: &[u16]) -> Object {
    let mut cps = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        let u = u32::from(units[i]);
        if (0xD800..0xDC00).contains(&u) && i + 1 < units.len() {
            let lo = u32::from(units[i + 1]);
            if (0xDC00..0xE000).contains(&lo) {
                cps.push(0x1_0000 + ((u - 0xD800) << 10) + (lo - 0xDC00));
                i += 2;
                continue;
            }
        }
        cps.push(u);
        i += 1;
    }
    Object::str_from_codepoints(cps)
}

/// Reinterpret a registry data blob as UTF-16 units (little-endian,
/// like `Reg2Py`'s `retDataSize / sizeof(WCHAR)` — a trailing odd
/// byte is dropped).
fn utf16_of_bytes(data: &[u8]) -> Vec<u16> {
    data.chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Serialize UTF-16 units back to the byte layout `RegSetValueExW`
/// expects.
fn bytes_of_utf16(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len() * 2);
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// A required `str` parameter → NUL-terminated UTF-16.
fn wide_arg(o: Option<&Object>, func: &str, param: &str) -> Result<Vec<u16>, RuntimeError> {
    let obj =
        o.ok_or_else(|| type_error(format!("{func}() missing required argument '{param}'")))?;
    match utf16_units(obj) {
        Some(mut units) => {
            units.push(0);
            Ok(units)
        }
        None => Err(type_error(format!(
            "{func}() argument '{param}' must be str, not {}",
            obj.type_name()
        ))),
    }
}

/// A `str | None` parameter → NUL-terminated UTF-16, or `None` for
/// the NULL pointer (CPython's `Py_UNICODE` converter with
/// `accept={str, NoneType}`).
fn wide_arg_opt(
    o: Option<&Object>,
    func: &str,
    param: &str,
) -> Result<Option<Vec<u16>>, RuntimeError> {
    match o {
        None | Some(Object::None) => Ok(None),
        Some(obj) => match utf16_units(obj) {
            Some(mut units) => {
                units.push(0);
                Ok(Some(units))
            }
            None => Err(type_error(format!(
                "{func}() argument '{param}' must be str or None, not {}",
                obj.type_name()
            ))),
        },
    }
}

/// The `PCWSTR` for an optional wide buffer (`None` → NULL).
fn opt_ptr(buf: Option<&Vec<u16>>) -> *const u16 {
    buf.map_or(std::ptr::null(), |v| v.as_ptr())
}

// ---------------------------------------------------------------------------
// The PyHKEY handle type
// ---------------------------------------------------------------------------

thread_local! {
    static HKEY_CLASS: RefCell<Option<Rc<TypeObject>>> = const { RefCell::new(None) };
}

/// The `PyHKEY` type object, built once per thread (poll-object
/// pattern — identity is per-thread but behavior is keyed on the
/// class name, which is what [`hkey_self`]/[`key_from_arg`] check).
fn hkey_type() -> Rc<TypeObject> {
    HKEY_CLASS.with(|slot| {
        if let Some(c) = slot.borrow().as_ref() {
            return c.clone();
        }
        let bt = crate::builtin_types::builtin_types();
        let mut dict = DictData::default();
        for (name, m) in [
            ("Close", method("Close", hkey_close)),
            ("Detach", method("Detach", hkey_detach)),
            ("__enter__", method("__enter__", hkey_enter)),
            ("__exit__", method("__exit__", hkey_exit)),
            // The number protocol: bool(key) is False once closed,
            // int(key)/operator.index(key) yield the raw handle
            // (PyHKEY's nb_bool / nb_int / nb_index slots).
            ("__bool__", method("__bool__", hkey_bool)),
            ("__int__", method("__int__", hkey_int)),
            ("__index__", method("__index__", hkey_int)),
            ("__str__", method("__str__", hkey_str)),
            // Dealloc closes the handle (PyHKEY_deallocFunc) so an
            // un-`with`-ed key doesn't leak past GC.
            ("__del__", method("__del__", hkey_del)),
        ] {
            dict.insert(DictKey(Object::from_static(name)), m);
        }
        // Not directly instantiable — PyHKEY_Type has no tp_new; only
        // the module functions mint handles.
        dict.insert(
            DictKey(Object::from_static("__new__")),
            builtin("__new__", hkey_new_disallowed),
        );
        dict.insert(
            DictKey(Object::from_static("__module__")),
            Object::from_static("winreg"),
        );
        let cls = TypeObject::new_user("PyHKEY", vec![bt.object_.clone()], dict)
            .expect("PyHKEY class must linearise");
        *slot.borrow_mut() = Some(cls.clone());
        cls
    })
}

fn hkey_new_disallowed(_args: &[Object]) -> Result<Object, RuntimeError> {
    Err(type_error("cannot create 'winreg.PyHKEY' instances"))
}

/// Wrap a freshly opened `HKEY` in a `PyHKEY`. The Python-visible
/// `handle` attribute carries the unsigned face of the pointer value
/// (real registry handles are small kernel handles, so this is the
/// value `Detach()`/`int()` must return).
fn new_pyhkey(h: reg::HKEY) -> Object {
    let inst = Rc::new(PyInstance::new(hkey_type()));
    inst.dict.borrow_mut().insert(
        DictKey(Object::from_static("handle")),
        Object::int_from_i128(h as usize as i128),
    );
    Object::Instance(inst)
}

/// The receiver of a `PyHKEY` method.
fn hkey_self(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) if i.cls().name == "PyHKEY" => Ok(i.clone()),
        _ => Err(type_error("descriptor requires a 'winreg.PyHKEY' object")),
    }
}

/// Read the wrapped handle value (0 = closed/detached).
fn peek_handle(inst: &PyInstance) -> i128 {
    inst.dict
        .borrow()
        .get(&DictKey(Object::from_static("handle")))
        .and_then(as_int_i128)
        .unwrap_or(0)
}

/// Read *and neutralize* the wrapped handle — the shared core of
/// `Close`/`Detach`/`CloseKey`/dealloc. Zeroing before any OS call
/// makes every path idempotent (`PyHKEY_Close` sets `hkey = 0`
/// unconditionally).
fn take_handle(inst: &PyInstance) -> i128 {
    let key = DictKey(Object::from_static("handle"));
    let mut d = inst.dict.borrow_mut();
    let h = d.get(&key).and_then(as_int_i128).unwrap_or(0);
    d.insert(key, Object::Int(0));
    h
}

/// `PyHKEY_Close` semantics: neutralize, then `RegCloseKey` if a live
/// handle was held — already-closed is a silent no-op.
fn close_hkey_instance(inst: &PyInstance) -> Result<Object, RuntimeError> {
    let h = take_handle(inst);
    if h != 0 {
        let hk = hkey_from_i128(h);
        check(reg_call(|| unsafe { reg::RegCloseKey(hk) }))?;
    }
    Ok(Object::None)
}

/// `key.Close()` — close the underlying handle; idempotent.
fn hkey_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = hkey_self(args)?;
    close_hkey_instance(&inst)
}

/// `key.Detach()` → int — hand ownership of the raw handle to the
/// caller and neutralize the object (no close happens; the caller is
/// now responsible, typically across a thread or pickle boundary).
fn hkey_detach(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = hkey_self(args)?;
    Ok(Object::int_from_i128(take_handle(&inst)))
}

fn hkey_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = hkey_self(args)?;
    Ok(args[0].clone())
}

/// `__exit__` closes and never suppresses the exception (returns
/// `None`, which is falsy — PyHKEY___exit___impl).
fn hkey_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = hkey_self(args)?;
    close_hkey_instance(&inst)
}

fn hkey_bool(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = hkey_self(args)?;
    Ok(Object::Bool(peek_handle(&inst) != 0))
}

fn hkey_int(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = hkey_self(args)?;
    Ok(Object::int_from_i128(peek_handle(&inst)))
}

fn hkey_str(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = hkey_self(args)?;
    let h = peek_handle(&inst);
    Ok(Object::from_str(format!("<PyHKEY:0x{h:08x}>")))
}

/// GC-time close. Failure is swallowed — CPython's dealloc has no way
/// to raise either.
fn hkey_del(args: &[Object]) -> Result<Object, RuntimeError> {
    if let Ok(inst) = hkey_self(args) {
        let h = take_handle(&inst);
        if h != 0 {
            let _ = unsafe { reg::RegCloseKey(hkey_from_i128(h)) };
        }
    }
    Ok(Object::None)
}

// ---------------------------------------------------------------------------
// key argument conversion (PyHKEY_AsHKEY)
// ---------------------------------------------------------------------------

/// An int handle value → `HKEY`. The predefined-key range
/// (0x8000_0000..=0xFFFF_FFFF as the unsigned 32-bit ints CPython
/// documents) must become the sign-extended pseudo-handles the SDK's
/// `HKEY_*` macros produce — the kernel matches pseudo-handles by
/// exact pointer value on 64-bit. Real handles (small kernel handle
/// values, never in that range) and full 64-bit values from
/// `int(hkey)` pass through bit-faithfully.
fn hkey_from_i128(v: i128) -> reg::HKEY {
    if (0x8000_0000..=0xFFFF_FFFF).contains(&v) {
        (v as u32 as i32) as isize as reg::HKEY
    } else {
        v as usize as reg::HKEY
    }
}

/// `PyHKEY_AsHKEY`: every key parameter accepts a `PyHKEY` *or* a
/// plain int. A closed `PyHKEY` converts to the NULL key (the OS call
/// then fails with `ERROR_INVALID_HANDLE`), matching CPython.
fn key_from_arg(o: Option<&Object>, func: &str) -> Result<reg::HKEY, RuntimeError> {
    let o =
        o.ok_or_else(|| type_error(format!("{func}() missing required argument 'key' (pos 1)")))?;
    if let Object::Instance(i) = o {
        if i.cls().name == "PyHKEY" {
            return Ok(hkey_from_i128(peek_handle(i)));
        }
    }
    match as_int_i128(o) {
        Some(v) => Ok(hkey_from_i128(v)),
        None => Err(type_error("The object is not a PyHKEY object")),
    }
}

// ---------------------------------------------------------------------------
// Value marshalling (Reg2Py / Py2Reg)
// ---------------------------------------------------------------------------

/// `Py2Reg`'s failure message, verbatim.
const CONVERT_ERR: &str = "Could not convert the data to the specified type.";

/// `Reg2Py`: registry data blob + type → Python object.
fn reg_to_py(data: &[u8], typ: u32) -> Object {
    match typ {
        // A DWORD blob shorter than 4 bytes reads as 0 (Reg2Py's
        // size-mismatch fallback) rather than raising — registry data
        // is attacker-adjacent input and CPython chose leniency.
        reg::REG_DWORD => {
            if data.len() >= 4 {
                Object::Int(i64::from(u32::from_le_bytes([
                    data[0], data[1], data[2], data[3],
                ])))
            } else {
                Object::Int(0)
            }
        }
        reg::REG_DWORD_BIG_ENDIAN => {
            if data.len() >= 4 {
                Object::Int(i64::from(u32::from_be_bytes([
                    data[0], data[1], data[2], data[3],
                ])))
            } else {
                Object::Int(0)
            }
        }
        // REG_QWORD_LITTLE_ENDIAN is the same numeric type (11).
        reg::REG_QWORD => {
            if data.len() >= 8 {
                let mut b = [0u8; 8];
                b.copy_from_slice(&data[..8]);
                Object::int_from_i128(i128::from(u64::from_le_bytes(b)))
            } else {
                Object::Int(0)
            }
        }
        // "REG_SZ should be a NUL terminated string, but only by
        // convention" (winreg.c) — consume up to the first NUL to
        // match reg.exe/regedit.exe on malformed data; well-formed
        // data just loses its single terminator.
        reg::REG_SZ | reg::REG_EXPAND_SZ => {
            let units = utf16_of_bytes(data);
            let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            str_from_utf16(&units[..len])
        }
        // A double-NUL-terminated block of NUL-terminated strings; an
        // empty string terminates the list early and a missing final
        // terminator is tolerated (fixupMultiSZ).
        reg::REG_MULTI_SZ => {
            let units = utf16_of_bytes(data);
            let mut items = Vec::new();
            let mut start = 0usize;
            while start < units.len() && units[start] != 0 {
                let end = units[start..]
                    .iter()
                    .position(|&u| u == 0)
                    .map_or(units.len(), |p| start + p);
                items.push(str_from_utf16(&units[start..end]));
                start = end + 1;
            }
            Object::new_list(items)
        }
        // REG_BINARY — and every type this module doesn't understand
        // — surfaces as bytes, or None when empty ("all unknown data
        // types" comment in Reg2Py).
        _ => {
            if data.is_empty() {
                Object::None
            } else {
                Object::Bytes(Rc::from(data))
            }
        }
    }
}

/// `PyLong_AsUnsignedLong` shape for REG_DWORD data: negative raises
/// OverflowError (not the generic conversion ValueError), matching
/// how Py2Reg lets the pending overflow propagate.
fn u32_data_value(value: &Object) -> Result<u32, RuntimeError> {
    let v = as_int_i128(value).ok_or_else(|| value_error(CONVERT_ERR))?;
    if v < 0 {
        return Err(overflow_error(
            "can't convert negative value to unsigned int",
        ));
    }
    u32::try_from(v)
        .map_err(|_| overflow_error("Python int too large to convert to C unsigned long"))
}

/// `PyLong_AsUnsignedLongLong` shape for REG_QWORD data.
fn u64_data_value(value: &Object) -> Result<u64, RuntimeError> {
    let v = as_int_i128(value).ok_or_else(|| value_error(CONVERT_ERR))?;
    if v < 0 {
        return Err(overflow_error(
            "can't convert negative value to unsigned int",
        ));
    }
    u64::try_from(v)
        .map_err(|_| overflow_error("Python int too large to convert to C unsigned long long"))
}

/// `Py2Reg`: Python object + declared type → registry data blob.
fn py_to_reg(value: &Object, typ: u32) -> Result<Vec<u8>, RuntimeError> {
    match typ {
        reg::REG_DWORD | reg::REG_DWORD_BIG_ENDIAN => {
            // None stores 0 (Py2Reg's REG_DWORD None branch).
            let v = match value {
                Object::None => 0,
                _ => u32_data_value(value)?,
            };
            Ok(if typ == reg::REG_DWORD_BIG_ENDIAN {
                v.to_be_bytes().to_vec()
            } else {
                v.to_le_bytes().to_vec()
            })
        }
        reg::REG_QWORD => {
            let v = match value {
                Object::None => 0,
                _ => u64_data_value(value)?,
            };
            Ok(v.to_le_bytes().to_vec())
        }
        // Stored with the trailing NUL (`len + 1` wide chars in
        // Py2Reg); None stores the empty string.
        reg::REG_SZ | reg::REG_EXPAND_SZ => {
            let mut units = match value {
                Object::None => Vec::new(),
                _ => utf16_units(value).ok_or_else(|| value_error(CONVERT_ERR))?,
            };
            units.push(0);
            Ok(bytes_of_utf16(&units))
        }
        // A list (exactly — Py2Reg PyList_Checks) of str, each
        // NUL-terminated, with the block's extra terminator.
        reg::REG_MULTI_SZ => {
            let Object::List(list) = value else {
                return Err(value_error(CONVERT_ERR));
            };
            let mut units: Vec<u16> = Vec::new();
            for item in list.borrow().iter() {
                let s = utf16_units(item).ok_or_else(|| value_error(CONVERT_ERR))?;
                units.extend_from_slice(&s);
                units.push(0);
            }
            units.push(0);
            Ok(bytes_of_utf16(&units))
        }
        // REG_BINARY and all unknown types: any buffer-protocol
        // object; None stores no data at all (NULL/0 in Py2Reg,
        // which is how REG_NONE values are written).
        _ => match value {
            Object::None => Ok(Vec::new()),
            Object::Bytes(b) => Ok(b.to_vec()),
            Object::ByteArray(b) => Ok(b.borrow().clone()),
            Object::MemoryView(mv) => Ok(mv.to_bytes()),
            _ => Err(value_error(CONVERT_ERR)),
        },
    }
}

// ---------------------------------------------------------------------------
// Module functions
// ---------------------------------------------------------------------------

/// `CloseKey(hkey)` — closes an int handle directly, or neutralizes a
/// `PyHKEY` exactly like its `Close()` method (so the object tests
/// False afterwards).
fn winreg_close_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let obj = required_arg(args, 0, "CloseKey", "hkey")?;
    if let Object::Instance(i) = obj {
        if i.cls().name == "PyHKEY" {
            return close_hkey_instance(i);
        }
    }
    let h = key_from_arg(Some(obj), "CloseKey")?;
    check(reg_call(|| unsafe { reg::RegCloseKey(h) }))?;
    Ok(Object::None)
}

/// `ConnectRegistry(computer_name, key)` → `PyHKEY` — `None` connects
/// to the local machine (the NULL machine name).
fn winreg_connect_registry(args: &[Object]) -> Result<Object, RuntimeError> {
    let name_obj = required_arg(args, 0, "ConnectRegistry", "computer_name")?;
    let name = wide_arg_opt(Some(name_obj), "ConnectRegistry", "computer_name")?;
    let key = key_from_arg(args.get(1), "ConnectRegistry")?;
    let name_ptr = opt_ptr(name.as_ref());
    let mut out: reg::HKEY = std::ptr::null_mut();
    let rc = reg_call(|| unsafe { reg::RegConnectRegistryW(name_ptr, key, &raw mut out) });
    check(rc)?;
    Ok(new_pyhkey(out))
}

/// `CreateKey(key, sub_key)` → `PyHKEY`. CPython uses the legacy
/// `RegCreateKeyW`; the `RegCreateKeyExW` spelling with
/// `MAXIMUM_ALLOWED` is its documented equivalent (and `None`/empty
/// `sub_key` re-opens `key` itself, which pip's `pep514` probing
/// relies on).
fn winreg_create_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "CreateKey")?;
    let sub = wide_arg_opt(args.get(1), "CreateKey", "sub_key")?.unwrap_or_else(|| vec![0]);
    let mut out: reg::HKEY = std::ptr::null_mut();
    let rc = reg_call(|| unsafe {
        reg::RegCreateKeyExW(
            key,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            reg::REG_OPTION_NON_VOLATILE,
            MAXIMUM_ALLOWED,
            std::ptr::null(),
            &raw mut out,
            std::ptr::null_mut(),
        )
    });
    check(rc)?;
    Ok(new_pyhkey(out))
}

/// `CreateKeyEx(key, sub_key, reserved=0, access=KEY_WRITE)` →
/// `PyHKEY`.
fn winreg_create_key_ex(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let key = key_from_arg(arg_or_kw(args, kwargs, 0, "key"), "CreateKeyEx")?;
    let sub = wide_arg_opt(
        arg_or_kw(args, kwargs, 1, "sub_key"),
        "CreateKeyEx",
        "sub_key",
    )?
    .unwrap_or_else(|| vec![0]);
    let reserved = match arg_or_kw(args, kwargs, 2, "reserved") {
        Some(o) => u32_arg(o, "CreateKeyEx", "reserved")?,
        None => 0,
    };
    let access = match arg_or_kw(args, kwargs, 3, "access") {
        Some(o) => u32_arg(o, "CreateKeyEx", "access")?,
        None => reg::KEY_WRITE,
    };
    let mut out: reg::HKEY = std::ptr::null_mut();
    let rc = reg_call(|| unsafe {
        reg::RegCreateKeyExW(
            key,
            sub.as_ptr(),
            reserved,
            std::ptr::null(),
            reg::REG_OPTION_NON_VOLATILE,
            access,
            std::ptr::null(),
            &raw mut out,
            std::ptr::null_mut(),
        )
    });
    check(rc)?;
    Ok(new_pyhkey(out))
}

/// `DeleteKey(key, sub_key)` — the subkey must have no children
/// (the API refuses recursive deletes; `shutil`-style recursion is a
/// Python-level affair).
fn winreg_delete_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "DeleteKey")?;
    let sub = wide_arg(args.get(1), "DeleteKey", "sub_key")?;
    check(reg_call(|| unsafe {
        reg::RegDeleteKeyW(key, sub.as_ptr())
    }))?;
    Ok(Object::None)
}

/// `DeleteKeyEx(key, sub_key, access=KEY_WOW64_64KEY, reserved=0)` —
/// the WOW64-aware delete (CPython loads `RegDeleteKeyExW`
/// dynamically for pre-Vista compatibility; we link it directly).
fn winreg_delete_key_ex(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let key = key_from_arg(arg_or_kw(args, kwargs, 0, "key"), "DeleteKeyEx")?;
    let sub = wide_arg(
        arg_or_kw(args, kwargs, 1, "sub_key"),
        "DeleteKeyEx",
        "sub_key",
    )?;
    let access = match arg_or_kw(args, kwargs, 2, "access") {
        Some(o) => u32_arg(o, "DeleteKeyEx", "access")?,
        None => reg::KEY_WOW64_64KEY,
    };
    let reserved = match arg_or_kw(args, kwargs, 3, "reserved") {
        Some(o) => u32_arg(o, "DeleteKeyEx", "reserved")?,
        None => 0,
    };
    let rc = reg_call(|| unsafe { reg::RegDeleteKeyExW(key, sub.as_ptr(), access, reserved) });
    check(rc)?;
    Ok(Object::None)
}

/// `DeleteValue(key, value)` — `None` deletes the key's default
/// value.
fn winreg_delete_value(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "DeleteValue")?;
    let value = wide_arg_opt(args.get(1), "DeleteValue", "value")?;
    let value_ptr = opt_ptr(value.as_ref());
    check(reg_call(|| unsafe { reg::RegDeleteValueW(key, value_ptr) }))?;
    Ok(Object::None)
}

fn winreg_disable_reflection_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "DisableReflectionKey")?;
    check(reg_call(|| unsafe { reg::RegDisableReflectionKey(key) }))?;
    Ok(Object::None)
}

fn winreg_enable_reflection_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "EnableReflectionKey")?;
    check(reg_call(|| unsafe { reg::RegEnableReflectionKey(key) }))?;
    Ok(Object::None)
}

/// `QueryReflectionKey(key)` → bool — True when reflection is
/// *disabled* (the API's out-parameter polarity, kept as-is like
/// CPython).
fn winreg_query_reflection_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "QueryReflectionKey")?;
    let mut disabled = 0i32;
    let rc = reg_call(|| unsafe { reg::RegQueryReflectionKey(key, &raw mut disabled) });
    check(rc)?;
    Ok(Object::Bool(disabled != 0))
}

/// `EnumKey(key, index)` → str. The 257-wide buffer is winreg.c's:
/// key names cap at 255 UCS-2 chars, +1 terminator, +1 for paranoia.
fn winreg_enum_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "EnumKey")?;
    let index = u32_arg(
        required_arg(args, 1, "EnumKey", "index")?,
        "EnumKey",
        "index",
    )?;
    let mut buf = [0u16; 257];
    let mut len = buf.len() as u32;
    let ptr = buf.as_mut_ptr();
    let rc = reg_call(|| unsafe {
        reg::RegEnumKeyExW(
            key,
            index,
            ptr,
            &raw mut len,
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    });
    // Enumeration past the end surfaces as OSError from
    // ERROR_NO_MORE_ITEMS — callers loop until it (CPython does the
    // same; there is no sentinel return).
    check(rc)?;
    Ok(str_from_utf16(&buf[..len as usize]))
}

/// `EnumValue(key, index)` → `(name, value, type)`. Buffer sizes come
/// from `RegQueryInfoKeyW` (max name/data across the key), and the
/// data buffer doubles on `ERROR_MORE_DATA` — another writer can grow
/// a value between the two calls (winreg.c's retry loop).
fn winreg_enum_value(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "EnumValue")?;
    let index = u32_arg(
        required_arg(args, 1, "EnumValue", "index")?,
        "EnumValue",
        "index",
    )?;
    let mut max_name = 0u32;
    let mut max_data = 0u32;
    let rc = reg_call(|| unsafe {
        reg::RegQueryInfoKeyW(
            key,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut max_name,
            &raw mut max_data,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    });
    check(rc)?;
    // +1 for the terminators the counts exclude.
    let mut name_buf = vec![0u16; max_name as usize + 1];
    let mut data_buf = vec![0u8; max_data as usize + 1];
    loop {
        let mut name_len = name_buf.len() as u32;
        let mut data_len = data_buf.len() as u32;
        let mut typ = 0u32;
        let name_ptr = name_buf.as_mut_ptr();
        let data_ptr = data_buf.as_mut_ptr();
        let rc = reg_call(|| unsafe {
            reg::RegEnumValueW(
                key,
                index,
                name_ptr,
                &raw mut name_len,
                std::ptr::null(),
                &raw mut typ,
                data_ptr,
                &raw mut data_len,
            )
        });
        if rc == ERROR_MORE_DATA {
            let grown = data_buf.len() * 2;
            data_buf.resize(grown, 0);
            continue;
        }
        check(rc)?;
        let name = str_from_utf16(&name_buf[..name_len as usize]);
        let value = reg_to_py(&data_buf[..data_len as usize], typ);
        return Ok(Object::new_tuple(vec![
            name,
            value,
            Object::Int(i64::from(typ)),
        ]));
    }
}

/// `ExpandEnvironmentStrings(string)` → str — `%NAME%` expansion via
/// the same API `REG_EXPAND_SZ` consumers use.
fn winreg_expand_environment_strings(args: &[Object]) -> Result<Object, RuntimeError> {
    let src = wide_arg(args.first(), "ExpandEnvironmentStrings", "string")?;
    // Size query first (returns the required buffer length in wide
    // chars, including the terminator); 0 is failure.
    let needed = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    let mut buf = vec![0u16; needed as usize];
    let src_ptr = src.as_ptr();
    let dst_ptr = buf.as_mut_ptr();
    let written = reg_call(|| unsafe { ExpandEnvironmentStringsW(src_ptr, dst_ptr, needed) });
    if written == 0 {
        return Err(nt_support::last_win32_error_to_py(None));
    }
    let len = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
    Ok(str_from_utf16(&buf[..len]))
}

/// `FlushKey(key)` — the synchronous hive flush ("Registry equivalent
/// of a commit", per the CPython docstring; rarely needed).
fn winreg_flush_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "FlushKey")?;
    check(reg_call(|| unsafe { reg::RegFlushKey(key) }))?;
    Ok(Object::None)
}

/// `LoadKey(key, sub_key, file_name)` — mount a saved hive under
/// `key\sub_key` (needs SeRestorePrivilege; the API reports the
/// failure when it's missing, so no privilege pre-check here).
fn winreg_load_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "LoadKey")?;
    let sub = wide_arg(args.get(1), "LoadKey", "sub_key")?;
    let file = wide_arg(args.get(2), "LoadKey", "file_name")?;
    let rc = reg_call(|| unsafe { reg::RegLoadKeyW(key, sub.as_ptr(), file.as_ptr()) });
    check(rc)?;
    Ok(Object::None)
}

/// `OpenKey(key, sub_key, reserved=0, access=KEY_READ)` → `PyHKEY`.
/// `OpenKeyEx` is registered as the same function (CPython aliases
/// the two implementations).
fn winreg_open_key(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(arg_or_kw(args, kwargs, 0, "key"), "OpenKey")?;
    let sub = wide_arg_opt(arg_or_kw(args, kwargs, 1, "sub_key"), "OpenKey", "sub_key")?;
    let reserved = match arg_or_kw(args, kwargs, 2, "reserved") {
        Some(o) => u32_arg(o, "OpenKey", "reserved")?,
        None => 0,
    };
    let access = match arg_or_kw(args, kwargs, 3, "access") {
        Some(o) => u32_arg(o, "OpenKey", "access")?,
        None => reg::KEY_READ,
    };
    let sub_ptr = opt_ptr(sub.as_ref());
    let mut out: reg::HKEY = std::ptr::null_mut();
    let rc =
        reg_call(|| unsafe { reg::RegOpenKeyExW(key, sub_ptr, reserved, access, &raw mut out) });
    check(rc)?;
    Ok(new_pyhkey(out))
}

/// `QueryInfoKey(key)` → `(num_subkeys, num_values, last_modified)`,
/// the timestamp being the raw FILETIME quadword — 100-nanosecond
/// intervals since Jan 1, 1601 (winreg.c packs the two halves into a
/// LARGE_INTEGER and returns QuadPart).
fn winreg_query_info_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "QueryInfoKey")?;
    let mut nsubkeys = 0u32;
    let mut nvalues = 0u32;
    let mut ft = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let rc = reg_call(|| unsafe {
        reg::RegQueryInfoKeyW(
            key,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
            &raw mut nsubkeys,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut nvalues,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut ft,
        )
    });
    check(rc)?;
    let quad = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    Ok(Object::new_tuple(vec![
        Object::Int(i64::from(nsubkeys)),
        Object::Int(i64::from(nvalues)),
        Object::int_from_i128(i128::from(quad)),
    ]))
}

/// The default (unnamed) value of `key`, REG_SZ-only — the shared
/// tail of `QueryValue` after any subkey open.
fn query_default_value(key: reg::HKEY) -> Result<Object, RuntimeError> {
    let mut typ = 0u32;
    let mut buf = vec![0u8; 512];
    let size = loop {
        let mut size = buf.len() as u32;
        let data_ptr = buf.as_mut_ptr();
        let rc = reg_call(|| unsafe {
            reg::RegQueryValueExW(
                key,
                std::ptr::null(),
                std::ptr::null(),
                &raw mut typ,
                data_ptr,
                &raw mut size,
            )
        });
        if rc == ERROR_MORE_DATA {
            let need = (size as usize).max(buf.len() * 2);
            buf.resize(need, 0);
            continue;
        }
        // "FILE_NOT_FOUND means that the value is undefined, not that
        // the key doesn't exist" (winreg.c) — an unset default value
        // reads as the empty string.
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(Object::from_static(""));
        }
        check(rc)?;
        break size;
    };
    if typ != reg::REG_SZ {
        // Non-string default values are a hard error, reported as the
        // Win32 ERROR_INVALID_DATA like CPython.
        return Err(nt_support::win32_error_to_py(
            ERROR_INVALID_DATA as i32,
            None,
        ));
    }
    Ok(reg_to_py(&buf[..size as usize], reg::REG_SZ))
}

/// `QueryValue(key, sub_key)` → str — the legacy default-value read.
/// A non-empty `sub_key` is opened with `KEY_QUERY_VALUE` first (and
/// always closed again, even on error), per the 3.13 rewrite of
/// winreg_QueryValue_impl.
fn winreg_query_value(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "QueryValue")?;
    let sub = wide_arg_opt(args.get(1), "QueryValue", "sub_key")?;
    let mut child = key;
    let mut opened = false;
    if let Some(s) = &sub {
        // len > 1: the buffer holds more than the terminator.
        if s.len() > 1 {
            let sub_ptr = s.as_ptr();
            let rc = reg_call(|| unsafe {
                reg::RegOpenKeyExW(key, sub_ptr, 0, reg::KEY_QUERY_VALUE, &raw mut child)
            });
            check(rc)?;
            opened = true;
        }
    }
    let result = query_default_value(child);
    if opened {
        // Close failure is swallowed — the read already succeeded or
        // failed on its own terms (CPython ignores this rc too).
        let _ = unsafe { reg::RegCloseKey(child) };
    }
    result
}

/// `QueryValueEx(key, name)` → `(value, type)`. Size probe first,
/// then the `ERROR_MORE_DATA` doubling loop (a concurrent writer can
/// grow the value between the probe and the read).
fn winreg_query_value_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "QueryValueEx")?;
    let name = wide_arg_opt(args.get(1), "QueryValueEx", "name")?;
    let name_ptr = opt_ptr(name.as_ref());
    let mut buf_size = 0u32;
    let rc = reg_call(|| unsafe {
        reg::RegQueryValueExW(
            key,
            name_ptr,
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut buf_size,
        )
    });
    if rc == ERROR_MORE_DATA {
        buf_size = 256;
    } else {
        check(rc)?;
    }
    let mut buf = vec![0u8; buf_size as usize];
    let mut typ = 0u32;
    let size = loop {
        let mut size = buf.len() as u32;
        let data_ptr = buf.as_mut_ptr();
        let rc = reg_call(|| unsafe {
            reg::RegQueryValueExW(
                key,
                name_ptr,
                std::ptr::null(),
                &raw mut typ,
                data_ptr,
                &raw mut size,
            )
        });
        if rc == ERROR_MORE_DATA {
            let grown = (buf.len() * 2).max(256);
            buf.resize(grown, 0);
            continue;
        }
        check(rc)?;
        break size;
    };
    let value = reg_to_py(&buf[..size as usize], typ);
    Ok(Object::new_tuple(vec![value, Object::Int(i64::from(typ))]))
}

/// `SaveKey(key, file_name)` — write the subtree to a hive file
/// (needs SeBackupPrivilege; NULL security attributes make the file
/// inherit-default, like CPython's `pSA = NULL`).
fn winreg_save_key(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "SaveKey")?;
    let file = wide_arg(args.get(1), "SaveKey", "file_name")?;
    let rc = reg_call(|| unsafe { reg::RegSaveKeyW(key, file.as_ptr(), std::ptr::null()) });
    check(rc)?;
    Ok(Object::None)
}

/// `SetValue(key, sub_key, type, value)` — the legacy default-value
/// write. `type` must be REG_SZ (a `TypeError`, not `ValueError` —
/// winreg.c checks it before touching the API), and a non-empty
/// `sub_key` is created/opened with `KEY_SET_VALUE` first, per the
/// 3.13 rewrite of winreg_SetValue_impl.
fn winreg_set_value(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "SetValue")?;
    let sub = wide_arg_opt(args.get(1), "SetValue", "sub_key")?;
    let typ = u32_arg(
        required_arg(args, 2, "SetValue", "type")?,
        "SetValue",
        "type",
    )?;
    if typ != reg::REG_SZ {
        return Err(type_error("type must be winreg.REG_SZ"));
    }
    let value = wide_arg(args.get(3), "SetValue", "value")?; // NUL-terminated
    let mut child = key;
    let mut opened = false;
    if let Some(s) = &sub {
        if s.len() > 1 {
            let sub_ptr = s.as_ptr();
            let rc = reg_call(|| unsafe {
                reg::RegCreateKeyExW(
                    key,
                    sub_ptr,
                    0,
                    std::ptr::null(),
                    reg::REG_OPTION_NON_VOLATILE,
                    reg::KEY_SET_VALUE,
                    std::ptr::null(),
                    &raw mut child,
                    std::ptr::null_mut(),
                )
            });
            check(rc)?;
            opened = true;
        }
    }
    // The stored data includes the terminator (wcslen + 1 in
    // winreg.c), hence the full buffer length here.
    let byte_len = (value.len() * 2) as u32;
    let value_ptr = value.as_ptr().cast::<u8>();
    let rc = reg_call(|| unsafe {
        reg::RegSetValueExW(child, std::ptr::null(), 0, reg::REG_SZ, value_ptr, byte_len)
    });
    if opened {
        let _ = unsafe { reg::RegCloseKey(child) };
    }
    check(rc)?;
    Ok(Object::None)
}

/// `SetValueEx(key, value_name, reserved, type, value)` — the typed
/// value write. `reserved` is accepted and ignored (CPython's clinic
/// takes it as an arbitrary object and passes 0 to the API).
fn winreg_set_value_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let key = key_from_arg(args.first(), "SetValueEx")?;
    let name = wide_arg_opt(args.get(1), "SetValueEx", "value_name")?;
    let _reserved = required_arg(args, 2, "SetValueEx", "reserved")?;
    let typ = u32_arg(
        required_arg(args, 3, "SetValueEx", "type")?,
        "SetValueEx",
        "type",
    )?;
    let value = required_arg(args, 4, "SetValueEx", "value")?;
    let data = py_to_reg(value, typ)?;
    let name_ptr = opt_ptr(name.as_ref());
    // NULL data pointer for empty payloads — a dangling Vec pointer
    // would be technically non-null-but-invalid, and the API accepts
    // NULL with cbData 0 (how REG_NONE/empty REG_BINARY are written).
    let data_ptr = if data.is_empty() {
        std::ptr::null()
    } else {
        data.as_ptr()
    };
    let data_len = data.len() as u32;
    let rc = reg_call(|| unsafe { reg::RegSetValueExW(key, name_ptr, 0, typ, data_ptr, data_len) });
    check(rc)?;
    Ok(Object::None)
}
