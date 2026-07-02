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
    /// Bind this method as a free function (used inside a module
    /// dict). Type-bound methods get bound separately via the bound-
    /// method machinery.
    pub fn bind(&self) -> Object {
        wrap_c_function(self.name.clone(), self.func, self.flags, None)
    }

    /// Bind this method as an unbound class member: invocations
    /// through an instance will pass the instance as the first
    /// argument. The wrapper extracts the receiver from `args[0]`
    /// and routes it to the C function's `self` parameter, leaving
    /// only the trailing user-supplied args in the tuple.
    pub fn bind_unbound(&self) -> Object {
        wrap_c_method_function(self.name.clone(), self.func, self.flags)
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
    for a in args {
        argv.push(crate::object::into_owned(a.clone()));
    }
    for (_, v) in kwargs {
        argv.push(crate::object::into_owned(v.clone()));
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
    let mut d = DictData::new();
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
        let arg = crate::object::into_owned(args[0].clone());
        let r = crate::interp::ensure_active(|| unsafe { func(self_ptr, arg) });
        unsafe { crate::object::Py_DecRef(arg) };
        r
    } else if (flags & METH_FASTCALL) != 0 {
        unsafe { call_fastcall(func, self_ptr, args, kwargs, flags) }
    } else {
        let tuple = crate::object::into_owned(Object::new_tuple(args.to_vec()));
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
) -> Object {
    let qualified = format!("_capi:{name}");
    let static_name: &'static str = Box::leak(qualified.into_boxed_str());
    let display_name: &'static str = Box::leak(name.into_boxed_str());
    let call = move |args: &[Object]| -> Result<Object, RuntimeError> {
        bridge_invoke_method(func, display_name, flags, args, &[])
    };
    let call_kw: Option<
        Box<dyn Fn(&[Object], &[(String, Object)]) -> Result<Object, RuntimeError> + Send + Sync>,
    > = if (flags & METH_KEYWORDS) != 0 {
        Some(Box::new(
            move |args: &[Object], kwargs: &[(String, Object)]| {
                bridge_invoke_method(func, display_name, flags, args, kwargs)
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
        let arg = crate::object::into_owned(rest[0].clone());
        let r = crate::interp::ensure_active(|| unsafe { func(self_ptr, arg) });
        unsafe { crate::object::Py_DecRef(arg) };
        r
    } else if (flags & METH_FASTCALL) != 0 {
        unsafe { call_fastcall(func, self_ptr, rest, kwargs, flags) }
    } else {
        let tuple = crate::object::into_owned(Object::new_tuple(rest.to_vec()));
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
    let dict = Rc::new(RefCell::new(DictData::new()));
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
        if !def_ref.m_methods.is_null() {
            let entries = unsafe { collect_methods(def_ref.m_methods) };
            for e in entries {
                d.insert(DictKey(Object::from_str(e.name.clone())), e.bind());
            }
        }
    }
    let module = Rc::new(PyModule {
        name,
        filename: None,
        dict,
    });
    // PEP 3121: a single-phase extension that declares `m_size > 0` (pandas'
    // vendored ujson) allocates per-module state here and writes into it via
    // `PyModule_GetState`. Key the block by the module's native `Rc` identity
    // (stable across the fresh per-crossing C boxes) before the `Rc` is moved
    // into the crossing.
    if def_ref.m_size > 0 {
        let key = Rc::as_ptr(&module) as usize;
        crate::wave5_pandas::ensure_module_state(key, def_ref.m_size as usize);
    }
    // CPython adds a single-phase module with per-interpreter state
    // (`m_size >= 0`) to `interp->modules_by_index`, so the extension can later
    // re-fetch it via `PyState_FindModule(def)`. pandas' vendored ujson relies
    // on this to reach its cached `Series`/`DataFrame`/`Index` types.
    if def_ref.m_size >= 0 {
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
            eprintln!("[mpi] {full_name}: build_module_spec -> {}", if spec.is_null() { "NULL" } else { "ok" });
        }
        let m = unsafe { create(spec, def) };
        if !spec.is_null() {
            unsafe { crate::object::Py_DecRef(spec) };
        }
        if trace {
            eprintln!("[mpi] {full_name}: create slot -> {}", if m.is_null() { "NULL" } else { "ok" });
        }
        m
    } else {
        // Default creation: a fresh module preloaded with m_methods.
        unsafe { PyModule_Create2(def, 1013) }
    };
    if module.is_null() {
        let pending = crate::errors::take_pending()
            .map(|p| {
                let ty = p
                    .ty
                    .as_ref()
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| "Exception".to_owned());
                let msg = crate::errors::message_for(&p.value);
                if msg.is_empty() {
                    format!("module create slot raised {ty}")
                } else {
                    format!("module create slot raised {ty}: {msg}")
                }
            })
            .unwrap_or_else(|| "module creation slot returned NULL".to_owned());
        return Err(pending);
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
            let pending = crate::errors::take_pending()
                .map(|p| {
                    let ty =
                        p.ty.as_ref()
                            .map(|t| t.name.clone())
                            .unwrap_or_else(|| "?".to_owned());
                    format!("{ty}: {}", p.value.to_str())
                })
                .unwrap_or_else(|| format!("Py_mod_exec slot returned {rc}"));
            if trace {
                eprintln!("[mpi] {full_name}: exec slot {i} FAILED -> {pending}");
            }
            unsafe { crate::object::Py_DecRef(module) };
            return Err(pending);
        }
        if let Some(p) = crate::errors::take_pending() {
            let ty = p
                .ty
                .as_ref()
                .map(|t| t.name.clone())
                .unwrap_or_else(|| "Exception".to_owned());
            let msg = crate::errors::message_for(&p.value);
            unsafe { crate::object::Py_DecRef(module) };
            return Err(if msg.is_empty() {
                format!("exec slot {i} left pending {ty}")
            } else {
                format!("exec slot {i} left pending {ty}: {msg}")
            });
        }
    }
    Ok(module)
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
    module
        .dict
        .borrow_mut()
        .insert(DictKey(Object::from_str(key)), v);
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
        d.insert(DictKey(Object::from_str(e.name.clone())), e.bind());
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
        let dict = Rc::new(RefCell::new(DictData::new()));
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
    crate::interp::with_current(|ctx| {
        let interp = unsafe { &*ctx.interp };
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
    let dict = Rc::new(RefCell::new(DictData::new()));
    dict.borrow_mut().insert(
        DictKey(Object::from_static("__name__")),
        Object::from_str(s.clone()),
    );
    dict.borrow_mut().insert(
        DictKey(Object::from_static("__doc__")),
        Object::None,
    );
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
    let callable = wrap_c_function(name, entry.ml_meth, entry.ml_flags, self_obj);
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
