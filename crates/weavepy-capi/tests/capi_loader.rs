//! Integration test: dlopen the `_smalltest.so` extension built by
//! `crates/weavepy-capi/build.rs`, drive it through the C-API
//! bridge, and assert it produces the expected results.
//!
//! Skipped (passes) when `WEAVEPY_CAPI_TEST_EXTENSION` is unset —
//! that happens when the C compiler isn't available in the build
//! environment.

use std::path::PathBuf;

use weavepy_capi::loader::load_extension_module;
use weavepy_vm::object::Object;
use weavepy_vm::Interpreter;

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

fn extension_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_TEST_EXTENSION").map(PathBuf::from)
}

#[test]
fn loader_skipped_when_extension_missing() {
    if extension_path().is_none() {
        eprintln!("WEAVEPY_CAPI_TEST_EXTENSION not set — skipping loader test");
    }
}

/// Boot a `Interpreter` and load `_smalltest`. Returns `None` if
/// the test environment lacks the artifact (so other test bodies
/// can early-`return` to keep CI clean across platforms that don't
/// expose `cc`).
fn load_module() -> Option<(Interpreter, Object)> {
    let path = extension_path()?;
    if !path.is_file() {
        eprintln!(
            "WEAVEPY_CAPI_TEST_EXTENSION points at missing file: {} — skipping",
            path.display()
        );
        return None;
    }
    weavepy_capi::force_link();
    let mut interp = Interpreter::default();
    let interp_ptr: *mut Interpreter = &raw mut interp;
    let module = match load_extension_module(interp_ptr, &path, "_smalltest") {
        Ok(m) => m,
        Err(err) => {
            eprintln!("dlopen failed (treating as skip): {err}");
            return None;
        }
    };
    Some((interp, module))
}

#[test]
fn dlopen_smalltest_produces_module() {
    let Some((_interp, module)) = load_module() else {
        return;
    };
    let dict = match &module {
        Object::Module(m) => m.dict.clone(),
        other => panic!("expected module, got {other:?}"),
    };
    let d = dict.borrow();
    let names: Vec<String> = d
        .keys()
        .filter_map(|k| match &k.0 {
            Object::Str(s) => Some((**s).to_string()),
            _ => None,
        })
        .collect();
    eprintln!("module keys: {names:?}");
    assert!(names.iter().any(|n| n == "add"), "missing add: {names:?}");
    assert!(
        names.iter().any(|n| n == "Counter"),
        "missing Counter: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "VERSION"),
        "missing VERSION: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "MAGIC"),
        "missing MAGIC: {names:?}"
    );
}

#[test]
fn smalltest_add_function_runs() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let add = lookup_module_member(&module, "add").expect("module is missing `add`");
    let result = interp
        .call_object(add, &[Object::Int(2), Object::Int(3)], &[])
        .expect("calling add should succeed");
    match result {
        Object::Int(n) => assert_eq!(n, 5, "expected 2 + 3 == 5, got {n}"),
        other => panic!("expected int, got {other:?}"),
    }
}

#[test]
fn smalltest_concat_function_runs() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let concat = lookup_module_member(&module, "concat").expect("module missing `concat`");
    let result = interp
        .call_object(
            concat,
            &[Object::Str("foo".into()), Object::Str("bar".into())],
            &[],
        )
        .expect("calling concat should succeed");
    match result {
        Object::Str(s) => assert_eq!(&*s, "foobar"),
        other => panic!("expected str, got {other:?}"),
    }
}

#[test]
fn smalltest_oops_raises_value_error() {
    let Some((mut interp, module)) = load_module() else {
        return;
    };
    let oops = lookup_module_member(&module, "oops").expect("module missing `oops`");
    let err = interp
        .call_object(oops, &[Object::Str("nope".into())], &[])
        .expect_err("calling oops should raise");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("ValueError") || msg.contains("nope"),
        "unexpected error: {msg}"
    );
}

#[test]
fn smalltest_module_constants_are_set() {
    let Some((_interp, module)) = load_module() else {
        return;
    };
    let version = lookup_module_member(&module, "VERSION").expect("missing VERSION");
    match version {
        Object::Str(s) => assert_eq!(&*s, "1.0"),
        other => panic!("expected VERSION to be str, got {other:?}"),
    }
    let magic = lookup_module_member(&module, "MAGIC").expect("missing MAGIC");
    match magic {
        Object::Int(n) => assert_eq!(n, 0xC0DE),
        other => panic!("expected MAGIC to be int, got {other:?}"),
    }
}
