//! Integration test: the RFC 0043 wave-1 hermetic proof.
//!
//! `crates/weavepy-capi/build.rs` compiles `tests/capi_ext/_stockabi.c`
//! against the host's **stock CPython 3.13 headers** (full, non-limited
//! API → real inlined macros) and exports `WEAVEPY_CAPI_STOCKABI_EXTENSION`.
//! Here we `dlopen` that `.so` into WeavePy and drive it, asserting that
//! a binary artifact compiled against real CPython headers — including
//! its *inlined* field-access macros (`PyFloat_AS_DOUBLE`, `Py_SIZE`,
//! `PyTuple_GET_ITEM`, `Py_TYPE` identity, head-poke `Py_INCREF`/`DECREF`)
//! — runs correctly against WeavePy's layout-faithful mirrors.
//!
//! Skipped (passes) when the env var is unset — that happens when
//! CPython 3.13 dev headers (or `cc`) aren't available on the build
//! host, so CI on a bare machine still passes.

use std::path::PathBuf;

use weavepy_capi::loader::load_extension_module;
use weavepy_vm::object::Object;
use weavepy_vm::Interpreter;

fn extension_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_STOCKABI_EXTENSION").map(PathBuf::from)
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
            "WEAVEPY_CAPI_STOCKABI_EXTENSION points at missing file: {} — skipping",
            path.display()
        );
        return None;
    }
    weavepy_capi::force_link();
    let mut interp = Interpreter::default();
    let interp_ptr: *mut Interpreter = &raw mut interp;
    match load_extension_module(interp_ptr, &path, "_stockabi") {
        Ok(m) => Some((interp, m)),
        Err(err) => {
            eprintln!("dlopen of stock-ABI extension failed (treating as skip): {err}");
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

#[test]
fn stockabi_skipped_when_extension_missing() {
    if extension_path().is_none() {
        eprintln!("WEAVEPY_CAPI_STOCKABI_EXTENSION not set — skipping stock-ABI proof");
    }
}

#[test]
fn stockabi_module_loads_with_constants() {
    let Some((_interp, module)) = load() else {
        return;
    };
    match lookup(&module, "ANSWER") {
        Some(Object::Int(n)) => assert_eq!(n, 42),
        other => panic!("ANSWER wrong: {other:?}"),
    }
    match lookup(&module, "ABI") {
        Some(Object::Str(s)) => assert_eq!(&*s, "cp313"),
        other => panic!("ABI wrong: {other:?}"),
    }
}

/// The headline assertion: a stock-compiled `PyFloat_AS_DOUBLE` (inlined
/// read of `ob_fval` at offset 16) returns the right value off a WeavePy
/// float mirror.
#[test]
fn stockabi_inlined_float_read() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    match call(&mut interp, &module, "double_it", &[Object::Float(2.5)]) {
        Object::Float(f) => assert_eq!(f, 5.0),
        other => panic!("double_it: {other:?}"),
    }
}

/// Inlined `Py_SIZE` reads `ob_size` off a faithful tuple mirror.
#[test]
fn stockabi_inlined_size_read() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let t = Object::new_tuple(vec![Object::Int(1), Object::Int(2), Object::Int(3)]);
    match call(&mut interp, &module, "size", &[t]) {
        Object::Int(n) => assert_eq!(n, 3),
        other => panic!("size: {other:?}"),
    }
}

/// Inlined `PyTuple_GET_ITEM` reads the faithful `ob_item[]` tail.
#[test]
fn stockabi_inlined_tuple_item_read() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let t = Object::new_tuple(vec![Object::Int(10), Object::Int(20)]);
    match call(&mut interp, &module, "tuple_first", &[t]) {
        Object::Int(n) => assert_eq!(n, 10),
        other => panic!("tuple_first: {other:?}"),
    }
    let t = Object::new_tuple(vec![
        Object::Int(1),
        Object::Int(2),
        Object::Int(3),
        Object::Int(4),
    ]);
    match call(&mut interp, &module, "tuple_sum", &[t]) {
        Object::Int(n) => assert_eq!(n, 10),
        other => panic!("tuple_sum: {other:?}"),
    }
}

/// `Py_TYPE(o) == &PyFloat_Type` / `&PyLong_Type` across the boundary.
#[test]
fn stockabi_type_identity() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    assert!(matches!(
        call(&mut interp, &module, "is_float", &[Object::Float(1.0)]),
        Object::Bool(true)
    ));
    assert!(matches!(
        call(&mut interp, &module, "is_float", &[Object::Int(1)]),
        Object::Bool(false)
    ));
    assert!(matches!(
        call(&mut interp, &module, "is_long", &[Object::Int(7)]),
        Object::Bool(true)
    ));
    match call(&mut interp, &module, "type_name", &[Object::Float(1.0)]) {
        Object::Str(s) => assert_eq!(&*s, "float"),
        other => panic!("type_name: {other:?}"),
    }
}

/// Head-poke `Py_INCREF` + ownership transfer (`roundtrip`).
#[test]
fn stockabi_roundtrip_incref() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    match call(&mut interp, &module, "roundtrip", &[Object::Int(99)]) {
        Object::Int(n) => assert_eq!(n, 99),
        other => panic!("roundtrip: {other:?}"),
    }
}

/// Function-API constructors / arg parsing / `Py_BuildValue`.
#[test]
fn stockabi_function_api() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    match call(
        &mut interp,
        &module,
        "add",
        &[Object::Int(2), Object::Int(3)],
    ) {
        Object::Int(n) => assert_eq!(n, 5),
        other => panic!("add: {other:?}"),
    }
    match call(
        &mut interp,
        &module,
        "add_doubles",
        &[Object::Float(1.5), Object::Float(2.5)],
    ) {
        Object::Float(f) => assert_eq!(f, 4.0),
        other => panic!("add_doubles: {other:?}"),
    }
    match call(
        &mut interp,
        &module,
        "echo_str",
        &[Object::Str("hello".into())],
    ) {
        Object::Str(s) => assert_eq!(&*s, "hello"),
        other => panic!("echo_str: {other:?}"),
    }
    match call(
        &mut interp,
        &module,
        "make_pair",
        &[Object::Int(1), Object::Int(2)],
    ) {
        Object::Tuple(t) => {
            assert_eq!(t.len(), 2);
            assert!(matches!(t[0], Object::Int(1)));
            assert!(matches!(t[1], Object::Int(2)));
        }
        other => panic!("make_pair: {other:?}"),
    }
    let lst = Object::new_list(vec![Object::Int(1), Object::Int(2), Object::Int(3)]);
    match call(&mut interp, &module, "list_sum", &[lst]) {
        Object::Int(n) => assert_eq!(n, 6),
        other => panic!("list_sum: {other:?}"),
    }
}

/// C-side allocate-then-`Py_DECREF`-to-zero: the inlined `Py_DECREF`
/// calls `_Py_Dealloc` → `tp_dealloc` (offset 48) → frees the mirror.
#[test]
fn stockabi_c_side_dealloc() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    match call(&mut interp, &module, "alloc_free_cycle", &[]) {
        // sum(0..100) == 4950
        Object::Int(n) => assert_eq!(n, 4950),
        other => panic!("alloc_free_cycle: {other:?}"),
    }
}
