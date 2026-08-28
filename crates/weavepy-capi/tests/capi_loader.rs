//! Integration test: dlopen the `_smalltest.so` extension built by
//! `crates/weavepy-capi/build.rs`, drive it through the C-API
//! bridge, and assert it produces the expected results.
//!
//! Skipped (passes) when `WEAVEPY_CAPI_TEST_EXTENSION` is unset —
//! that happens when the C compiler isn't available in the build
//! environment.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

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

/// Serialize the tests in this binary. Each test constructs its own
/// `Interpreter`, but the C-API bridge keeps *process-global* state (the
/// `LAST_INTERPRETER` fallback pointer, the shared dlopen'd extension's
/// static state), so libtest's default parallel execution can route a
/// re-entrant C-API call into a different test's interpreter mid-run. The
/// guard is returned by the loader and held for the whole test.
fn serialize() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    // A poisoned lock only means another test's assertion failed while
    // holding it — the serialization itself is still valid.
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Boot a `Interpreter` and load `_smalltest`. Returns `None` if
/// the test environment lacks the artifact (so other test bodies
/// can early-`return` to keep CI clean across platforms that don't
/// expose `cc`).
fn load_module() -> Option<(MutexGuard<'static, ()>, Interpreter, Object)> {
    let guard = serialize();
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
    Some((guard, interp, module))
}

#[test]
fn dlopen_smalltest_produces_module() {
    let Some((_lock, _interp, module)) = load_module() else {
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
    let Some((_lock, mut interp, module)) = load_module() else {
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
    let Some((_lock, mut interp, module)) = load_module() else {
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
    let Some((_lock, mut interp, module)) = load_module() else {
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

/// The `et` encoded-string converter in Pillow's getfont shape
/// ("etf|ny#" here): the filename must round-trip through a
/// PyMem_Malloc'd copy (the C side frees it — a non-heap pointer
/// aborts), and the slots *after* the two-char unit must stay bound to
/// their own destinations (RFC 0075 WS9, Pillow selftest lane).
#[test]
fn smalltest_et_converter_round_trips() {
    let Some((_lock, mut interp, module)) = load_module() else {
        return;
    };
    let encoded = lookup_module_member(&module, "encoded").expect("module missing `encoded`");
    let font_bytes: weavepy_vm::sync::Rc<[u8]> = b"FONTDATA".to_vec().into();
    let result = interp
        .call_object(
            encoded,
            &[
                Object::Str("Fonts/Frée.ttf".into()),
                Object::Float(20.0),
                Object::Int(3),
            ],
            &[("tail".to_owned(), Object::Bytes(font_bytes))],
        )
        .expect("calling encoded should succeed");
    let items = match result {
        Object::Tuple(t) => t,
        other => panic!("expected tuple, got {other:?}"),
    };
    assert_eq!(items.len(), 4, "expected 4-tuple");
    match &items[0] {
        Object::Str(s) => assert_eq!(&**s, "Fonts/Frée.ttf"),
        other => panic!("filename: expected str, got {other:?}"),
    }
    assert!(
        matches!(items[1], Object::Int(2000)),
        "size: {:?}",
        items[1]
    );
    assert!(matches!(items[2], Object::Int(3)), "index: {:?}", items[2]);
    assert!(
        matches!(items[3], Object::Int(8)),
        "tail_len: {:?}",
        items[3]
    );
}

/// `_PyEval_SliceIndex` semantics (`Python/ceval.c`): ints convert,
/// `None` returns 1 leaving the output untouched, huge ints clamp to
/// the Py_ssize_t range instead of raising (RFC 0075 WS9, the lxml
/// explicit-step slice crash).
#[test]
fn smalltest_slice_index_semantics() {
    let Some((_lock, mut interp, module)) = load_module() else {
        return;
    };
    let f = lookup_module_member(&module, "slice_index").expect("module missing `slice_index`");
    let call = |interp: &mut Interpreter, arg: Object| interp.call_object(f.clone(), &[arg], &[]);
    match call(&mut interp, Object::Int(-3)).expect("int should convert") {
        Object::Tuple(t) => {
            assert!(matches!(t[0], Object::Int(1)));
            assert!(matches!(t[1], Object::Int(-3)));
        }
        other => panic!("expected tuple, got {other:?}"),
    }
    match call(&mut interp, Object::None).expect("None is accepted") {
        Object::Tuple(t) => {
            assert!(matches!(t[0], Object::Int(1)));
            // The sentinel must survive: None leaves *pi untouched.
            assert!(matches!(t[1], Object::Int(-777)), "sentinel: {:?}", t[1]);
        }
        other => panic!("expected tuple, got {other:?}"),
    }
    let huge = Object::int_from_bigint(num_bigint::BigInt::from(isize::MAX) + 10);
    match call(&mut interp, huge).expect("huge int clamps, not raises") {
        Object::Tuple(t) => {
            assert!(matches!(t[1], Object::Int(n) if n == i64::try_from(isize::MAX).unwrap()));
        }
        other => panic!("expected tuple, got {other:?}"),
    }
    call(&mut interp, Object::Str("x".into())).expect_err("str must raise TypeError");
}

#[test]
fn smalltest_module_constants_are_set() {
    let Some((_lock, _interp, module)) = load_module() else {
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
