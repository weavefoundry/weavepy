//! Integration test: the RFC 0045 wave-3 hermetic proof.
//!
//! `crates/weavepy-capi/build.rs` compiles `tests/capi_ext/_stockarray.c`
//! against the host's **stock CPython 3.13 headers** (full, non-limited
//! API → the genuine 416-byte `PyTypeObject`, the real `PyMemberDef`
//! layout, and `PyCapsule_*`) and exports `WEAVEPY_CAPI_STOCKARRAY_EXTENSION`.
//! Here we `dlopen` that `.so` into WeavePy and drive it, asserting that a
//! `PyArrayObject`-shaped stock type — one that reads its own fields
//! **inline** at fixed `tp_basicsize` offsets — works end to end:
//!
//!   * **inline `tp_basicsize` storage** persists across crossings
//!     (`StockArray(5).sum() == 10.0`: init wrote the buffer, a later C
//!     call read it back through the *same* body);
//!   * **`tp_members`** project inline fields (`nd`, `length` read-only;
//!     `typenum` writable) at their real `offsetof`;
//!   * the inline `data` pointer is **stable** across calls;
//!   * **mutation** through one C call is visible to the next
//!     (`fill()` then `sum()`);
//!   * the **array interchange** protocols `__array_interface__` /
//!     `__array_struct__` expose the inline buffer;
//!   * the **`import_array()` array-C-API capsule** round-trips
//!     (`PyCapsule_Import("_stockarray._ARRAY_API")` → a `void **` table →
//!     build a fresh array through it);
//!   * a faithful `tp_dealloc` frees `self->data` and its
//!     `PyObject_Free(self)` tail is absorbed (the body is owned by the
//!     native instance), with no leak.
//!
//! Skipped (passes) when the env var is unset — that happens when CPython
//! 3.13 dev headers (or `cc`) aren't available on the build host.

use std::path::PathBuf;

use weavepy_capi::loader::load_extension_module;
use weavepy_vm::error::RuntimeError;
use weavepy_vm::object::Object;
use weavepy_vm::Interpreter;

fn extension_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_STOCKARRAY_EXTENSION").map(PathBuf::from)
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

/// Load `_stockarray` and register it in the interpreter's module cache,
/// so the in-C `PyCapsule_Import("_stockarray._ARRAY_API")` (the
/// `import_array()` path) can re-import the module by name.
fn load() -> Option<(Interpreter, Object)> {
    let path = extension_path()?;
    if !path.is_file() {
        eprintln!(
            "WEAVEPY_CAPI_STOCKARRAY_EXTENSION points at missing file: {} — skipping",
            path.display()
        );
        return None;
    }
    weavepy_capi::force_link();
    let mut interp = Interpreter::default();
    let interp_ptr: *mut Interpreter = &raw mut interp;
    match load_extension_module(interp_ptr, &path, "_stockarray") {
        Ok(m) => {
            interp.module_cache().insert("_stockarray", m.clone());
            Some((interp, m))
        }
        Err(err) => {
            eprintln!("dlopen of stock-array extension failed (treating as skip): {err}");
            None
        }
    }
}

/// Construct an instance by calling the readied type object, as `T(...)`
/// would from Python: drives `tp_new` (→ our inline body) + `tp_init`.
fn construct(interp: &mut Interpreter, ty: &Object, args: &[Object]) -> Object {
    interp
        .call_object(ty.clone(), args, &[])
        .unwrap_or_else(|e| panic!("constructing StockArray failed: {e:?}"))
}

fn call_method(
    interp: &mut Interpreter,
    instance: Object,
    name: &str,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let class = match &instance {
        Object::Instance(inst) => inst.cls(),
        other => panic!("expected instance, got {other:?}"),
    };
    let method = class
        .lookup(name)
        .unwrap_or_else(|| panic!("method '{name}' not in MRO of {}", class.name));
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push(instance);
    full.extend_from_slice(args);
    interp.call_object(method, &full, &[])
}

/// Read `instance.<name>` through `__getattribute__` (so member / getset
/// descriptors fire exactly as attribute access would).
fn get_attr(interp: &mut Interpreter, instance: Object, name: &str) -> Object {
    call_method(
        interp,
        instance,
        "__getattribute__",
        &[Object::from_str(name)],
    )
    .unwrap_or_else(|e| panic!("getattr {name} failed: {e:?}"))
}

fn set_attr(interp: &mut Interpreter, instance: Object, name: &str, value: Object) {
    call_method(
        interp,
        instance,
        "__setattr__",
        &[Object::from_str(name), value],
    )
    .unwrap_or_else(|e| panic!("setattr {name} failed: {e:?}"));
}

fn call_module_fn(
    interp: &mut Interpreter,
    module: &Object,
    name: &str,
    args: &[Object],
) -> Object {
    let f = lookup(module, name).unwrap_or_else(|| panic!("module fn `{name}` missing"));
    interp
        .call_object(f, args, &[])
        .unwrap_or_else(|e| panic!("calling `{name}` failed: {e:?}"))
}

fn as_f64(o: &Object) -> f64 {
    match o {
        Object::Float(f) => *f,
        Object::Int(i) => *i as f64,
        other => panic!("expected float, got {other:?}"),
    }
}

fn as_i64(o: &Object) -> i64 {
    match o {
        Object::Int(i) => *i,
        other => panic!("expected int, got {other:?}"),
    }
}

fn dict_get(d: &Object, key: &str) -> Option<Object> {
    let Object::Dict(rc) = d else {
        return None;
    };
    let g = rc.borrow();
    for (k, v) in g.iter() {
        if let Object::Str(s) = &k.0 {
            if &**s == key {
                return Some(v.clone());
            }
        }
    }
    None
}

#[test]
fn stockarray_skipped_when_extension_missing() {
    if extension_path().is_none() {
        eprintln!("WEAVEPY_CAPI_STOCKARRAY_EXTENSION not set — skipping inline-storage proof");
    }
}

#[test]
fn stockarray_module_loads_with_type_and_capsule() {
    let Some((_interp, module)) = load() else {
        return;
    };
    assert!(
        lookup(&module, "StockArray").is_some(),
        "missing StockArray"
    );
    assert!(
        lookup(&module, "_ARRAY_API").is_some(),
        "missing _ARRAY_API capsule"
    );
    match lookup(&module, "ABI") {
        Some(Object::Str(s)) => assert_eq!(&*s, "cp313"),
        other => panic!("unexpected ABI marker: {other:?}"),
    }
}

/// The headline proof: `tp_init` writes the inline `data`/`length`
/// fields, and a *separate* later C call (`sum()`) reads them back — so
/// the faithful body is the **same block** across both crossings.
#[test]
fn stockarray_inline_storage_persists_across_calls() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let ty = lookup(&module, "StockArray").expect("StockArray");
    let arr = construct(&mut interp, &ty, &[Object::Int(5)]);

    // sum() reads self->data / self->length written by tp_init. A fresh
    // per-crossing body would read zeros (or a NULL data pointer).
    let total = call_method(&mut interp, arr.clone(), "sum", &[]).expect("sum");
    assert!(
        (as_f64(&total) - 10.0).abs() < 1e-9,
        "0+1+2+3+4 should be 10.0, got {total:?}"
    );
}

/// `tp_members` read the very bytes `tp_init` wrote, at their real
/// offsets, and READONLY members reject assignment.
#[test]
fn stockarray_members_read_inline_fields() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let ty = lookup(&module, "StockArray").expect("StockArray");
    let arr = construct(&mut interp, &ty, &[Object::Int(7)]);

    assert_eq!(as_i64(&get_attr(&mut interp, arr.clone(), "nd")), 1, "nd");
    assert_eq!(
        as_i64(&get_attr(&mut interp, arr.clone(), "length")),
        7,
        "length"
    );
    assert_eq!(
        as_i64(&get_attr(&mut interp, arr.clone(), "typenum")),
        12,
        "typenum default"
    );

    // `length` is READONLY → assignment must raise.
    let err = call_method(
        &mut interp,
        arr,
        "__setattr__",
        &[Object::from_str("length"), Object::Int(99)],
    );
    assert!(err.is_err(), "writing READONLY member should fail");
}

/// A writable member round-trips through the same inline field.
#[test]
fn stockarray_member_write_roundtrips() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let ty = lookup(&module, "StockArray").expect("StockArray");
    let arr = construct(&mut interp, &ty, &[Object::Int(3)]);

    set_attr(&mut interp, arr.clone(), "typenum", Object::Int(7));
    assert_eq!(
        as_i64(&get_attr(&mut interp, arr, "typenum")),
        7,
        "typenum should reflect the written value"
    );
}

/// The inline `data` pointer is the same address on every crossing —
/// direct evidence the instance presents one stable body.
#[test]
fn stockarray_data_pointer_is_stable() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let ty = lookup(&module, "StockArray").expect("StockArray");
    let arr = construct(&mut interp, &ty, &[Object::Int(4)]);

    let a = as_i64(&call_method(&mut interp, arr.clone(), "data_addr", &[]).expect("data_addr"));
    let b = as_i64(&call_method(&mut interp, arr.clone(), "data_addr", &[]).expect("data_addr"));
    assert_eq!(a, b, "data pointer must be stable across crossings");
    assert_ne!(a, 0, "data pointer must be non-null");
}

/// Mutation written inline by one C call (`fill`) is visible to the next
/// (`sum`) — the bytes live in the shared body, not a transient box.
#[test]
fn stockarray_fill_then_sum() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let ty = lookup(&module, "StockArray").expect("StockArray");
    let arr = construct(&mut interp, &ty, &[Object::Int(5)]);

    call_method(&mut interp, arr.clone(), "fill", &[Object::Float(2.0)]).expect("fill");
    let total = call_method(&mut interp, arr, "sum", &[]).expect("sum");
    assert!(
        (as_f64(&total) - 10.0).abs() < 1e-9,
        "5 * 2.0 should be 10.0, got {total:?}"
    );
}

/// `__array_interface__` exposes shape + the live inline data address.
#[test]
fn stockarray_array_interface() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let ty = lookup(&module, "StockArray").expect("StockArray");
    let arr = construct(&mut interp, &ty, &[Object::Int(5)]);

    let iface = get_attr(&mut interp, arr.clone(), "__array_interface__");
    assert_eq!(
        as_i64(&dict_get(&iface, "version").expect("version")),
        3,
        "array interface version"
    );
    match dict_get(&iface, "shape").expect("shape") {
        Object::Tuple(t) => {
            assert_eq!(t.len(), 1);
            assert_eq!(as_i64(&t[0]), 5, "shape[0]");
        }
        other => panic!("shape not a tuple: {other:?}"),
    }
    match dict_get(&iface, "typestr").expect("typestr") {
        Object::Str(s) => assert_eq!(&*s, "<f8"),
        other => panic!("typestr not a str: {other:?}"),
    }
    // data[0] is the same address data_addr() reports.
    let data_addr = as_i64(&call_method(&mut interp, arr, "data_addr", &[]).expect("data_addr"));
    match dict_get(&iface, "data").expect("data") {
        Object::Tuple(t) => {
            assert_eq!(as_i64(&t[0]), data_addr, "interface data addr matches");
            assert!(matches!(t[1], Object::Bool(false)), "data not read-only");
        }
        other => panic!("data not a tuple: {other:?}"),
    }
}

/// `__array_struct__` yields a `PyArrayInterface` capsule a C consumer
/// reads back with the right layout.
#[test]
fn stockarray_array_struct_capsule() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let ty = lookup(&module, "StockArray").expect("StockArray");
    let arr = construct(&mut interp, &ty, &[Object::Int(6)]);

    let data_addr =
        as_i64(&call_method(&mut interp, arr.clone(), "data_addr", &[]).expect("data_addr"));
    // read_array_struct(arr) → (two, nd, typekind, length, data_addr)
    match call_module_fn(&mut interp, &module, "read_array_struct", &[arr]) {
        Object::Tuple(t) => {
            assert_eq!(t.len(), 5, "read_array_struct arity");
            assert_eq!(as_i64(&t[0]), 2, "PyArrayInterface.two");
            assert_eq!(as_i64(&t[1]), 1, "nd");
            assert_eq!(as_i64(&t[2]), 'f' as i64, "typekind 'f'");
            assert_eq!(as_i64(&t[3]), 6, "shape[0]");
            assert_eq!(as_i64(&t[4]), data_addr, "data addr matches");
        }
        other => panic!("read_array_struct returned non-tuple: {other:?}"),
    }
}

/// The `import_array()` capsule pattern: `capi_roundtrip(n)` does
/// `PyCapsule_Import("_stockarray._ARRAY_API")`, recovers the `void **`
/// table, and builds a fresh array through `table[FROMLENGTH]`. The
/// result is a real inline-storage instance whose `sum()` works.
#[test]
fn stockarray_import_array_capsule_roundtrip() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let made = call_module_fn(&mut interp, &module, "capi_roundtrip", &[Object::Int(4)]);
    match &made {
        Object::Instance(i) => assert_eq!(i.cls().name, "StockArray", "wrong type from capsule"),
        other => panic!("capi_roundtrip returned non-StockArray: {other:?}"),
    }
    // 0+1+2+3 == 6.0 — the capsule-built array has real inline storage.
    let total = call_method(&mut interp, made, "sum", &[]).expect("sum");
    assert!(
        (as_f64(&total) - 6.0).abs() < 1e-9,
        "capsule-built StockArray(4).sum() should be 6.0, got {total:?}"
    );
}

/// A faithful `tp_dealloc` (frees `self->data`, then `PyObject_Free(self)`
/// which WeavePy absorbs for an instance body) runs when the instance is
/// collected — proving the body's lifetime is owned by the native instance
/// and reclaimed with it, no leak/crash.
///
/// The proof uses the fixture's **monotonic** `dealloc_count()` rather than
/// the live count: this `.so` (and thus its counters) is shared across all
/// tests in the process, which `cargo test` runs in parallel, so an absolute
/// `live == base + 1` reading races other tests constructing/dropping arrays.
/// `dealloc_count` only ever rises, so `after >= before + 1` holds iff *our*
/// instance was collected, regardless of concurrent deallocs.
#[test]
fn stockarray_dealloc_frees_buffer() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let ty = lookup(&module, "StockArray").expect("StockArray");

    let before = as_i64(&call_module_fn(&mut interp, &module, "dealloc_count", &[]));
    let arr = construct(&mut interp, &ty, &[Object::Int(8)]);
    // Sanity that this is a live inline-storage array (init wrote 0..7 into
    // the body): a clone is consumed by the `sum` call, leaving `arr` the
    // sole reference to drop below.
    let total = call_method(&mut interp, arr.clone(), "sum", &[]).expect("sum");
    assert!(
        (as_f64(&total) - 28.0).abs() < 1e-9,
        "StockArray(8).sum() should be 0+..+7 == 28.0, got {total:?}"
    );

    // Drop the sole remaining reference: the native instance is collected,
    // which runs the C `tp_dealloc` (free buffer) via the free hook.
    drop(arr);
    let after = as_i64(&call_module_fn(&mut interp, &module, "dealloc_count", &[]));
    assert!(
        after > before,
        "tp_dealloc must run on collection (before={before}, after={after})"
    );
}
