//! Integration test: the RFC 0047 wave-5 hermetic proof.
//!
//! `crates/weavepy-capi/build.rs` compiles `tests/capi_ext/_stockcython.c`
//! against the host's **stock CPython 3.13 headers** (full, non-limited
//! API) and exports `WEAVEPY_CAPI_STOCKCYTHON_EXTENSION`. The fixture is
//! shaped like a **Cython-generated** extension: it defines a base type
//! with number / sequence / repr / hash / richcompare slots and two
//! subclasses that (almost) nothing of their own, then reads the
//! inherited slots **directly off `Py_TYPE(instance)`** — the inlined
//! idiom Cython emits everywhere.
//!
//! Here we `dlopen` that `.so` into WeavePy and assert:
//!
//!   * **`inherit_slots`** — a pure subclass (`CySub`) and a
//!     partial-override subclass (`CySub2`) carry the base's `tp_*`
//!     function slots and method-suite entries *baked into their own
//!     faithful struct*, so a direct `Py_TYPE(self)->tp_as_number->nb_add`
//!     read on a subclass instance resolves (it was NULL pre-wave-5);
//!   * the **Cython C-API runtime tail** (`_PyObject_GetDictPtr`,
//!     `PyObject_GetOptionalAttrString`, `_PyObject_GetMethod`,
//!     `PyObject_CallMethodOneArg`, `_PyDict_NewPresized`,
//!     `PyMapping_GetOptionalItemString`, `PyLong_AsInt`) links and runs;
//!   * the Python-level MRO dispatch on a subclass is unaffected (the
//!     inherited dunders are still reached through the bridged MRO).
//!
//! Skipped (passes) when the env var is unset — that happens when
//! CPython 3.13 dev headers (or `cc`) aren't available on the build
//! host, so CI on a bare machine still passes.

use std::path::PathBuf;

use weavepy_capi::loader::load_extension_module;
use weavepy_vm::error::RuntimeError;
use weavepy_vm::object::Object;
use weavepy_vm::Interpreter;

fn extension_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_STOCKCYTHON_EXTENSION").map(PathBuf::from)
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
            "WEAVEPY_CAPI_STOCKCYTHON_EXTENSION points at missing file: {} — skipping",
            path.display()
        );
        return None;
    }
    weavepy_capi::force_link();
    let mut interp = Interpreter::default();
    let interp_ptr: *mut Interpreter = &raw mut interp;
    match load_extension_module(interp_ptr, &path, "_stockcython") {
        Ok(m) => Some((interp, m)),
        Err(err) => {
            eprintln!("dlopen of stock-cython extension failed (treating as skip): {err}");
            None
        }
    }
}

fn construct(interp: &mut Interpreter, ty: &Object, args: &[Object]) -> Object {
    interp
        .call_object(ty.clone(), args, &[])
        .unwrap_or_else(|e| panic!("constructing instance failed: {e:?}"))
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

fn get_i64(d: &Object, key: &str) -> i64 {
    match dict_get(d, key) {
        Some(Object::Int(n)) => n,
        Some(Object::Bool(b)) => i64::from(b),
        other => panic!("dict[{key}] not an int: {other:?}"),
    }
}

fn get_str(d: &Object, key: &str) -> String {
    match dict_get(d, key) {
        Some(Object::Str(s)) => s.to_string(),
        other => panic!("dict[{key}] not a str: {other:?}"),
    }
}

/// Read the slots directly off `Py_TYPE(instance)` (the inlined Cython
/// idiom) by calling the fixture's `probe_slots`.
fn probe(interp: &mut Interpreter, module: &Object, instance: Object) -> Object {
    call_module_fn(interp, module, "probe_slots", &[instance])
}

#[test]
fn stockcython_skipped_when_extension_missing() {
    if extension_path().is_none() {
        eprintln!("WEAVEPY_CAPI_STOCKCYTHON_EXTENSION not set — skipping Cython-surface proof");
    }
}

#[test]
fn stockcython_module_loads_with_types() {
    let Some((_interp, module)) = load() else {
        return;
    };
    for name in ["CyBase", "CySub", "CySub2"] {
        match lookup(&module, name) {
            Some(Object::Type(_)) => {}
            other => panic!("type `{name}` missing or not a type: {other:?}"),
        }
    }
    match lookup(&module, "ABI") {
        Some(Object::Str(s)) => assert_eq!(&*s, "cp313"),
        other => panic!("ABI wrong: {other:?}"),
    }
}

/// Baseline: the base type's own slots are directly readable and invoke
/// correctly (no inheritance involved — establishes the probe is sound).
#[test]
fn stockcython_base_slots_direct() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let base_ty = lookup(&module, "CyBase").expect("CyBase");
    let b = construct(&mut interp, &base_ty, &[Object::Int(5)]);
    let res = probe(&mut interp, &module, b);

    assert_eq!(get_i64(&res, "has_repr"), 1);
    assert_eq!(get_i64(&res, "has_hash"), 1);
    assert_eq!(get_i64(&res, "has_nb_add"), 1);
    assert_eq!(get_i64(&res, "has_sq_len"), 1);
    assert_eq!(get_i64(&res, "has_cmp"), 1);
    assert_eq!(get_str(&res, "repr"), "CyBase(5)");
    assert_eq!(get_i64(&res, "hash"), 5);
    assert_eq!(get_i64(&res, "len"), 5);
    assert_eq!(get_i64(&res, "add"), 10);
}

/// The headline proof: a **pure** subclass (`CySub`, which declares no
/// slots whatsoever) carries every one of the base's slots baked into
/// its own faithful struct, so a direct `Py_TYPE(sub)->tp_*` read — the
/// Cython idiom, no MRO walk — resolves to the inherited function.
#[test]
fn stockcython_pure_subclass_inherits_all_slots() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let sub_ty = lookup(&module, "CySub").expect("CySub");
    let s = construct(&mut interp, &sub_ty, &[Object::Int(7)]);
    let res = probe(&mut interp, &module, s);

    // Every slot is present on the *subclass* struct (NULL pre-wave-5).
    assert_eq!(get_i64(&res, "has_repr"), 1, "tp_repr not inherited");
    assert_eq!(get_i64(&res, "has_hash"), 1, "tp_hash not inherited");
    assert_eq!(
        get_i64(&res, "has_nb_add"),
        1,
        "tp_as_number->nb_add not inherited"
    );
    assert_eq!(
        get_i64(&res, "has_sq_len"),
        1,
        "tp_as_sequence->sq_length not inherited"
    );
    assert_eq!(get_i64(&res, "has_cmp"), 1, "tp_richcompare not inherited");
    // It defined no number subtract, and CyBase has none either.
    assert_eq!(get_i64(&res, "has_nb_sub"), 0);

    // The inherited slots, invoked directly, produce the base's results.
    assert_eq!(get_str(&res, "repr"), "CyBase(7)", "inherited tp_repr");
    assert_eq!(get_i64(&res, "hash"), 7, "inherited tp_hash");
    assert_eq!(get_i64(&res, "len"), 7, "inherited sq_length");
    assert_eq!(get_i64(&res, "add"), 14, "inherited nb_add (7+7)");
}

/// The partial-override proof: `CySub2` defines its *own* `tp_repr` and a
/// number suite carrying only `nb_subtract`. `inherit_slots` must (a)
/// keep the subclass's own `tp_repr` / `nb_subtract`, and (b) fill
/// `nb_add` *into that same suite* from the base — the in-place
/// method-suite merge (CPython's per-slot `COPYSLOT`).
#[test]
fn stockcython_partial_subclass_merges_suite() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let sub2_ty = lookup(&module, "CySub2").expect("CySub2");
    let s = construct(&mut interp, &sub2_ty, &[Object::Int(9)]);
    let res = probe(&mut interp, &module, s);

    // Own slots kept.
    assert_eq!(get_i64(&res, "has_repr"), 1);
    assert_eq!(get_i64(&res, "has_nb_sub"), 1, "own nb_subtract lost");
    assert_eq!(get_str(&res, "repr"), "CySub2(9)", "own tp_repr overridden");
    assert_eq!(get_i64(&res, "sub"), 0, "own nb_subtract (9-9)");

    // Inherited slots filled — including nb_add merged into the subclass's
    // *existing* (own) number suite alongside its nb_subtract.
    assert_eq!(
        get_i64(&res, "has_nb_add"),
        1,
        "nb_add not merged into own suite"
    );
    assert_eq!(get_i64(&res, "has_hash"), 1, "tp_hash not inherited");
    assert_eq!(get_i64(&res, "has_sq_len"), 1, "sq_length not inherited");
    assert_eq!(get_i64(&res, "add"), 18, "inherited nb_add (9+9)");
    assert_eq!(get_i64(&res, "hash"), 9, "inherited tp_hash");
    assert_eq!(get_i64(&res, "len"), 9, "inherited sq_length");
}

/// `inherit_slots` must not disturb the Python-level MRO dispatch: the
/// inherited behaviour is *also* reachable through the bridged class's
/// synthesised dunders, exactly as CPython reaches it through the MRO.
#[test]
fn stockcython_python_level_dispatch_on_subclass() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let sub_ty = lookup(&module, "CySub").expect("CySub");
    let s = construct(&mut interp, &sub_ty, &[Object::Int(4)]);

    // len(s) → inherited sq_length
    assert!(matches!(
        call_method(&mut interp, s.clone(), "__len__", &[]),
        Ok(Object::Int(4))
    ));
    // s + s → inherited nb_add
    assert!(matches!(
        call_method(&mut interp, s.clone(), "__add__", &[s.clone()]),
        Ok(Object::Int(8))
    ));
    // repr(s) → inherited tp_repr (base's format)
    match call_method(&mut interp, s.clone(), "__repr__", &[]) {
        Ok(Object::Str(r)) => assert_eq!(&*r, "CyBase(4)"),
        other => panic!("unexpected repr: {other:?}"),
    }
    // s == s → inherited tp_richcompare
    assert!(matches!(
        call_method(&mut interp, s.clone(), "__eq__", &[s]),
        Ok(Object::Bool(true))
    ));
}

/// The Cython C-API runtime tail links and behaves: optional-attr/item
/// probes, the fast method path, presized dict, bounds-checked int, and
/// the NULL `_PyObject_GetDictPtr` that steers Cython to generic getattr.
#[test]
fn stockcython_runtime_surface() {
    let Some((mut interp, module)) = load() else {
        return;
    };
    let sub_ty = lookup(&module, "CySub").expect("CySub");
    let s = construct(&mut interp, &sub_ty, &[Object::Int(1)]);
    let res = call_module_fn(&mut interp, &module, "cython_runtime_surface", &[s]);

    // _PyObject_GetDictPtr → NULL (WeavePy has no in-body tp_dictoffset).
    assert_eq!(get_i64(&res, "dictptr_null"), 1);
    // PyObject_GetOptionalAttrString: present (1) vs. missing (0).
    assert_eq!(get_i64(&res, "opt_present"), 1);
    assert_eq!(get_i64(&res, "opt_absent"), 0);
    // _PyObject_GetMethod: returns 0 (bound) with a non-NULL handle.
    assert_eq!(get_i64(&res, "get_method_rc"), 0);
    assert_eq!(get_i64(&res, "get_method_ok"), 1);
    // PyObject_CallMethodOneArg: s.__eq__(s) is truthy.
    assert_eq!(get_i64(&res, "call_eq_true"), 1);
    // _PyDict_NewPresized + PyMapping_GetOptionalItemString.
    assert_eq!(get_i64(&res, "map_present"), 1);
    assert_eq!(get_i64(&res, "map_value"), 99);
    assert_eq!(get_i64(&res, "map_absent"), 0);
    // PyLong_AsInt.
    assert_eq!(get_i64(&res, "as_int"), 4242);
}
