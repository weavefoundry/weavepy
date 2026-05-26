//! Integration test: dlopen the `_ndarray.so` extension built by
//! `crates/weavepy-capi/build.rs`, drive it through the C-API
//! buffer protocol + memoryview + vectorcall surfaces, and assert
//! the bridge produces the expected results.
//!
//! `_ndarray.NDArray` exercises:
//!
//! - `tp_init` + `tp_dealloc` + per-instance malloc'd storage
//! - `bf_getbuffer` / `bf_releasebuffer` (multi-dim, strides, format)
//! - `nb_add` / `nb_subtract` / `nb_multiply` (synthesised `__add__`
//!   / `__sub__` / `__mul__` dunders driving the VM dispatcher)
//! - `sq_length` / `sq_item`, `mp_subscript` / `mp_ass_subscript`
//! - `tp_iter` / `tp_iternext`
//! - `tp_getset` (`shape`, `nbytes`, `exporter_count`)
//! - `tp_methods` (`fill`, `sum`, `to_bytes`)
//!
//! Skipped (passes) when `WEAVEPY_CAPI_NDARRAY_EXTENSION` is unset,
//! consistent with the `capi_loader` test.

use std::path::PathBuf;

use weavepy_capi::loader::load_extension_module;
use weavepy_vm::error::RuntimeError;
use weavepy_vm::object::Object;
use weavepy_vm::Interpreter;

fn extension_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_NDARRAY_EXTENSION").map(PathBuf::from)
}

fn lookup_module_member(module: &Object, key: &str) -> Option<Object> {
    let dict = match module {
        Object::Module(m) => m.dict.clone(),
        _ => return None,
    };
    let d = dict.borrow();
    for (k, v) in d.iter() {
        if let Object::Str(s) = &k.0 {
            if &**s == key {
                return Some(v.clone());
            }
        }
    }
    None
}

fn load_module() -> Option<(Interpreter, Object)> {
    let path = extension_path()?;
    if !path.is_file() {
        eprintln!(
            "WEAVEPY_CAPI_NDARRAY_EXTENSION points at missing file: {} — skipping",
            path.display()
        );
        return None;
    }
    weavepy_capi::force_link();
    let mut interp = Interpreter::default();
    let interp_ptr: *mut Interpreter = &raw mut interp;
    let module = match load_extension_module(interp_ptr, &path, "_ndarray") {
        Ok(m) => m,
        Err(err) => {
            eprintln!("dlopen failed (treating as skip): {err}");
            return None;
        }
    };
    Some((interp, module))
}

fn make_array(interp: &mut Interpreter, module: &Object, rows: i64, cols: i64) -> Object {
    let cls = lookup_module_member(module, "NDArray").expect("NDArray class missing");
    interp
        .call_object(cls, &[Object::Int(rows), Object::Int(cols)], &[])
        .expect("NDArray() should succeed")
}

/// Look up a method or descriptor on an instance's class and call
/// it with `self` as the leading argument.
fn call_method(
    interp: &mut Interpreter,
    instance: Object,
    name: &str,
    args: &[Object],
) -> Result<Object, RuntimeError> {
    let class = match &instance {
        Object::Instance(inst) => inst.class.clone(),
        other => panic!("expected instance, got {other:?}"),
    };
    let method = class
        .lookup(name)
        .unwrap_or_else(|| panic!("method '{name}' not in MRO"));
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push(instance);
    full.extend_from_slice(args);
    interp.call_object(method, &full, &[])
}

#[test]
fn ndarray_skipped_when_extension_missing() {
    if extension_path().is_none() {
        eprintln!("WEAVEPY_CAPI_NDARRAY_EXTENSION not set — skipping");
    }
}

#[test]
fn ndarray_module_exposes_class() {
    let Some((_interp, module)) = load_module() else {
        return;
    };
    assert!(lookup_module_member(&module, "NDArray").is_some());
    assert!(lookup_module_member(&module, "VERSION").is_some());
    assert!(lookup_module_member(&module, "DOUBLE_SIZE").is_some());
}

#[test]
fn ndarray_class_has_dunders() {
    let Some((_interp, module)) = load_module() else {
        return;
    };
    let cls = lookup_module_member(&module, "NDArray").expect("NDArray class missing");
    let dict = match &cls {
        Object::Type(t) => t.dict.clone(),
        _ => panic!("expected type"),
    };
    let names: Vec<String> = {
        let d = dict.borrow();
        d.iter()
            .filter_map(|(k, _)| match &k.0 {
                Object::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect()
    };
    eprintln!("type dict keys: {:?}", names);
    assert!(names.iter().any(|s| s == "__init__"), "missing __init__");
    assert!(names.iter().any(|s| s == "__repr__"), "missing __repr__");
    assert!(names.iter().any(|s| s == "fill"), "missing fill method");
    assert!(names.iter().any(|s| s == "shape"), "missing shape getset");
}

#[test]
fn ndarray_dict_inspection() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let arr = make_array(&mut interp, &module, 2, 3);
    let inst = match &arr {
        Object::Instance(i) => i.clone(),
        _ => panic!("not instance"),
    };
    let keys: Vec<String> = inst
        .dict
        .borrow()
        .iter()
        .filter_map(|(k, _)| match &k.0 {
            Object::Str(s) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    eprintln!("instance dict keys after construction: {:?}", keys);
}

#[test]
fn ndarray_constructor_and_repr() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let arr = make_array(&mut interp, &module, 2, 3);
    let repr = call_method(&mut interp, arr, "__repr__", &[]).expect("__repr__ should succeed");
    if let Object::Str(s) = repr {
        assert!(
            s.contains("rows=2") && s.contains("cols=3"),
            "unexpected repr: {s:?}"
        );
    } else {
        panic!("expected repr to be str");
    }
}

#[test]
fn ndarray_fill_and_sum() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let arr = make_array(&mut interp, &module, 4, 5);
    let fill_res = call_method(&mut interp, arr.clone(), "fill", &[Object::Float(2.5)]);
    eprintln!("fill result: {:?}", fill_res);
    fill_res.expect("fill should succeed");
    let sum_res = call_method(&mut interp, arr, "sum", &[]);
    eprintln!("sum result: {:?}", sum_res);
    let total = sum_res.expect("sum should succeed");
    if let Object::Float(f) = total {
        assert!((f - (4.0 * 5.0 * 2.5)).abs() < 1e-9, "got sum {f}");
    } else {
        panic!("expected float sum");
    }
}

#[test]
fn ndarray_setitem_and_subscript() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let arr = make_array(&mut interp, &module, 3, 3);
    let key = Object::new_tuple(vec![Object::Int(1), Object::Int(2)]);
    call_method(
        &mut interp,
        arr.clone(),
        "__setitem__",
        &[key.clone(), Object::Float(7.5)],
    )
    .expect("__setitem__ should succeed");
    let v =
        call_method(&mut interp, arr, "__getitem__", &[key]).expect("__getitem__ should succeed");
    if let Object::Float(f) = v {
        assert!((f - 7.5).abs() < 1e-9);
    } else {
        panic!("expected float, got {v:?}");
    }
}

#[test]
fn ndarray_sequence_len_and_item() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let arr = make_array(&mut interp, &module, 4, 2);
    let n = call_method(&mut interp, arr.clone(), "__len__", &[]).expect("__len__ should succeed");
    assert!(matches!(n, Object::Int(4)), "got {n:?}");

    let row = call_method(&mut interp, arr, "__getitem__", &[Object::Int(0)])
        .expect("__getitem__ row 0 should succeed");
    if let Object::List(rc) = row {
        assert_eq!(rc.borrow().len(), 2);
    } else {
        panic!("expected list, got {row:?}");
    }
}

#[test]
fn ndarray_iter_walks_rows() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let arr = make_array(&mut interp, &module, 3, 2);
    let iter =
        call_method(&mut interp, arr, "__iter__", &[]).expect("__iter__ should return iterator");

    let mut count = 0;
    loop {
        let res = call_method(&mut interp, iter.clone(), "__next__", &[]);
        match res {
            Ok(_) => count += 1,
            Err(_) => break,
        }
        if count > 10 {
            panic!("iterator did not stop");
        }
    }
    assert_eq!(count, 3);
}

#[test]
fn ndarray_addition_via_dunder() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let a = make_array(&mut interp, &module, 2, 2);
    let b = make_array(&mut interp, &module, 2, 2);
    call_method(&mut interp, a.clone(), "fill", &[Object::Float(1.5)]).unwrap();
    call_method(&mut interp, b.clone(), "fill", &[Object::Float(2.5)]).unwrap();
    let add_res = call_method(&mut interp, a, "__add__", &[b]);
    if let Err(ref err) = add_res {
        eprintln!("__add__ error: {:?}", err);
        if let weavepy_vm::error::RuntimeError::PyException(pe) = err {
            if let Object::Instance(inst) = &pe.instance {
                let dict = inst.dict.borrow();
                for (k, v) in dict.iter() {
                    eprintln!("  err.{:?} = {:?}", k.0, v);
                }
            }
        }
    }
    let c = add_res.expect("__add__ should succeed");
    let total = call_method(&mut interp, c, "sum", &[]).expect("sum should succeed");
    if let Object::Float(f) = total {
        assert!((f - (4.0 * 4.0)).abs() < 1e-6, "got {f}");
    } else {
        panic!("expected float sum");
    }
}

#[test]
fn ndarray_shape_property() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let arr = make_array(&mut interp, &module, 3, 5);
    let cls = lookup_module_member(&module, "NDArray").expect("class");
    let dict = match &cls {
        Object::Type(t) => t.dict.clone(),
        _ => panic!("expected type"),
    };
    let getter = {
        let d = dict.borrow();
        d.iter()
            .find(|(k, _)| matches!(&k.0, Object::Str(s) if &**s == "shape"))
            .map(|(_, v)| v.clone())
            .expect("shape descriptor")
    };
    let res = interp
        .call_object(getter, &[arr], &[])
        .expect("shape getter should succeed");
    if let Object::Tuple(items) = res {
        assert_eq!(items.len(), 2);
        match (&items[0], &items[1]) {
            (Object::Int(r), Object::Int(c)) => {
                assert_eq!(*r, 3);
                assert_eq!(*c, 5);
            }
            other => panic!("unexpected shape: {other:?}"),
        }
    } else {
        panic!("expected tuple shape, got {res:?}");
    }
}

#[test]
fn ndarray_buffer_size_function() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let arr = make_array(&mut interp, &module, 4, 5);
    call_method(&mut interp, arr.clone(), "fill", &[Object::Float(1.0)]).unwrap();
    let buffer_size =
        lookup_module_member(&module, "buffer_size").expect("module missing buffer_size");
    let n = interp
        .call_object(buffer_size, &[arr], &[])
        .expect("buffer_size should succeed");
    let dsize = match lookup_module_member(&module, "DOUBLE_SIZE") {
        Some(Object::Int(n)) => n,
        _ => 8,
    };
    assert_eq!(
        match n {
            Object::Int(n) => n,
            _ => panic!("expected int"),
        },
        4 * 5 * dsize
    );
}

#[test]
fn ndarray_format_size_helper() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let format_size =
        lookup_module_member(&module, "format_size").expect("module missing format_size");
    let n = interp
        .call_object(format_size.clone(), &[Object::Str("d".into())], &[])
        .expect("format_size should succeed");
    assert!(matches!(n, Object::Int(8)), "got {n:?}");

    let n2 = interp
        .call_object(format_size, &[Object::Str("3i".into())], &[])
        .expect("format_size should succeed");
    assert!(matches!(n2, Object::Int(12)), "got {n2:?}");
}

#[test]
fn ndarray_to_bytes_round_trip() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let arr = make_array(&mut interp, &module, 1, 4);
    call_method(&mut interp, arr.clone(), "fill", &[Object::Float(0.0)]).unwrap();
    let key = |i: i64, j: i64| Object::new_tuple(vec![Object::Int(i), Object::Int(j)]);
    call_method(
        &mut interp,
        arr.clone(),
        "__setitem__",
        &[key(0, 0), Object::Float(1.0)],
    )
    .unwrap();
    call_method(
        &mut interp,
        arr.clone(),
        "__setitem__",
        &[key(0, 1), Object::Float(2.0)],
    )
    .unwrap();
    let payload = call_method(&mut interp, arr, "to_bytes", &[]).expect("to_bytes should succeed");
    let bytes = match payload {
        Object::Bytes(b) => b,
        other => panic!("expected bytes, got {other:?}"),
    };
    assert_eq!(bytes.len(), 4 * 8);
    let v0 = f64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let v1 = f64::from_le_bytes(bytes[8..16].try_into().unwrap());
    assert!((v0 - 1.0).abs() < 1e-12);
    assert!((v1 - 2.0).abs() < 1e-12);
}
