//! `PyModule_Create2`, `PyMethodDef`, and the bridge that turns a
//! C function pointer into a [`weavepy_vm::object::BuiltinFn`].

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use weavepy_vm::sync::Rc;
use weavepy_vm::sync::RefCell;

use weavepy_vm::error::{type_error, RuntimeError};
use weavepy_vm::object::{BuiltinFn, DictData, DictKey, MethodWrapper, Object, PyModule};

use crate::object::PyObject;

// Method calling-convention flags. Mirror the Python.h header.
pub const METH_VARARGS: c_int = 0x0001;
pub const METH_KEYWORDS: c_int = 0x0002;
pub const METH_NOARGS: c_int = 0x0004;
pub const METH_O: c_int = 0x0008;
pub const METH_CLASS: c_int = 0x0010;
pub const METH_STATIC: c_int = 0x0020;
pub const METH_COEXIST: c_int = 0x0040;
pub const METH_FASTCALL: c_int = 0x0080;
pub const METH_METHOD: c_int = 0x0200;

/// Layout matches `PyMethodDef` in the header.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PyMethodDef {
    pub ml_name: *const c_char,
    pub ml_meth: Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>,
    pub ml_flags: c_int,
    pub ml_doc: *const c_char,
}

unsafe impl Send for PyMethodDef {}
unsafe impl Sync for PyMethodDef {}

/// Layout matches `PyModuleDef` in the header.
#[repr(C)]
pub struct PyModuleDef {
    pub m_base: PyModuleDef_Base,
    pub m_name: *const c_char,
    pub m_doc: *const c_char,
    pub m_size: isize,
    pub m_methods: *mut PyMethodDef,
    pub m_slots: *mut PyModuleDef_Slot,
    pub m_traverse: *mut std::ffi::c_void,
    pub m_clear: *mut std::ffi::c_void,
    pub m_free: *mut std::ffi::c_void,
}

#[repr(C)]
pub struct PyModuleDef_Base {
    pub ob_base: PyObject,
    pub m_init: Option<unsafe extern "C" fn() -> *mut PyObject>,
    pub m_index: isize,
    pub m_copy: *mut PyObject,
}

#[repr(C)]
pub struct PyModuleDef_Slot {
    pub slot: c_int,
    pub value: *mut std::ffi::c_void,
}

/// Decoded `PyMethodDef` entry, used internally when building a
/// type's method dict from a `Py_tp_methods` slot.
#[derive(Clone)]
pub struct MethodEntry {
    pub name: String,
    pub doc: Option<String>,
    pub func: Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>,
    pub flags: c_int,
}

impl MethodEntry {
    /// Bind this method as a module-level function: CPython builds
    /// these with `m_self = module` (`_add_methods_to_object`), and
    /// stateful extensions read their per-module state straight off
    /// that self (zope.interface's `implementedBy` does
    /// `PyModule_GetState(self)->…` unchecked — handing it `None`
    /// is a segfault). The captured strong `Rc` makes a
    /// module → dict → builtin → module cycle, which is fine: modules
    /// are process-immortal (they sit in `sys.modules`).
    pub fn bind(&self, module: Object) -> Object {
        wrap_c_function(self.name.clone(), self.func, self.flags, Some(module))
    }

    /// Bind this method as an unbound class member: invocations
    /// through an instance will pass the instance as the first
    /// argument. The wrapper extracts the receiver from `args[0]`
    /// and routes it to the C function's `self` parameter, leaving
    /// only the trailing user-supplied args in the tuple.
    ///
    /// `defining_class` is the late-bound C type pointer for
    /// `METH_METHOD` (`PyCMethod`) entries — the heap type's pointer is
    /// stored into the shared cell once the type box exists (see
    /// `assemble_type_dict`'s callers).
    pub fn bind_unbound(
        &self,
        defining_class: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Object {
        wrap_c_method_function(self.name.clone(), self.func, self.flags, defining_class)
    }
}

/// Walk a null-terminated `PyMethodDef` array, decoding entries.
///
/// SAFETY: `defs` must point at a `PyMethodDef[]` whose final
/// entry has `ml_name == NULL`. The function returns owned
/// [`MethodEntry`]s that hold the C string by reference; callers
/// must keep the underlying memory alive (which is fine for
/// extension modules — the array lives in the extension's
/// `.rodata`).
pub unsafe fn collect_methods(mut defs: *mut PyMethodDef) -> Vec<MethodEntry> {
    let mut out = Vec::new();
    if defs.is_null() {
        return out;
    }
    loop {
        let entry = unsafe { *defs };
        if entry.ml_name.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr(entry.ml_name) }
            .to_string_lossy()
            .into_owned();
        let doc = if entry.ml_doc.is_null() {
            None
        } else {
            Some(
                unsafe { CStr::from_ptr(entry.ml_doc) }
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        out.push(MethodEntry {
            name,
            doc,
            func: entry.ml_meth,
            flags: entry.ml_flags,
        });
        defs = unsafe { defs.add(1) };
    }
    out
}

/// Invoke a `METH_FASTCALL` (optionally `| METH_KEYWORDS`) C function
/// (RFC 0046, wave 4). The vectorcall convention hands the callee a bare
/// `PyObject *const *` array plus an explicit `Py_ssize_t nargs` rather
/// than an args tuple — numpy's `add_docstring`, `arr_add_docstring`, and
/// the `_ArrayFunctionDispatcher` machinery are all fastcall — so the
/// stock `func(self, tuple)` path fed a fastcall callee a garbage `nargs`
/// (it read whatever was in the third register, typically 0, hence
/// "missing required positional argument 0").
///
/// Positional args become a contiguous owned array; the
/// `| METH_KEYWORDS` variant packs the keyword *values* immediately
/// after the positionals in that same array and rides their names in a
/// trailing `kwnames` tuple (the CPython vectorcall convention — see
/// `PyObject_Vectorcall`). `nargs` reports only the positional count, so
/// the callee's `npy_parse_arguments` reads `args[nargs + i]` for the
/// `i`-th `kwnames` entry. `numpy`'s `empty`/`zeros`/`array` are all
/// `METH_FASTCALL | METH_KEYWORDS`, so `np.empty(2, dtype=float32)`
/// reaches the C core through here.
unsafe fn call_fastcall(
    func: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    self_ptr: *mut PyObject,
    args: &[Object],
    kwargs: &[(String, Object)],
    flags: c_int,
) -> *mut PyObject {
    if std::env::var_os("WEAVEPY_TRACE_CALL").is_some() && !kwargs.is_empty() {
        let keys: Vec<&str> = kwargs.iter().map(|(k, _)| k.as_str()).collect();
        eprintln!("[TRACE_FASTCALL] nargs={} kwargs={:?}", args.len(), keys);
    }
    let mut argv: Vec<*mut PyObject> = Vec::with_capacity(args.len() + kwargs.len());
    {
        // RFC 0047 (wave 5): argument scalars mint through the canonical
        // pin cache — the callee may store a pointer borrowed (pandas'
        // khash keys); see `mirror::ScalarPinKey`.
        let _pin = crate::mirror::enter_arg_pin();
        for a in args {
            argv.push(crate::object::into_owned(a.clone()));
        }
        for (_, v) in kwargs {
            argv.push(crate::object::into_owned(v.clone()));
        }
    }
    let nargs = args.len() as crate::object::PySsizeT;
    let argv_ptr = argv.as_ptr();
    let kwnames: *mut PyObject = if kwargs.is_empty() {
        ptr::null_mut()
    } else {
        let names: Vec<Object> = kwargs
            .iter()
            .map(|(k, _)| Object::from_str(k.as_str()))
            .collect();
        // CPython interns kwname strings; extensions match them against
        // their own `PyUnicode_InternFromString` pointers by identity
        // (orjson's `option=`/`default=` dispatch).
        let _intern = crate::mirror::enter_intern_scope();
        crate::object::into_owned(Object::new_tuple(names))
    };
    let result = if (flags & METH_KEYWORDS) != 0 {
        #[allow(clippy::missing_transmute_annotations)]
        let fast_kw: unsafe extern "C" fn(
            *mut PyObject,
            *const *mut PyObject,
            crate::object::PySsizeT,
            *mut PyObject,
        ) -> *mut PyObject = unsafe { std::mem::transmute(func) };
        crate::interp::ensure_active(|| unsafe { fast_kw(self_ptr, argv_ptr, nargs, kwnames) })
    } else {
        #[allow(clippy::missing_transmute_annotations)]
        let fast: unsafe extern "C" fn(
            *mut PyObject,
            *const *mut PyObject,
            crate::object::PySsizeT,
        ) -> *mut PyObject = unsafe { std::mem::transmute(func) };
        crate::interp::ensure_active(|| unsafe { fast(self_ptr, argv_ptr, nargs) })
    };
    for &a in &argv {
        unsafe { crate::object::Py_DecRef(a) };
    }
    if !kwnames.is_null() {
        unsafe { crate::object::Py_DecRef(kwnames) };
    }
    result
}

/// Build the owned `kwds` pointer for the `METH_VARARGS | METH_KEYWORDS`
/// bridge (the legacy `func(self, args, kwds)` convention). Returns
/// **NULL** for a keyword-less call — CPython passes `tp_call` a NULL
/// `kwds` then, and an empty WeavePy dict mirror reads as garbage size
/// through `PyDict_GET_SIZE`. Caller owns the result (NULL-safe to
/// `Py_DecRef`).
fn build_kwargs_dict(kwargs: &[(String, Object)]) -> *mut PyObject {
    if kwargs.is_empty() {
        return ptr::null_mut();
    }
    let mut d = DictData::default();
    for (k, v) in kwargs {
        d.insert(DictKey(Object::from_str(k.as_str())), v.clone());
    }
    crate::object::into_owned(Object::Dict(Rc::new(RefCell::new(d))))
}

/// Wrap a C function pointer in a [`BuiltinFn`] backed by a Rust
/// closure that performs the Python → C bridge:
///
/// 1. Build a fresh `PyObject *` tuple from the args
/// 2. Stash the args' refcount = 1
/// 3. Invoke the C function (with `self` if present)
/// 4. Read the return value (or null on error)
/// 5. Decref the tuple
/// 6. Translate any pending exception into a [`RuntimeError`]
fn wrap_c_function(
    name: String,
    func: Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>,
    flags: c_int,
    self_obj: Option<Object>,
) -> Object {
    let static_name: &'static str = Box::leak(name.into_boxed_str());
    let self_call = self_obj.clone();
    let call = move |args: &[Object]| -> Result<Object, RuntimeError> {
        bridge_invoke(func, static_name, self_call.as_ref(), flags, args, &[])
    };
    // Only `METH_KEYWORDS` functions get a kwargs-aware entry point; for
    // everyone else the VM keeps rejecting keyword arguments.
    let call_kw: Option<
        Box<dyn Fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError> + Send + Sync>,
    > = if (flags & METH_KEYWORDS) != 0 {
        let self_kw = self_obj;
        Some(Box::new(
            move |args: &[Object], kwargs: &[(String, Object)]| {
                bridge_invoke(func, static_name, self_kw.as_ref(), flags, args, kwargs)
            },
        ))
    } else {
        None
    };
    Object::Builtin(Rc::new(BuiltinFn {
        name: static_name,
        binds_instance: false,
        call: Box::new(call),
        call_kw,
    }))
}

/// Shared module-function bridge body used by both the positional-only
/// (`call`) and kwargs-aware (`call_kw`) entry points produced by
/// [`wrap_c_function`]. `kwargs` is always empty on the `call` path.
fn bridge_invoke(
    func: Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>,
    static_name: &'static str,
    self_obj: Option<&Object>,
    flags: c_int,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    let Some(func) = func else {
        return Err(type_error(format!("'{static_name}' is null")));
    };
    if std::env::var_os("WEAVEPY_TRACE_CALL").is_some() {
        let keys: Vec<&str> = kwargs.iter().map(|(k, _)| k.as_str()).collect();
        eprintln!(
            "[TRACE_BRIDGE] {static_name}() flags={flags:#x} nargs={} kwargs={:?}",
            args.len(),
            keys
        );
    }
    crate::interp::ensure_initialised();
    crate::errors::clear_thread_local();

    let self_ptr = match self_obj {
        Some(o) => crate::object::into_owned(o.clone()),
        None => crate::singletons::none_ptr(),
    };
    let drop_self = |self_ptr: *mut PyObject| {
        if !std::ptr::eq(self_ptr, crate::singletons::none_ptr()) {
            unsafe { crate::object::Py_DecRef(self_ptr) };
        }
    };

    // Build the args object based on the calling convention.
    let result = if (flags & METH_NOARGS) != 0 {
        if !args.is_empty() {
            drop_self(self_ptr);
            return Err(type_error(format!(
                "{static_name}() takes no arguments ({} given)",
                args.len()
            )));
        }
        crate::interp::ensure_active(|| unsafe { func(self_ptr, crate::singletons::none_ptr()) })
    } else if (flags & METH_O) != 0 {
        if args.len() != 1 {
            drop_self(self_ptr);
            return Err(type_error(format!(
                "{static_name}() takes exactly one argument ({} given)",
                args.len()
            )));
        }
        let arg = {
            let _pin = crate::mirror::enter_arg_pin();
            crate::object::into_owned(args[0].clone())
        };
        let r = crate::interp::ensure_active(|| unsafe { func(self_ptr, arg) });
        unsafe { crate::object::Py_DecRef(arg) };
        r
    } else if (flags & METH_FASTCALL) != 0 {
        unsafe { call_fastcall(func, self_ptr, args, kwargs, flags) }
    } else {
        let tuple = crate::mirror::args_tuple_out(Object::new_tuple(args.to_vec()));
        let r = if (flags & METH_KEYWORDS) != 0 {
            #[allow(clippy::missing_transmute_annotations)]
            let with_kw: unsafe extern "C" fn(
                *mut PyObject,
                *mut PyObject,
                *mut PyObject,
            ) -> *mut PyObject = unsafe { std::mem::transmute(func) };
            let kw = build_kwargs_dict(kwargs);
            let r = crate::interp::ensure_active(|| unsafe { with_kw(self_ptr, tuple, kw) });
            unsafe { crate::object::Py_DecRef(kw) };
            r
        } else {
            crate::interp::ensure_active(|| unsafe { func(self_ptr, tuple) })
        };
        unsafe { crate::object::Py_DecRef(tuple) };
        r
    };

    drop_self(self_ptr);

    // Translate the result.
    if result.is_null() {
        // The C function indicated failure. Pull the pending
        // error and convert.
        if let Some(p) = crate::errors::take_pending() {
            return Err(crate::errors::to_runtime_error(p));
        }
        return Err(type_error(format!(
            "{static_name}() returned NULL without setting an exception"
        )));
    }
    let out = unsafe { crate::object::clone_object(result) };
    unsafe { crate::object::Py_DecRef(result) };
    Ok(out)
}

/// Wrap a `tp_methods` entry as a class-bound method. The first
/// element of `args` is the receiver, which is routed to the C
/// function's `self` parameter; everything after is forwarded as
/// the args tuple (or as the lone METH_O argument).
///
/// The wrapper's `BuiltinFn.name` is prefixed with `_capi:` so the
/// VM's name-keyed builtin routing (which intercepts canonical
/// names such as `sum`, `iter`, `min`, …) doesn't fire on a
/// user-defined extension method that happens to share a name with
/// a Python built-in.
fn wrap_c_method_function(
    name: String,
    func: Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>,
    flags: c_int,
    defining_class: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> Object {
    let qualified = format!("_capi:{name}");
    let static_name: &'static str = Box::leak(qualified.into_boxed_str());
    let display_name: &'static str = Box::leak(name.into_boxed_str());
    let cell = defining_class.clone();
    let call = move |args: &[Object]| -> Result<Object, RuntimeError> {
        bridge_invoke_method(func, display_name, flags, args, &[], &cell)
    };
    let call_kw: Option<
        Box<dyn Fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError> + Send + Sync>,
    > = if (flags & METH_KEYWORDS) != 0 {
        let cell_kw = defining_class;
        Some(Box::new(
            move |args: &[Object], kwargs: &[(String, Object)]| {
                bridge_invoke_method(func, display_name, flags, args, kwargs, &cell_kw)
            },
        ))
    } else {
        None
    };
    Object::Builtin(Rc::new(BuiltinFn {
        name: static_name,
        // C-type method defs are method descriptors: they bind.
        binds_instance: true,
        call: Box::new(call),
        call_kw,
    }))
}

/// Shared `tp_methods` bridge body; the receiver is `args[0]` (routed to
/// the C function's `self`) and everything after is forwarded
/// positionally (with keyword args carried per the calling convention).
fn bridge_invoke_method(
    func: Option<unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject>,
    display_name: &'static str,
    flags: c_int,
    args: &[Object],
    kwargs: &[(String, Object)],
    defining_class: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> Result<Object, RuntimeError> {
    let Some(func) = func else {
        return Err(type_error(format!("'{display_name}' is null")));
    };
    crate::interp::ensure_initialised();
    crate::errors::clear_thread_local();

    if args.is_empty() {
        return Err(type_error(format!(
            "{display_name}() takes at least 1 argument (self) (0 given)"
        )));
    }
    let self_ptr = crate::object::into_owned(args[0].clone());
    let rest = &args[1..];

    let result = if (flags & METH_NOARGS) != 0 {
        if !rest.is_empty() {
            unsafe { crate::object::Py_DecRef(self_ptr) };
            return Err(type_error(format!(
                "{display_name}() takes no arguments ({} given)",
                rest.len()
            )));
        }
        crate::interp::ensure_active(|| unsafe { func(self_ptr, crate::singletons::none_ptr()) })
    } else if (flags & METH_O) != 0 {
        if rest.len() != 1 {
            unsafe { crate::object::Py_DecRef(self_ptr) };
            return Err(type_error(format!(
                "{display_name}() takes exactly one argument ({} given)",
                rest.len()
            )));
        }
        let arg = {
            let _pin = crate::mirror::enter_arg_pin();
            crate::object::into_owned(rest[0].clone())
        };
        let r = crate::interp::ensure_active(|| unsafe { func(self_ptr, arg) });
        unsafe { crate::object::Py_DecRef(arg) };
        r
    } else if (flags & METH_METHOD) != 0 {
        // `PyCMethod`: `(self, defining_class, args, nargs, kwnames)`.
        // The class pointer was stamped into the shared cell when the
        // heap type box was built.
        let cls = defining_class.load(std::sync::atomic::Ordering::Relaxed);
        if cls == 0 {
            unsafe { crate::object::Py_DecRef(self_ptr) };
            return Err(type_error(format!(
                "{display_name}(): defining class is not available"
            )));
        }
        unsafe {
            crate::modsupport_ext::call_meth_method(
                func,
                self_ptr,
                cls as *mut crate::types::PyTypeObject,
                rest,
                kwargs,
            )
        }
    } else if (flags & METH_FASTCALL) != 0 {
        unsafe { call_fastcall(func, self_ptr, rest, kwargs, flags) }
    } else {
        let tuple = crate::mirror::args_tuple_out(Object::new_tuple(rest.to_vec()));
        let r = if (flags & METH_KEYWORDS) != 0 {
            #[allow(clippy::missing_transmute_annotations)]
            let with_kw: unsafe extern "C" fn(
                *mut PyObject,
                *mut PyObject,
                *mut PyObject,
            ) -> *mut PyObject = unsafe { std::mem::transmute(func) };
            let kw = build_kwargs_dict(kwargs);
            let r = crate::interp::ensure_active(|| unsafe { with_kw(self_ptr, tuple, kw) });
            unsafe { crate::object::Py_DecRef(kw) };
            r
        } else {
            crate::interp::ensure_active(|| unsafe { func(self_ptr, tuple) })
        };
        unsafe { crate::object::Py_DecRef(tuple) };
        r
    };

    unsafe { crate::object::Py_DecRef(self_ptr) };

    if result.is_null() {
        if let Some(p) = crate::errors::take_pending() {
            return Err(crate::errors::to_runtime_error(p));
        }
        return Err(type_error(format!(
            "{display_name}() returned NULL without setting an exception"
        )));
    }
    let out = unsafe { crate::object::clone_object(result) };
    unsafe { crate::object::Py_DecRef(result) };
    Ok(out)
}

/// `PyModule_Create2(def, api)` — extension entry point. Returns a
/// fresh module object whose dict is preloaded with the entries
/// in `def->m_methods`.
#[no_mangle]
pub unsafe extern "C" fn PyModule_Create2(def: *mut PyModuleDef, _api: c_int) -> *mut PyObject {
    crate::interp::ensure_initialised();
    if def.is_null() {
        crate::errors::set_runtime_error("PyModule_Create2 with null def");
        return ptr::null_mut();
    }
    let def_ref = unsafe { &*def };
    let name = if def_ref.m_name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(def_ref.m_name) }
            .to_string_lossy()
            .into_owned()
    };
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_str(name.clone()),
        );
        if !def_ref.m_doc.is_null() {
            let doc = unsafe { CStr::from_ptr(def_ref.m_doc) }
                .to_string_lossy()
                .into_owned();
            d.insert(
                DictKey(Object::from_static("__doc__")),
                Object::from_str(doc),
            );
        }
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static(""),
        );
        d.insert(DictKey(Object::from_static("__loader__")), Object::None);
        d.insert(DictKey(Object::from_static("__spec__")), Object::None);
    }
    let module = Rc::new(PyModule {
        name,
        filename: None,
        dict,
    });
    // Methods bind *after* the module exists so each carries the module
    // as its `m_self` (CPython's `_add_methods_to_object` contract).
    if !def_ref.m_methods.is_null() {
        let entries = unsafe { collect_methods(def_ref.m_methods) };
        let mut d = module.dict.borrow_mut();
        for e in entries {
            let bound = e.bind(Object::Module(module.clone()));
            d.insert(DictKey(Object::from_str(e.name.clone())), bound);
        }
    }
    // PEP 3121: a single-phase extension that declares `m_size > 0` (pandas'
    // vendored ujson) allocates per-module state here and writes into it via
    // `PyModule_GetState`. Key the block by the module's native `Rc` identity
    // (stable across the fresh per-crossing C boxes) before the `Rc` is moved
    // into the crossing.
    if def_ref.m_size > 0 {
        let key = Rc::as_ptr(&module) as usize;
        crate::wave5_pandas::ensure_module_state(key, def_ref.m_size as usize);
    }
    // Remember the def for `PyModule_GetDef` (PEP 3121 / PEP 489;
    // `_testsinglephase` asserts def identity across re-imports).
    crate::modsupport_ext::register_module_def(
        Rc::as_ptr(&module) as usize,
        def as *mut core::ffi::c_void,
    );
    // CPython adds a single-phase module with per-interpreter state
    // (`m_size >= 0`) to `interp->modules_by_index`, so the extension can later
    // re-fetch it via `PyState_FindModule(def)`. pandas' vendored ujson relies
    // on this to reach its cached `Series`/`DataFrame`/`Index` types.
    // Multi-phase defs (m_slots set) are never put in modules_by_index —
    // `PyState_FindModule` must answer NULL for them (test_try_registration).
    if def_ref.m_size >= 0 && def_ref.m_slots.is_null() {
        crate::wave5_pandas::register_find_module(
            def as *mut core::ffi::c_void,
            Object::Module(module.clone()),
        );
    }
    crate::object::into_owned(Object::Module(module))
}

/// PEP 489 module slot ids (mirror the header).
pub const PY_MOD_CREATE: c_int = 1;
pub const PY_MOD_EXEC: c_int = 2;
pub const PY_MOD_MULTIPLE_INTERPRETERS: c_int = 3;
/// `_Py_mod_LAST_SLOT` — 3.13 defines create/exec/multiple_interpreters/gil.
pub const PY_MOD_LAST_SLOT: c_int = 4;

/// `PyModuleDef_Init(def)` — entry point for a multi-phase (PEP 489)
/// extension. Unlike single-phase `PyModule_Create2`, the def is *not*
/// turned into a module here: it is tagged as a module-def object and
/// returned, so the loader (mirroring CPython's import machinery) can
/// run the `Py_mod_create`/`Py_mod_exec` slots itself.
#[no_mangle]
pub unsafe extern "C" fn PyModuleDef_Init(def: *mut PyModuleDef) -> *mut PyObject {
    crate::interp::ensure_initialised();
    if def.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        let base = &mut (*def).m_base;
        base.ob_base.ob_refcnt = crate::object::IMMORTAL_REFCNT;
        base.ob_base.ob_type = crate::types::PyModuleDef_Type.as_ptr();
    }
    def as *mut PyObject
}

/// Does `raw` point at a `PyModuleDef` tagged by [`PyModuleDef_Init`]
/// (i.e. a multi-phase extension's return value)?
pub unsafe fn is_module_def(raw: *mut PyObject) -> bool {
    if raw.is_null() {
        return false;
    }
    unsafe { (*raw).ob_type == crate::types::PyModuleDef_Type.as_ptr() }
}

/// Walk a null-terminated `PyModuleDef_Slot[]` array.
unsafe fn slots_of(def: *mut PyModuleDef) -> Vec<PyModuleDef_Slot> {
    let mut out = Vec::new();
    let mut p = unsafe { (*def).m_slots };
    if p.is_null() {
        return out;
    }
    loop {
        let s = unsafe { &*p };
        if s.slot == 0 {
            break;
        }
        out.push(PyModuleDef_Slot {
            slot: s.slot,
            value: s.value,
        });
        p = unsafe { p.add(1) };
    }
    out
}

/// Run a multi-phase extension's full init: create the module (default
/// or via a `Py_mod_create` slot) and execute every `Py_mod_exec` slot.
/// Returns the populated module, or an error string on failure.
///
/// SAFETY: `def` must be a live `PyModuleDef` tagged by
/// `PyModuleDef_Init`. Must run inside an active extension context.
pub unsafe fn run_multiphase_init(
    def: *mut PyModuleDef,
    full_name: &str,
) -> Result<*mut PyObject, String> {
    let slots = unsafe { slots_of(def) };
    let trace = std::env::var_os("WEAVEPY_TRACE_MPI").is_some();
    if trace {
        eprintln!(
            "[mpi] enter run_multiphase_init name={full_name} nslots={}",
            slots.len()
        );
    }

    // Slot-table validation, mirroring CPython's `PyModule_FromDefAndSpec2`
    // (extension.test_loader's bad_slot_large / bad_slot_negative /
    // multiple_create_slots variants assert SystemError with this wording).
    let set_system_error = |msg: String| {
        crate::errors::set_pending(
            Some(
                weavepy_vm::builtin_types::builtin_types()
                    .system_error
                    .clone(),
            ),
            Object::from_str(msg.clone()),
        );
        msg
    };
    if unsafe { (*def).m_size } < 0 {
        return Err(set_system_error(format!(
            "module {full_name}: m_size may not be negative for multi-phase initialization"
        )));
    }
    for slot in &slots {
        if slot.slot < 1 || slot.slot > PY_MOD_LAST_SLOT {
            return Err(set_system_error(format!(
                "module {full_name} uses unknown slot ID {}",
                slot.slot
            )));
        }
    }
    if slots.iter().filter(|s| s.slot == PY_MOD_CREATE).count() > 1 {
        return Err(set_system_error(format!(
            "module {full_name} has multiple create slots"
        )));
    }
    if slots
        .iter()
        .filter(|s| s.slot == PY_MOD_MULTIPLE_INTERPRETERS)
        .count()
        > 1
    {
        return Err(set_system_error(format!(
            "module {full_name} has more than one 'multiple interpreters' slots"
        )));
    }

    // PEP 684 gate (CPython `module_from_def`'s multiple-interpreters
    // check): in a sub-interpreter with the extension check in effect,
    // `NOT_SUPPORTED` never loads and the default `SUPPORTED`
    // (shared-GIL-only) refuses when the interpreter owns its GIL
    // (test_util's IncompatibleExtensionModuleRestrictionsTests).
    let multi_support = slots
        .iter()
        .find(|s| s.slot == PY_MOD_MULTIPLE_INTERPRETERS)
        .map_or(1usize, |s| s.value as usize);
    let gate = weavepy_vm::vm_singletons::current_interpreter_ptr()
        // SAFETY: published by the enclosing extension-call context; the
        // GIL keeps access exclusive.
        .and_then(|p| unsafe { (*p).subinterp_extension_gate() });
    if let Some(own_gil) = gate {
        let incompatible = match multi_support {
            0 => true,    // Py_MOD_MULTIPLE_INTERPRETERS_NOT_SUPPORTED
            1 => own_gil, // Py_MOD_MULTIPLE_INTERPRETERS_SUPPORTED
            _ => false,   // Py_MOD_PER_INTERPRETER_GIL_SUPPORTED
        };
        if incompatible {
            let msg = format!("module {full_name} does not support loading in subinterpreters");
            crate::errors::set_pending(
                Some(
                    weavepy_vm::builtin_types::builtin_types()
                        .import_error
                        .clone(),
                ),
                Object::from_str(msg.clone()),
            );
            return Err(msg);
        }
    }

    // Phase 1: create the module object.
    let create_slot = slots.iter().find(|s| s.slot == PY_MOD_CREATE);
    let module: *mut PyObject = if let Some(slot) = create_slot {
        if trace {
            eprintln!("[mpi] {full_name}: create slot -> calling");
        }
        let create: unsafe extern "C" fn(*mut PyObject, *mut PyModuleDef) -> *mut PyObject =
            unsafe { std::mem::transmute(slot.value) };
        let spec = unsafe { build_module_spec(full_name) };
        if trace {
            eprintln!(
                "[mpi] {full_name}: build_module_spec -> {}",
                if spec.is_null() { "NULL" } else { "ok" }
            );
        }
        let m = unsafe { create(spec, def) };
        if !spec.is_null() {
            unsafe { crate::object::Py_DecRef(spec) };
        }
        if trace {
            eprintln!(
                "[mpi] {full_name}: create slot -> {}",
                if m.is_null() { "NULL" } else { "ok" }
            );
        }
        m
    } else {
        // Default creation: a fresh module preloaded with m_methods.
        unsafe { PyModule_Create2(def, 1013) }
    };
    if module.is_null() {
        // Leave a pending exception exactly as the slot raised it —
        // extension.test_loader's create_raise asserts the original
        // type; only synthesize when the slot forgot to raise.
        if !crate::errors::has_pending() {
            set_system_error(format!(
                "creation of module {full_name} failed without setting an exception"
            ));
        }
        return Err(format!("creation of module {full_name} failed"));
    }
    if crate::errors::has_pending() {
        // create_unreported_exception: succeeded but left an exception
        // behind — CPython converts this to SystemError chained via
        // `__cause__` (`_PyErr_FormatFromCause`).
        unsafe { crate::object::Py_DecRef(module) };
        let msg = format!("creation of module {full_name} raised unreported exception");
        crate::errors::set_pending_system_error_from_cause(msg.clone());
        return Err(msg);
    }

    // A `Py_mod_create` slot may legitimately return a non-module object
    // (extension.test_loader's `nonmodule` variants) — but then the def
    // must not carry exec slots (CPython's `PyModule_FromDefAndSpec2`).
    let is_module = matches!(
        unsafe { crate::object::clone_object(module) },
        Object::Module(_)
    );
    if !is_module {
        if slots.iter().any(|s| s.slot == PY_MOD_EXEC) {
            unsafe { crate::object::Py_DecRef(module) };
            return Err(set_system_error(format!(
                "module {full_name} specifies execution slots, but did not create a module"
            )));
        }
        // CPython's `PyModule_FromDefAndSpec2` still attaches
        // `def->m_methods` to the returned object, whatever it is
        // (`_add_methods_to_object`; issue 27782's
        // `nonmodule_with_methods` fixture calls `ns.bar(10, 1)`).
        let m_methods = unsafe { (*def).m_methods };
        if !m_methods.is_null() {
            let target = unsafe { crate::object::clone_object(module) };
            for e in unsafe { collect_methods(m_methods) } {
                let name = e.name.clone();
                let bound = e.bind(target.clone());
                let value = crate::object::into_owned(bound);
                let cname = std::ffi::CString::new(name).unwrap_or_default();
                unsafe {
                    crate::abstract_::PyObject_SetAttrString(module, cname.as_ptr(), value);
                    crate::object::Py_DecRef(value);
                }
            }
        }
        return Ok(module);
    }

    // PEP 3121/489 bookkeeping for the real-module case: allocate the
    // `m_size` state block and remember the def for `PyModule_GetDef` /
    // `PyState_FindModule` round-trips (a custom create slot bypasses
    // `PyModule_Create2`, which normally does this).
    unsafe {
        if let Object::Module(m) = crate::object::clone_object(module) {
            let key = Rc::as_ptr(&m) as usize;
            let size = (*def).m_size;
            if size > 0 {
                crate::wave5_pandas::ensure_module_state(key, size as usize);
            }
            crate::modsupport_ext::register_module_def(key, def as *mut core::ffi::c_void);
        }
    }

    // Make the module discoverable under its full dotted name while the
    // exec slots run (numpy reads `__name__` and re-imports siblings).
    // CPython's import machinery also sets `__package__` (the parent
    // package) before running a module's body; an extension's relative
    // imports (`from ._pcg64 cimport …` in numpy.random._generator) resolve
    // against it, so derive it from the dotted name here. A leaf module's
    // package is its name minus the last component; a top-level module's is
    // empty.
    unsafe {
        if let Object::Module(m) = crate::object::clone_object(module) {
            let mut d = m.dict.borrow_mut();
            d.insert(
                DictKey(Object::from_static("__name__")),
                Object::from_str(full_name.to_owned()),
            );
            let package = match full_name.rsplit_once('.') {
                Some((head, _)) => head.to_owned(),
                None => String::new(),
            };
            d.insert(
                DictKey(Object::from_static("__package__")),
                Object::from_str(package),
            );
        }
    }

    // Phase 2: run every Py_mod_exec slot in order.
    for (i, slot) in slots.iter().filter(|s| s.slot == PY_MOD_EXEC).enumerate() {
        if trace {
            eprintln!("[mpi] {full_name}: exec slot {i} -> calling");
        }
        let exec: unsafe extern "C" fn(*mut PyObject) -> c_int =
            unsafe { std::mem::transmute(slot.value) };
        let rc = unsafe { exec(module) };
        if trace {
            eprintln!("[mpi] {full_name}: exec slot {i} -> rc={rc}");
        }
        if rc != 0 {
            // Describe the failure for the Err string (peek, don't
            // consume: the loader propagates the pending exception
            // verbatim — exec_raise asserts the original type).
            let description = match crate::errors::take_pending() {
                Some(p) => {
                    let ty =
                        p.ty.as_ref()
                            .map(|t| t.name.clone())
                            .unwrap_or_else(|| "?".to_owned());
                    // pybind11's module-init catch-all masks the real
                    // failure behind ImportError("initialization failed")
                    // chained via `raise_from` — surface the cause chain,
                    // it's the only diagnostic the extension left behind.
                    let s = format!("{ty}: {}{}", p.value.to_str(), cause_chain_suffix(&p.value));
                    crate::errors::set_pending(p.ty, p.value);
                    s
                }
                // exec_err: rc != 0 with nothing raised → SystemError.
                None => set_system_error(format!(
                    "execution of module {full_name} failed without setting an exception"
                )),
            };
            if trace {
                eprintln!("[mpi] {full_name}: exec slot {i} FAILED -> {description}");
            }
            unsafe { crate::object::Py_DecRef(module) };
            return Err(description);
        }
        if crate::errors::has_pending() {
            // exec_unreported_exception: rc == 0 with an exception left
            // behind — CPython converts this to SystemError chained via
            // `__cause__` (`_PyErr_FormatFromCause`).
            unsafe { crate::object::Py_DecRef(module) };
            let msg = format!("execution of module {full_name} raised unreported exception");
            crate::errors::set_pending_system_error_from_cause(msg.clone());
            return Err(msg);
        }
    }
    Ok(module)
}

/// Render an exception's `__cause__`/`__context__` chain as a `"; caused
/// by …"` suffix for multi-phase-init error strings (depth-capped).
fn cause_chain_suffix(value: &Object) -> String {
    let mut out = String::new();
    let mut cur = value.clone();
    for _ in 0..8 {
        let next = match &cur {
            Object::Instance(inst) => inst
                .slot_get("__cause__")
                .filter(|c| !matches!(c, Object::None))
                .or_else(|| {
                    inst.slot_get("__context__")
                        .filter(|c| !matches!(c, Object::None))
                }),
            _ => None,
        };
        match next {
            Some(c) => {
                out.push_str(&format!("; caused by {}: {}", c.type_name(), c.to_str()));
                cur = c;
            }
            None => break,
        }
    }
    out
}

/// `PyModule_FromDefAndSpec2(def, spec, api_version)` — PEP 489 phase 1
/// only: create the module object for `def` (via its `Py_mod_create`
/// slot when present, default creation otherwise) *without* running
/// exec slots; the loader calls [`PyModule_ExecDef`] afterwards. cffi's
/// embedding glue (cryptography's `_openssl`) drives multi-phase init
/// through this pair rather than returning the tagged def.
///
/// # Safety
/// `def` must be a live `PyModuleDef`; `spec` a module spec with a
/// readable `name` attribute.
#[no_mangle]
pub unsafe extern "C" fn PyModule_FromDefAndSpec2(
    def: *mut PyModuleDef,
    spec: *mut PyObject,
    _api_version: c_int,
) -> *mut PyObject {
    if def.is_null() {
        crate::errors::set_runtime_error("PyModule_FromDefAndSpec2 with null def");
        return ptr::null_mut();
    }
    // The dotted module name comes from `spec.name`.
    let full_name = if spec.is_null() {
        String::new()
    } else {
        let name_obj = unsafe {
            crate::abstract_::PyObject_GetAttrString(spec, b"name\0".as_ptr() as *const c_char)
        };
        if name_obj.is_null() {
            crate::errors::clear_thread_local();
            String::new()
        } else {
            let s = match unsafe { crate::object::clone_object(name_obj) } {
                Object::Str(s) => s.to_string(),
                other => other.to_str(),
            };
            unsafe { crate::object::Py_DecRef(name_obj) };
            s
        }
    };
    let slots = unsafe { slots_of(def) };
    let module: *mut PyObject = if let Some(slot) = slots.iter().find(|s| s.slot == PY_MOD_CREATE) {
        let create: unsafe extern "C" fn(*mut PyObject, *mut PyModuleDef) -> *mut PyObject =
            unsafe { std::mem::transmute(slot.value) };
        unsafe { create(spec, def) }
    } else {
        unsafe { PyModule_Create2(def, 1013) }
    };
    if module.is_null() {
        return ptr::null_mut();
    }
    if !full_name.is_empty() {
        if let Object::Module(m) = unsafe { crate::object::clone_object(module) } {
            let mut d = m.dict.borrow_mut();
            d.insert(
                DictKey(Object::from_static("__name__")),
                Object::from_str(full_name.clone()),
            );
            let package = match full_name.rsplit_once('.') {
                Some((head, _)) => head.to_owned(),
                None => String::new(),
            };
            d.insert(
                DictKey(Object::from_static("__package__")),
                Object::from_str(package),
            );
        }
    }
    module
}

/// `PyModule_ExecDef(module, def)` — PEP 489 phase 2: run every
/// `Py_mod_exec` slot of `def` against `module`.
///
/// # Safety
/// Both pointers must be live; `def`'s slot array must be well-formed.
#[no_mangle]
pub unsafe extern "C" fn PyModule_ExecDef(module: *mut PyObject, def: *mut PyModuleDef) -> c_int {
    if module.is_null() || def.is_null() {
        crate::errors::set_runtime_error("PyModule_ExecDef with null argument");
        return -1;
    }
    for slot in unsafe { slots_of(def) }
        .iter()
        .filter(|s| s.slot == PY_MOD_EXEC)
    {
        let exec: unsafe extern "C" fn(*mut PyObject) -> c_int =
            unsafe { std::mem::transmute(slot.value) };
        if unsafe { exec(module) } != 0 {
            return -1;
        }
    }
    0
}

/// `PyModule_GetNameObject(m)` — a **new** str reference to the module's
/// `__name__`.
///
/// # Safety
/// `m` must be null or a live module pointer.
#[no_mangle]
pub unsafe extern "C" fn PyModule_GetNameObject(m: *mut PyObject) -> *mut PyObject {
    if m.is_null() {
        crate::errors::set_type_error("PyModule_GetNameObject: not a module");
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(m) } {
        Object::Module(module) => {
            let name = module
                .dict
                .borrow()
                .get(&weavepy_vm::object::StrKey("__name__"))
                .cloned()
                .unwrap_or_else(|| Object::from_str(module.name.clone()));
            crate::object::into_owned(name)
        }
        _ => {
            crate::errors::set_type_error("PyModule_GetNameObject: not a module");
            ptr::null_mut()
        }
    }
}

/// `PyModule_Add(m, name, value)` — 3.13 spelling of `AddObject` that
/// steals `value` even on failure.
///
/// # Safety
/// Same contract as [`PyModule_AddObject`].
#[no_mangle]
pub unsafe extern "C" fn PyModule_Add(
    m: *mut PyObject,
    name: *const c_char,
    value: *mut PyObject,
) -> c_int {
    let rc = unsafe { PyModule_AddObjectRef(m, name, value) };
    if !value.is_null() {
        unsafe { crate::object::Py_DecRef(value) };
    }
    rc
}

/// Build a minimal `importlib.machinery.ModuleSpec(name, None)` for a
/// `Py_mod_create` slot. Returns NULL (and clears the error) if the
/// spec can't be constructed; numpy never uses a create slot.
unsafe fn build_module_spec(full_name: &str) -> *mut PyObject {
    let machinery =
        unsafe { PyImport_ImportModule(b"importlib.machinery\0".as_ptr() as *const c_char) };
    if machinery.is_null() {
        crate::errors::clear_thread_local();
        return ptr::null_mut();
    }
    let cls = unsafe {
        crate::abstract_::PyObject_GetAttrString(
            machinery,
            b"ModuleSpec\0".as_ptr() as *const c_char,
        )
    };
    unsafe { crate::object::Py_DecRef(machinery) };
    if cls.is_null() {
        crate::errors::clear_thread_local();
        return ptr::null_mut();
    }
    let name_obj = crate::object::into_owned(Object::from_str(full_name.to_owned()));
    let args = unsafe { crate::containers::PyTuple_New(2) };
    unsafe {
        crate::containers::PyTuple_SetItem(args, 0, name_obj);
        crate::object::Py_IncRef(crate::singletons::none_ptr());
        crate::containers::PyTuple_SetItem(args, 1, crate::singletons::none_ptr());
    }
    let spec = unsafe { crate::abstract_::PyObject_CallObject(cls, args) };
    unsafe {
        crate::object::Py_DecRef(cls);
        crate::object::Py_DecRef(args);
    }
    if spec.is_null() {
        crate::errors::clear_thread_local();
    }
    spec
}

/// Add `(name, value)` to `m`'s dict, taking ownership of `value`.
#[no_mangle]
pub unsafe extern "C" fn PyModule_AddObject(
    m: *mut PyObject,
    name: *const c_char,
    value: *mut PyObject,
) -> c_int {
    if m.is_null() || name.is_null() || value.is_null() {
        return -1;
    }
    let module = match unsafe { crate::object::clone_object(m) } {
        Object::Module(m) => m,
        _ => {
            crate::errors::set_type_error("PyModule_AddObject: not a module");
            return -1;
        }
    };
    let key = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    let v = unsafe { crate::object::clone_object(value) };
    let key_obj = Object::from_str(key);
    module
        .dict
        .borrow_mut()
        .insert(DictKey(key_obj.clone()), v.clone());
    // RFC 0069 WS5 follow-up: on CPython the stolen reference lives on in
    // the module dict, so the extension's *borrowed* pointer stays valid —
    // numpy's `PyInit__simd` does `PyModule_AddObject(m, "targets", d)` and
    // keeps filling `d` through the borrowed pointer afterwards. Our module
    // dict stores a VM clone, so without a retain the steal-decref below
    // frees the box and the extension writes into freed memory (the
    // heap-layout-dependent `_simd` import SIGSEGV). Retain the value box
    // for the module's lifetime, exactly like `PyDict_SetItemString` does
    // for plain dicts; the retain is released when the slot is overwritten
    // or the module box is freed (`invalidate_borrowed_cache`).
    crate::containers::dict_retain_value(m, crate::containers::dict_key_id(&key_obj), value, v);
    unsafe { crate::object::Py_DecRef(value) };
    0
}

/// Same as `PyModule_AddObject` but increments the reference count
/// of `value` rather than stealing it.
#[no_mangle]
pub unsafe extern "C" fn PyModule_AddObjectRef(
    m: *mut PyObject,
    name: *const c_char,
    value: *mut PyObject,
) -> c_int {
    if value.is_null() {
        return -1;
    }
    unsafe {
        crate::object::Py_IncRef(value);
        PyModule_AddObject(m, name, value)
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyModule_AddStringConstant(
    m: *mut PyObject,
    name: *const c_char,
    value: *const c_char,
) -> c_int {
    let v = if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    };
    let p = crate::object::into_owned(Object::from_str(v));
    unsafe { PyModule_AddObject(m, name, p) }
}

#[no_mangle]
pub unsafe extern "C" fn PyModule_AddIntConstant(
    m: *mut PyObject,
    name: *const c_char,
    value: i64,
) -> c_int {
    let p = crate::object::into_owned(Object::Int(value));
    unsafe { PyModule_AddObject(m, name, p) }
}

#[no_mangle]
pub unsafe extern "C" fn PyModule_AddType(
    m: *mut PyObject,
    ty: *mut crate::types::PyTypeObject,
) -> c_int {
    if ty.is_null() {
        return -1;
    }
    // CPython's `PyModule_AddType` readies the type first
    // (`Modules/../Objects/moduleobject.c` → `PyType_Ready`). A classic
    // static type registered *only* through `PyModule_AddType` (e.g.
    // `_testbuffer.staticarray`) relies on this — without it the type
    // crosses unbridged and proxies as a non-callable foreign 'object'.
    if unsafe { crate::types::PyType_Ready(ty) } < 0 {
        return -1;
    }
    // The type pointer is itself the PyObject we want to install
    // (PyTypeObject extends PyObject).
    let name_ptr = unsafe { (*ty).tp_name };
    let name_owned: Vec<u8> = if name_ptr.is_null() {
        b"<anonymous>".to_vec()
    } else {
        unsafe { CStr::from_ptr(name_ptr) }
            .to_bytes()
            .iter()
            .copied()
            .take_while(|b| *b != b'.' || true)
            .collect()
    };
    let mut bare: Vec<u8> = name_owned
        .split(|b| *b == b'.')
        .last()
        .unwrap_or(b"")
        .to_vec();
    bare.push(0);
    unsafe {
        crate::object::Py_IncRef(ty as *mut PyObject);
        PyModule_AddObject(m, bare.as_ptr() as *const c_char, ty as *mut PyObject)
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyModule_AddFunctions(m: *mut PyObject, defs: *mut PyMethodDef) -> c_int {
    if m.is_null() || defs.is_null() {
        return -1;
    }
    let module = match unsafe { crate::object::clone_object(m) } {
        Object::Module(m) => m,
        _ => return -1,
    };
    let entries = unsafe { collect_methods(defs) };
    let mut d = module.dict.borrow_mut();
    for e in entries {
        let bound = e.bind(Object::Module(module.clone()));
        d.insert(DictKey(Object::from_str(e.name.clone())), bound);
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyModule_GetDict(m: *mut PyObject) -> *mut PyObject {
    if m.is_null() {
        return ptr::null_mut();
    }
    let module = match unsafe { crate::object::clone_object(m) } {
        Object::Module(m) => m,
        _ => return ptr::null_mut(),
    };
    crate::object::into_owned(Object::Dict(module.dict.clone()))
}

#[no_mangle]
pub unsafe extern "C" fn PyModule_GetName(m: *mut PyObject) -> *const c_char {
    if m.is_null() {
        return ptr::null();
    }
    let module = match unsafe { crate::object::clone_object(m) } {
        Object::Module(m) => m,
        _ => return ptr::null(),
    };
    // Allocate a `CString` and leak it so the returned pointer is
    // stable across the call. CPython keeps the name in the
    // module's dict; we materialise a leak per query, which is fine
    // for the relatively rare callers.
    let mut bytes: Vec<u8> = module.name.as_bytes().to_vec();
    bytes.push(0);
    Box::leak(bytes.into_boxed_slice()).as_ptr() as *const c_char
}

#[no_mangle]
pub unsafe extern "C" fn PyModule_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Module(_)).into()
}

/// `PyImport_ImportModule(name)` — look the name up in
/// `sys.modules`, importing if necessary. Requires an active
/// interpreter context.
#[no_mangle]
pub unsafe extern "C" fn PyImport_ImportModule(name: *const c_char) -> *mut PyObject {
    crate::interp::ensure_initialised();
    if name.is_null() {
        return ptr::null_mut();
    }
    let s = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    let result = crate::interp::with_interp_mut(|interp| interp.import_path(&s));
    match result {
        Some(Ok(obj)) => crate::object::into_owned(obj),
        Some(Err(err)) => {
            install_runtime_error(err);
            ptr::null_mut()
        }
        None => {
            crate::errors::set_runtime_error(format!(
                "PyImport_ImportModule({s:?}): no active interpreter"
            ));
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyImport_AddModule(name: *const c_char) -> *mut PyObject {
    crate::interp::ensure_initialised();
    if name.is_null() {
        return ptr::null_mut();
    }
    let s = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    let result = crate::interp::with_current(|ctx| {
        let interp = unsafe { &mut *ctx.interp };
        let cache = interp.module_cache().clone();
        if let Some(m) = cache.get(&s) {
            return Some(m);
        }
        let dict = Rc::new(RefCell::new(DictData::default()));
        dict.borrow_mut().insert(
            DictKey(Object::from_static("__name__")),
            Object::from_str(s.clone()),
        );
        let module = Object::Module(Rc::new(PyModule {
            name: s.clone(),
            filename: None,
            dict,
        }));
        cache.insert(&s, module.clone());
        Some(module)
    });
    match result.flatten() {
        Some(m) => crate::object::into_owned(m),
        None => ptr::null_mut(),
    }
}

/// `PyImport_AddModuleRef(name)` (3.13) — like `PyImport_AddModule` but
/// returns a **new** reference. WeavePy's `PyImport_AddModule` already
/// hands back an owned reference, so this is the same call. Cython's
/// `__Pyx_PyImport_AddModuleRef` is `#define`d to this on 3.13.
#[no_mangle]
pub unsafe extern "C" fn PyImport_AddModuleRef(name: *const c_char) -> *mut PyObject {
    unsafe { PyImport_AddModule(name) }
}

/// `PyImport_GetModuleDict()` — the genuine `sys.modules` dict (a borrowed
/// reference in CPython). WeavePy's `sys.modules` *is* a real dict backed
/// by the interpreter's [`ModuleCache`], so registrations Cython performs
/// here (`PyDict_SetItemString(modules, "cyreal", m)`) flow into the live
/// module table. We hand back an owned reference to that same dict; the
/// underlying storage is interpreter-lived, so treating it as borrowed
/// (the caller never decrefs) does not free it.
#[no_mangle]
pub unsafe extern "C" fn PyImport_GetModuleDict() -> *mut PyObject {
    crate::interp::ensure_initialised();
    // Use the effective interpreter (active extension context, published VM
    // pointer, or last-seen) rather than requiring an ACTIVE context: mypyc's
    // `CPyImport_ImportMany` reads the result with `PyTuple_GET_*` style
    // direct access and dereferences it unconditionally, so a NULL here from
    // a re-entrant/ctypes call path is an instant segfault.
    crate::interp::with_interp_mut(|interp| {
        let modules = interp.module_cache().modules.clone();
        crate::object::into_owned(Object::Dict(modules))
    })
    .unwrap_or(ptr::null_mut())
}

/// `PyModule_NewObject(name)` — create a fresh module object named `name`
/// (a unicode object) **without** registering it in `sys.modules`, matching
/// CPython. Cython's PEP 489 create slot (`__pyx_pymod_create`) calls this.
#[no_mangle]
pub unsafe extern "C" fn PyModule_NewObject(name: *mut PyObject) -> *mut PyObject {
    crate::interp::ensure_initialised();
    let s = match unsafe { crate::object::clone_object(name) } {
        Object::Str(s) => s.to_string(),
        other => {
            crate::errors::set_type_error(format!(
                "PyModule_NewObject: name must be a string, not {}",
                other.type_name()
            ));
            return ptr::null_mut();
        }
    };
    let dict = Rc::new(RefCell::new(DictData::default()));
    dict.borrow_mut().insert(
        DictKey(Object::from_static("__name__")),
        Object::from_str(s.clone()),
    );
    dict.borrow_mut()
        .insert(DictKey(Object::from_static("__doc__")), Object::None);
    let module = Object::Module(Rc::new(PyModule {
        name: s,
        filename: None,
        dict,
    }));
    crate::object::into_owned(module)
}

#[no_mangle]
pub unsafe extern "C" fn PyImport_GetModule(name: *mut PyObject) -> *mut PyObject {
    let name_str = match unsafe { crate::object::clone_object(name) } {
        Object::Str(s) => s.to_string(),
        _ => return ptr::null_mut(),
    };
    crate::interp::with_current(|ctx| {
        let interp = unsafe { &*ctx.interp };
        interp.module_cache().get(&name_str)
    })
    .flatten()
    .map_or(ptr::null_mut(), |m| crate::object::into_owned(m))
}

/// `PyClassMethod_New(callable)` — wrap `callable` in a `classmethod`
/// descriptor, returning a new reference (Python `classmethod(callable)`).
/// Cython emits this for a `@classmethod` assigned in a class body — e.g.
/// frozenlist's `__class_getitem__ = classmethod(types.GenericAlias)`.
#[no_mangle]
pub unsafe extern "C" fn PyClassMethod_New(callable: *mut PyObject) -> *mut PyObject {
    if callable.is_null() {
        crate::errors::set_type_error("PyClassMethod_New: callable is NULL");
        return ptr::null_mut();
    }
    let func = unsafe { crate::object::clone_object(callable) };
    crate::object::into_owned(Object::ClassMethod(MethodWrapper::new(func)))
}

/// `PyDescr_NewClassMethod(type, method)` — build a `classmethod_descriptor`
/// from a single C `PyMethodDef` for installation in `type`'s dict. Read
/// later as `Type.method`, the descriptor binds `Type` as the call's first
/// argument (the classmethod protocol). WeavePy bridges the C function into
/// a builtin and wraps it in [`Object::ClassMethod`], whose MRO-lookup path
/// (`crate::abstract_`) binds the owning class — so a subsequent
/// `Type.method(...)` reaches the C function with the type as `self`.
#[no_mangle]
pub unsafe extern "C" fn PyDescr_NewClassMethod(
    _type: *mut crate::types::PyTypeObject,
    method: *mut PyMethodDef,
) -> *mut PyObject {
    if method.is_null() {
        crate::errors::set_type_error("PyDescr_NewClassMethod: method is NULL");
        return ptr::null_mut();
    }
    let entry = unsafe { *method };
    if entry.ml_name.is_null() {
        crate::errors::set_type_error("PyDescr_NewClassMethod: method has no name");
        return ptr::null_mut();
    }
    let name = unsafe { CStr::from_ptr(entry.ml_name) }
        .to_string_lossy()
        .into_owned();
    let callable = wrap_c_function(name, entry.ml_meth, entry.ml_flags, None);
    crate::object::into_owned(Object::ClassMethod(MethodWrapper::new(callable)))
}

/// Sidecar for [`PyCFunction_NewEx`]-minted builtins (RFC 0066 WS3): the
/// caller's real `PyMethodDef*` and the bound `self`, keyed by the
/// `Rc<BuiltinFn>` data pointer. `BuiltinFn` captures `self` only inside
/// its call closures, so the faithful `PyCFunctionObject` mirror body
/// cannot recover it from the VM object alone — yet stock extensions
/// read both fields straight off the struct: pybind11's
/// `initialize_generic` does `PyCFunction_GET_SELF(sibling)` (the
/// capsule holding its `function_record` chain) on every overload def,
/// and fails module init outright on NULL. Each entry pins a clone of
/// the builtin so the key `Rc` can never be dropped and its address
/// reused.
struct CFuncExtra {
    /// Keeps the keyed `Rc<BuiltinFn>` alive for the life of the entry.
    _pinned: Object,
    /// The `self` passed to `PyCFunction_NewEx` (`None` for C NULL).
    self_obj: Option<Object>,
    /// The caller's `PyMethodDef*` (owned by the extension, stable).
    ml: usize,
}

thread_local! {
    static CFUNC_EXTRAS: std::cell::RefCell<std::collections::HashMap<usize, CFuncExtra>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn cfunc_extra_key(callable: &Object) -> Option<usize> {
    match callable {
        Object::Builtin(rc) => Some(Rc::as_ptr(rc) as *const () as usize),
        _ => None,
    }
}

/// Look up the faithful-struct extras for a builtin crossing into C:
/// `(self, PyMethodDef*)`. Consulted by the mirror's `fill_body` every
/// time a `PyCFunctionObject` body is minted, so the fields survive any
/// number of VM round-trips (each crossing mints a fresh mirror).
pub(crate) fn cfunction_extra(obj: &Object) -> Option<(Option<Object>, usize)> {
    let key = cfunc_extra_key(obj)?;
    CFUNC_EXTRAS.with(|t| t.borrow().get(&key).map(|e| (e.self_obj.clone(), e.ml)))
}

/// `PyCFunction_NewEx(ml, self, module)` — build a builtin function/method
/// object from a single `PyMethodDef`, binding `self` (NULL → unbound). The
/// `module` owner is informational under WeavePy's model. Used by
/// [`crate::wave5_pandas::PyCMethod_New`] and any method-table install that
/// reaches for the public constructor.
#[no_mangle]
pub unsafe extern "C" fn PyCFunction_NewEx(
    ml: *mut PyMethodDef,
    self_: *mut PyObject,
    _module: *mut PyObject,
) -> *mut PyObject {
    if ml.is_null() {
        crate::errors::set_type_error("PyCFunction_NewEx: method def is NULL");
        return ptr::null_mut();
    }
    let entry = unsafe { *ml };
    if entry.ml_name.is_null() {
        crate::errors::set_type_error("PyCFunction_NewEx: method has no name");
        return ptr::null_mut();
    }
    let name = unsafe { CStr::from_ptr(entry.ml_name) }
        .to_string_lossy()
        .into_owned();
    let self_obj = if self_.is_null() || std::ptr::eq(self_, crate::singletons::none_ptr()) {
        None
    } else {
        Some(unsafe { crate::object::clone_object(self_) })
    };
    let callable = wrap_c_function(name, entry.ml_meth, entry.ml_flags, self_obj.clone());
    if let Some(key) = cfunc_extra_key(&callable) {
        CFUNC_EXTRAS.with(|t| {
            t.borrow_mut().insert(
                key,
                CFuncExtra {
                    _pinned: callable.clone(),
                    self_obj,
                    ml: ml as usize,
                },
            );
        });
    }
    crate::object::into_owned(callable)
}

fn install_runtime_error(err: RuntimeError) {
    match err {
        RuntimeError::PyException(pe) => {
            let cls = match &pe.instance {
                Object::Instance(inst) => Some(inst.cls()),
                _ => None,
            };
            crate::errors::set_pending(cls, Object::from_str(pe.message()));
        }
        RuntimeError::Internal(msg) => {
            crate::errors::set_runtime_error(msg);
        }
    }
}
