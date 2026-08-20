//! The `sys` built-in module.
//!
//! Tracks CPython 3.13's `sys` module shape for the attributes we
//! support. `argv`, `path`, and `modules` are all backed by the
//! interpreter's [`ModuleCache`] so writes flow both ways.
//!
//! Anything that touches host I/O streams (`sys.stdout`,
//! `sys.stderr`) is deferred to RFC 0014, when we land the `io`
//! module and Python file objects.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, FileBackend, Object, PyFile, PyModule};

/// CPython compatibility version we advertise. This is intentionally
/// independent from the WeavePy package version (see
/// `weavepy-cli/src/main.rs`); user code that inspects
/// `sys.version_info` is checking *Python language* compatibility, not
/// the WeavePy build identity.
pub const PY_VERSION: (i64, i64, i64) = (3, 13, 0);

/// Build the `sys` module against the given interpreter handles.
/// Most state lives on the [`ModuleCache`]; `frame_stack`,
/// `exc_info_stack`, and the user-installable hooks come from the
/// interpreter itself so module-level callables can read live state.
pub fn build_with_state(
    cache: &ModuleCache,
    frame_stack: crate::object::FrameStack,
    exc_info_stack: Rc<RefCell<Vec<crate::error::PyException>>>,
    excepthook: Rc<RefCell<Object>>,
    unraisable_hook: Rc<RefCell<Object>>,
) -> Rc<PyModule> {
    let module = build(cache);
    {
        let mut d = module.dict.borrow_mut();
        // RFC 0025: route through the active per-thread handles so
        // worker threads see *their* frame / exception state, not
        // the main interpreter's. The `frame_stack` / `exc_info_stack`
        // closure captures below are kept as fallbacks for embedders
        // that build the `sys` module before any interpreter has
        // activated handles for the current thread.
        let fs_fallback = frame_stack.clone();
        d.insert(
            DictKey(Object::from_static("_getframe")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_getframe",
                binds_instance: false,
                call: Box::new(move |args| {
                    if let Some(h) = crate::vm_singletons::current_thread_handles() {
                        sys_getframe(args, &h.frame_stack)
                    } else {
                        sys_getframe(args, &fs_fallback)
                    }
                }),
                call_kw: None,
            })),
        );
        let fs_fallback_modname = frame_stack.clone();
        d.insert(
            DictKey(Object::from_static("_getframemodulename")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "_getframemodulename",
                binds_instance: false,
                call: Box::new(move |args| {
                    if let Some(h) = crate::vm_singletons::current_thread_handles() {
                        sys_getframemodulename(args, &h.frame_stack)
                    } else {
                        sys_getframemodulename(args, &fs_fallback_modname)
                    }
                }),
                call_kw: None,
            })),
        );
        let es_fallback = exc_info_stack.clone();
        d.insert(
            DictKey(Object::from_static("exc_info")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "exc_info",
                binds_instance: false,
                call: Box::new(move |_| {
                    if let Some(h) = crate::vm_singletons::current_thread_handles() {
                        sys_exc_info(&h.exc_info_stack)
                    } else {
                        sys_exc_info(&es_fallback)
                    }
                }),
                call_kw: None,
            })),
        );
        let es_fallback_exc = exc_info_stack.clone();
        d.insert(
            DictKey(Object::from_static("exception")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "exception",
                binds_instance: false,
                call: Box::new(move |_| {
                    if let Some(h) = crate::vm_singletons::current_thread_handles() {
                        sys_exception(&h.exc_info_stack)
                    } else {
                        sys_exception(&es_fallback_exc)
                    }
                }),
                call_kw: None,
            })),
        );
        d.insert(
            DictKey(Object::from_static("__excepthook__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "excepthook",
                binds_instance: false,
                call: Box::new(sys_default_excepthook),
                call_kw: None,
            })),
        );
        let eh = excepthook.clone();
        d.insert(
            DictKey(Object::from_static("excepthook")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "excepthook",
                binds_instance: false,
                call: Box::new(move |args| {
                    let hook = eh.borrow().clone();
                    // If a user hook is installed, the *call* path
                    // lives in the VM (we can't dispatch Python from
                    // a builtin here). Surface a stable error so the
                    // VM-level dispatch wraps us.
                    if !matches!(hook, Object::None) {
                        return Ok(Object::None);
                    }
                    sys_default_excepthook(args)
                }),
                call_kw: None,
            })),
        );
        let uh = unraisable_hook.clone();
        let default_unraisablehook = Object::Builtin(Rc::new(BuiltinFn {
            name: "unraisablehook",
            binds_instance: false,
            call: Box::new(move |args| {
                let _ = uh.borrow().clone();
                // Called explicitly (a wrapper hook chaining into the
                // saved original — test.libregrtest's
                // regrtest_unraisable_hook): perform the default
                // report from the UnraisableHookArgs object. The VM's
                // own unraisable path never routes here — it prints
                // directly — so this is exactly the "call the builtin
                // as a function" surface.
                let Some(arg) = args.first() else {
                    return Err(crate::error::type_error(
                        "unraisablehook() takes exactly one argument",
                    ));
                };
                if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                    // SAFETY: published by the enclosing VM frame on
                    // this thread; the GIL keeps the pointer exclusive.
                    let interp = unsafe { &mut *ptr };
                    interp.default_unraisablehook_call(arg)?;
                }
                Ok(Object::None)
            }),
            call_kw: None,
        }));
        d.insert(
            DictKey(Object::from_static("unraisablehook")),
            default_unraisablehook.clone(),
        );
        // Like `__excepthook__`: the pristine default, kept reachable so
        // a wrapper hook can restore or chain into it.
        d.insert(
            DictKey(Object::from_static("__unraisablehook__")),
            default_unraisablehook,
        );
        d.insert(
            DictKey(Object::from_static("settrace")),
            builtin("settrace", sys_settrace),
        );
        d.insert(
            DictKey(Object::from_static("call_tracing")),
            builtin("call_tracing", sys_call_tracing),
        );
        d.insert(
            DictKey(Object::from_static("monitoring")),
            crate::stdlib::sys_monitoring::build(),
        );
        d.insert(
            DictKey(Object::from_static("setprofile")),
            builtin("setprofile", sys_setprofile),
        );
        d.insert(
            DictKey(Object::from_static("gettrace")),
            builtin("gettrace", sys_gettrace),
        );
        d.insert(
            DictKey(Object::from_static("getprofile")),
            builtin("getprofile", sys_getprofile),
        );
        // Internal hooks behind `threading.settrace_all_threads` /
        // `setprofile_all_threads` (PEP 669-adjacent, gh-93503).
        d.insert(
            DictKey(Object::from_static("_settraceallthreads")),
            builtin("_settraceallthreads", sys_settraceallthreads),
        );
        d.insert(
            DictKey(Object::from_static("_setprofileallthreads")),
            builtin("_setprofileallthreads", sys_setprofileallthreads),
        );
        d.insert(
            DictKey(Object::from_static("getsizeof")),
            builtin("getsizeof", sys_getsizeof),
        );
        // PEP 578 audit hooks. `sys.audit(event, *args)` walks the
        // registered hook list; `sys.addaudithook(hook)` appends to
        // the per-thread list. We deliberately *don't* fire from
        // here — the call-out is performed by
        // ``crate::stdlib::sys::audit_event`` which the VM and
        // stdlib invoke at the documented event sites
        // (`open`, `compile`, `exec`, `import`, `subprocess.Popen`,
        // `socket.connect`, `marshal.loads`, …). Calling
        // ``sys.audit`` from user code is also supported and
        // routes through the same registry.
        d.insert(
            DictKey(Object::from_static("audit")),
            builtin("audit", sys_audit),
        );
        d.insert(
            DictKey(Object::from_static("addaudithook")),
            builtin("addaudithook", sys_addaudithook),
        );
        d.insert(DictKey(Object::from_static("flags")), sys_flags_value());
        // Default to `False`, matching CPython. The CLI/embedder
        // overrides this through `apply_run_options` when `-B` or
        // `PYTHONDONTWRITEBYTECODE` was set.
        d.insert(
            DictKey(Object::from_static("dont_write_bytecode")),
            Object::Bool(false),
        );
        d.insert(
            DictKey(Object::from_static("ps1")),
            Object::from_static(">>> "),
        );
        d.insert(
            DictKey(Object::from_static("ps2")),
            Object::from_static("... "),
        );
        d.insert(
            DictKey(Object::from_static("warnoptions")),
            Object::new_list(Vec::new()),
        );
        d.insert(
            DictKey(Object::from_static("hexversion")),
            Object::Int((PY_VERSION.0 << 24) | (PY_VERSION.1 << 16) | (PY_VERSION.2 << 8) | 0xF0),
        );
        d.insert(
            DictKey(Object::from_static("api_version")),
            Object::Int(1013),
        );
        d.insert(DictKey(Object::from_static("float_info")), sys_float_info());
        d.insert(DictKey(Object::from_static("int_info")), sys_int_info());
        d.insert(DictKey(Object::from_static("hash_info")), sys_hash_info());
        // `float.__repr__` uses the shortest round-tripping form ("short"),
        // as every modern CPython build does; test_float asserts on this.
        d.insert(
            DictKey(Object::from_static("float_repr_style")),
            Object::from_static("short"),
        );
        d.insert(
            DictKey(Object::from_static("thread_info")),
            sys_thread_info(),
        );

        // RFC 0029 — import machinery state. The frozen
        // `importlib._bootstrap` module overwrites `meta_path`,
        // `path_hooks`, and `path_importer_cache` on first import
        // with real importer objects; until then they hold empty
        // collections so `importlib.util.find_spec("name")` doesn't
        // crash trying to walk a missing attribute.
        d.insert(
            DictKey(Object::from_static("meta_path")),
            Object::new_list(Vec::new()),
        );
        d.insert(
            DictKey(Object::from_static("path_hooks")),
            Object::new_list(Vec::new()),
        );
        d.insert(
            DictKey(Object::from_static("path_importer_cache")),
            Object::new_dict(),
        );
        d.insert(DictKey(Object::from_static("pycache_prefix")), Object::None);
        d.insert(
            DictKey(Object::from_static("maxunicode")),
            Object::Int(0x0010_FFFF),
        );
        d.insert(
            DictKey(Object::from_static("platlibdir")),
            Object::from_static(if cfg!(windows) { "Lib" } else { "lib" }),
        );
        d.insert(
            DictKey(Object::from_static("tracebacklimit")),
            Object::Int(1000),
        );
        // Standard library module name allowlist — used by tools
        // that need to know which `import x` reaches the stdlib
        // vs. a third-party package. Matches the documented
        // CPython 3.13 set (lowercase, no underscore-private
        // helpers).
        d.insert(
            DictKey(Object::from_static("stdlib_module_names")),
            stdlib_module_names_value(),
        );

        // `last_type` / `last_value` / `last_traceback` —
        // populated by the REPL's exception loop. Pre-seed to
        // None so user inspection doesn't AttributeError.
        d.insert(DictKey(Object::from_static("last_type")), Object::None);
        d.insert(DictKey(Object::from_static("last_value")), Object::None);
        d.insert(DictKey(Object::from_static("last_traceback")), Object::None);
        d.insert(DictKey(Object::from_static("last_exc")), Object::None);

        // `_current_frames` / `_current_exceptions` — dicts keyed by
        // `threading.get_ident()` covering *every* live thread (the
        // faulthandler registry is WeavePy's tstate-list analogue; the
        // caller holds the GIL, so peer stacks are quiescent).
        {
            let fs_cf = frame_stack.clone();
            d.insert(
                DictKey(Object::from_static("_current_frames")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "_current_frames",
                    binds_instance: false,
                    call: Box::new(move |_args| {
                        let mut d = DictData::default();
                        for (ident, stack, _exc) in
                            crate::stdlib::faulthandler_mod::thread_snapshots()
                        {
                            if let Some(f) = crate::object::materialize_stack_top(&stack) {
                                d.insert(DictKey(Object::Int(ident as i64)), Object::Frame(f));
                            }
                        }
                        if d.is_empty() {
                            // Pre-threading fallback: the registering
                            // interpreter's own stack.
                            if let Some(f) = crate::object::materialize_stack_top(&fs_cf) {
                                let ident = crate::vm_singletons::current_worker_thread_id();
                                d.insert(DictKey(Object::Int(ident as i64)), Object::Frame(f));
                            }
                        }
                        Ok(Object::Dict(Rc::new(RefCell::new(d))))
                    }),
                    call_kw: None,
                })),
            );
            d.insert(
                DictKey(Object::from_static("_current_exceptions")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "_current_exceptions",
                    binds_instance: false,
                    call: Box::new(move |_args| {
                        let mut d = DictData::default();
                        for (ident, _stack, exc) in
                            crate::stdlib::faulthandler_mod::thread_snapshots()
                        {
                            // 3.12+: the value is the handled exception
                            // *instance* (None outside any `except`).
                            let value = exc
                                .borrow()
                                .last()
                                .map(|top| top.instance.clone())
                                .unwrap_or(Object::None);
                            d.insert(DictKey(Object::Int(ident as i64)), value);
                        }
                        Ok(Object::Dict(Rc::new(RefCell::new(d))))
                    }),
                    call_kw: None,
                })),
            );
        }

        // PEP 703 introspection (3.13): this is a GIL build, and the GIL
        // cannot be disabled.
        d.insert(
            DictKey(Object::from_static("_is_gil_enabled")),
            builtin("_is_gil_enabled", |_| Ok(Object::Bool(true))),
        );
        d.insert(
            DictKey(Object::from_static("getswitchinterval")),
            builtin("getswitchinterval", |_| {
                Ok(Object::Float(SWITCH_INTERVAL.with(|c| c.get())))
            }),
        );
        d.insert(
            DictKey(Object::from_static("setswitchinterval")),
            builtin("setswitchinterval", sys_setswitchinterval),
        );
        d.insert(
            DictKey(Object::from_static("getrefcount")),
            builtin("getrefcount", sys_getrefcount),
        );
        // `sys._clear_type_cache()` drops CPython's method-lookup cache; the
        // observable contract (test_type_cache) is only that existing type
        // version tags survive and are never reused, which WeavePy's
        // monotonic per-type `attr_version` counters give for free.
        d.insert(
            DictKey(Object::from_static("_clear_type_cache")),
            builtin("_clear_type_cache", |_| Ok(Object::None)),
        );
        d.insert(
            DictKey(Object::from_static("get_coroutine_origin_tracking_depth")),
            builtin("get_coroutine_origin_tracking_depth", |_| {
                Ok(Object::Int(coroutine_origin_tracking_depth()))
            }),
        );
        d.insert(
            DictKey(Object::from_static("set_coroutine_origin_tracking_depth")),
            builtin(
                "set_coroutine_origin_tracking_depth",
                sys_set_coroutine_origin_tracking_depth,
            ),
        );
        d.insert(
            DictKey(Object::from_static("get_asyncgen_hooks")),
            builtin("get_asyncgen_hooks", sys_get_asyncgen_hooks),
        );
        d.insert(
            DictKey(Object::from_static("set_asyncgen_hooks")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "set_asyncgen_hooks",
                binds_instance: false,
                call: Box::new(|args| sys_set_asyncgen_hooks(args, &[])),
                call_kw: Some(Box::new(sys_set_asyncgen_hooks)),
            })),
        );
        // `displayhook` — invoked by the REPL after every
        // evaluated expression. Default writes `repr(value)` to
        // stdout and stashes the value in `builtins._`. The hook
        // is overrideable; the original is preserved on
        // `__displayhook__`.
        d.insert(
            DictKey(Object::from_static("displayhook")),
            builtin("displayhook", sys_displayhook),
        );
        d.insert(
            DictKey(Object::from_static("__displayhook__")),
            builtin("displayhook", sys_displayhook),
        );

        // PEP 553 — `breakpointhook` / `__breakpointhook__`: the default
        // hook honours $PYTHONBREAKPOINT ('0' → no-op, empty/unset →
        // `pdb.set_trace`, otherwise a dotted callable path, warning on
        // an unimportable value). `breakpoint()` dispatches through the
        // live `sys.breakpointhook` binding (test_builtin
        // TestBreakpoint).
        for name in ["breakpointhook", "__breakpointhook__"] {
            d.insert(
                DictKey(Object::from_static(name)),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "breakpointhook",
                    binds_instance: false,
                    call: Box::new(|args| sys_breakpointhook_kw(args, &[])),
                    call_kw: Some(Box::new(sys_breakpointhook_kw)),
                })),
            );
        }

        // `sys.builtin_module_names` — exposed as a tuple for
        // user-introspection code (e.g. `importlib.util.find_spec`).
        d.insert(
            DictKey(Object::from_static("builtin_module_names")),
            builtin_module_names_value(),
        );
        // sys.gettrace/getprofile stubs (no actual tracing yet).
    }
    module
}

pub fn build(cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("sys"),
        );
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static(""),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "Provides access to interpreter-internal state and the import system.",
            ),
        );

        // Shared with the loader.
        d.insert(
            DictKey(Object::from_static("modules")),
            Object::Dict(cache.modules.clone()),
        );
        d.insert(
            DictKey(Object::from_static("path")),
            Object::List(cache.path.clone()),
        );
        d.insert(
            DictKey(Object::from_static("argv")),
            Object::List(cache.argv.clone()),
        );
        // PEP 587's `sys.orig_argv`: the process argv exactly as the
        // interpreter received it (executable + options + script args),
        // before option stripping produced `sys.argv`. Decoded through
        // the RFC 0050 surrogateescape bridge — `std::env::args()`
        // panics outright on non-UTF-8 argv (bpo-35883).
        d.insert(
            DictKey(Object::from_static("orig_argv")),
            Object::new_list(
                crate::os_args_bridged()
                    .iter()
                    .map(|a| crate::argv_str_to_object(a))
                    .collect::<Vec<_>>(),
            ),
        );

        // Static identity. Shaped like CPython's `sys.version`
        // (`VERSION (buildno[, date[, time]]) [compiler]`) so
        // `platform._sys_version()` — used by `platform`,
        // `sysconfig`, `wsgiref`, and large parts of `test.support`
        // — parses it instead of raising `ValueError`. The build/
        // compiler tokens are tagged `WeavePy`; `python_implementation()`
        // still reports `CPython` (no PyPy/Jython/IronPython marker), so
        // implementation-gated stdlib tests behave as on CPython.
        //
        // On Windows the compiler token also carries CPython's MSC arch
        // tag (`64 bit (AMD64)` / `64 bit (ARM64)`): that substring is
        // what `sysconfig.get_platform()` sniffs to answer `win-amd64`
        // — the value setuptools bakes into wheel tags and build dirs
        // (RFC 0064 WS3). Without it the platform reads as `win32`.
        d.insert(
            DictKey(Object::from_static("version")),
            Object::from_str(format!(
                "{}.{}.{} (WeavePy) [WeavePy{}]",
                PY_VERSION.0,
                PY_VERSION.1,
                PY_VERSION.2,
                version_arch_tag()
            )),
        );
        d.insert(
            DictKey(Object::from_static("version_info")),
            version_info_value(),
        );
        d.insert(
            DictKey(Object::from_static("platform")),
            Object::from_static(host_platform()),
        );
        // RFC 0063 WS1 — the Windows identity surface.
        #[cfg(windows)]
        {
            // CPython's `sys.winver` is the version tag its registry keys
            // and DLL name carry (Python/sysmodule.c sets it from
            // MS_DLL_ID); `sysconfig`/`venv`/pip read it on Windows.
            d.insert(
                DictKey(Object::from_static("winver")),
                Object::from_static("3.13"),
            );
            // CPython publishes the HMODULE of python3xx.dll here. Since
            // RFC 0064 the runtime ships as a real `python313.dll` loaded
            // by the `weavepy.exe` shim, so the handle is the module's:
            // nonzero whenever this interpreter is running out of the DLL
            // (the shipped configuration), 0 in a statically-linked
            // embedder (e.g. Rust test harnesses) — the truthful answer
            // for a process with no Python DLL. `ctypes.pythonapi`
            // constructs against this handle.
            d.insert(
                DictKey(Object::from_static("dllhandle")),
                Object::Int(python_dll_handle()),
            );
            d.insert(
                DictKey(Object::from_static("getwindowsversion")),
                builtin("getwindowsversion", sys_getwindowsversion),
            );
            // PEP 529: WeavePy's filesystem encoding is permanently UTF-8.
            // CPython's switch re-enables the pre-3.6 mbcs mode, which
            // WeavePy never had — accept the call and do nothing.
            d.insert(
                DictKey(Object::from_static("_enablelegacywindowsfsencoding")),
                builtin("_enablelegacywindowsfsencoding", |_| Ok(Object::None)),
            );
        }
        // CPython-on-macOS build detail: the framework name when built
        // as a macOS framework, `""` otherwise (the common case, and
        // ours). `pydoc`/`platform`/`site` read it unconditionally.
        d.insert(
            DictKey(Object::from_static("_framework")),
            Object::from_static(""),
        );
        // RFC 0055 WS1 — version-control build stamp, CPython's
        // `sys._git` 3-tuple `(project, branch, revision)`. CPython
        // reports empty branch/revision strings when built outside a
        // checkout; WeavePy does the same (`platform._sys_version`
        // and its test suite read the attribute unconditionally).
        d.insert(
            DictKey(Object::from_static("_git")),
            Object::new_tuple(vec![
                Object::from_static("WeavePy"),
                Object::from_static(""),
                Object::from_static(""),
            ]),
        );
        d.insert(
            DictKey(Object::from_static("byteorder")),
            Object::from_static(if cfg!(target_endian = "little") {
                "little"
            } else {
                "big"
            }),
        );
        d.insert(
            DictKey(Object::from_static("maxsize")),
            Object::Int(i64::MAX),
        );
        // argv[0]-derived (like CPython's getpath), NOT current_exe():
        // on Linux /proc/self/exe pre-resolves symlinks, and a venv's
        // `bin/python` must keep its symlink identity here or venv
        // detection (pyvenv.cfg next to the executable) never fires.
        let executable = crate::stdlib_tree::program_exe().map_or(Object::from_static(""), |p| {
            Object::from_str(p.to_string_lossy().into_owned())
        });
        d.insert(
            DictKey(Object::from_static("executable")),
            executable.clone(),
        );
        // `sys._base_executable` mirrors `sys.executable` outside a venv
        // (CPython sets it to the real interpreter; `test_os.PidTests` and
        // `subprocess` reach for it when re-launching the interpreter).
        // Inside a venv, CPython's getpath derives it from pyvenv.cfg's
        // `home` key + the executable's basename (RFC 0055 WS1;
        // `test_venv.test_sysconfig` asserts the venv python reports
        // the *base* interpreter here).
        d.insert(
            DictKey(Object::from_static("_base_executable")),
            venv_base_executable().map_or_else(|| executable.clone(), Object::from_str),
        );
        // Installation prefixes. CPython computes these in getpath.c;
        // RFC 0053 anchors them on the materialized stdlib tree
        // (`{prefix}/lib/weavepy3.13`), so `sysconfig`'s
        // `{installed_base}`-relative schemes and `site.getsitepackages`
        // resolve inside a real, existing installation. When the tree is
        // unavailable, approximate with the executable's grandparent
        // directory (the usual `<prefix>/bin/python` layout). Defined
        // natively — not just in `site.py` — because embedders skip site
        // initialization and module-scope stdlib code reads them at
        // import time (`gettext._default_localedir` uses
        // `sys.base_prefix`).
        {
            let prefix = crate::stdlib_tree::prefix()
                .map(std::path::Path::to_path_buf)
                .or_else(|| {
                    crate::stdlib_tree::program_exe().and_then(|p| {
                        p.parent()
                            .and_then(|d| d.parent())
                            .map(std::path::Path::to_path_buf)
                    })
                })
                .map_or(Object::from_static(""), |p| {
                    Object::from_str(p.to_string_lossy().into_owned())
                });
            for name in ["prefix", "exec_prefix", "base_prefix", "base_exec_prefix"] {
                d.insert(DictKey(Object::from_static(name)), prefix.clone());
            }
        }
        // RFC 0053 WS4 — a release build carries no ABI flags; the
        // verbatim `sysconfig` derives `_sysconfigdata_*` names from it.
        d.insert(
            DictKey(Object::from_static("abiflags")),
            Object::from_static(""),
        );
        // The verbatim `site.setcopyright()` builds the interactive
        // `copyright` object from this (CPython's is assembled in
        // `getcopyright.c`).
        d.insert(
            DictKey(Object::from_static("copyright")),
            Object::from_static(
                "Copyright (c) 2001-2024 Python Software Foundation.\nAll Rights Reserved.",
            ),
        );
        // RFC 0053 — the materialized stdlib directory (CPython 3.11+'s
        // `sys._stdlib_dir`). `None` when the tree is unavailable.
        d.insert(
            DictKey(Object::from_static("_stdlib_dir")),
            crate::stdlib_tree::stdlib_dir().map_or(Object::None, |p| {
                Object::from_str(p.to_string_lossy().into_owned())
            }),
        );
        d.insert(
            DictKey(Object::from_static("implementation")),
            implementation_value(),
        );

        // Callables.
        d.insert(
            DictKey(Object::from_static("exit")),
            builtin("exit", sys_exit),
        );
        // RFC 0026 — private helper so `runpy.run_module()` can
        // execute frozen modules. Looks up a frozen source by name;
        // returns ``None`` if the module isn't frozen (or doesn't
        // exist). Mirrors CPython's `_imp.get_frozen_source` shape.
        // Both helpers go through `ModuleCache::frozen_source` (not the
        // raw table) so the `_imp._override_frozen_modules_for_tests`
        // knob hides the frozen test modules here too — the Python-level
        // `FrozenImporter.find_spec` keys off `sys._is_frozen`.
        {
            let cache_for_source = cache.clone();
            d.insert(
                DictKey(Object::from_static("_get_frozen_source")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "_get_frozen_source",
                    binds_instance: false,
                    call: Box::new(move |args| {
                        let name = match args.first() {
                            Some(Object::Str(s)) => s.to_string(),
                            _ => return Err(type_error("_get_frozen_source() expects a string")),
                        };
                        Ok(cache_for_source
                            .frozen_source(&name)
                            .map(|src| Object::from_static(src.source))
                            .unwrap_or(Object::None))
                    }),
                    call_kw: None,
                })),
            );
        }
        {
            let cache_for_probe = cache.clone();
            d.insert(
                DictKey(Object::from_static("_is_frozen")),
                Object::Builtin(Rc::new(BuiltinFn {
                    name: "_is_frozen",
                    binds_instance: false,
                    call: Box::new(move |args| {
                        let name = match args.first() {
                            Some(Object::Str(s)) => s.to_string(),
                            _ => return Ok(Object::Bool(false)),
                        };
                        if cache_for_probe.frozen_source(&name).is_none() {
                            return Ok(Object::Bool(false));
                        }
                        // Mirror the VM importer's precedence: a source file
                        // on the path entries before the stdlib landmark
                        // shadows the frozen copy (anywhere on the path for
                        // bundled third-party facades), so `FrozenImporter`
                        // must decline and let `PathFinder` claim the name —
                        // `runpy`/`-m` resolve through `find_spec`
                        // (test_import's script-shadowing suites).
                        let shadowed = if crate::import::ModuleCache::is_third_party_facade(&name) {
                            cache_for_probe.find_source(&name).is_some()
                        } else {
                            cache_for_probe
                                .find_source_shadowing_stdlib(&name)
                                .is_some()
                        };
                        Ok(Object::Bool(!shadowed))
                    }),
                    call_kw: None,
                })),
            );
        }
        d.insert(
            DictKey(Object::from_static("getrecursionlimit")),
            builtin("getrecursionlimit", sys_getrecursionlimit),
        );
        d.insert(
            DictKey(Object::from_static("setrecursionlimit")),
            builtin("setrecursionlimit", sys_setrecursionlimit),
        );
        d.insert(
            DictKey(Object::from_static("get_int_max_str_digits")),
            builtin("get_int_max_str_digits", sys_get_int_max_str_digits),
        );
        d.insert(
            DictKey(Object::from_static("set_int_max_str_digits")),
            builtin("set_int_max_str_digits", sys_set_int_max_str_digits),
        );
        d.insert(
            DictKey(Object::from_static("intern")),
            builtin("intern", sys_intern),
        );
        d.insert(
            DictKey(Object::from_static("is_finalizing")),
            builtin("is_finalizing", |_args| {
                Ok(Object::Bool(crate::vm_singletons::is_finalizing()))
            }),
        );
        d.insert(
            DictKey(Object::from_static("getdefaultencoding")),
            builtin("getdefaultencoding", sys_getdefaultencoding),
        );
        d.insert(
            DictKey(Object::from_static("getfilesystemencoding")),
            builtin("getfilesystemencoding", sys_getfilesystemencoding),
        );
        d.insert(
            DictKey(Object::from_static("getfilesystemencodeerrors")),
            builtin("getfilesystemencodeerrors", sys_getfilesystemencodeerrors),
        );

        // Standard I/O streams. We expose them as file-like objects
        // sharing the interpreter's host sinks, so `print()` and
        // direct writes via `sys.stdout.write(...)` agree.
        let stdout_sink: Rc<RefCell<dyn std::io::Write + Send + Sync>> =
            Rc::new(RefCell::new(std::io::stdout()));
        let stderr_sink: Rc<RefCell<dyn std::io::Write + Send + Sync>> =
            Rc::new(RefCell::new(std::io::stderr()));
        // CPython's `init_sys_streams`: a standard stream whose fd is
        // closed at startup (e.g. spawned with `os.close(0)` in a
        // preexec hook) is `None`, not a broken file object
        // (`test_cmd_line.test_no_stdin` and friends).
        #[cfg(unix)]
        let fd_valid = |fd: i32| unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1;
        #[cfg(not(unix))]
        let fd_valid = |_fd: i32| true;
        d.insert(
            DictKey(Object::from_static("stdout")),
            if fd_valid(1) {
                Object::File(Rc::new(PyFile::new(
                    "<stdout>",
                    "w",
                    FileBackend::Stdout(stdout_sink),
                )))
            } else {
                Object::None
            },
        );
        d.insert(
            DictKey(Object::from_static("stderr")),
            if fd_valid(2) {
                Object::File(Rc::new(PyFile::new(
                    "<stderr>",
                    "w",
                    FileBackend::Stderr(stderr_sink),
                )))
            } else {
                Object::None
            },
        );
        d.insert(
            DictKey(Object::from_static("stdin")),
            if fd_valid(0) {
                Object::File(Rc::new(PyFile::new("<stdin>", "r", FileBackend::Stdin)))
            } else {
                Object::None
            },
        );
        // `sys.__stdout__` et al. record the *original* streams so code
        // that rebinds `sys.stdout` can restore them. They alias the same
        // objects at startup.
        for name in ["stdout", "stderr", "stdin"] {
            let dunder = format!("__{name}__");
            let v = d
                .get(&crate::object::StrKey(name))
                .cloned()
                .expect("stream just inserted");
            d.insert(DictKey(Object::from_str(dunder)), v);
        }
    }
    Rc::new(PyModule {
        name: "sys".to_owned(),
        filename: None,
        dict,
    })
}

/// CPython's NT compiler-bracket arch suffix (`[MSC v.19xx 64 bit
/// (AMD64)]`), reduced to the part `sysconfig.get_platform()` and
/// `platform.architecture()` actually sniff. Empty off Windows —
/// POSIX `get_platform()` reads `os.uname()`, not `sys.version`.
const fn version_arch_tag() -> &'static str {
    if cfg!(all(windows, target_arch = "x86_64")) {
        " 64 bit (AMD64)"
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        " 64 bit (ARM64)"
    } else if cfg!(all(windows, target_arch = "x86")) {
        " 32 bit (Intel)"
    } else {
        ""
    }
}

fn host_platform() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "freebsd") {
        "freebsd"
    } else {
        "unknown"
    }
}

fn implementation_value() -> Object {
    // `sys.implementation` is a `types.SimpleNamespace`-shaped object
    // in CPython. RFC 0023 added [`Object::SimpleNamespace`] so we
    // can match the shape exactly — attribute access via `.name`
    // / `.version` works, but the value isn't a dict.
    let mut d = DictData::default();
    d.insert(
        DictKey(Object::from_static("name")),
        Object::from_static("weavepy"),
    );
    d.insert(
        DictKey(Object::from_static("version")),
        version_info_value(),
    );
    d.insert(
        DictKey(Object::from_static("hexversion")),
        Object::Int((PY_VERSION.0 << 24) | (PY_VERSION.1 << 16) | (PY_VERSION.2 << 8) | 0xF0),
    );
    d.insert(
        DictKey(Object::from_static("cache_tag")),
        Object::from_static(crate::pycache::CACHE_TAG),
    );
    // RFC 0055 WS1 — CPython's multiarch tag for the compile target
    // (`darwin`, `x86_64-linux-gnu`, …). Only present on the platforms
    // where CPython defines it, matching `hasattr` probes in
    // `sysconfig`/`test.support`.
    if !crate::stdlib::sysconfig_native::MULTIARCH.is_empty() {
        d.insert(
            DictKey(Object::from_static("_multiarch")),
            Object::from_static(crate::stdlib::sysconfig_native::MULTIARCH),
        );
    }
    Object::SimpleNamespace(Rc::new(RefCell::new(d)))
}

/// Contents of the governing `pyvenv.cfg` (next to the executable's
/// directory or one level up), or `None` outside a virtual environment.
fn venv_cfg_contents() -> Option<String> {
    // argv[0]-derived: the venv executable is a symlink whose identity
    // `current_exe()` destroys on Linux (see stdlib_tree::program_exe).
    let exe = crate::stdlib_tree::program_exe()?;
    let exe_dir = exe.parent()?;
    let cfg = [
        exe_dir.join("pyvenv.cfg"),
        exe_dir.parent()?.join("pyvenv.cfg"),
    ]
    .into_iter()
    .find(|p| p.is_file())?;
    std::fs::read_to_string(cfg).ok()
}

/// Case-insensitive `key = value` lookup in pyvenv.cfg contents.
fn venv_cfg_lookup(contents: &str, wanted: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim().eq_ignore_ascii_case(wanted)).then(|| value.trim().to_owned())
    })
}

/// When the running executable lives inside a virtual environment,
/// return the base interpreter path. Prefer the explicit `executable`
/// key venv writes since 3.11; fall back to CPython getpath's
/// `{home}/{basename(executable)}` reconstruction. `None` outside a
/// venv.
fn venv_base_executable() -> Option<String> {
    let contents = venv_cfg_contents()?;
    if let Some(executable) = venv_cfg_lookup(&contents, "executable") {
        if std::path::Path::new(&executable).is_file() {
            return Some(executable);
        }
    }
    let home = venv_cfg_lookup(&contents, "home")?;
    let exe = crate::stdlib_tree::program_exe()?;
    let base = std::path::Path::new(&home).join(exe.file_name()?);
    base.is_file().then(|| base.to_string_lossy().into_owned())
}

/// CPython getpath's stdlib-zip landmark: `{base_prefix}/{platlibdir}/
/// python{XY}{abi_thread}.zip`. Listed on `sys.path` whether or not the
/// archive exists (CPython does the same), and — crucially for venvs
/// created from a non-installed build (`test_venv.
/// test_zippath_from_non_installed_posix`) — anchored on the *base*
/// prefix derived from pyvenv.cfg's `home` key.
pub(crate) fn stdlib_zip_path() -> Option<String> {
    let base_prefix: std::path::PathBuf = venv_cfg_contents()
        .and_then(|contents| venv_cfg_lookup(&contents, "home"))
        .map(|home| {
            let home = std::path::PathBuf::from(home);
            // `home` is the base executable's directory (`{base}/bin` on
            // POSIX); the prefix is one level up.
            match (home.file_name(), home.parent()) {
                (Some(name), Some(parent)) if name == "bin" => parent.to_path_buf(),
                _ => home,
            }
        })
        .or_else(|| crate::stdlib_tree::prefix().map(std::path::Path::to_path_buf))
        .or_else(|| {
            crate::stdlib_tree::program_exe()
                .and_then(|p| Some(p.parent()?.parent()?.to_path_buf()))
        })?;
    let zip = base_prefix
        .join("lib")
        .join(format!("python{}{}.zip", PY_VERSION.0, PY_VERSION.1));
    Some(zip.to_string_lossy().into_owned())
}

/// `sys.builtin_module_names` — the per-OS truthful inventory (RFC 0063
/// WS1). Only modules `register_all` (`stdlib/mod.rs`) builds *natively*
/// belong here: names that ship as frozen Python source (random, json,
/// re, …) must not appear, because stdlib consumers take membership as
/// "no Python source exists" (`pyclbr._readmodule` early-returns an
/// empty tree for them — `test_pyclbr.test_others`). The registration
/// table itself isn't enumerable from here without widening `mod.rs`,
/// so this list mirrors it by hand — keep the two in sync.
///
/// Two deliberate exceptions, matching CPython's *observable* contract:
/// `posix` (unix) and `nt` (Windows) are listed even though WeavePy's
/// are frozen shims over the native `os`, because `Lib/os.py` itself
/// detects the platform via `'posix' in sys.builtin_module_names` /
/// `'nt' in ...` — those membership probes are the load-bearing
/// consumers. Sorted, as CPython's tuple is.
fn builtin_module_names_value() -> Object {
    let mut names: Vec<&'static str> = vec![
        "_abc",
        "_ast",
        "_asyncio",
        "_bisect",
        "_blake2",
        "_bz2",
        "_codecs",
        "_contextvars",
        "_csv",
        "_ctypes_native",
        "_functools",
        "_gzip",
        "_heapq",
        "_https",
        "_imp",
        "_io",
        "_itertools",
        "_json",
        "_locale",
        "_lzma",
        "_md5",
        "_multiprocessing",
        "_operator",
        "_random",
        "_sha1",
        "_sha2",
        "_sha3",
        "_signal",
        "_socket",
        "_sqlite3",
        "_sre",
        "_ssl",
        "_statistics",
        "_string",
        "_struct",
        "_subprocess",
        "_symtable",
        "_sysconfig",
        "_testinternalcapi",
        "_thread",
        "_tokenize_core",
        "_tracemalloc",
        "_warnings",
        "_weakref",
        "_weave_frame",
        "_xxsubinterpreters",
        "atexit",
        "binascii",
        "cmath",
        "errno",
        "faulthandler",
        "gc",
        "hashlib",
        "marshal",
        "math",
        "mmap",
        // (`os` is deliberately absent: CPython's `os` is Python source
        // — its startup-only native fast path here is the analogue of
        // CPython's *frozen* `os`, which is not a builtin either.)
        "pyexpat",
        "select",
        "sys",
        "time",
        "unicodedata",
        "zlib",
    ];
    // POSIX-only registrations (`#[cfg(unix)]` in `register_all`), plus
    // the `posix` shim exception documented above.
    #[cfg(unix)]
    names.extend([
        "_posixshmem",
        "_posixsubprocess",
        "fcntl",
        "posix",
        "resource",
        "termios",
    ]);
    // The RFC 0063 Windows-native quartet (`#[cfg(windows)]` in
    // `register_all`), plus the `nt` shim exception documented above.
    #[cfg(windows)]
    names.extend(["_overlapped", "_winapi", "msvcrt", "nt", "winreg"]);
    names.sort_unstable();
    Object::new_tuple(names.into_iter().map(Object::from_static).collect())
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// `sys.exit([code])` — modelled as raising `SystemExit(code)`. The
/// VM doesn't special-case this in its main loop, so it walks out as
/// an ordinary uncaught exception (so `try: sys.exit(1) except
/// SystemExit:` works). When it reaches the top level the CLI honours
/// it like CPython — terminating with `code` and printing no traceback
/// (see `Interpreter`/`Error::system_exit_code` and the CLI's
/// `exit_with_system_exit`).
fn sys_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() > 1 {
        return Err(type_error(format!(
            "exit expected at most 1 argument, got {}",
            args.len()
        )));
    }
    // A tuple payload spreads into the exception's `args` (CPython's
    // `PyErr_SetObject` uses a tuple value as the args tuple), so
    // `sys.exit((42,))` reports `code == 42` while `sys.exit((17, 23))`
    // keeps the whole tuple as the code (test_sys test_exit).
    let exc_args: Vec<Object> = match args.first() {
        Some(Object::Tuple(items)) => items.to_vec(),
        Some(other) => vec![other.clone()],
        None => vec![],
    };
    let code = match exc_args.len() {
        0 => Object::None,
        1 => exc_args[0].clone(),
        _ => Object::new_tuple(exc_args.clone()),
    };
    let inst = crate::builtin_types::make_exception_with_class(
        crate::builtin_types::builtin_types().system_exit.clone(),
        "",
    );
    if let Object::Instance(inst_rc) = &inst {
        inst_rc.slot_set("code", code.clone());
        inst_rc.slot_set("args", Object::new_tuple(exc_args));
    }
    Err(RuntimeError::PyException(crate::error::PyException::new(
        inst,
    )))
}

/// `sys.call_tracing(func, args)` — call `func(*args)` with tracing
/// re-enabled. CPython's `_PyEval_CallTracing` saves `tstate->tracing`
/// and resets it to zero so trace events fire inside the call even when
/// invoked from within a trace callback — pdb's `debug` command depends
/// on this to stop inside the recursive debugger (test_pdb's
/// test_errors_in_command sees `> <string>(1)<module>()`).
fn sys_call_tracing(args: &[Object]) -> Result<Object, RuntimeError> {
    let [func, tuple] = args else {
        return Err(type_error(format!(
            "call_tracing expected 2 arguments, got {}",
            args.len()
        )));
    };
    let call_args: Vec<Object> = match tuple {
        Object::Tuple(items) => items.to_vec(),
        other => {
            return Err(type_error(format!(
                "call_tracing() argument 2 must be tuple, not {}",
                other.type_name()
            )))
        }
    };
    let interp = crate::builtins::reentrant_interp()?;
    let g = interp.builtins_dict();
    let _reenabled = crate::trace::TracingReenabled::new();
    interp.call(func, &call_args, &[], &g)
}

fn sys_getrecursionlimit(args: &[Object]) -> Result<Object, RuntimeError> {
    reject_args("getrecursionlimit", args)?;
    Ok(Object::Int(crate::recursion::recursion_limit() as i64))
}

/// Zero-argument clinic check (`sys.getrecursionlimit(42)` →
/// "takes no arguments" — test_sys pins several of these).
fn reject_args(name: &str, args: &[Object]) -> Result<(), RuntimeError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(type_error(format!(
            "{name}() takes no arguments ({} given)",
            args.len()
        )))
    }
}

thread_local! {
    // PEP 0467 int<->str conversion cap. WeavePy doesn't yet *enforce* the
    // limit on conversion, but `sys.get/set_int_max_str_digits` must round-trip
    // (test_int reads/sets it; many modules query it at import).
    static INT_MAX_STR_DIGITS: std::cell::Cell<i64> = const { std::cell::Cell::new(4300) };
    // `sys.setswitchinterval` value: advisory for WeavePy's GIL (the
    // holder yields on bytecode-count boundaries, not a timer), but the
    // set/get pair must round-trip (test_sys test_switchinterval).
    static SWITCH_INTERVAL: std::cell::Cell<f64> = const { std::cell::Cell::new(0.005) };
}

fn sys_setswitchinterval(args: &[Object]) -> Result<Object, RuntimeError> {
    let [arg] = args else {
        return Err(type_error(format!(
            "setswitchinterval() takes exactly one argument ({} given)",
            args.len()
        )));
    };
    let interval = match arg {
        Object::Float(f) => *f,
        Object::Int(i) => *i as f64,
        Object::Bool(b) => f64::from(*b),
        other => {
            return Err(type_error(format!(
                "must be real number, not {}",
                other.type_name()
            )))
        }
    };
    if interval <= 0.0 {
        return Err(crate::error::value_error(
            "switch interval must be strictly positive",
        ));
    }
    SWITCH_INTERVAL.with(|c| c.set(interval));
    Ok(Object::None)
}

/// The current per-thread int↔str conversion digit cap (0 = unlimited).
/// Read by the str→int / int→str conversion paths to enforce PEP 0467.
pub fn int_max_str_digits() -> i64 {
    INT_MAX_STR_DIGITS.with(|c| c.get())
}

/// Startup override for the digit cap (`-X int_max_str_digits` /
/// `PYTHONINTMAXSTRDIGITS`, already validated by the CLI).
pub fn set_int_max_str_digits(n: i64) {
    INT_MAX_STR_DIGITS.with(|c| c.set(n));
    // The parser enforces the same cap on decimal int literals (CPython's
    // parsenumber goes through PyLong_FromString).
    weavepy_parser::set_int_literal_max_digits(n);
}

fn sys_get_int_max_str_digits(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(INT_MAX_STR_DIGITS.with(|c| c.get())))
}

fn sys_set_int_max_str_digits(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = match args.first() {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => return Err(type_error("'maxdigits' must be an integer")),
    };
    // CPython rejects values in (0, 640); 0 disables the limit.
    if n != 0 && n < 640 {
        return Err(value_error("maxdigits must be 0 or larger than 640"));
    }
    set_int_max_str_digits(n);
    Ok(Object::None)
}

fn sys_setrecursionlimit(args: &[Object]) -> Result<Object, RuntimeError> {
    // RFC 0037 (WS1) — the limit is now enforced by the dispatch loop's
    // recursion guard rather than left to the native stack.
    let limit = match args.first() {
        Some(Object::Int(n)) => *n,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::Long(n)) => {
            // Absurdly large limits are accepted by CPython; clamp to a
            // value the usize counter can represent.
            use num_traits::ToPrimitive;
            n.to_i64().unwrap_or(i64::MAX)
        }
        Some(_) => return Err(type_error("'limit' must be an integer")),
        None => return Err(type_error("setrecursionlimit expected 1 argument, got 0")),
    };
    if limit < 1 {
        return Err(value_error(
            "recursion limit must be greater or equal than 1",
        ));
    }
    match crate::recursion::set_limit(limit as usize) {
        Ok(()) => Ok(Object::None),
        Err(depth) => Err(RuntimeError::PyException(crate::error::PyException::new(
            crate::builtin_types::make_exception(
                "RecursionError",
                format!(
                    "cannot set the recursion limit to {limit} at the recursion depth {depth}: the limit is too low"
                ),
            ),
        ))),
    }
}

// Real interning: equal strings collapse to a single canonical object so
// `intern(a) is intern(b)` holds for `a == b`. CPython (and code that
// relies on it, e.g. `pathlib`'s `sys.intern(str(x))` over path parts,
// exercised by `test_parts_interning`) keeps a process-wide pool; ours is
// per-thread, which matches WeavePy's per-thread interpreter model.
//
// The pool is shared with the VM's instance-attribute store: CPython
// interns attribute names inside `PyObject_SetAttr`, which is what makes
// `sorted(x.__dict__)[0] is sorted(pickle.loads(s).__dict__)[0]` hold
// (pickle's `load_build` inserts `sys.intern(k)` keys —
// test_pickle test_attribute_name_interning).
thread_local! {
    static INTERN_POOL: RefCell<std::collections::HashMap<String, Object>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Canonicalize `name` through the interpreter's intern pool, seeding it
/// on first sight. Returns the pooled `Object::Str`.
pub(crate) fn intern_name(name: &str) -> Object {
    INTERN_POOL.with(|pool| {
        let mut map = pool.borrow_mut();
        if let Some(existing) = map.get(name) {
            existing.clone()
        } else {
            let obj = Object::from_str(name);
            map.insert(name.to_owned(), obj.clone());
            obj
        }
    })
}

/// Is `o` the pooled (`sys.intern`) instance for its value? `marshal`
/// writes such strings with the `*_INTERNED` type codes so a round-trip
/// preserves the interned identity (RFC 0060, test_marshal.testIntern).
pub(crate) fn str_is_interned(o: &Object) -> bool {
    let Object::Str(_) = o else { return false };
    INTERN_POOL.with(|pool| {
        pool.borrow()
            .get(&o.to_str())
            .is_some_and(|pooled| pooled.is_same(o))
    })
}

fn sys_intern(args: &[Object]) -> Result<Object, RuntimeError> {
    match args.first() {
        Some(s @ Object::Str(_)) => INTERN_POOL.with(|pool| {
            let key = s.to_str();
            let mut map = pool.borrow_mut();
            if let Some(existing) = map.get(&key) {
                Ok(existing.clone())
            } else {
                map.insert(key, s.clone());
                Ok(s.clone())
            }
        }),
        // A lone-surrogate-bearing str is still a str to CPython's intern —
        // pathlib interns every path part, and surrogateescape'd filenames
        // carry surrogates (test_pathlib's `P(base + '\udfff')` probes).
        // The WStr identity passes through unpooled: interning is an
        // optimization, and equal-value pooling only matters for the plain
        // Str fast path (test_parts_interning uses ASCII parts).
        Some(s @ Object::WStr(_)) => Ok(s.clone()),
        _ => Err(type_error("sys.intern() argument must be str")),
    }
}

fn sys_getdefaultencoding(args: &[Object]) -> Result<Object, RuntimeError> {
    reject_args("getdefaultencoding", args)?;
    // CPython 3 always returns "utf-8" here.
    Ok(Object::from_static("utf-8"))
}

fn sys_getfilesystemencoding(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::from_static("utf-8"))
}

fn sys_getfilesystemencodeerrors(_args: &[Object]) -> Result<Object, RuntimeError> {
    // POSIX (Linux/macOS) uses `surrogateescape` for the filesystem error
    // handler (CPython `Py_FileSystemDefaultEncodeErrors`); only Windows uses
    // `surrogatepass`. WeavePy targets POSIX, so report `surrogateescape` —
    // this is what PEP 383 round-tripping (`os.fsdecode`/`fsencode`) relies on.
    Ok(Object::from_static("surrogateescape"))
}

fn sys_getframe(
    args: &[Object],
    frame_stack: &crate::object::FrameStack,
) -> Result<Object, RuntimeError> {
    if args.len() > 1 {
        return Err(type_error(format!(
            "_getframe expected at most 1 argument, got {}",
            args.len()
        )));
    }
    let depth = match args.first() {
        Some(Object::Int(d)) => *d as usize,
        None => 0,
        _ => return Err(type_error("depth must be an int")),
    };
    // The topmost frame is the currently-executing one, which is
    // the *callee* of `sys._getframe`. CPython considers the
    // calling frame as depth 0; we mirror by indexing from the back.
    let len = frame_stack.borrow().len();
    if depth >= len {
        return Err(value_error("call stack is not deep enough"));
    }
    let idx = len - 1 - depth;
    // RFC 0058: the spine holds cheap shells; the Python-visible
    // frame object is materialised on demand right here.
    match crate::object::materialize_stack_at(frame_stack, idx) {
        Some(py) => {
            // PEP 578: `sys._getframe` audits with the frame object.
            audit_event("sys._getframe", &[Object::Frame(py.clone())])?;
            Ok(Object::Frame(py))
        }
        None => Err(value_error("call stack is not deep enough")),
    }
}

/// `sys._getframemodulename(depth=0)` (3.12+): the `__name__` of the
/// globals of the frame `depth` levels up, or `None` when the stack
/// isn't that deep. Unlike `_getframe` it never raises for shallow
/// stacks and doesn't materialise a frame object.
fn sys_getframemodulename(
    args: &[Object],
    frame_stack: &crate::object::FrameStack,
) -> Result<Object, RuntimeError> {
    let depth = match args.first() {
        Some(o) => match o {
            Object::Int(d) => *d,
            _ => return Err(type_error("depth must be an int")),
        },
        None => 0,
    };
    audit_event("sys._getframemodulename", &[Object::Int(depth)])?;
    if depth < 0 {
        return Ok(Object::None);
    }
    let len = frame_stack.borrow().len();
    let depth = depth as usize;
    if depth >= len {
        return Ok(Object::None);
    }
    let idx = len - 1 - depth;
    let Some(py) = crate::object::materialize_stack_at(frame_stack, idx) else {
        return Ok(Object::None);
    };
    let name = py
        .globals
        .borrow()
        .get(&crate::object::StrKey("__name__"))
        .cloned();
    Ok(name.unwrap_or(Object::None))
}

/// `sys.exception()` (PEP 3134 / 3.11+): the exception instance currently
/// being handled, or `None` if not in an `except`. Equivalent to
/// `sys.exc_info()[1]`. The verbatim CPython `contextlib` relies on this.
fn sys_exception(
    exc_info_stack: &Rc<RefCell<Vec<crate::error::PyException>>>,
) -> Result<Object, RuntimeError> {
    let stack = exc_info_stack.borrow();
    Ok(stack
        .last()
        .map(|top| top.instance.clone())
        .unwrap_or(Object::None))
}

fn sys_exc_info(
    exc_info_stack: &Rc<RefCell<Vec<crate::error::PyException>>>,
) -> Result<Object, RuntimeError> {
    let stack = exc_info_stack.borrow();
    if let Some(top) = stack.last() {
        let inst = top.instance.clone();
        let type_obj = match &inst {
            Object::Instance(i) => Object::Type(i.cls()),
            _ => Object::None,
        };
        let tb = match &inst {
            Object::Instance(i) => i.slot_get("__traceback__").unwrap_or(Object::None),
            _ => Object::None,
        };
        Ok(Object::new_tuple(vec![type_obj, inst, tb]))
    } else {
        Ok(Object::new_tuple(vec![
            Object::None,
            Object::None,
            Object::None,
        ]))
    }
}

fn sys_default_excepthook(args: &[Object]) -> Result<Object, RuntimeError> {
    // `sys.__excepthook__(type, value, tb)` — CPython's pristine hook
    // renders the full traceback (source lines, carets, chained
    // causes/contexts) to `sys.stderr`. Route through the Python
    // `traceback` module when an interpreter is on the stack; fall
    // back to a bare "Type: msg" line otherwise.
    if args.len() != 3 {
        return Err(type_error(format!(
            "excepthook expected 3 arguments, got {}",
            args.len()
        )));
    }
    let value = args.get(1).cloned().unwrap_or(Object::None);
    // A non-exception `value` doesn't raise out of the hook — CPython's
    // `PyErr_Display` reports the internal TypeError on stderr instead
    // (test_sys.ExceptHookTest.test_excepthook feeds `('1', 1, 1)`).
    let value_is_exception = matches!(
        &value,
        Object::Instance(i) if i.cls().mro.borrow().iter().any(|t| t.name == "BaseException")
    );
    if !value_is_exception {
        let msg = format!(
            "TypeError: print_exception(): Exception expected for value, {} found\n",
            value.type_name()
        );
        if let Ok(interp) = crate::builtins::reentrant_interp() {
            let g = interp.builtins_dict();
            if let Some(target) = interp.current_sys_attr("stderr") {
                if let Ok(write) = interp.load_attr_public(&target, "write") {
                    let _ = interp.call(&write, &[Object::from_str(msg.clone())], &[], &g);
                    return Ok(Object::None);
                }
            }
        }
        eprint!("{msg}");
        return Ok(Object::None);
    }
    if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
        // SAFETY: published by `publish_interpreter_ptr` from a
        // `&mut Interpreter` still on the call stack above us; the
        // GIL makes this thread's access exclusive.
        let interp = unsafe { &mut *ptr };
        if interp.print_exception_via_traceback(&value) {
            return Ok(Object::None);
        }
    }
    let kind = match &value {
        Object::Instance(i) => i.cls().name.clone(),
        _ => "Exception".to_owned(),
    };
    let msg = crate::builtin_types::exception_message(&value).unwrap_or_default();
    if msg.is_empty() {
        eprintln!("{kind}");
    } else {
        eprintln!("{kind}: {msg}");
    }
    Ok(Object::None)
}

// Trace and profile hooks live in the runtime's thread-local registry
// (:mod:`crate::trace`) so the VM dispatcher and ``sys.gettrace`` /
// ``sys.getprofile`` see the same value. Line-level event firing
// inside the interpreter dispatch is gated behind RFC 0031; for now
// these accessors are observable but do not call back into the hook
// at every opcode (that requires deeper VM surgery and a perf
// trade-off discussion).

fn sys_settrace(args: &[Object]) -> Result<Object, RuntimeError> {
    audit_event("sys.settrace", &[])?;
    let hook = args.first().cloned().unwrap_or(Object::None);
    crate::trace::set_trace_hook(hook);
    Ok(Object::None)
}

fn sys_addaudithook(args: &[Object]) -> Result<Object, RuntimeError> {
    let hook = args.first().cloned().unwrap_or(Object::None);
    // CPython fires `sys.addaudithook` *before* registering. An
    // existing hook can veto the registration by raising RuntimeError
    // (or a subclass) — the error is swallowed and the hook is simply
    // not added (test_audit test_block_add_hook). Any other exception
    // propagates to the caller.
    if let Err(err) = audit_event("sys.addaudithook", &[]) {
        if err_is_exception_named(&err, "RuntimeError") {
            return Ok(Object::None);
        }
        return Err(err);
    }
    crate::trace::add_audit_hook(hook);
    Ok(Object::None)
}

/// True when `err` is a Python exception whose class (or an MRO
/// ancestor) is named `name`.
pub(crate) fn err_is_exception_named(err: &RuntimeError, name: &str) -> bool {
    let RuntimeError::PyException(pe) = err else {
        return false;
    };
    match &pe.instance {
        Object::Instance(i) => i.cls().mro.borrow().iter().any(|t| t.name == name),
        Object::Type(t) => t.mro.borrow().iter().any(|t| t.name == name),
        _ => false,
    }
}

/// PEP 578 — `sys.audit(event, *args)`. Walks the registered audit
/// hooks and invokes each with `(event, args)`. Stdlib code should
/// prefer [`audit_event`] which inserts the call without paying for
/// the dict lookup.
fn sys_audit(args: &[Object]) -> Result<Object, RuntimeError> {
    let event = match args.first() {
        Some(Object::Str(s)) => s.to_string(),
        Some(other) => {
            return Err(crate::error::type_error(format!(
                "sys.audit() argument 1 must be str, not '{}'",
                other.type_name()
            )))
        }
        None => return Err(crate::error::type_error("sys.audit() missing event name")),
    };
    let rest: Vec<Object> = args.iter().skip(1).cloned().collect();
    audit_event(&event, &rest)?;
    Ok(Object::None)
}

/// Fire a PEP 578 audit event. Stdlib code (and the VM) calls this
/// at documented event sites (`open`, `compile`, `exec`,
/// `socket.connect`, `subprocess.Popen`, `import`, …).
///
/// CPython semantics (`sys_audit_tstate`):
/// - hooks fire in registration order;
/// - the first exception a hook raises *aborts the audited
///   operation* — it propagates to whoever fired the event (this is
///   how hooks veto operations, e.g. test_audit's RuntimeError
///   cascades);
/// - tracing/profiling is disabled while a hook runs unless the hook
///   object has a truthy `__cantrace__` attribute.
pub fn audit_event(event: &str, args: &[Object]) -> Result<(), RuntimeError> {
    if !crate::trace::any_audit_active() {
        return Ok(());
    }
    let hooks = crate::trace::audit_hooks();
    if hooks.is_empty() {
        return Ok(());
    }
    let Some(_guard) = crate::trace::ReentryGuard::acquire() else {
        return Ok(());
    };
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return Ok(());
    };
    // SAFETY: `ptr` was published by `publish_interpreter_ptr` from
    // a `&mut Interpreter` that is still on the call stack above us
    // (the guard pops on drop). The reentry guard ensures we don't
    // re-enter a Python frame that's currently borrowing the
    // interpreter mutably. Mutation from this thread is exclusive
    // because the VM holds the GIL across the whole audit event.
    let interp = unsafe { &mut *ptr };
    let arg_tuple = Object::new_tuple(args.to_vec());
    let outer = interp.builtins_dict();
    // `PyThreadState_EnterTracing`: hooks run untraced by default.
    let _suppress = crate::trace::TracingSuppressGuard::enter();
    for hook in hooks {
        let can_trace = match interp.load_attr_public(&hook, "__cantrace__") {
            Ok(v) => v.is_truthy(),
            Err(err) => {
                if err_is_exception_named(&err, "AttributeError") {
                    false
                } else {
                    return Err(err);
                }
            }
        };
        let call_args = [Object::from_str(event.to_string()), arg_tuple.clone()];
        let result = if can_trace {
            let _allow = crate::trace::TracingAllowGuard::enter();
            interp.call_object_with_globals(&hook, &call_args, &[], &outer)
        } else {
            interp.call_object_with_globals(&hook, &call_args, &[], &outer)
        };
        result?;
    }
    Ok(())
}

fn sys_setprofile(args: &[Object]) -> Result<Object, RuntimeError> {
    audit_event("sys.setprofile", &[])?;
    let hook = args.first().cloned().unwrap_or(Object::None);
    crate::trace::set_profile_hook(hook);
    Ok(Object::None)
}

fn sys_settraceallthreads(args: &[Object]) -> Result<Object, RuntimeError> {
    let hook = args.first().cloned().unwrap_or(Object::None);
    crate::trace::set_trace_all_threads(hook);
    Ok(Object::None)
}

fn sys_setprofileallthreads(args: &[Object]) -> Result<Object, RuntimeError> {
    let hook = args.first().cloned().unwrap_or(Object::None);
    crate::trace::set_profile_all_threads(hook);
    Ok(Object::None)
}

fn sys_gettrace(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(crate::trace::trace_hook_raw().unwrap_or(Object::None))
}

fn sys_getprofile(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(crate::trace::profile_hook_raw().unwrap_or(Object::None))
}

/// Best-effort CPython-shaped `sys.getsizeof` estimate. Shared with
/// `tracemalloc`'s per-object accounting so `get_traced_memory()` and
/// `sys.getsizeof` agree (`test_tracemalloc.test_get_traced_memory`
/// computes the expected traced size from `sys.getsizeof(b'')`).
pub(crate) fn sizeof_estimate(o: &Object) -> i64 {
    match o {
        Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None => 28,
        // CPython's compact-unicode layout: 40-byte ASCII struct or 56-byte
        // wide struct, plus len+1 units of the kind width
        // (test_str.test_raiseMemError pins all four kinds).
        Object::Str(s) => {
            let len = crate::builtins::str_char_len(s) as i64;
            match s.chars().map(u32::from).max().unwrap_or(0) {
                0..=0x7f => 40 + len + 1,
                0x80..=0xff => 56 + (len + 1),
                0x100..=0xffff => 56 + 2 * (len + 1),
                _ => 56 + 4 * (len + 1),
            }
        }
        Object::WStr(s) => {
            let len = s.len() as i64;
            match s.iter().copied().max().unwrap_or(0) {
                0..=0x7f => 40 + len + 1,
                0x80..=0xff => 56 + (len + 1),
                0x100..=0xffff => 56 + 2 * (len + 1),
                _ => 56 + 4 * (len + 1),
            }
        }
        Object::Bytes(b) => 33 + b.len() as i64,
        Object::ByteArray(b) => 56 + b.borrow().len() as i64,
        Object::List(l) => 56 + (l.borrow().len() as i64) * 8,
        Object::Tuple(t) => 40 + (t.len() as i64) * 8,
        Object::Dict(d) => 64 + (d.borrow().len() as i64) * 16,
        Object::Set(s) => 216 + (s.borrow().len() as i64) * 16,
        Object::FrozenSet(s) => 216 + (s.len() as i64) * 16,
        // CPython: `sys.getsizeof(cell)` is 40 on 64-bit builds.
        Object::Cell(_) => 40,
        // CPython `memoryobject.c`: sizeof(PyMemoryViewObject) embeds one
        // shape/strides/suboffsets triple, plus one more per extra
        // dimension (test_buffer.test_memoryview_sizeof pins the layout).
        Object::MemoryView(mv) => {
            let ptr = std::mem::size_of::<usize>() as i64;
            let ndim = mv.shape_dims().len().max(1) as i64;
            18 * ptr + 3 * ptr * ndim
        }
        _ => 16,
    }
}

fn sys_getsizeof(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `getsizeof` is a per-object slot. We answer with a
    // best-effort estimate so user code doesn't crash, but make no
    // promise of accuracy.
    let size = args.first().map(sizeof_estimate).unwrap_or(0);
    Ok(Object::Int(size))
}

/// CPython 3.13 `sys.flags` struct-sequence field order. `tuple(sys.flags)`
/// must yield these in exactly this order (`test_multiprocessing` /
/// `test_sys` compare `sys.flags` across a spawned child via the tuple form).
pub(crate) const SYS_FLAGS_FIELDS: &[&str] = &[
    "debug",
    "inspect",
    "interactive",
    "optimize",
    "dont_write_bytecode",
    "no_user_site",
    "no_site",
    "ignore_environment",
    "verbose",
    "bytes_warning",
    "quiet",
    "hash_randomization",
    "isolated",
    "dev_mode",
    "utf8_mode",
    "warn_default_encoding",
    "safe_path",
    "int_max_str_digits",
];

/// `sys.version_info` (and `sys.implementation.version`) — CPython's
/// `PyStructSequence` with named fields; `sys.version_info.major` is
/// one of the most common introspection idioms in the ecosystem
/// (RFC 0055 WS1: `test_venv`'s zip-path probe and `test_embed`'s
/// first failure were both this attribute).
const VERSION_INFO_FIELDS: &[&str] = &["major", "minor", "micro", "releaselevel", "serial"];

/// The visible (tuple-indexed) fields of `sys.getwindowsversion()` —
/// CPython's `windows_version_fields` has `n_in_sequence = 5`; the
/// remaining five members are attribute-only.
#[cfg(windows)]
const WINDOWS_VERSION_VISIBLE: [&str; 5] = ["major", "minor", "build", "platform", "service_pack"];

/// The `HMODULE` of `python313.dll` when this interpreter is running
/// out of the runtime DLL (RFC 0064 WS1: the shipped exe is a shim
/// that loads it), 0 when statically linked (embedder test harnesses).
/// `GetModuleHandleW` peeks at the process's loaded-module list
/// without loading anything and without taking a reference.
#[cfg(windows)]
fn python_dll_handle() -> i64 {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    // "python313.dll" as static UTF-16, NUL-terminated.
    const NAME: &[u16] = &[
        b'p' as u16,
        b'y' as u16,
        b't' as u16,
        b'h' as u16,
        b'o' as u16,
        b'n' as u16,
        b'3' as u16,
        b'1' as u16,
        b'3' as u16,
        b'.' as u16,
        b'd' as u16,
        b'l' as u16,
        b'l' as u16,
        0,
    ];
    // SAFETY: NAME is a valid NUL-terminated UTF-16 string.
    let handle = unsafe { GetModuleHandleW(NAME.as_ptr()) };
    handle as usize as i64
}

/// `sys.getwindowsversion()` — the 10-member struct sequence of
/// `Python/sysmodule.c`'s `sys_getwindowsversion_impl`. Sourced from
/// ntdll's `RtlGetVersion` rather than `GetVersionExW`: the latter lies
/// under the compatibility-manifest shims (an unmanifested process is
/// told "6.2" forever), which is the same problem CPython works around
/// by re-reading kernel32.dll's version resource. RtlGetVersion reports
/// the true version, so `platform_version` comes from the same call.
#[cfg(windows)]
fn sys_getwindowsversion(_args: &[Object]) -> Result<Object, RuntimeError> {
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOEXW;
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(info: *mut OSVERSIONINFOEXW) -> i32;
    }
    let mut info: OSVERSIONINFOEXW = unsafe { std::mem::zeroed() };
    info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOEXW>() as u32;
    // NTSTATUS 0 == STATUS_SUCCESS; the call cannot fail for a
    // correctly-sized buffer, but stay honest anyway.
    if unsafe { RtlGetVersion(&raw mut info) } != 0 {
        return Err(crate::error::os_error("RtlGetVersion failed"));
    }
    let ty = crate::stdlib::os::struct_seq_type_layout(
        "getwindowsversion",
        "sys",
        [
            "major",
            "minor",
            "build",
            "platform",
            "service_pack",
            "service_pack_major",
            "service_pack_minor",
            "suite_mask",
            "product_type",
            "platform_version",
        ]
        .iter()
        .map(|f| Some(*f))
        .collect(),
        WINDOWS_VERSION_VISIBLE.len(),
    );
    let visible = vec![
        Object::Int(i64::from(info.dwMajorVersion)),
        Object::Int(i64::from(info.dwMinorVersion)),
        Object::Int(i64::from(info.dwBuildNumber)),
        Object::Int(i64::from(info.dwPlatformId)),
        Object::from_str(crate::stdlib::nt_support::from_wide_nul(&info.szCSDVersion)),
    ];
    let obj = crate::stdlib::os::struct_seq_instance(ty, &WINDOWS_VERSION_VISIBLE, visible);
    // The five hidden named members (attribute-only, exactly like
    // `time.struct_time`'s `tm_zone`/`tm_gmtoff` extras): fill them
    // straight into the instance dict, which bypasses the readonly
    // `__setattr__` guard the struct-seq type installs.
    if let Object::Instance(inst) = &obj {
        let mut d = inst.dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("service_pack_major")),
            Object::Int(i64::from(info.wServicePackMajor)),
        );
        d.insert(
            DictKey(Object::from_static("service_pack_minor")),
            Object::Int(i64::from(info.wServicePackMinor)),
        );
        d.insert(
            DictKey(Object::from_static("suite_mask")),
            Object::Int(i64::from(info.wSuiteMask)),
        );
        d.insert(
            DictKey(Object::from_static("product_type")),
            Object::Int(i64::from(info.wProductType)),
        );
        d.insert(
            DictKey(Object::from_static("platform_version")),
            Object::new_tuple(vec![
                Object::Int(i64::from(info.dwMajorVersion)),
                Object::Int(i64::from(info.dwMinorVersion)),
                Object::Int(i64::from(info.dwBuildNumber)),
            ]),
        );
    }
    Ok(obj)
}

/// Mark a sys struct-sequence type as non-instantiable — CPython
/// creates `sys.version_info` / `sys.flags` with
/// `Py_TPFLAGS_DISALLOW_INSTANTIATION` (test_sys
/// test_sys_version_info_no_instantiation / test_sys_flags_no_…).
fn disallow_instantiation(ty: &Rc<crate::types::TypeObject>, qualname: &'static str) {
    use crate::object::BuiltinFn;
    let mut d = ty.dict.borrow_mut();
    let key = DictKey(Object::from_static("__new__"));
    if d.get(&key).is_some() {
        return;
    }
    d.insert(
        key,
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__new__",
            binds_instance: false,
            call: Box::new(move |_args| {
                Err(type_error(format!("cannot create '{qualname}' instances")))
            }),
            call_kw: None,
        })),
    );
}

fn version_info_value() -> Object {
    let ty = crate::stdlib::os::struct_seq_type("version_info", "sys", VERSION_INFO_FIELDS);
    disallow_instantiation(&ty, "sys.version_info");
    let values = vec![
        Object::Int(PY_VERSION.0),
        Object::Int(PY_VERSION.1),
        Object::Int(PY_VERSION.2),
        Object::from_static("final"),
        Object::Int(0),
    ];
    crate::stdlib::os::struct_seq_instance(ty, VERSION_INFO_FIELDS, values)
}

fn sys_flags_value() -> Object {
    // CPython exposes `sys.flags` as a real `PyStructSequence` (a `tuple`
    // subclass): addressable by attribute (`sys.flags.optimize`) *and* by
    // index, with `len()`/iteration over the field values. `tuple(sys.flags)`
    // is used by `test_multiprocessing` to round-trip flags through a spawned
    // child, so a plain namespace (not iterable) is insufficient.
    // 3.13's `flags` carries `gil` as a *hidden* named field: reachable
    // as `sys.flags.gil` but excluded from `len()`/indexing/iteration
    // (`len(sys.flags) == 18` in test_sys.test_sys_flags while
    // test_cmd_line.test_python_gil reads `sys.flags.gil`).
    let slots: Vec<Option<&'static str>> = SYS_FLAGS_FIELDS
        .iter()
        .map(|f| Some(*f))
        .chain(std::iter::once(Some("gil")))
        .collect();
    let ty =
        crate::stdlib::os::struct_seq_type_layout("flags", "sys", slots, SYS_FLAGS_FIELDS.len());
    disallow_instantiation(&ty, "sys.flags");
    let values: Vec<Object> = SYS_FLAGS_FIELDS
        .iter()
        .map(|f| match *f {
            // CPython's default cap on int<->str conversion size (PEP 0467 /
            // `-X int_max_str_digits`). test_int reads this off `sys.flags`.
            "int_max_str_digits" => Object::Int(4300),
            // WeavePy stores `str` as UTF-8, so UTF-8 mode is on unless the
            // CLI explicitly passes `-X utf8=0` (applied in apply_run_options).
            "utf8_mode" => Object::Int(1),
            // The two bool fields on CPython's `sys.flags`.
            "dev_mode" | "safe_path" => Object::Bool(false),
            _ => Object::Int(0),
        })
        .collect();
    let flags = crate::stdlib::os::struct_seq_instance(ty, SYS_FLAGS_FIELDS, values);
    if let Object::Instance(inst) = &flags {
        // This build always runs with the GIL (no free-threading).
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("gil")), Object::Int(1));
    }
    flags
}

/// `sys.float_info` field order (CPython `floatinfo_fields`).
const FLOAT_INFO_FIELDS: &[&str] = &[
    "max",
    "max_exp",
    "max_10_exp",
    "min",
    "min_exp",
    "min_10_exp",
    "dig",
    "mant_dig",
    "epsilon",
    "radix",
    "rounds",
];

fn sys_float_info() -> Object {
    // A real struct sequence (`len(sys.float_info) == 11`, indexable,
    // attribute access) — test_sys test_attributes counts it.
    let ty = crate::stdlib::os::struct_seq_type("float_info", "sys", FLOAT_INFO_FIELDS);
    let values = vec![
        Object::Float(f64::MAX),
        Object::Int(1024),
        Object::Int(308),
        Object::Float(f64::MIN_POSITIVE),
        Object::Int(-1021),
        Object::Int(-307),
        Object::Int(15),
        Object::Int(53),
        Object::Float(f64::EPSILON),
        Object::Int(2),
        Object::Int(1),
    ];
    crate::stdlib::os::struct_seq_instance(ty, FLOAT_INFO_FIELDS, values)
}

/// `sys.int_info` field order (CPython `int_info_fields`).
const INT_INFO_FIELDS: &[&str] = &[
    "bits_per_digit",
    "sizeof_digit",
    "default_max_str_digits",
    "str_digits_check_threshold",
];

fn sys_int_info() -> Object {
    let ty = crate::stdlib::os::struct_seq_type("int_info", "sys", INT_INFO_FIELDS);
    let values = vec![
        Object::Int(30),
        Object::Int(4),
        Object::Int(4300),
        Object::Int(640),
    ];
    crate::stdlib::os::struct_seq_instance(ty, INT_INFO_FIELDS, values)
}

/// `sys.hash_info` field order (CPython `hash_info_fields`).
const HASH_INFO_FIELDS: &[&str] = &[
    "width",
    "modulus",
    "inf",
    "nan",
    "imag",
    "algorithm",
    "hash_bits",
    "seed_bits",
    "cutoff",
];

fn sys_hash_info() -> Object {
    let ty = crate::stdlib::os::struct_seq_type("hash_info", "sys", HASH_INFO_FIELDS);
    // `_PyHASH_MODULUS` on a 64-bit build is the Mersenne prime 2**61-1,
    // which is also the modulus `python_int_hash`/`py_hash_double` reduce
    // through. test_numeric_tower derives `_PyHASH_MODULUS` from this field
    // and checks exact Fraction hashes against it, so it must match.
    let values = vec![
        Object::Int(64),
        Object::Int((1i64 << 61) - 1),
        Object::Int(314_159),
        Object::Int(0),
        Object::Int(1_000_003),
        Object::from_static("siphash13"),
        Object::Int(64),
        Object::Int(128),
        Object::Int(0),
    ];
    crate::stdlib::os::struct_seq_instance(ty, HASH_INFO_FIELDS, values)
}

/// Whether `name` is a documented stdlib module name. The module
/// shadowing diagnostics (attribute miss / `IMPORT_FROM` on a module
/// whose file sits in the script directory) consult this, mirroring
/// CPython's `sys.stdlib_module_names` lookup (error path only, so
/// rebuilding the set is acceptable).
pub fn is_stdlib_module_name(name: &str) -> bool {
    match stdlib_module_names_value() {
        Object::FrozenSet(s) => s.contains(&DictKey(Object::from_str(name))),
        _ => false,
    }
}

/// `sys.stdlib_module_names` — the documented set of standard-
/// library module names. CPython 3.13 ships a frozenset; we
/// mirror that with a [`Object::FrozenSet`].
fn stdlib_module_names_value() -> Object {
    use crate::object::SetData;
    let names: &[&'static str] = &[
        "_abc",
        "_aix_support",
        "_ast",
        "_asyncio",
        "_bisect",
        "_blake2",
        "_bz2",
        "_codecs",
        "_codecs_cn",
        "_codecs_hk",
        "_codecs_iso2022",
        "_codecs_jp",
        "_codecs_kr",
        "_codecs_tw",
        "_collections",
        "_collections_abc",
        "_compat_pickle",
        "_compression",
        "_contextvars",
        "_csv",
        "_ctypes",
        "_curses",
        "_curses_panel",
        "_datetime",
        "_decimal",
        "_elementtree",
        "_frozen_importlib",
        "_frozen_importlib_external",
        "_functools",
        "_hashlib",
        "_heapq",
        "_imp",
        "_io",
        "_json",
        "_locale",
        "_lsprof",
        "_lzma",
        "_markupbase",
        "_md5",
        "_multibytecodec",
        "_multiprocessing",
        "_opcode",
        "_operator",
        "_osx_support",
        "_pickle",
        "_posixshmem",
        "_posixsubprocess",
        "_py_abc",
        "_pydecimal",
        "_pyio",
        "_queue",
        "_random",
        "_sha1",
        "_sha2",
        "_sha3",
        "_signal",
        "_sitebuiltins",
        "_socket",
        "_sqlite3",
        "_sre",
        "_ssl",
        "_stat",
        "_string",
        "_strptime",
        "_struct",
        "_symtable",
        "_sysconfig",
        "_thread",
        "_threading_local",
        "_tkinter",
        "_tokenize",
        "_tracemalloc",
        "_uuid",
        "_warnings",
        "_weakref",
        "_weakrefset",
        "_zoneinfo",
        "abc",
        "antigravity",
        "argparse",
        "array",
        "ast",
        "asynchat",
        "asyncio",
        "asyncore",
        "atexit",
        "audioop",
        "base64",
        "bdb",
        "binascii",
        "bisect",
        "builtins",
        "bz2",
        "cProfile",
        "calendar",
        "cgi",
        "cgitb",
        "chunk",
        "cmath",
        "cmd",
        "code",
        "codecs",
        "codeop",
        "collections",
        "colorsys",
        "compileall",
        "concurrent",
        "configparser",
        "contextlib",
        "contextvars",
        "copy",
        "copyreg",
        "crypt",
        "csv",
        "ctypes",
        "curses",
        "dataclasses",
        "datetime",
        "dbm",
        "decimal",
        "difflib",
        "dis",
        "doctest",
        "email",
        "encodings",
        "ensurepip",
        "enum",
        "errno",
        "faulthandler",
        "fcntl",
        "filecmp",
        "fileinput",
        "fnmatch",
        "fractions",
        "ftplib",
        "functools",
        "gc",
        "genericpath",
        "getopt",
        "getpass",
        "gettext",
        "glob",
        "graphlib",
        "grp",
        "gzip",
        "hashlib",
        "heapq",
        "hmac",
        "html",
        "http",
        "idlelib",
        "imaplib",
        "imghdr",
        "imp",
        "importlib",
        "inspect",
        "io",
        "ipaddress",
        "itertools",
        "json",
        "keyword",
        "linecache",
        "locale",
        "logging",
        "lzma",
        "mailbox",
        "mailcap",
        "marshal",
        "math",
        "mimetypes",
        "mmap",
        "modulefinder",
        "msilib",
        "msvcrt",
        "multiprocessing",
        "netrc",
        "nis",
        "nntplib",
        "ntpath",
        "numbers",
        "opcode",
        "operator",
        "optparse",
        "os",
        "ossaudiodev",
        "pathlib",
        "pdb",
        "pickle",
        "pickletools",
        "pipes",
        "pkgutil",
        "platform",
        "plistlib",
        "poplib",
        "posix",
        "posixpath",
        "pprint",
        "profile",
        "pstats",
        "pty",
        "pwd",
        "py_compile",
        "pyclbr",
        "pydoc",
        "pydoc_data",
        "pyexpat",
        "queue",
        "quopri",
        "random",
        "re",
    ];
    let mut set = SetData::default();
    for n in names {
        set.insert(DictKey(Object::from_static(n)));
    }
    // Two-shot to dodge the 200-element array literal limit.
    for n in &[
        "readline",
        "reprlib",
        "resource",
        "rlcompleter",
        "runpy",
        "sched",
        "secrets",
        "select",
        "selectors",
        "shelve",
        "shlex",
        "shutil",
        "signal",
        "site",
        "smtpd",
        "smtplib",
        "sndhdr",
        "socket",
        "socketserver",
        "spwd",
        "sqlite3",
        "sre_compile",
        "sre_constants",
        "sre_parse",
        "ssl",
        "stat",
        "statistics",
        "string",
        "stringprep",
        "struct",
        "subprocess",
        "sunau",
        "symtable",
        "sys",
        "sysconfig",
        "syslog",
        "tabnanny",
        "tarfile",
        "telnetlib",
        "tempfile",
        "termios",
        "test",
        "textwrap",
        "threading",
        "time",
        "timeit",
        "tkinter",
        "token",
        "tokenize",
        "tomllib",
        "trace",
        "traceback",
        "tracemalloc",
        "tty",
        "turtle",
        "turtledemo",
        "types",
        "typing",
        "unicodedata",
        "unittest",
        "urllib",
        "uu",
        "uuid",
        "venv",
        "warnings",
        "wave",
        "weakref",
        "webbrowser",
        "winreg",
        "winsound",
        "wsgiref",
        "xdrlib",
        "xml",
        "xmlrpc",
        "zipapp",
        "zipfile",
        "zipimport",
        "zlib",
        "zoneinfo",
    ] {
        set.insert(DictKey(Object::from_static(n)));
    }
    Object::FrozenSet(Rc::new(crate::object::FrozenSetObj::new(set)))
}

/// `sys.getrefcount(obj)` — best-effort, derived from the real
/// `Rc::strong_count` of the payload. Infrastructure references
/// (the cycle-GC registry's handle, weakref slots' strong clones)
/// are discounted so the number tracks *program-visible* bindings;
/// `+1` accounts for the argument reference, like CPython. The
/// exact number is implementation-specific even in CPython.
fn sys_getrefcount(args: &[Object]) -> Result<Object, RuntimeError> {
    let Some(obj) = args.first() else {
        return Err(type_error("getrefcount() takes exactly 1 argument"));
    };
    let strong = crate::gc_trace::strong_count_for(obj);
    let id = crate::weakref_registry::id_of(obj);
    let registry = usize::from(crate::gc_trace::is_tracked(id));
    let weak_clones = crate::weakref_registry::strong_clone_count(id);
    // A dropped-but-registry-pinned memoryview (dead under CPython
    // refcounting) must not count through its exporter edge.
    let zombie_refs = crate::gc_trace::zombie_memoryview_refs_to(id);
    // The clone in our `args` slice plays the role of CPython's
    // "+1 for the argument reference" — no extra increment needed.
    let visible = strong
        .saturating_sub(registry)
        .saturating_sub(weak_clones)
        .saturating_sub(zombie_refs);
    Ok(Object::Int(visible.max(1) as i64))
}

thread_local! {
    /// PEP 565-era coroutine origin tracking depth
    /// (`sys.set_coroutine_origin_tracking_depth`). Per-thread in
    /// CPython (a `PyThreadState` field).
    static CORO_ORIGIN_DEPTH: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// Current `sys.get_coroutine_origin_tracking_depth()` value; read by
/// the interpreter when constructing coroutine objects.
pub fn coroutine_origin_tracking_depth() -> i64 {
    CORO_ORIGIN_DEPTH.with(std::cell::Cell::get)
}

fn sys_set_coroutine_origin_tracking_depth(args: &[Object]) -> Result<Object, RuntimeError> {
    let depth = match args.first() {
        Some(Object::Int(i)) => *i,
        Some(Object::Bool(b)) => i64::from(*b),
        _ => {
            return Err(type_error(
                "set_coroutine_origin_tracking_depth() takes an integer",
            ))
        }
    };
    if depth < 0 {
        return Err(crate::error::value_error("depth must be >= 0"));
    }
    CORO_ORIGIN_DEPTH.with(|c| c.set(depth));
    Ok(Object::None)
}

thread_local! {
    /// PEP 525 `sys.set_asyncgen_hooks` — `(firstiter, finalizer)`.
    /// Per-thread in CPython (a `PyThreadState` field).
    static ASYNCGEN_HOOKS: std::cell::RefCell<(Object, Object)> =
        const { std::cell::RefCell::new((Object::None, Object::None)) };
}

/// The currently-installed `(firstiter, finalizer)` asyncgen hooks.
pub fn asyncgen_hooks() -> (Object, Object) {
    ASYNCGEN_HOOKS.with(|h| h.borrow().clone())
}

fn check_asyncgen_hook(v: &Object, which: &str) -> Result<(), RuntimeError> {
    let callable = matches!(
        v,
        Object::Function(_)
            | Object::Builtin(_)
            | Object::BoundMethod(_)
            | Object::Type(_)
            | Object::StaticMethod(_)
    ) || matches!(v, Object::Instance(inst) if inst.cls().lookup("__call__").is_some());
    if matches!(v, Object::None) || callable {
        Ok(())
    } else {
        Err(type_error(format!(
            "callable {which} expected, got {}",
            v.type_name()
        )))
    }
}

fn sys_set_asyncgen_hooks(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let mut firstiter = args.first().cloned();
    let mut finalizer = args.get(1).cloned();
    for (k, v) in kwargs {
        match k.as_str() {
            "firstiter" => firstiter = Some(v.clone()),
            "finalizer" => finalizer = Some(v.clone()),
            other => {
                return Err(type_error(format!(
                    "set_asyncgen_hooks() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    if let Some(f) = &firstiter {
        check_asyncgen_hook(f, "firstiter")?;
    }
    if let Some(f) = &finalizer {
        check_asyncgen_hook(f, "finalizer")?;
    }
    ASYNCGEN_HOOKS.with(|h| {
        let mut h = h.borrow_mut();
        if let Some(f) = firstiter {
            h.0 = f;
        }
        if let Some(f) = finalizer {
            h.1 = f;
        }
    });
    Ok(Object::None)
}

fn sys_get_asyncgen_hooks(_args: &[Object]) -> Result<Object, RuntimeError> {
    let (firstiter, finalizer) = asyncgen_hooks();
    Ok(Object::new_tuple(vec![firstiter, finalizer]))
}

/// Default `sys.displayhook`: if the value is None do nothing,
/// otherwise print `repr(value)` and stash on
/// `builtins._`. Matches CPython's reference implementation.
/// PEP 553 default `sys.breakpointhook`. Mirrors CPython's
/// `sysmodule.c:sys_breakpointhook`: consult `$PYTHONBREAKPOINT`
/// (`os.environ` writes through to the process env, so
/// `EnvironmentVarGuard` changes are visible here), resolve the dotted
/// callable fresh on every call (so `unittest.mock.patch('pdb.set_trace')`
/// intercepts), and warn `RuntimeWarning` on an unimportable value.
fn sys_breakpointhook_kw(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr().ok_or_else(|| {
        crate::error::runtime_error("sys.breakpointhook requires a running interpreter")
    })?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let envar = std::env::var("PYTHONBREAKPOINT").unwrap_or_default();
    if envar == "0" {
        return Ok(Object::None);
    }
    let hookname = if envar.is_empty() {
        "pdb.set_trace".to_owned()
    } else {
        envar.clone()
    };
    let (modname, funcname) = match hookname.rfind('.') {
        Some(i) => (hookname[..i].to_owned(), hookname[i + 1..].to_owned()),
        None => ("builtins".to_owned(), hookname.clone()),
    };
    let hook = if modname.is_empty() || funcname.is_empty() {
        None
    } else {
        match interp.import_path(&modname) {
            Ok(module) => interp.load_attr_public(&module, &funcname).ok(),
            Err(_) => None,
        }
    };
    let Some(hook) = hook else {
        interp.warn_runtime_from_builtin(format!(
            "Ignoring unimportable $PYTHONBREAKPOINT: \"{envar}\""
        ))?;
        return Ok(Object::None);
    };
    interp.call_object(hook, args, kwargs)
}

fn sys_displayhook(args: &[Object]) -> Result<Object, RuntimeError> {
    let [value] = args else {
        return Err(type_error(format!(
            "displayhook() takes exactly one argument ({} given)",
            args.len()
        )));
    };
    // Route through the interpreter's shared default-hook body so the
    // repr lands on the *current* `sys.stdout` (captured_stdout swaps a
    // StringIO in) and `builtins._` updates — test_sys
    // DisplayHookTest.test_original_displayhook.
    match crate::builtins::reentrant_interp() {
        Ok(interp) => {
            let g = interp.builtins_dict();
            interp.displayhook_default(value.clone(), &g)
        }
        Err(_) => {
            // No interpreter on the stack (embedder call): plain echo.
            if matches!(value, Object::None) {
                return Ok(Object::None);
            }
            println!("{}", value.repr());
            Ok(Object::None)
        }
    }
}

/// `sys.thread_info` field order (CPython `threadinfo_fields`).
const THREAD_INFO_FIELDS: &[&str] = &["name", "lock", "version"];

fn sys_thread_info() -> Object {
    // A real struct sequence (`len(sys.thread_info) == 3` —
    // test_sys test_thread_info). WeavePy's `_thread` runs on OS
    // threads: pthreads everywhere but Windows (`nt` there), guarded by
    // the CPython-style mutex+cond GIL dance.
    let name = if cfg!(windows) { "nt" } else { "pthread" };
    let ty = crate::stdlib::os::struct_seq_type("thread_info", "sys", THREAD_INFO_FIELDS);
    let values = vec![
        Object::from_static(name),
        Object::from_static("mutex+cond"),
        Object::None,
    ];
    crate::stdlib::os::struct_seq_instance(ty, THREAD_INFO_FIELDS, values)
}
