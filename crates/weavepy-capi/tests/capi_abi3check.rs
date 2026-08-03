//! Integration test: the RFC 0056 WS5 limited-API (abi3) proof.
//!
//! `crates/weavepy-capi/build.rs` compiles `tests/capi_ext/_abi3check.c`
//! against the host's stock CPython 3.13 headers with
//! `Py_LIMITED_API = 0x030D0000` and exports
//! `WEAVEPY_CAPI_ABI3CHECK_EXTENSION`. Here we dlopen that artifact into
//! WeavePy and drive the surface a PyO3 `abi3-py313` wheel binds:
//! PEP 489 multiphase init (exec slot), a `PyType_FromSpec` heap type,
//! `PyObject_Vectorcall`, `PyGILState_*`, `PyInterpreterState_Get`, and
//! the recursion-guard pair.
//!
//! Skipped (passes) when the env var is unset — CPython 3.13 dev
//! headers (or `cc`) aren't available on the build host.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use weavepy_capi::loader::load_extension_module;
use weavepy_vm::object::Object;
use weavepy_vm::Interpreter;

fn extension_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_ABI3CHECK_EXTENSION").map(PathBuf::from)
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

/// Serialize the tests in this binary (the C-API bridge keeps
/// process-global state; see `capi_stockabi.rs`).
fn serialize() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn load() -> Option<(MutexGuard<'static, ()>, Interpreter, Object)> {
    let guard = serialize();
    let path = extension_path()?;
    if !path.is_file() {
        eprintln!(
            "WEAVEPY_CAPI_ABI3CHECK_EXTENSION points at missing file: {} — skipping",
            path.display()
        );
        return None;
    }
    weavepy_capi::force_link();
    let mut interp = Interpreter::default();
    let interp_ptr: *mut Interpreter = &raw mut interp;
    match load_extension_module(interp_ptr, &path, "_abi3check") {
        Ok(m) => Some((guard, interp, m)),
        Err(err) => {
            panic!("dlopen of abi3 fixture failed: {err}");
        }
    }
}

fn call(interp: &mut Interpreter, module: &Object, name: &str, args: &[Object]) -> Object {
    let f = lookup(module, name).unwrap_or_else(|| panic!("module missing `{name}`"));
    interp
        .call_object(f, args, &[])
        .unwrap_or_else(|e| panic!("calling `{name}` failed: {e:?}"))
}

#[test]
fn abi3check_skipped_when_extension_missing() {
    if extension_path().is_none() {
        eprintln!("WEAVEPY_CAPI_ABI3CHECK_EXTENSION not set — skipping abi3 proof");
    }
}

/// PEP 489 multiphase init: the module materialises through
/// `PyModuleDef_Init` + the `Py_mod_exec` slot, which must have run
/// (it plants `EXEC_RAN`/`ABI`).
#[test]
fn abi3check_multiphase_exec_ran() {
    let Some((_lock, _interp, module)) = load() else {
        return;
    };
    match lookup(&module, "EXEC_RAN") {
        Some(Object::Int(n)) => assert_eq!(n, 1),
        other => panic!("EXEC_RAN wrong: {other:?}"),
    }
    match lookup(&module, "ABI") {
        Some(Object::Str(s)) => assert_eq!(&*s, "abi3"),
        other => panic!("ABI wrong: {other:?}"),
    }
}

/// The runtime-state trio pyo3-ffi binds at init: GILState round-trip,
/// a live `PyInterpreterState_Get`, and balanced recursion guards.
#[test]
fn abi3check_gil_roundtrip() {
    let Some((_lock, mut interp, module)) = load() else {
        return;
    };
    match call(&mut interp, &module, "gil_roundtrip", &[]) {
        Object::Bool(true) => {}
        other => panic!("gil_roundtrip: {other:?}"),
    }
}

#[test]
fn abi3check_interp_alive() {
    let Some((_lock, mut interp, module)) = load() else {
        return;
    };
    match call(&mut interp, &module, "interp_alive", &[]) {
        Object::Bool(true) => {}
        other => panic!("interp_alive: {other:?}"),
    }
}

#[test]
fn abi3check_recursion_guard() {
    let Some((_lock, mut interp, module)) = load() else {
        return;
    };
    match call(&mut interp, &module, "recursion_guard", &[]) {
        Object::Bool(true) => {}
        other => panic!("recursion_guard: {other:?}"),
    }
}

/// `PyObject_Vectorcall` under the abi3 spelling drives a WeavePy
/// callable with a flat argument stack: `pow(2, 6)` → 64.
#[test]
fn abi3check_vectorcall() {
    let Some((_lock, mut interp, module)) = load() else {
        return;
    };
    let pow = {
        let builtins = interp.builtins_dict();
        let d = builtins.borrow();
        d.iter()
            .find_map(|(k, v)| match &k.0 {
                Object::Str(s) if &**s == "pow" => Some(v.clone()),
                _ => None,
            })
            .expect("builtins expose pow")
    };
    match call(
        &mut interp,
        &module,
        "vectorcall_call",
        &[pow, Object::Int(2), Object::Int(6)],
    ) {
        Object::Int(n) => assert_eq!(n, 64),
        other => panic!("vectorcall_call: {other:?}"),
    }
}

/// Function-call (no macro) sequence access: `PySequence_GetItem` +
/// `PyLong_AsLong` over a WeavePy list.
#[test]
fn abi3check_sum_ints() {
    let Some((_lock, mut interp, module)) = load() else {
        return;
    };
    let list = Object::new_list(vec![Object::Int(10), Object::Int(30), Object::Int(2)]);
    match call(&mut interp, &module, "sum_ints", &[list]) {
        Object::Int(n) => assert_eq!(n, 42),
        other => panic!("sum_ints: {other:?}"),
    }
}

/// The `PyType_FromSpec` heap type (the `#[pyclass]` shape): construct,
/// call methods, observe per-instance state.
#[test]
fn abi3check_fromspec_counter_type() {
    let Some((_lock, mut interp, module)) = load() else {
        return;
    };
    let counter_cls = lookup(&module, "Counter").expect("Counter type exported");
    let counter = interp
        .call_object(counter_cls, &[], &[])
        .expect("Counter() constructs");
    let incr = interp
        .load_attr_public(&counter, "incr")
        .expect("incr method");
    for expected in 1..=3i64 {
        match interp.call_object(incr.clone(), &[], &[]) {
            Ok(Object::Int(n)) => assert_eq!(n, expected),
            other => panic!("incr: {other:?}"),
        }
    }
    let value = interp
        .load_attr_public(&counter, "value")
        .expect("value method");
    match interp.call_object(value, &[], &[]) {
        Ok(Object::Int(n)) => assert_eq!(n, 3),
        other => panic!("value: {other:?}"),
    }
}
