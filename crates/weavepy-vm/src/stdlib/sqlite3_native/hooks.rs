//! User-defined functions, aggregates, window functions, collations,
//! and the authorizer/progress/trace hooks — the re-entry surface where
//! SQLite calls back into Python mid-`sqlite3_step`.
//!
//! Exception policy (CPython's): a Python error inside a callback makes
//! the enclosing SQL statement fail with `OperationalError("user-defined
//! function raised exception")` (functions) or is swallowed
//! (authorizer denies, progress interrupts). The *original* exception is
//! reported through the unraisable machinery only when
//! `sqlite3.enable_callback_tracebacks(True)` is on — we print it to
//! stderr like CPython's default unraisable hook would.

#![allow(unsafe_op_in_unsafe_fn)]

use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::ffi;

use super::{
    call, checked_conn, interp, is_callable, kwarg, operational_error_class,
    programming_error_class, raise, raise_sqlite_error,
};
use crate::error::{type_error, value_error, RuntimeError};
use crate::object::Object;

/// Clinic-style deprecation for keyword arguments that become
/// positional-only in Python 3.15 (gh-107948). Fires when any of the
/// listed keywords is present.
fn warn_kwargs_deprecated(
    kwargs: &[(String, Object)],
    names: &[&str],
    message: &str,
) -> Result<(), RuntimeError> {
    if kwargs.iter().any(|(k, _)| names.contains(&k.as_str())) {
        interp()?.warn_deprecation_from_builtin(message.to_owned())?;
    }
    Ok(())
}

static CALLBACK_TRACEBACKS: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_callback_tracebacks(enabled: bool) {
    CALLBACK_TRACEBACKS.store(enabled, Ordering::Relaxed);
}

fn callback_tracebacks() -> bool {
    CALLBACK_TRACEBACKS.load(Ordering::Relaxed)
}

/// CPython's `print_or_clear_traceback`: a callback exception is either
/// routed through the unraisable machinery (when
/// `enable_callback_tracebacks(True)`) — which honours a user-installed
/// `sys.unraisablehook`, as test_sqlite3's callback tests require — or
/// silently cleared. It never propagates into the enclosing statement
/// beyond the generic sqlite error the callback registers.
fn print_or_clear(e: RuntimeError, callable: &Object) {
    if !callback_tracebacks() {
        return;
    }
    if let Ok(ip) = interp() {
        let ctx = callable.repr();
        ip.write_unraisable_msg(&e, callable, &ctx, None);
    }
}

/// Does the error match the named builtin exception class?
fn exc_matches(e: &RuntimeError, name: &str) -> bool {
    if let RuntimeError::PyException(exc) = e {
        if let Object::Instance(inst) = &exc.instance {
            return crate::builtin_types::builtin_types()
                .by_name(name)
                .is_some_and(|cls| inst.cls().is_subclass_of(&cls));
        }
    }
    false
}

/// Register the callback failure on the sqlite context the way CPython's
/// `set_sqlite_error` does: OverflowError → TOOBIG (surfaces as
/// `DataError("string or blob too big")`), MemoryError → NOMEM,
/// everything else → the generic message.
unsafe fn result_error(ctx: *mut ffi::sqlite3_context, e: &RuntimeError, fallback: &str) {
    if exc_matches(e, "OverflowError") {
        ffi::sqlite3_result_error_toobig(ctx);
    } else if exc_matches(e, "MemoryError") {
        ffi::sqlite3_result_error_nomem(ctx);
    } else {
        let msg = std::ffi::CString::new(fallback).expect("static callback error message");
        ffi::sqlite3_result_error(ctx, msg.as_ptr(), -1);
    }
}

// ---------------------------------------------------------------
// Value marshalling for callbacks
// ---------------------------------------------------------------

/// Convert `sqlite3_value` arguments into Python objects
/// (`_pysqlite_build_py_params`).
unsafe fn build_py_params(argc: c_int, argv: *mut *mut ffi::sqlite3_value) -> Vec<Object> {
    let mut out = Vec::with_capacity(argc as usize);
    for i in 0..argc {
        let v = *argv.add(i as usize);
        let obj = match ffi::sqlite3_value_type(v) {
            ffi::SQLITE_NULL => Object::None,
            ffi::SQLITE_INTEGER => Object::Int(ffi::sqlite3_value_int64(v)),
            ffi::SQLITE_FLOAT => Object::Float(ffi::sqlite3_value_double(v)),
            ffi::SQLITE_BLOB => {
                let p = ffi::sqlite3_value_blob(v).cast::<u8>();
                let n = ffi::sqlite3_value_bytes(v) as usize;
                if p.is_null() || n == 0 {
                    Object::new_bytes(Vec::new())
                } else {
                    Object::new_bytes(std::slice::from_raw_parts(p, n).to_vec())
                }
            }
            _ => {
                let p = ffi::sqlite3_value_text(v).cast::<u8>();
                let n = ffi::sqlite3_value_bytes(v) as usize;
                let bytes = if p.is_null() || n == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(p, n).to_vec()
                };
                Object::from_str(String::from_utf8_lossy(&bytes).into_owned())
            }
        };
        out.push(obj);
    }
    out
}

/// Write a Python result into the sqlite context
/// (`_pysqlite_set_result`).
unsafe fn set_result(ctx: *mut ffi::sqlite3_context, value: &Object) -> Result<(), RuntimeError> {
    match value {
        Object::None => ffi::sqlite3_result_null(ctx),
        Object::Bool(b) => ffi::sqlite3_result_int64(ctx, i64::from(*b)),
        Object::Int(i) => ffi::sqlite3_result_int64(ctx, *i),
        Object::Long(b) => match num_traits::ToPrimitive::to_i64(b.as_ref()) {
            Some(i) => ffi::sqlite3_result_int64(ctx, i),
            None => {
                return Err(crate::error::overflow_error(
                    "Python int too large to convert to SQLite INTEGER",
                ))
            }
        },
        Object::Float(f) => ffi::sqlite3_result_double(ctx, *f),
        other => {
            // Strings (incl. WStr — lone surrogates raise
            // UnicodeEncodeError like PyUnicode_AsUTF8) come first, then
            // the buffer protocol, then CPython's unsupported-type error.
            if let Some(text) = super::as_text(other) {
                let text = text?;
                ffi::sqlite3_result_text64(
                    ctx,
                    text.as_ptr().cast::<c_char>(),
                    text.len() as u64,
                    transient(),
                    ffi::SQLITE_UTF8 as u8,
                );
                return Ok(());
            }
            match super::buffer_bytes(other) {
                Some(bytes) => {
                    let bytes = bytes?;
                    ffi::sqlite3_result_blob64(
                        ctx,
                        bytes.as_ptr().cast::<c_void>(),
                        bytes.len() as u64,
                        transient(),
                    );
                }
                None => {
                    return Err(type_error(format!(
                        "user-defined function returned unsupported type '{}'",
                        other.type_name_owned()
                    )))
                }
            }
        }
    }
    Ok(())
}

fn transient() -> ffi::sqlite3_destructor_type {
    // SAFETY: the documented SQLITE_TRANSIENT sentinel ((void(*)(void*))-1).
    Some(unsafe { std::mem::transmute::<isize, unsafe extern "C" fn(*mut c_void)>(-1isize) })
}

// ---------------------------------------------------------------
// Scalar functions
// ---------------------------------------------------------------

/// Boxed payload handed to sqlite as pApp for functions/aggregates.
struct FuncData {
    callable: Object,
}

unsafe extern "C" fn func_destroy(p: *mut c_void) {
    if !p.is_null() {
        drop(Box::from_raw(p.cast::<FuncData>()));
    }
}

unsafe extern "C" fn scalar_func_cb(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let data = &*(ffi::sqlite3_user_data(ctx) as *const FuncData);
    let args = build_py_params(argc, argv);
    let result = interp().and_then(|ip| call(ip, &data.callable, &args));
    match result.and_then(|v| set_result(ctx, &v)) {
        Ok(()) => {}
        Err(e) => {
            result_error(ctx, &e, "user-defined function raised exception");
            print_or_clear(e, &data.callable);
        }
    }
}

pub(crate) fn conn_create_function(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    // `deterministic` is keyword-only (`create_function(name, narg,
    // func, /, *, deterministic=False)`).
    if args.len() > 4 {
        return Err(type_error(format!(
            "create_function() takes at most 3 positional arguments ({} given)",
            args.len() - 1
        )));
    }
    warn_kwargs_deprecated(
        kwargs,
        &["name", "narg", "func"],
        "Passing keyword arguments 'name', 'narg' and 'func' to \
         _sqlite3.Connection.create_function() is deprecated. Parameters 'name', 'narg' \
         and 'func' will become positional-only in Python 3.15.",
    )?;
    let name = match args
        .get(1)
        .or_else(|| kwarg(kwargs, "name"))
        .and_then(super::as_text)
    {
        Some(text) => text?,
        None => return Err(type_error("create_function() argument 'name' must be str")),
    };
    let narg = args
        .get(2)
        .or_else(|| kwarg(kwargs, "narg"))
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("create_function() argument 'narg' must be int"))?;
    let func = args
        .get(3)
        .or_else(|| kwarg(kwargs, "func"))
        .cloned()
        .ok_or_else(|| type_error("create_function() missing argument 'func'"))?;
    let deterministic = kwarg(kwargs, "deterministic")
        .map(|o| !matches!(o, Object::Bool(false) | Object::Int(0)))
        .unwrap_or(false);

    let db = state.borrow().db_ptr();
    let c_name =
        std::ffi::CString::new(name.clone()).map_err(|_| value_error("embedded null byte"))?;
    let mut flags = ffi::SQLITE_UTF8;
    if deterministic {
        flags |= ffi::SQLITE_DETERMINISTIC;
    }
    if matches!(func, Object::None) {
        // Remove the function.
        // SAFETY: live db handle; NULL callbacks drop the registration.
        let rc = unsafe {
            ffi::sqlite3_create_function_v2(
                db,
                c_name.as_ptr(),
                narg as c_int,
                flags,
                std::ptr::null_mut(),
                None,
                None,
                None,
                None,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(raise_sqlite_error(db, rc));
        }
        return Ok(Object::None);
    }
    let data = Box::into_raw(Box::new(FuncData {
        callable: func.clone(),
    }));
    // SAFETY: live db handle; `data` ownership passes to sqlite, released
    // via func_destroy when the function is redefined or the db closes.
    let rc = unsafe {
        ffi::sqlite3_create_function_v2(
            db,
            c_name.as_ptr(),
            narg as c_int,
            flags,
            data.cast::<c_void>(),
            Some(scalar_func_cb),
            None,
            None,
            Some(func_destroy),
        )
    };
    if rc != ffi::SQLITE_OK {
        return Err(raise(operational_error_class(), "Error creating function"));
    }
    state.borrow_mut().hook_refs.push(func);
    Ok(Object::None)
}

// ---------------------------------------------------------------
// Aggregates + window functions
// ---------------------------------------------------------------

/// Per-invocation aggregate context: a pointer slot allocated by
/// sqlite; stores a leaked `Box<Object>` holding the instance.
unsafe fn aggregate_instance(
    ctx: *mut ffi::sqlite3_context,
    data: &FuncData,
    create: bool,
) -> Result<Option<*mut Object>, ()> {
    let slot = ffi::sqlite3_aggregate_context(ctx, std::mem::size_of::<*mut Object>() as c_int)
        .cast::<*mut Object>();
    if slot.is_null() {
        return Err(());
    }
    if (*slot).is_null() {
        if !create {
            return Ok(None);
        }
        let inst = match interp().and_then(|ip| call(ip, &data.callable, &[])) {
            Ok(o) => o,
            Err(e) => {
                print_or_clear(e, &data.callable);
                return Err(());
            }
        };
        *slot = Box::into_raw(Box::new(inst));
    }
    Ok(Some(*slot))
}

/// Look up + call a method on the aggregate instance, mapping a failed
/// *lookup* (AttributeError) to CPython's "'X' method not defined"
/// message and a failed *call* to "'X' method raised error".
unsafe fn call_agg_method(
    ctx: *mut ffi::sqlite3_context,
    data: &FuncData,
    inst: &Object,
    name: &str,
    args: &[Object],
) -> Option<Object> {
    let looked_up = interp().and_then(|ip| ip.load_attr_public(inst, name));
    let (res, not_defined) = match looked_up {
        Ok(m) => (interp().and_then(|ip| call(ip, &m, args)), false),
        Err(e) => {
            let attr = exc_matches(&e, "AttributeError");
            (Err(e), attr)
        }
    };
    match res {
        Ok(v) => Some(v),
        Err(e) => {
            let msg = format!(
                "user-defined aggregate's '{name}' method {}",
                if not_defined {
                    "not defined"
                } else {
                    "raised error"
                }
            );
            result_error(ctx, &e, &msg);
            print_or_clear(e, &data.callable);
            None
        }
    }
}

unsafe extern "C" fn agg_step_cb(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let data = &*(ffi::sqlite3_user_data(ctx) as *const FuncData);
    let inst = match aggregate_instance(ctx, data, true) {
        Ok(Some(p)) => p,
        _ => {
            let msg =
                std::ffi::CString::new("user-defined aggregate's '__init__' method raised error")
                    .expect("static");
            ffi::sqlite3_result_error(ctx, msg.as_ptr(), -1);
            return;
        }
    };
    let args = build_py_params(argc, argv);
    call_agg_method(ctx, data, &*inst, "step", &args);
}

unsafe extern "C" fn agg_final_cb(ctx: *mut ffi::sqlite3_context) {
    let data = &*(ffi::sqlite3_user_data(ctx) as *const FuncData);
    let inst_ptr = match aggregate_instance(ctx, data, false) {
        Ok(Some(p)) => p,
        // The step handler never ran (no rows matched): the result stays
        // SQL NULL and no instance is created (`final_callback`).
        _ => return,
    };
    let inst = Box::from_raw(inst_ptr);
    // Zero the slot so a duplicate xFinal can't double-free.
    let slot = ffi::sqlite3_aggregate_context(ctx, 0).cast::<*mut Object>();
    if !slot.is_null() {
        *slot = std::ptr::null_mut();
    }
    if let Some(v) = call_agg_method(ctx, data, &inst, "finalize", &[]) {
        if let Err(e) = set_result(ctx, &v) {
            result_error(
                ctx,
                &e,
                "user-defined aggregate's 'finalize' method raised error",
            );
            print_or_clear(e, &data.callable);
        }
    }
}

unsafe extern "C" fn agg_value_cb(ctx: *mut ffi::sqlite3_context) {
    let data = &*(ffi::sqlite3_user_data(ctx) as *const FuncData);
    let inst_ptr = match aggregate_instance(ctx, data, false) {
        Ok(Some(p)) => p,
        _ => return,
    };
    let inst = &*inst_ptr;
    if let Some(v) = call_agg_method(ctx, data, inst, "value", &[]) {
        if let Err(e) = set_result(ctx, &v) {
            result_error(
                ctx,
                &e,
                "user-defined aggregate's 'value' method raised error",
            );
            print_or_clear(e, &data.callable);
        }
    }
}

unsafe extern "C" fn agg_inverse_cb(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let data = &*(ffi::sqlite3_user_data(ctx) as *const FuncData);
    let inst_ptr = match aggregate_instance(ctx, data, false) {
        Ok(Some(p)) => p,
        _ => return,
    };
    let inst = &*inst_ptr;
    let args = build_py_params(argc, argv);
    call_agg_method(ctx, data, inst, "inverse", &args);
}

pub(crate) fn conn_create_aggregate(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    warn_kwargs_deprecated(
        kwargs,
        &["name", "n_arg", "aggregate_class"],
        "Passing keyword arguments 'name', 'n_arg' and 'aggregate_class' to \
         _sqlite3.Connection.create_aggregate() is deprecated. Parameters 'name', \
         'n_arg' and 'aggregate_class' will become positional-only in Python 3.15.",
    )?;
    let name = match args
        .get(1)
        .or_else(|| kwarg(kwargs, "name"))
        .and_then(super::as_text)
    {
        Some(text) => text?,
        None => return Err(type_error("create_aggregate() argument 'name' must be str")),
    };
    let n_arg = args
        .get(2)
        .or_else(|| kwarg(kwargs, "n_arg"))
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("create_aggregate() argument 'n_arg' must be int"))?;
    let aggregate_class = args
        .get(3)
        .or_else(|| kwarg(kwargs, "aggregate_class"))
        .cloned()
        .ok_or_else(|| type_error("create_aggregate() missing 'aggregate_class'"))?;

    let db = state.borrow().db_ptr();
    let c_name =
        std::ffi::CString::new(name.clone()).map_err(|_| value_error("embedded null byte"))?;
    if matches!(aggregate_class, Object::None) {
        // SAFETY: live db handle; NULL callbacks drop the registration.
        let rc = unsafe {
            ffi::sqlite3_create_function_v2(
                db,
                c_name.as_ptr(),
                n_arg as c_int,
                ffi::SQLITE_UTF8,
                std::ptr::null_mut(),
                None,
                None,
                None,
                None,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(raise_sqlite_error(db, rc));
        }
        return Ok(Object::None);
    }
    let data = Box::into_raw(Box::new(FuncData {
        callable: aggregate_class.clone(),
    }));
    // SAFETY: live db handle; data ownership passes to sqlite.
    let rc = unsafe {
        ffi::sqlite3_create_function_v2(
            db,
            c_name.as_ptr(),
            n_arg as c_int,
            ffi::SQLITE_UTF8,
            data.cast::<c_void>(),
            None,
            Some(agg_step_cb),
            Some(agg_final_cb),
            Some(func_destroy),
        )
    };
    if rc != ffi::SQLITE_OK {
        return Err(raise(operational_error_class(), "Error creating aggregate"));
    }
    state.borrow_mut().hook_refs.push(aggregate_class);
    Ok(Object::None)
}

pub(crate) fn conn_create_window_function(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    let name = match args
        .get(1)
        .or_else(|| kwarg(kwargs, "name"))
        .and_then(super::as_text)
    {
        Some(text) => text?,
        None => {
            return Err(type_error(
                "create_window_function() argument 'name' must be str",
            ))
        }
    };
    let num_params = args
        .get(2)
        .or_else(|| kwarg(kwargs, "num_params"))
        .and_then(Object::as_i64)
        .ok_or_else(|| type_error("create_window_function() argument 'num_params' must be int"))?;
    let aggregate_class = args
        .get(3)
        .or_else(|| kwarg(kwargs, "aggregate_class"))
        .cloned()
        .ok_or_else(|| type_error("create_window_function() missing 'aggregate_class'"))?;

    let db = state.borrow().db_ptr();
    let c_name =
        std::ffi::CString::new(name.clone()).map_err(|_| value_error("embedded null byte"))?;
    if matches!(aggregate_class, Object::None) {
        // SAFETY: live db handle; NULL callbacks drop the registration.
        let rc = unsafe {
            ffi::sqlite3_create_window_function(
                db,
                c_name.as_ptr(),
                num_params as c_int,
                ffi::SQLITE_UTF8,
                std::ptr::null_mut(),
                None,
                None,
                None,
                None,
                None,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(raise_sqlite_error(db, rc));
        }
        return Ok(Object::None);
    }
    let data = Box::into_raw(Box::new(FuncData {
        callable: aggregate_class.clone(),
    }));
    // SAFETY: live db handle; data ownership passes to sqlite.
    let rc = unsafe {
        ffi::sqlite3_create_window_function(
            db,
            c_name.as_ptr(),
            num_params as c_int,
            ffi::SQLITE_UTF8,
            data.cast::<c_void>(),
            Some(agg_step_cb),
            Some(agg_final_cb),
            Some(agg_value_cb),
            Some(agg_inverse_cb),
            Some(func_destroy),
        )
    };
    if rc != ffi::SQLITE_OK {
        return Err(raise(
            programming_error_class(),
            "Error creating window function",
        ));
    }
    state.borrow_mut().hook_refs.push(aggregate_class);
    Ok(Object::None)
}

// ---------------------------------------------------------------
// Collations
// ---------------------------------------------------------------

unsafe extern "C" fn collation_cb(
    p: *mut c_void,
    n1: c_int,
    s1: *const c_void,
    n2: c_int,
    s2: *const c_void,
) -> c_int {
    let data = &*(p as *const FuncData);
    let a = String::from_utf8_lossy(std::slice::from_raw_parts(s1.cast::<u8>(), n1 as usize))
        .into_owned();
    let b = String::from_utf8_lossy(std::slice::from_raw_parts(s2.cast::<u8>(), n2 as usize))
        .into_owned();
    let res = interp().and_then(|ip| {
        call(
            ip,
            &data.callable,
            &[Object::from_str(a), Object::from_str(b)],
        )
    });
    match res {
        Ok(v) => {
            let n = v.as_i64().unwrap_or(0);
            n.clamp(-1, 1) as c_int
        }
        Err(e) => {
            print_or_clear(e, &data.callable);
            0
        }
    }
}

pub(crate) fn conn_create_collation(args: &[Object]) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    // str subclasses are accepted; no str methods are ever called on the
    // name (test_create_collation_bad_upper).
    let name = match args.get(1).and_then(super::as_text) {
        Some(text) => text?,
        None => {
            return Err(type_error(format!(
                "create_collation() argument 'name' must be str, not {}",
                args.get(1).map(Object::type_name_owned).unwrap_or_default()
            )))
        }
    };
    let callable = args
        .get(2)
        .cloned()
        .ok_or_else(|| type_error("create_collation() missing 'callable'"))?;
    if !matches!(callable, Object::None) && !is_callable(&callable) {
        return Err(type_error("parameter must be callable"));
    }
    let db = state.borrow().db_ptr();
    let c_name =
        std::ffi::CString::new(name.clone()).map_err(|_| value_error("embedded null byte"))?;
    if matches!(callable, Object::None) {
        // SAFETY: live db handle; NULL comparator removes the collation.
        let rc = unsafe {
            ffi::sqlite3_create_collation_v2(
                db,
                c_name.as_ptr(),
                ffi::SQLITE_UTF8,
                std::ptr::null_mut(),
                None,
                None,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(raise_sqlite_error(db, rc));
        }
        return Ok(Object::None);
    }
    let data = Box::into_raw(Box::new(FuncData {
        callable: callable.clone(),
    }));
    // SAFETY: live db handle; data ownership passes to sqlite via the
    // xDestroy callback.
    let rc = unsafe {
        ffi::sqlite3_create_collation_v2(
            db,
            c_name.as_ptr(),
            ffi::SQLITE_UTF8,
            data.cast::<c_void>(),
            Some(collation_cb),
            Some(func_destroy),
        )
    };
    if rc != ffi::SQLITE_OK {
        // sqlite did not take ownership on failure.
        // SAFETY: `data` was just leaked above and never registered.
        unsafe { drop(Box::from_raw(data)) };
        return Err(raise_sqlite_error(db, rc));
    }
    state.borrow_mut().hook_refs.push(callable);
    Ok(Object::None)
}

// ---------------------------------------------------------------
// Authorizer / progress / trace
// ---------------------------------------------------------------

unsafe extern "C" fn authorizer_cb(
    p: *mut c_void,
    action: c_int,
    arg1: *const c_char,
    arg2: *const c_char,
    dbname: *const c_char,
    source: *const c_char,
) -> c_int {
    let data = &*(p as *const FuncData);
    let to_obj = |s: *const c_char| {
        if s.is_null() {
            Object::None
        } else {
            Object::from_str(super::cstr_to_string(s))
        }
    };
    let res = interp().and_then(|ip| {
        call(
            ip,
            &data.callable,
            &[
                Object::Int(i64::from(action)),
                to_obj(arg1),
                to_obj(arg2),
                to_obj(dbname),
                to_obj(source),
            ],
        )
    });
    match res {
        // `authorizer_callback` in connection.c: only ints count, via
        // PyLong_AsInt — an overflowing value is an *error* (unraisable
        // OverflowError, then DENY); a non-int is silently DENY.
        Ok(v @ (Object::Int(_) | Object::Long(_) | Object::Bool(_))) => {
            match v.as_i64().and_then(|n| c_int::try_from(n).ok()) {
                Some(n) => n,
                None => {
                    print_or_clear(
                        crate::error::overflow_error("Python int too large to convert to C int"),
                        &data.callable,
                    );
                    ffi::SQLITE_DENY
                }
            }
        }
        Ok(_) => ffi::SQLITE_DENY,
        Err(e) => {
            print_or_clear(e, &data.callable);
            ffi::SQLITE_DENY
        }
    }
}

pub(crate) fn conn_set_authorizer(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    warn_kwargs_deprecated(
        kwargs,
        &["authorizer_callback"],
        "Passing keyword argument 'authorizer_callback' to \
         _sqlite3.Connection.set_authorizer() is deprecated. Parameter \
         'authorizer_callback' will become positional-only in Python 3.15.",
    )?;
    let callback = args
        .get(1)
        .or_else(|| kwarg(kwargs, "authorizer_callback"))
        .cloned()
        .ok_or_else(|| type_error("set_authorizer() missing 'authorizer_callback'"))?;
    let db = state.borrow().db_ptr();
    if matches!(callback, Object::None) {
        // SAFETY: live db handle; NULL clears the authorizer.
        unsafe { ffi::sqlite3_set_authorizer(db, None, std::ptr::null_mut()) };
        return Ok(Object::None);
    }
    let data = Box::into_raw(Box::new(FuncData {
        callable: callback.clone(),
    }));
    // SAFETY: live db handle. sqlite has no destructor slot for the
    // authorizer; the box is kept alive by hook_refs and leaked on
    // rebind — bounded by the connection's lifetime, like CPython's
    // per-connection ref slots.
    let rc = unsafe { ffi::sqlite3_set_authorizer(db, Some(authorizer_cb), data.cast::<c_void>()) };
    if rc != ffi::SQLITE_OK {
        // SAFETY: registration failed; reclaim the box.
        unsafe { drop(Box::from_raw(data)) };
        return Err(raise_sqlite_error(db, rc));
    }
    state.borrow_mut().hook_refs.push(callback);
    Ok(Object::None)
}

unsafe extern "C" fn progress_cb(p: *mut c_void) -> c_int {
    let data = &*(p as *const FuncData);
    // PyObject_IsTrue semantics: the handler's return value drives its
    // own __bool__, and a raising __bool__ interrupts the query
    // (unraisable + nonzero), like `progress_callback` in connection.c.
    let res = interp().and_then(|ip| {
        let v = call(ip, &data.callable, &[])?;
        let globals = ip.builtins_dict();
        ip.obj_truthy(&v, &globals)
    });
    match res {
        Ok(interrupt) => c_int::from(interrupt),
        Err(e) => {
            print_or_clear(e, &data.callable);
            1
        }
    }
}

pub(crate) fn conn_set_progress_handler(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    warn_kwargs_deprecated(
        kwargs,
        &["progress_handler"],
        "Passing keyword argument 'progress_handler' to \
         _sqlite3.Connection.set_progress_handler() is deprecated. Parameter \
         'progress_handler' will become positional-only in Python 3.15.",
    )?;
    let callback = args
        .get(1)
        .or_else(|| kwarg(kwargs, "progress_handler"))
        .cloned()
        .ok_or_else(|| type_error("set_progress_handler() missing 'progress_handler'"))?;
    let n = args
        .get(2)
        .or_else(|| kwarg(kwargs, "n"))
        .and_then(Object::as_i64)
        .unwrap_or(0);
    let db = state.borrow().db_ptr();
    if matches!(callback, Object::None) {
        // SAFETY: live db handle; NULL clears the handler.
        unsafe { ffi::sqlite3_progress_handler(db, 0, None, std::ptr::null_mut()) };
        return Ok(Object::None);
    }
    let data = Box::into_raw(Box::new(FuncData {
        callable: callback.clone(),
    }));
    // SAFETY: live db handle; box kept alive via hook_refs (no
    // destructor slot in the sqlite API, as with the authorizer).
    unsafe {
        ffi::sqlite3_progress_handler(db, n as c_int, Some(progress_cb), data.cast::<c_void>())
    };
    state.borrow_mut().hook_refs.push(callback);
    Ok(Object::None)
}

unsafe extern "C" fn trace_cb(
    kind: std::os::raw::c_uint,
    p: *mut c_void,
    stmt: *mut c_void,
    _x: *mut c_void,
) -> c_int {
    if kind != ffi::SQLITE_TRACE_STMT as std::os::raw::c_uint {
        return 0;
    }
    let data = &*(p as *const FuncData);
    // Expanded SQL matches CPython (bound parameters substituted when
    // possible; falls back to the raw text).
    let expanded = ffi::sqlite3_expanded_sql(stmt.cast::<ffi::sqlite3_stmt>());
    let sql = if expanded.is_null() {
        // Over SQLITE_LIMIT_LENGTH (or OOM): CPython reports an
        // unraisable DataError and traces the unexpanded statement.
        print_or_clear(
            raise(
                super::data_error_class(),
                "Expanded SQL string exceeds the maximum string length",
            ),
            &data.callable,
        );
        let raw = ffi::sqlite3_sql(stmt.cast::<ffi::sqlite3_stmt>());
        super::cstr_to_string(raw)
    } else {
        let s = super::cstr_to_string(expanded);
        ffi::sqlite3_free(expanded.cast::<c_void>());
        s
    };
    let res = interp().and_then(|ip| call(ip, &data.callable, &[Object::from_str(sql)]));
    if let Err(e) = res {
        print_or_clear(e, &data.callable);
    }
    0
}

pub(crate) fn conn_set_trace_callback(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let state = checked_conn(args.first().unwrap_or(&Object::None))?;
    warn_kwargs_deprecated(
        kwargs,
        &["trace_callback"],
        "Passing keyword argument 'trace_callback' to \
         _sqlite3.Connection.set_trace_callback() is deprecated. Parameter \
         'trace_callback' will become positional-only in Python 3.15.",
    )?;
    let callback = args
        .get(1)
        .or_else(|| kwarg(kwargs, "trace_callback"))
        .cloned()
        .ok_or_else(|| type_error("set_trace_callback() missing 'trace_callback'"))?;
    let db = state.borrow().db_ptr();
    if matches!(callback, Object::None) {
        // SAFETY: live db handle; zero mask + NULL clears tracing.
        unsafe { ffi::sqlite3_trace_v2(db, 0, None, std::ptr::null_mut()) };
        return Ok(Object::None);
    }
    let data = Box::into_raw(Box::new(FuncData {
        callable: callback.clone(),
    }));
    // SAFETY: live db handle; box kept alive via hook_refs.
    unsafe {
        ffi::sqlite3_trace_v2(
            db,
            ffi::SQLITE_TRACE_STMT as std::os::raw::c_uint,
            Some(trace_cb),
            data.cast::<c_void>(),
        )
    };
    state.borrow_mut().hook_refs.push(callback);
    Ok(Object::None)
}
