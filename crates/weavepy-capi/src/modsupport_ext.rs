//! RFC 0068 WS4 — the C-API surface CPython's `_testsinglephase.c` /
//! `_testmultiphase.c` fixtures link against (compiled verbatim by
//! `weavepy-conformance/build.rs` and loaded by `test_importlib`'s
//! extension suite): PEP 489 module-state accessors, the Argument
//! Clinic fastcall helpers, `PyTime_*`, and `_PyNamespace_New`.

use core::ffi::{c_char, c_int, c_void};
use std::ffi::CStr;

use weavepy_vm::sync::RefCell;

use weavepy_vm::object::{DictData, DictKey, Object, PyModule};
use weavepy_vm::sync::Rc;

use crate::object::{PyObject, PySsizeT};
use crate::types::PyTypeObject;

// ---------------------------------------------------------------------------
// Module def / state registries
// ---------------------------------------------------------------------------

/// `PyModuleDef*` for each module created from a def (`PyModule_Create2`
/// and the multi-phase path both register), keyed by the module's native
/// `Rc` identity — the same key `MODULE_STATE` uses. Backs
/// [`PyModule_GetDef`], which `_testsinglephase` uses to prove the def is
/// process-global across re-imports.
static DEF_BY_MODULE: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());

/// Record `module → def` (idempotent per module).
pub(crate) fn register_module_def(module_key: usize, def: *mut c_void) {
    if def.is_null() {
        return;
    }
    if let Ok(mut g) = DEF_BY_MODULE.lock() {
        if !g.iter().any(|(k, _)| *k == module_key) {
            g.push((module_key, def as usize));
        }
    }
}

fn module_native_key(module: *mut PyObject) -> Option<usize> {
    if module.is_null() {
        return None;
    }
    match unsafe { crate::object::clone_object(module) } {
        Object::Module(rc) => Some(Rc::as_ptr(&rc) as usize),
        _ => None,
    }
}

/// `PyModule_GetDef(m)` — the `PyModuleDef` the module was created from,
/// or NULL (without error) for a non-def module, matching CPython's
/// behaviour for modules created by `PyModule_New`.
#[no_mangle]
pub unsafe extern "C" fn PyModule_GetDef(module: *mut PyObject) -> *mut c_void {
    let Some(key) = module_native_key(module) else {
        crate::errors::set_type_error("PyModule_GetDef() argument must be a module");
        return core::ptr::null_mut();
    };
    DEF_BY_MODULE
        .lock()
        .ok()
        .and_then(|g| {
            g.iter()
                .find(|(k, _)| *k == key)
                .map(|&(_, d)| d as *mut c_void)
        })
        .unwrap_or(core::ptr::null_mut())
}

/// `PyModule_New(name)` — a bare module object with only `__name__` set.
#[no_mangle]
pub unsafe extern "C" fn PyModule_New(name: *const c_char) -> *mut PyObject {
    crate::interp::ensure_initialised();
    if name.is_null() {
        crate::errors::set_type_error("PyModule_New: name is NULL");
        return core::ptr::null_mut();
    }
    let name = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    let dict = Rc::new(RefCell::new(DictData::default()));
    dict.borrow_mut().insert(
        DictKey(Object::from_static("__name__")),
        Object::from_str(name.clone()),
    );
    dict.borrow_mut()
        .insert(DictKey(Object::from_static("__doc__")), Object::None);
    let module = Rc::new(PyModule {
        name,
        filename: None,
        dict,
    });
    crate::object::into_owned(Object::Module(module))
}

/// Does this def declare PEP 489 slots? The `PyState_*` registration
/// family rejects multi-phase defs (CPython's `_PyState_AddModule`;
/// extension.test_loader's `test_try_registration`).
unsafe fn def_has_slots(def: *mut c_void) -> bool {
    if def.is_null() {
        return false;
    }
    let def = def as *mut crate::module::PyModuleDef;
    !unsafe { (*def).m_slots }.is_null()
}

fn set_system_error(msg: &str) {
    crate::errors::set_pending(
        Some(
            weavepy_vm::builtin_types::builtin_types()
                .system_error
                .clone(),
        ),
        Object::from_str(msg.to_owned()),
    );
}

/// `PyState_AddModule(module, def)` — register the module in the
/// per-interpreter `modules_by_index` equivalent so `PyState_FindModule`
/// resolves it later. Multi-phase defs are rejected with `SystemError`.
#[no_mangle]
pub unsafe extern "C" fn PyState_AddModule(module: *mut PyObject, def: *mut c_void) -> c_int {
    if module.is_null() || def.is_null() {
        crate::errors::set_runtime_error("PyState_AddModule: NULL argument");
        return -1;
    }
    if unsafe { def_has_slots(def) } {
        set_system_error("PyState_AddModule called on module with slots");
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(module) };
    crate::wave5_pandas::register_find_module(def, obj);
    0
}

/// `PyState_RemoveModule(def)` — drop the `PyState_FindModule`
/// registration. Multi-phase defs are rejected with `SystemError`.
#[no_mangle]
pub unsafe extern "C" fn PyState_RemoveModule(def: *mut c_void) -> c_int {
    if unsafe { def_has_slots(def) } {
        set_system_error("PyState_RemoveModule called on module with slots");
        return -1;
    }
    crate::wave5_pandas::unregister_find_module(def);
    0
}

/// `PyUnstable_Module_SetGIL(module, gil)` — free-threading opt-out
/// declaration; WeavePy always runs with the GIL, so this is a no-op.
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_Module_SetGIL(
    _module: *mut PyObject,
    _gil: *mut c_void,
) -> c_int {
    0
}

// ---------------------------------------------------------------------------
// PyType_GetModule / PyType_GetModuleState
// ---------------------------------------------------------------------------

/// `PyType_GetModule(type)` — the module a heap type was created under
/// (`PyType_FromModuleAndSpec`). Borrowed reference; `SystemError` when
/// the type carries no module, matching CPython.
#[no_mangle]
pub unsafe extern "C" fn PyType_GetModule(ty: *mut PyObject) -> *mut PyObject {
    let module = crate::abi313::lookup_type_module(ty);
    if module.is_null() {
        crate::errors::set_pending(
            Some(
                weavepy_vm::builtin_types::builtin_types()
                    .system_error
                    .clone(),
            ),
            Object::from_str("PyType_GetModule: type_getmodule".to_owned()),
        );
    }
    module
}

/// `PyType_GetModuleState(type)` — `PyModule_GetState(PyType_GetModule(type))`.
#[no_mangle]
pub unsafe extern "C" fn PyType_GetModuleState(ty: *mut PyObject) -> *mut c_void {
    let module = unsafe { PyType_GetModule(ty) };
    if module.is_null() {
        return core::ptr::null_mut();
    }
    crate::wave5_pandas::PyModule_GetState(module)
}

// ---------------------------------------------------------------------------
// _PyNamespace_New
// ---------------------------------------------------------------------------

/// `_PyNamespace_New(kwds)` — a `types.SimpleNamespace` seeded from the
/// `kwds` dict (`_testsinglephase.state_initialized()` returns one).
#[no_mangle]
pub unsafe extern "C" fn _PyNamespace_New(kwds: *mut PyObject) -> *mut PyObject {
    crate::interp::ensure_initialised();
    let data = if kwds.is_null() {
        DictData::default()
    } else {
        match unsafe { crate::object::clone_object(kwds) } {
            Object::Dict(d) => d.borrow().clone(),
            _ => {
                crate::errors::set_type_error("_PyNamespace_New: kwds must be a dict");
                return core::ptr::null_mut();
            }
        }
    };
    crate::object::into_owned(Object::SimpleNamespace(Rc::new(RefCell::new(data))))
}

// ---------------------------------------------------------------------------
// PyTime
// ---------------------------------------------------------------------------

/// `PyTime_Monotonic(&t)` — monotonic clock in nanoseconds.
#[no_mangle]
pub unsafe extern "C" fn PyTime_Monotonic(result: *mut i64) -> c_int {
    if result.is_null() {
        return -1;
    }
    // A process-stable anchor keeps values well inside i64 nanoseconds.
    static ANCHOR: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let anchor = ANCHOR.get_or_init(std::time::Instant::now);
    let ns = anchor.elapsed().as_nanos().min(i64::MAX as u128) as i64;
    unsafe { *result = ns };
    0
}

/// `PyTime_AsSecondsDouble(t)` — nanoseconds → seconds.
#[no_mangle]
pub unsafe extern "C" fn PyTime_AsSecondsDouble(t: i64) -> f64 {
    t as f64 / 1e9
}

// ---------------------------------------------------------------------------
// Argument Clinic fastcall helpers
// ---------------------------------------------------------------------------

/// `_PyArg_CheckPositional(name, nargs, min, max)` — the clinic guard for
/// positional-only signatures; error wording mirrors CPython's
/// `getargs.c` so `assertRaisesRegex` matches.
#[no_mangle]
pub unsafe extern "C" fn _PyArg_CheckPositional(
    name: *const c_char,
    nargs: PySsizeT,
    min: PySsizeT,
    max: PySsizeT,
) -> c_int {
    let display = if name.is_null() {
        "function".to_owned()
    } else {
        format!("{}()", unsafe { CStr::from_ptr(name) }.to_string_lossy())
    };
    if nargs < min {
        let expected = if min == max {
            format!("exactly {min}")
        } else {
            format!("at least {min}")
        };
        crate::errors::set_type_error(format!(
            "{display} takes {expected} argument{} ({nargs} given)",
            if min == 1 { "" } else { "s" }
        ));
        return 0;
    }
    if nargs == 0 {
        return 1;
    }
    if nargs > max {
        let expected = if min == max {
            format!("exactly {max}")
        } else {
            format!("at most {max}")
        };
        crate::errors::set_type_error(format!(
            "{display} takes {expected} argument{} ({nargs} given)",
            if max == 1 { "" } else { "s" }
        ));
        return 0;
    }
    1
}

/// Mirror of the `_PyArg_Parser` struct in `cpython/modsupport.h`
/// (3.13 layout: format/keywords/fname/custom_msg, a one-byte once
/// flag, then the derived int fields).
#[repr(C)]
pub struct PyArgParser {
    pub format: *const c_char,
    pub keywords: *const *const c_char,
    pub fname: *const c_char,
    pub custom_msg: *const c_char,
    pub once: u8,
    pub is_kwtuple_owned: c_int,
    pub pos: c_int,
    pub min: c_int,
    pub max: c_int,
    pub kwtuple: *mut PyObject,
    pub next: *mut PyArgParser,
}

fn parser_name(parser: &PyArgParser) -> String {
    if !parser.fname.is_null() {
        unsafe { CStr::from_ptr(parser.fname) }
            .to_string_lossy()
            .into_owned()
    } else {
        "function".to_owned()
    }
}

/// `_PyArg_UnpackKeywords` — the clinic keyword unpacker. Distributes the
/// positional `args` and the `kwnames`-keyed keyword values into `buf`
/// slot-by-slot (parser.keywords order), leaving optional slots NULL.
/// Returns `buf` on success, NULL with an exception set on error.
#[no_mangle]
pub unsafe extern "C" fn _PyArg_UnpackKeywords(
    args: *const *mut PyObject,
    nargs: PySsizeT,
    kwargs: *mut PyObject,
    kwnames: *mut PyObject,
    parser: *mut PyArgParser,
    minpos: c_int,
    maxpos: c_int,
    minkw: c_int,
    buf: *mut *mut PyObject,
) -> *const *mut PyObject {
    crate::interp::ensure_initialised();
    if parser.is_null() || buf.is_null() {
        crate::errors::set_runtime_error("_PyArg_UnpackKeywords: NULL parser/buf");
        return core::ptr::null();
    }
    let parser = unsafe { &*parser };
    let fname = parser_name(parser);

    // Collect the parser's keyword names (positional-or-keyword +
    // keyword-only, in declaration order).
    let mut keywords: Vec<String> = Vec::new();
    if !parser.keywords.is_null() {
        let mut p = parser.keywords;
        unsafe {
            while !(*p).is_null() {
                keywords.push(CStr::from_ptr(*p).to_string_lossy().into_owned());
                p = p.add(1);
            }
        }
    }
    let total = keywords.len().max(maxpos as usize);

    // Fast-path shape check on positionals.
    if nargs > maxpos as PySsizeT {
        crate::errors::set_type_error(format!(
            "{fname}() takes at most {maxpos} positional argument{} ({nargs} given)",
            if maxpos == 1 { "" } else { "s" }
        ));
        return core::ptr::null();
    }
    if nargs < minpos as PySsizeT && kwnames.is_null() && kwargs.is_null() {
        crate::errors::set_type_error(format!(
            "{fname}() takes at least {minpos} positional argument{} ({nargs} given)",
            if minpos == 1 { "" } else { "s" }
        ));
        return core::ptr::null();
    }

    unsafe {
        for i in 0..total {
            *buf.add(i) = core::ptr::null_mut();
        }
        for i in 0..nargs as usize {
            *buf.add(i) = *args.add(i);
        }
    }

    // Keyword values arrive either as a `kwnames` tuple trailing the
    // positional array (fastcall) or as a `kwargs` dict (legacy).
    let mut kw_pairs: Vec<(String, *mut PyObject)> = Vec::new();
    if !kwnames.is_null() {
        if let Object::Tuple(names) = unsafe { crate::object::clone_object(kwnames) } {
            for (i, n) in names.iter().enumerate() {
                let key = match n {
                    Object::Str(s) => s.to_string(),
                    _ => {
                        crate::errors::set_type_error("keywords must be strings");
                        return core::ptr::null();
                    }
                };
                kw_pairs.push((key, unsafe { *args.add(nargs as usize + i) }));
            }
        }
    } else if !kwargs.is_null() {
        if let Object::Dict(d) = unsafe { crate::object::clone_object(kwargs) } {
            for (k, v) in d.borrow().iter() {
                let key = match &k.0 {
                    Object::Str(s) => s.to_string(),
                    _ => {
                        crate::errors::set_type_error("keywords must be strings");
                        return core::ptr::null();
                    }
                };
                // The dict entries are borrowed for the duration of the
                // call; mint owned boxes and leak the +1 (clinic treats
                // buf entries as borrowed).
                let p = crate::object::into_owned(v.clone());
                kw_pairs.push((key, p));
            }
        }
    }

    // `parser.pos` counts positional-only parameters, which have no
    // keyword name; keywords[i] names parameter (pos + i).
    let pos_only = parser.pos.max(0) as usize;
    let mut kwcount = 0;
    for (key, value) in kw_pairs {
        match keywords.iter().position(|k| *k == key) {
            Some(idx) => {
                let slot = pos_only + idx;
                unsafe {
                    if slot < nargs as usize && !(*buf.add(slot)).is_null() {
                        crate::errors::set_type_error(format!(
                            "argument for {fname}() given by name ('{key}') and position ({})",
                            slot + 1
                        ));
                        return core::ptr::null();
                    }
                    *buf.add(slot) = value;
                }
                kwcount += 1;
            }
            None => {
                crate::errors::set_type_error(format!(
                    "'{key}' is an invalid keyword argument for {fname}()"
                ));
                return core::ptr::null();
            }
        }
    }

    // Required-argument accounting: every slot below minpos must be
    // filled, and at least `minkw` keyword-only arguments must appear.
    unsafe {
        for i in 0..minpos as usize {
            if (*buf.add(i)).is_null() {
                crate::errors::set_type_error(format!(
                    "{fname}() missing required positional argument {}",
                    i + 1
                ));
                return core::ptr::null();
            }
        }
    }
    if kwcount < minkw {
        crate::errors::set_type_error(format!(
            "{fname}() missing required keyword-only argument(s)"
        ));
        return core::ptr::null();
    }
    buf as *const *mut PyObject
}

// ---------------------------------------------------------------------------
// METH_METHOD dispatch support
// ---------------------------------------------------------------------------

/// Invoke a `METH_METHOD | METH_FASTCALL | METH_KEYWORDS` C function
/// (`PyCMethod`): `(self, defining_class, args, nargs, kwnames)`.
/// The defining class is late-bound (the heap type pointer exists only
/// after the method dict is assembled) via an `AtomicUsize` cell shared
/// by all methods of the type — see `assemble_type_dict`.
pub(crate) unsafe fn call_meth_method(
    func: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject,
    self_ptr: *mut PyObject,
    defining_class: *mut PyTypeObject,
    args: &[Object],
    kwargs: &[(String, Object)],
) -> *mut PyObject {
    let mut argv: Vec<*mut PyObject> = Vec::with_capacity(args.len() + kwargs.len());
    {
        let _pin = crate::mirror::enter_arg_pin();
        for a in args {
            argv.push(crate::object::into_owned(a.clone()));
        }
        for (_, v) in kwargs {
            argv.push(crate::object::into_owned(v.clone()));
        }
    }
    let nargs = args.len() as PySsizeT;
    let kwnames: *mut PyObject = if kwargs.is_empty() {
        core::ptr::null_mut()
    } else {
        let names: Vec<Object> = kwargs
            .iter()
            .map(|(k, _)| Object::from_str(k.as_str()))
            .collect();
        let _intern = crate::mirror::enter_intern_scope();
        crate::object::into_owned(Object::new_tuple(names))
    };
    #[allow(clippy::missing_transmute_annotations)]
    let cmethod: unsafe extern "C" fn(
        *mut PyObject,
        *mut PyTypeObject,
        *const *mut PyObject,
        PySsizeT,
        *mut PyObject,
    ) -> *mut PyObject = unsafe { std::mem::transmute(func) };
    let argv_ptr = argv.as_ptr();
    let result = crate::interp::ensure_active(|| unsafe {
        cmethod(self_ptr, defining_class, argv_ptr, nargs, kwnames)
    });
    for &a in &argv {
        unsafe { crate::object::Py_DecRef(a) };
    }
    if !kwnames.is_null() {
        unsafe { crate::object::Py_DecRef(kwnames) };
    }
    result
}
