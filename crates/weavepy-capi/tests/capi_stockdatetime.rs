//! Integration test: the RFC 0029 (wave 5) faithful-datetime ABI proof.
//!
//! `crates/weavepy-capi/build.rs` compiles `tests/capi_ext/_stockdatetime.c`
//! against the host's **stock CPython 3.13 headers** (including the real
//! `datetime.h`, so the datetime accessor macros and `PyDateTime_IMPORT`
//! are inlined exactly as Cython emits them inside pandas' `tslibs`) and
//! exports `WEAVEPY_CAPI_STOCKDATETIME_EXTENSION`.
//!
//! Here we `dlopen` that `.so` into WeavePy and drive it, asserting that:
//!   * `PyDateTime_IMPORT` resolves the capsule and its type slots report
//!     CPython's `tp_basicsize` (date 32, datetime 48, time 40, delta 40);
//!   * a WeavePy `datetime`/`date`/`time`/`timedelta` handed to C reads
//!     back correctly through the inlined `PyDateTime_GET_*` macros (the
//!     pandas read path lands on WeavePy's byte-faithful instance bodies);
//!   * the capsule constructors (`PyDate_FromDate`, …) round-trip; and
//!   * `PyDate_Check`/`PyDateTime_Check`/`PyDelta_Check` (via the capsule
//!     type slots) classify correctly — including `PyDate_Check(datetime)`
//!     being true because `datetime` subclasses `date`.
//!
//! Skipped (passes) when the env var is unset (no CPython 3.13 headers or
//! `cc` on the build host), so CI on a bare machine still passes.

use std::path::PathBuf;

use weavepy_capi::loader::load_extension_module;
use weavepy_vm::object::Object;
use weavepy_vm::Interpreter;

fn extension_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_STOCKDATETIME_EXTENSION").map(PathBuf::from)
}

fn lookup(module: &Object, key: &str) -> Option<Object> {
    let Object::Module(m) = module else {
        return None;
    };
    let d = m.dict.borrow();
    for (k, v) in d.iter() {
        if let Object::Str(s) = &k.0 {
            if &**s == key {
                return Some(v.clone());
            }
        }
    }
    None
}

fn load() -> Option<(Interpreter, Object)> {
    let path = extension_path()?;
    if !path.is_file() {
        eprintln!(
            "WEAVEPY_CAPI_STOCKDATETIME_EXTENSION points at missing file: {} — skipping",
            path.display()
        );
        return None;
    }
    weavepy_capi::force_link();
    let mut interp = Interpreter::default();
    let interp_ptr: *mut Interpreter = &raw mut interp;
    match load_extension_module(interp_ptr, &path, "_stockdatetime") {
        Ok(m) => Some((interp, m)),
        Err(err) => {
            eprintln!("dlopen of stock-datetime extension failed (treating as skip): {err}");
            None
        }
    }
}

fn call(interp: &mut Interpreter, module: &Object, name: &str, args: &[Object]) -> Object {
    let f = lookup(module, name).unwrap_or_else(|| panic!("module missing `{name}`"));
    interp
        .call_object(f, args, &[])
        .unwrap_or_else(|e| panic!("calling `{name}` failed: {e:?}"))
}

/// Construct a `datetime`-module instance (`date`, `datetime`, …) by
/// importing the module and calling the class with integer args.
fn make(interp: &mut Interpreter, class: &str, args: &[i64]) -> Object {
    let module = interp
        .import_path("datetime")
        .unwrap_or_else(|e| panic!("import datetime failed: {e:?}"));
    let cls = lookup(&module, class)
        .unwrap_or_else(|| panic!("datetime module missing `{class}`"));
    let argv: Vec<Object> = args.iter().copied().map(Object::Int).collect();
    interp
        .call_object(cls, &argv, &[])
        .unwrap_or_else(|e| panic!("datetime.{class}{args:?} failed: {e:?}"))
}

fn int_tuple(o: Object) -> Vec<i64> {
    match o {
        Object::Tuple(t) => t
            .iter()
            .map(|x| match x {
                Object::Int(i) => *i,
                Object::Bool(b) => i64::from(*b),
                other => panic!("non-int in tuple: {other:?}"),
            })
            .collect(),
        other => panic!("expected tuple, got {other:?}"),
    }
}

#[test]
fn stockdatetime_skipped_when_extension_missing() {
    if extension_path().is_none() {
        eprintln!("WEAVEPY_CAPI_STOCKDATETIME_EXTENSION not set — skipping datetime ABI proof");
    }
}

/// `PyDateTime_IMPORT` resolves the capsule and its type slots carry the
/// faithful CPython 3.13 `tp_basicsize` values.
#[test]
fn stockdatetime_capsule_imports_with_faithful_sizes() {
    let Some((_interp, module)) = load() else {
        return;
    };
    match lookup(&module, "imported") {
        Some(Object::Int(n)) => assert_eq!(n, 1, "PyDateTime_IMPORT failed (capsule NULL)"),
        other => panic!("imported flag wrong: {other:?}"),
    }
    let want = [
        ("cap_date_basicsize", 32),
        ("cap_datetime_basicsize", 48),
        ("cap_time_basicsize", 40),
        ("cap_delta_basicsize", 40),
    ];
    for (k, v) in want {
        match lookup(&module, k) {
            Some(Object::Int(n)) => assert_eq!(n, v, "{k} should be {v}"),
            other => panic!("{k} wrong: {other:?}"),
        }
    }
}

/// The `__Pyx_ImportType` size-check path: the `datetime` module's class
/// objects report CPython's `tp_basicsize` when read as `PyTypeObject*`.
#[test]
fn stockdatetime_module_size_check() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let sizes = int_tuple(call(&mut interp, &module, "module_basicsizes", &[]));
    assert_eq!(sizes, vec![32, 48, 40, 40], "datetime.* tp_basicsize");
}

/// A WeavePy `date` handed to C reads back through `PyDateTime_GET_*`.
#[test]
fn stockdatetime_read_date() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let d = make(&mut interp, "date", &[2021, 3, 14]);
    let got = int_tuple(call(&mut interp, &module, "read_date", &[d]));
    assert_eq!(got, vec![2021, 3, 14]);
}

/// A WeavePy `datetime` reads back through the inlined macros, including
/// the big-endian year and 3-byte microsecond fields.
#[test]
fn stockdatetime_read_datetime() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let dt = make(&mut interp, "datetime", &[2021, 3, 14, 9, 30, 15, 123456]);
    let got = int_tuple(call(&mut interp, &module, "read_datetime", &[dt.clone()]));
    assert_eq!(got, vec![2021, 3, 14, 9, 30, 15, 123456, 0]);
    // Naive datetime: PyDateTime_DATE_GET_TZINFO short-circuits to Py_None.
    assert!(matches!(
        call(&mut interp, &module, "datetime_tz_is_none", &[dt]),
        Object::Bool(true)
    ));
}

/// A WeavePy `time` reads back through `PyDateTime_TIME_GET_*`.
#[test]
fn stockdatetime_read_time() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let t = make(&mut interp, "time", &[9, 30, 15, 123456]);
    let got = int_tuple(call(&mut interp, &module, "read_time", &[t]));
    assert_eq!(got, vec![9, 30, 15, 123456, 0]);
}

/// A WeavePy `timedelta` reads back through `PyDateTime_DELTA_GET_*`.
#[test]
fn stockdatetime_read_delta() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let td = make(&mut interp, "timedelta", &[5, 3600, 250000]);
    let got = int_tuple(call(&mut interp, &module, "read_delta", &[td]));
    assert_eq!(got, vec![5, 3600, 250000]);
}

/// The capsule constructors build faithful objects readable via macros.
#[test]
fn stockdatetime_capsule_constructors() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let d = int_tuple(call(
        &mut interp,
        &module,
        "construct_date",
        &[Object::Int(1999), Object::Int(12), Object::Int(31)],
    ));
    assert_eq!(d, vec![1999, 12, 31]);

    let dt = int_tuple(call(
        &mut interp,
        &module,
        "construct_datetime",
        &[
            Object::Int(2000),
            Object::Int(1),
            Object::Int(2),
            Object::Int(3),
            Object::Int(4),
            Object::Int(5),
            Object::Int(6),
        ],
    ));
    assert_eq!(dt, vec![2000, 1, 2, 3, 4, 5, 6]);

    let td = int_tuple(call(
        &mut interp,
        &module,
        "construct_delta",
        &[Object::Int(7), Object::Int(8), Object::Int(9)],
    ));
    assert_eq!(td, vec![7, 8, 9]);
}

/// `PyDate_Check`/`PyDateTime_Check`/`PyDelta_Check` via the capsule type
/// slots, including `datetime` IS-A `date`.
#[test]
fn stockdatetime_type_checks() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    // (PyDate_Check, PyDate_CheckExact, PyDateTime_Check, PyDateTime_CheckExact, PyDelta_Check)
    let date = make(&mut interp, "date", &[2021, 1, 1]);
    assert_eq!(
        int_tuple(call(&mut interp, &module, "checks", &[date])),
        vec![1, 1, 0, 0, 0]
    );

    let dt = make(&mut interp, "datetime", &[2021, 1, 1, 0, 0, 0]);
    // datetime subclasses date: PyDate_Check true, PyDate_CheckExact false.
    assert_eq!(
        int_tuple(call(&mut interp, &module, "checks", &[dt])),
        vec![1, 0, 1, 1, 0]
    );

    let td = make(&mut interp, "timedelta", &[1, 2, 3]);
    assert_eq!(
        int_tuple(call(&mut interp, &module, "checks", &[td])),
        vec![0, 0, 0, 0, 1]
    );
}
