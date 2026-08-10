//! Native stand-in for CPython's `_testinternalcapi` C test helper.
//!
//! CPython's regression suite imports this extension to observe
//! interpreter internals. WeavePy implements the handful of probes the
//! conformance targets use, mapped onto *our* equivalent internal
//! state rather than faked answers:
//!
//! - `has_inline_values(obj)` — CPython 3.13 reports whether an
//!   instance's attributes still live in the object's inline value
//!   array (no materialised dict escape). WeavePy instances always
//!   carry a dict, but the *observable lifecycle* CPython tests —
//!   fresh managed-dict instances are inline, `del obj.__dict__` /
//!   `obj.__dict__ = d` and attribute-count blowups de-inline — is
//!   tracked faithfully via [`PyInstance::inline_values`] plus a
//!   capacity check mirroring CPython's shared-keys limit (30).

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::RuntimeError;
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

use parking_lot::{Condvar, Mutex};
use std::sync::Arc;
use std::thread::JoinHandle;

/// CPython's `SHARED_KEYS_MAX_SIZE`: instances whose dict outgrows the
/// shared-keys capacity stop using inline values.
const INLINE_CAPACITY: usize = 30;

/// Delegate a `_testinternalcapi` entry point to the frozen
/// `_weave_iseq` helper module (the instruction-sequence fixture —
/// `new_instruction_sequence` / `assemble_code_object`, RFC 0060 WS1).
/// Runs through the live interpreter: the helper is plain Python.
fn iseq_call(
    name: &'static str,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
        RuntimeError::Internal("_testinternalcapi: no running interpreter".to_owned())
    })?;
    // SAFETY: published by an enclosing VM frame still live on this
    // thread; the GIL keeps the access exclusive.
    let vm = unsafe { &mut *ptr };
    let builtins = vm.builtins_dict();
    let import_fn = builtins
        .borrow()
        .get(&DictKey(Object::from_static("__import__")))
        .cloned()
        .ok_or_else(|| RuntimeError::Internal("_testinternalcapi: no __import__".to_owned()))?;
    let module = vm.call_object_with_globals(
        &import_fn,
        &[Object::from_static("_weave_iseq")],
        &[],
        &builtins,
    )?;
    let f = vm.load_attr_public(&module, name)?;
    vm.call_object_with_globals(&f, args, kwargs, &builtins)
}

/// A raw, non-Python OS thread spawned by `_spawn_pthread_waiter` that simply
/// blocks until `_end_spawned_pthread` releases it. It deliberately bypasses
/// WeavePy's `_thread`/`threading` machinery so it is invisible to
/// `threading.enumerate()`/`active_count()` — exactly like the raw `pthread`
/// CPython's `_testcapi._spawn_pthread_waiter` creates. Its sole observable
/// effect is bumping the live OS-thread count, which `os.fork()` detects to
/// emit the multi-threaded-fork `DeprecationWarning`
/// (`test_os.ForkTests.test_fork_warns_when_non_python_thread_exists`).
struct PthreadWaiter {
    handle: JoinHandle<()>,
    stop: Arc<WaiterGate>,
}

struct WaiterGate {
    flag: Mutex<bool>,
    cv: Condvar,
}

/// The currently-live raw waiter, if any. A process-global `parking_lot::Mutex`
/// (not the VM's `Rc`-based cells) so the spawn/end pair can stash and reclaim
/// the `JoinHandle` across calls.
static WAITER: Mutex<Option<PthreadWaiter>> = Mutex::new(None);

/// `_testcapi._spawn_pthread_waiter()` — create one raw OS thread that parks
/// until `_end_spawned_pthread()`. Spawning a second without ending the first
/// raises, matching the C helper's single-slot contract.
fn spawn_pthread_waiter(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut slot = WAITER.lock();
    if slot.is_some() {
        return Err(crate::error::runtime_error(
            "_spawn_pthread_waiter: a waiter thread is already running",
        ));
    }
    let gate = Arc::new(WaiterGate {
        flag: Mutex::new(false),
        cv: Condvar::new(),
    });
    let gate_for_thread = gate.clone();
    let handle = std::thread::Builder::new()
        .name("testcapi-pthread-waiter".to_owned())
        .spawn(move || {
            let mut stopped = gate_for_thread.flag.lock();
            while !*stopped {
                gate_for_thread.cv.wait(&mut stopped);
            }
        })
        .map_err(|e| crate::error::runtime_error(format!("_spawn_pthread_waiter: {e}")))?;
    *slot = Some(PthreadWaiter { handle, stop: gate });
    Ok(Object::None)
}

/// `_testcapi._end_spawned_pthread()` — signal the parked waiter to exit and
/// join it. A no-op if no waiter is live (so a `finally:` cleanup is safe even
/// when spawning failed).
fn end_spawned_pthread(_args: &[Object]) -> Result<Object, RuntimeError> {
    let waiter = WAITER.lock().take();
    if let Some(w) = waiter {
        {
            let mut flag = w.stop.flag.lock();
            *flag = true;
            w.stop.cv.notify_all();
        }
        let _ = w.handle.join();
    }
    Ok(Object::None)
}

/// Read a type's attribute-resolution version counter (WeavePy's analogue
/// of CPython's `tp_version_tag` invalidation signal). The frozen
/// `_testcapi` shim derives its `type_get_version`/`type_modified` family
/// from this: a class-dict or MRO change bumps the counter, which the shim
/// treats as "version tag reset to 0" (test_type_cache).
fn type_attr_version(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(Object::Type(t)) => Ok(Object::Int(i64::from(t.attr_version.get()))),
        _ => Err(crate::error::type_error("argument must be a type")),
    }
}

/// `_testcapi.fatal_error(message, release_gil=False)`: invoke
/// `Py_FatalError` with the C-side function name CPython's helper reports
/// (`_testcapi_fatal_error_impl`). Never returns — the process dumps a
/// traceback to stderr and aborts. `release_gil` only changes *when* the
/// GIL is dropped in CPython; the observable output is identical.
fn fatal_error(args: &[Object]) -> Result<Object, RuntimeError> {
    let msg = match args.first() {
        Some(Object::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        Some(Object::Str(s)) => s.as_ref().to_owned(),
        _ => {
            return Err(crate::error::type_error(
                "fatal_error() argument 1 must be bytes",
            ))
        }
    };
    crate::stdlib::faulthandler_mod::py_fatal_error("_testcapi_fatal_error_impl", &msg)
}

fn has_inline_values(args: &[Object]) -> Result<Object, RuntimeError> {
    let inline = match args.first() {
        Some(Object::Instance(inst)) => {
            inst.cls().has_managed_dict()
                && !inst.cls().has_var_sized_base()
                && inst.inline_values.get()
                && inst.dict.borrow().len() <= INLINE_CAPACITY
        }
        _ => false,
    };
    Ok(Object::Bool(inline))
}

/// Pull an attribute off a `RuntimeError::PyException`'s instance dict
/// (the Unicode error objects store `start`/`reason` there directly).
fn exc_attr(err: &RuntimeError, name: &'static str) -> Option<Object> {
    match err {
        RuntimeError::PyException(pyexc) => match &pyexc.instance {
            Object::Instance(inst) => inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_static(name)))
                .cloned(),
            _ => None,
        },
        _ => None,
    }
}

/// `_Py_EncodeLocaleEx`/`_Py_DecodeLocaleEx` only speak these handlers;
/// anything else is `ValueError('unsupported error handler')`.
fn locale_check_handler(errors: &str) -> Result<(), RuntimeError> {
    match errors {
        "strict" | "surrogateescape" | "surrogatepass" => Ok(()),
        _ => Err(crate::error::value_error("unsupported error handler")),
    }
}

/// `EncodeLocaleEx(text, current_locale, errors)` — encode with the
/// filesystem encoding (UTF-8), reporting failures as CPython's
/// `RuntimeError("encode error: pos=N, reason=...")`.
fn encode_locale_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let cps: Vec<u32> = match args.first() {
        Some(Object::Str(s)) => s.chars().map(|c| c as u32).collect(),
        Some(Object::WStr(w)) => w.to_vec(),
        _ => {
            return Err(crate::error::type_error(
                "EncodeLocaleEx() argument 'unicode' must be str",
            ))
        }
    };
    let errors = match args.get(2) {
        Some(Object::Str(s)) => s.to_string(),
        None => "strict".to_owned(),
        _ => return Err(crate::error::type_error("errors must be str")),
    };
    locale_check_handler(&errors)?;
    match crate::stdlib::codecs_engine::utf8_encode(&cps, &errors) {
        Ok(b) => Ok(Object::Bytes(Rc::from(b.into_boxed_slice()))),
        Err(e) => {
            let pos = match exc_attr(&e, "start") {
                Some(Object::Int(i)) => i,
                _ => 0,
            };
            let reason = match exc_attr(&e, "reason") {
                Some(Object::Str(s)) => s.to_string(),
                _ => "encoding error".to_owned(),
            };
            Err(crate::error::runtime_error(format!(
                "encode error: pos={pos}, reason={reason}"
            )))
        }
    }
}

/// `DecodeLocaleEx(encoded, current_locale, errors)` — the decode
/// counterpart of [`encode_locale_ex`].
fn decode_locale_ex(args: &[Object]) -> Result<Object, RuntimeError> {
    let data = match args.first().and_then(|o| o.as_bytes_view()) {
        Some(b) => b,
        None => {
            return Err(crate::error::type_error(
                "DecodeLocaleEx() argument 'str' must be bytes",
            ))
        }
    };
    let errors = match args.get(2) {
        Some(Object::Str(s)) => s.to_string(),
        None => "strict".to_owned(),
        _ => return Err(crate::error::type_error("errors must be str")),
    };
    locale_check_handler(&errors)?;
    match crate::stdlib::codecs_engine::utf8_decode(&data, &errors, true) {
        Ok((obj, _)) => Ok(obj),
        Err(e) => {
            let pos = match exc_attr(&e, "start") {
                Some(Object::Int(i)) => i,
                _ => 0,
            };
            let reason = match exc_attr(&e, "reason") {
                Some(Object::Str(s)) => s.to_string(),
                _ => "decoding error".to_owned(),
            };
            Err(crate::error::runtime_error(format!(
                "decode error: pos={pos}, reason={reason}"
            )))
        }
    }
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_testinternalcapi"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("WeavePy stand-in for CPython internal-API test probes."),
        );
        d.insert(
            DictKey(Object::from_static("fatal_error")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "fatal_error",
                binds_instance: false,
                call: Box::new(fatal_error),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("_type_attr_version")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_type_attr_version",
                binds_instance: false,
                call: Box::new(type_attr_version),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("has_inline_values")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "has_inline_values",
                binds_instance: false,
                call: Box::new(has_inline_values),
                call_kw: None,
            })),
        );
        // Raw-`pthread` spawn/join helpers re-exported by the frozen
        // `_testcapi` shim. These create a genuine non-Python OS thread so
        // `os.fork()`'s multi-threaded-fork `DeprecationWarning` fires even
        // though `threading` never sees the thread
        // (`test_os.test_fork_warns_when_non_python_thread_exists`).
        d.insert(
            DictKey(Object::from_static("_spawn_pthread_waiter")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_spawn_pthread_waiter",
                binds_instance: false,
                call: Box::new(spawn_pthread_waiter),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("_end_spawned_pthread")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_end_spawned_pthread",
                binds_instance: false,
                call: Box::new(end_spawned_pthread),
                call_kw: None,
            })),
        );
        // `_Py_EncodeLocaleEx`/`_Py_DecodeLocaleEx` probes
        // (`test_codecs.LocaleCodecTest`): the filesystem-encoding
        // (UTF-8) coders with the locale-restricted handler set.
        d.insert(
            DictKey(Object::from_static("EncodeLocaleEx")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "EncodeLocaleEx",
                binds_instance: false,
                call: Box::new(encode_locale_ex),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("DecodeLocaleEx")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "DecodeLocaleEx",
                binds_instance: false,
                call: Box::new(decode_locale_ex),
                call_kw: None,
            })),
        );
        // `_PyTraceMalloc_GetTraceback(domain, ptr)` — the traceback of a
        // domain-tracked block, or None (`test_tracemalloc.TestCAPI`).
        d.insert(
            DictKey(Object::from_static("_PyTraceMalloc_GetTraceback")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_PyTraceMalloc_GetTraceback",
                binds_instance: false,
                call: Box::new(tracemalloc_get_traceback),
                call_kw: None,
            })),
        );
        // `get_recursion_depth()` — the live Python call depth on this
        // thread, read straight off the RFC 0037 recursion guard.
        // `test.support.get_recursion_depth()`/`infinite_recursion()` use it
        // to size `sys.setrecursionlimit` windows (RFC 0048).
        // RFC 0060 WS1 — the compiler assemble-stage fixtures
        // (`test_compiler_assemble` via bytecode_helper's
        // AssemblerTestCase). Implemented in the frozen `_weave_iseq`
        // helper; these entries just delegate.
        d.insert(
            DictKey(Object::from_static("new_instruction_sequence")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "new_instruction_sequence",
                binds_instance: false,
                call: Box::new(|args| iseq_call("new_instruction_sequence", args, &[])),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("assemble_code_object")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "assemble_code_object",
                binds_instance: false,
                call: Box::new(|args| iseq_call("assemble_code_object", args, &[])),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("get_recursion_depth")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "get_recursion_depth",
                binds_instance: false,
                call: Box::new(|_args| {
                    // The builtin call itself does not hold a guard, so the
                    // depth here is the caller's frame depth. `test.support`
                    // subtracts one for its own frame; mirror CPython by
                    // reporting the count including the caller.
                    let depth = crate::recursion::current_depth().max(1);
                    Ok(Object::Int(depth as i64))
                }),
                call_kw: None,
            })),
        );
    }
    {
        let mut d = dict.borrow_mut();
        // `get_config()` — the runtime-config dict `test.support` probes.
        // WeavePy ships PEP 657 column positions (RFC 0033/0037), so
        // `code_debug_ranges` is truthfully 1.
        d.insert(
            DictKey(Object::from_static("get_config")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "get_config",
                binds_instance: false,
                call: Box::new(|_args| {
                    let cfg = Rc::new(RefCell::new(DictData::default()));
                    {
                        let mut c = cfg.borrow_mut();
                        c.insert(
                            DictKey(Object::from_static("code_debug_ranges")),
                            Object::Int(1),
                        );
                    }
                    Ok(Object::Dict(cfg))
                }),
                call_kw: None,
            })),
        );
        // Immortalization knobs: WeavePy has no deferred-immortalization
        // pass, so suppression is a sound no-op and nothing is deferred.
        d.insert(
            DictKey(Object::from_static("suppress_immortalization")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "suppress_immortalization",
                binds_instance: false,
                call: Box::new(|_args| Ok(Object::None)),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("get_immortalize_deferred")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "get_immortalize_deferred",
                binds_instance: false,
                call: Box::new(|_args| Ok(Object::Bool(false))),
                call_kw: None,
            })),
        );
        // The `_PyTime_t` conversion API (`Python/pytime.c`), exercised
        // exhaustively by `test_time.TestCPyTime`/`TestOldPyTime`. WeavePy's
        // timestamps are i64 nanoseconds like CPython's PyTime_t, so these
        // are exact ports of the C rounding/overflow arithmetic.
        d.insert(
            DictKey(Object::from_static("SIZEOF_TIME_T")),
            Object::Int(std::mem::size_of::<libc::time_t>() as i64),
        );
        for (name, f) in [
            (
                "_PyTime_FromSeconds",
                pytime_from_seconds as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("_PyTime_FromSecondsObject", pytime_from_seconds_object),
            ("_PyTime_AsTimeval", pytime_as_timeval),
            ("_PyTime_AsMilliseconds", pytime_as_milliseconds),
            ("_PyTime_AsMicroseconds", pytime_as_microseconds),
            ("_PyTime_ObjectToTime_t", pytime_object_to_time_t),
            ("_PyTime_ObjectToTimeval", pytime_object_to_timeval),
            ("_PyTime_ObjectToTimespec", pytime_object_to_timespec),
        ] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name,
                    binds_instance: false,
                    call: Box::new(f),
                    call_kw: None,
                })),
            );
        }
    }
    {
        let mut d = dict.borrow_mut();
        // RFC 0060 — rare-event counters (`test_optimizer.
        // TestRareEventCounters`): live values of the five interpreter
        // deopt-trigger counters, plus the reset the test's `setUp` calls.
        d.insert(
            DictKey(Object::from_static("get_rare_event_counters")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "get_rare_event_counters",
                binds_instance: false,
                call: Box::new(|_args| {
                    let out = Rc::new(RefCell::new(DictData::default()));
                    {
                        let mut o = out.borrow_mut();
                        let counts = crate::rare_events::snapshot();
                        for (name, n) in crate::rare_events::NAMES.iter().zip(counts) {
                            o.insert(DictKey(Object::from_static(name)), Object::Int(n as i64));
                        }
                    }
                    Ok(Object::Dict(out))
                }),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("reset_rare_event_counters")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "reset_rare_event_counters",
                binds_instance: false,
                call: Box::new(|_args| {
                    crate::rare_events::reset();
                    Ok(Object::None)
                }),
                call_kw: None,
            })),
        );
        // `_PyInterpreterState_SetEvalFrameFunc` probes: install/remove the
        // recording frame evaluator. Both count as a `set_eval_frame_func`
        // rare event, exactly like CPython's C helpers.
        d.insert(
            DictKey(Object::from_static("set_eval_frame_record")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "set_eval_frame_record",
                binds_instance: false,
                call: Box::new(set_eval_frame_record),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("set_eval_frame_default")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "set_eval_frame_default",
                binds_instance: false,
                call: Box::new(|_args| {
                    crate::trace::set_eval_frame_default();
                    crate::rare_events::bump(crate::rare_events::SET_EVAL_FRAME_FUNC);
                    Ok(Object::None)
                }),
                call_kw: None,
            })),
        );
        // `_Py_normalize_path` (Python/fileutils.c) — lexical path
        // normalization with posixpath.normpath semantics
        // (`test_fileutils.test_capi_normalize_path`).
        d.insert(
            DictKey(Object::from_static("normalize_path")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "normalize_path",
                binds_instance: false,
                call: Box::new(normalize_path),
                call_kw: None,
            })),
        );
        // PEP 509 `ma_version_tag` probe, re-exported by the `_testcapi`
        // shim for `test_dict_version`.
        d.insert(
            DictKey(Object::from_static("dict_get_version")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "dict_get_version",
                binds_instance: false,
                call: Box::new(dict_get_version),
                call_kw: None,
            })),
        );
        // PEP 590 vectorcall fixture types + heap-type factory
        // (test_call.TestPEP590), re-exported by the `_testcapi` shim.
        for (name, ty) in crate::stdlib::testcapi_call::method_descriptor_types() {
            d.insert(DictKey(Object::from_static(name)), Object::Type(ty));
        }
        d.insert(
            DictKey(Object::from_static("make_vectorcall_class")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "make_vectorcall_class",
                binds_instance: false,
                call: Box::new(crate::stdlib::testcapi_call::make_vectorcall_class),
                call_kw: None,
            })),
        );
        // PEP 669 C-API fire primitives (`PyMonitoring_EnterScope` /
        // `PyMonitoring_Fire*Event`), re-exported by the `_testcapi`
        // shim for test_monitoring.TestCApiEventGeneration.
        crate::stdlib::testcapi_monitoring::install(&mut d);
    }
    Rc::new(PyModule {
        name: "_testinternalcapi".to_owned(),
        filename: None,
        dict,
    })
}

/// `set_eval_frame_record(list)` — install the recording frame evaluator:
/// until `set_eval_frame_default()`, every frame evaluation appends its
/// code object to `list`.
fn set_eval_frame_record(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(l @ Object::List(_)) => {
            crate::trace::set_eval_frame_record(l.clone());
            crate::rare_events::bump(crate::rare_events::SET_EVAL_FRAME_FUNC);
            Ok(Object::None)
        }
        _ => Err(crate::error::type_error(
            "set_eval_frame_record expected a list",
        )),
    }
}

/// `_Py_normalize_path(path)` — CPython's C-side lexical normalization
/// (`Python/fileutils.c`), asserted against `posixpath.normpath` for
/// absolute inputs by `test_fileutils`. Straight port of the normpath
/// algorithm: collapse slash runs and `.`, resolve `..` lexically, and
/// keep exactly-two leading slashes distinct (POSIX implementation-
/// defined `//` roots).
fn normalize_path(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(Object::Str(path)) = args.first() else {
        return Err(crate::error::type_error("normalize_path expected str"));
    };
    let path: &str = path.as_ref();
    if path.is_empty() {
        return Ok(Object::from_static("."));
    }
    let initial_slashes = if path.starts_with('/') {
        if path.starts_with("//") && !path.starts_with("///") {
            2
        } else {
            1
        }
    } else {
        0
    };
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." || (initial_slashes == 0 && comps.is_empty()) || comps.last() == Some(&"..")
        {
            comps.push(comp);
        } else {
            comps.pop();
        }
    }
    let mut out = "/".repeat(initial_slashes);
    out.push_str(&comps.join("/"));
    if out.is_empty() {
        out.push('.');
    }
    Ok(Object::from_str(out))
}

/// `dict_get_version(d)` — the dict's PEP 509 version tag (`test_dict_version`;
/// dict subclasses carry their entries in the wrapped native payload).
fn dict_get_version(args: &[Object]) -> Result<Object, RuntimeError> {
    let d = match args.first() {
        Some(Object::Dict(d)) => d.clone(),
        Some(Object::Instance(inst)) => match inst.native.get() {
            Some(Object::Dict(d)) => d.clone(),
            _ => return Err(crate::error::type_error("expected dict")),
        },
        _ => return Err(crate::error::type_error("expected dict")),
    };
    Ok(Object::Int(crate::object::dict_version_get(&d) as i64))
}

/// `_PyTraceMalloc_GetTraceback(domain, ptr)` → most-recent-first frames
/// tuple or None (`test_tracemalloc.TestCAPI.get_traceback`).
fn tracemalloc_get_traceback(args: &[Object]) -> Result<Object, RuntimeError> {
    crate::stdlib::tracemalloc_real::capi_get_traceback(args)
}

// --- `Python/pytime.c` conversion API -----------------------------------

const SEC_TO_NS: i64 = 1_000_000_000;

/// A `_PyTime_round_t` argument (test_time passes `_PyTime` IntEnum members).
fn pytime_round_arg(o: Option<&Object>) -> Result<i32, RuntimeError> {
    o.and_then(|o| o.as_i64())
        .map(|v| v as i32)
        .ok_or_else(|| crate::error::type_error("an integer is required"))
}

/// An i64 timestamp argument; a Python int beyond 64 bits overflows like the
/// clinic `long long` conversion.
fn pytime_t_arg(o: Option<&Object>) -> Result<i64, RuntimeError> {
    match o {
        Some(Object::Int(v)) => Ok(*v),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        Some(Object::Long(b)) => {
            use num_traits::ToPrimitive;
            b.to_i64().ok_or_else(|| {
                crate::error::overflow_error("Python int too large to convert to C long long")
            })
        }
        _ => Err(crate::error::type_error("an integer is required")),
    }
}

/// `pytime_round()` on a double: FLOOR / CEILING / HALF_EVEN / UP
/// (away from zero).
fn pytime_round_f64(x: f64, round: i32) -> f64 {
    match round {
        0 => x.floor(),
        1 => x.ceil(),
        2 => x.round_ties_even(),
        _ => {
            if x >= 0.0 {
                x.ceil()
            } else {
                x.floor()
            }
        }
    }
}

/// `pytime_divide()`: integer division by `k > 1` under a rounding mode,
/// with CPython's exact tie-break (parity of the truncated quotient).
fn pytime_divide(t: i64, k: i64, round: i32) -> i64 {
    fn divide_round_away(t: i64, k: i64) -> i64 {
        let q = t / k;
        if t % k != 0 {
            if t >= 0 {
                q + 1
            } else {
                q - 1
            }
        } else {
            q
        }
    }
    match round {
        2 => {
            // HALF_EVEN
            let mut x = t / k;
            let abs_r = (t % k).abs();
            if abs_r > k / 2 || (abs_r == k / 2 && (x.abs() & 1) == 1) {
                if t >= 0 {
                    x += 1;
                } else {
                    x -= 1;
                }
            }
            x
        }
        1 => {
            // CEILING: truncation is already the ceiling for negatives.
            if t >= 0 {
                divide_round_away(t, k)
            } else {
                t / k
            }
        }
        0 => {
            // FLOOR: truncation is already the floor for positives.
            if t >= 0 {
                t / k
            } else {
                divide_round_away(t, k)
            }
        }
        _ => divide_round_away(t, k), // UP (away from zero)
    }
}

/// `_PyTime_FromSeconds(seconds)` — C int seconds to nanoseconds (a C int
/// times 10^9 always fits in i64).
fn pytime_from_seconds(args: &[Object]) -> Result<Object, RuntimeError> {
    let secs = match args.first() {
        Some(Object::Int(v)) => i32::try_from(*v)
            .map_err(|_| crate::error::overflow_error("signed integer is greater than maximum"))?,
        Some(Object::Bool(b)) => i32::from(*b),
        Some(Object::Long(_)) => {
            return Err(crate::error::overflow_error(
                "Python int too large to convert to C int",
            ))
        }
        _ => return Err(crate::error::type_error("an integer is required")),
    };
    Ok(Object::Int(i64::from(secs) * SEC_TO_NS))
}

/// `pytime_from_double()`: seconds (double) to nanoseconds under a rounding
/// mode, with the `(double)PyTime_MIN <= d < -(double)PyTime_MIN` overflow
/// window from `Python/pytime.c`.
fn pytime_ns_from_double(value: f64, round: i32) -> Result<i64, RuntimeError> {
    if value.is_nan() {
        return Err(crate::error::value_error(
            "Invalid value NaN (not a number)",
        ));
    }
    let d = pytime_round_f64(value * SEC_TO_NS as f64, round);
    if !((i64::MIN as f64) <= d && d < -(i64::MIN as f64)) {
        return Err(crate::error::overflow_error(
            "timestamp too large to convert to C PyTime_t",
        ));
    }
    Ok(d as i64)
}

/// `_PyTime_FromSecondsObject(obj, round)` — int or float seconds to ns.
fn pytime_from_seconds_object(args: &[Object]) -> Result<Object, RuntimeError> {
    let round = pytime_round_arg(args.get(1))?;
    match args.first() {
        Some(Object::Float(d)) => Ok(Object::Int(pytime_ns_from_double(*d, round)?)),
        other => {
            let secs = pytime_t_arg(other)?;
            let ns = secs.checked_mul(SEC_TO_NS).ok_or_else(|| {
                crate::error::overflow_error("timestamp too large to convert to C PyTime_t")
            })?;
            Ok(Object::Int(ns))
        }
    }
}

/// `_PyTime_AsTimeval(t, round)` → `(tv_sec, tv_usec)` with `tv_usec` in
/// `[0, 10^6)`.
fn pytime_as_timeval(args: &[Object]) -> Result<Object, RuntimeError> {
    let t = pytime_t_arg(args.first())?;
    let round = pytime_round_arg(args.get(1))?;
    let us = pytime_divide(t, 1_000, round);
    Ok(Object::new_tuple(vec![
        Object::Int(us.div_euclid(1_000_000)),
        Object::Int(us.rem_euclid(1_000_000)),
    ]))
}

fn pytime_as_milliseconds(args: &[Object]) -> Result<Object, RuntimeError> {
    let t = pytime_t_arg(args.first())?;
    let round = pytime_round_arg(args.get(1))?;
    Ok(Object::Int(pytime_divide(t, 1_000_000, round)))
}

fn pytime_as_microseconds(args: &[Object]) -> Result<Object, RuntimeError> {
    let t = pytime_t_arg(args.first())?;
    let round = pytime_round_arg(args.get(1))?;
    Ok(Object::Int(pytime_divide(t, 1_000, round)))
}

/// `_PyTime_DoubleToTimet()`: cast with a round-trip check.
fn double_to_time_t(d: f64) -> Result<i64, RuntimeError> {
    let intpart = d as i64; // saturating in Rust; the check below rejects it
    let err = d - intpart as f64;
    if err <= -1.0 || err >= 1.0 {
        return Err(crate::error::overflow_error(
            "timestamp out of range for platform time_t",
        ));
    }
    Ok(intpart)
}

/// `_PyLong_AsTime_t()` for our purposes: any i64 fits time_t on 64-bit.
fn long_as_time_t(o: Option<&Object>) -> Result<i64, RuntimeError> {
    match o {
        Some(Object::Int(v)) => Ok(*v),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        Some(Object::Long(b)) => {
            use num_traits::ToPrimitive;
            b.to_i64().ok_or_else(|| {
                crate::error::overflow_error("timestamp out of range for platform time_t")
            })
        }
        _ => Err(crate::error::type_error("an integer is required")),
    }
}

/// `_PyTime_ObjectToTime_t(obj, round)` → whole seconds.
fn pytime_object_to_time_t(args: &[Object]) -> Result<Object, RuntimeError> {
    let round = pytime_round_arg(args.get(1))?;
    match args.first() {
        Some(Object::Float(d)) => {
            if d.is_nan() {
                return Err(crate::error::value_error(
                    "Invalid value NaN (not a number)",
                ));
            }
            Ok(Object::Int(double_to_time_t(pytime_round_f64(*d, round))?))
        }
        other => Ok(Object::Int(long_as_time_t(other)?)),
    }
}

/// `pytime_object_to_denominator()`: split seconds into `(sec, frac)` with
/// `frac` in `[0, denominator)` — the modf-then-round dance from pytime.c
/// (test_time's `create_converter` mirrors it step for step).
fn pytime_object_to_denominator(args: &[Object], denominator: i64) -> Result<Object, RuntimeError> {
    let round = pytime_round_arg(args.get(1))?;
    match args.first() {
        Some(Object::Float(d)) => {
            if d.is_nan() {
                return Err(crate::error::value_error(
                    "Invalid value NaN (not a number)",
                ));
            }
            let denom = denominator as f64;
            let mut intpart = d.trunc();
            let mut floatpart = (d - intpart) * denom;
            floatpart = pytime_round_f64(floatpart, round);
            if floatpart >= denom {
                floatpart -= denom;
                intpart += 1.0;
            } else if floatpart < 0.0 {
                floatpart += denom;
                intpart -= 1.0;
            }
            let sec = double_to_time_t(intpart)?;
            Ok(Object::new_tuple(vec![
                Object::Int(sec),
                Object::Int(floatpart as i64),
            ]))
        }
        other => Ok(Object::new_tuple(vec![
            Object::Int(long_as_time_t(other)?),
            Object::Int(0),
        ])),
    }
}

fn pytime_object_to_timeval(args: &[Object]) -> Result<Object, RuntimeError> {
    pytime_object_to_denominator(args, 1_000_000)
}

fn pytime_object_to_timespec(args: &[Object]) -> Result<Object, RuntimeError> {
    pytime_object_to_denominator(args, 1_000_000_000)
}
