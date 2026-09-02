//! RFC 0076 WS5: the C-API tail that **torch** (`libtorch_python`) binds.
//!
//! On macOS `_C.cpython-*.so` links `libtorch_python.dylib`, which binds
//! every referenced Python symbol *eagerly* at `dlopen` (chained fixups):
//! one missing symbol is a clean `ImportError` for the whole wheel. The
//! surface torch needs beyond what previous waves exported splits into
//! three tiers:
//!
//! 1. **Load-bearing at import/runtime** — struct sequences (torch mints
//!    every `torch.return_types.*` with `PyStructSequence_InitType` at
//!    import), the function/code introspection getters, `PyLong_AsSize_t`,
//!    `PyErr_WarnExplicit`.
//! 2. **Profiler/dynamo surface** — dict watchers, code-extra slots, the
//!    frame-eval hooks (`torch.compile`), `PyEval_SetProfileAllThreads`.
//!    These are registries that never fire / honest side tables; dynamo
//!    itself is out of scope (it needs a host toolchain and is gated
//!    upstream).
//! 3. **Thread-walk surface** — `PyInterpreterState_ThreadHead` /
//!    `PyThreadState_Next`, used by the profiler to sample stacks. We
//!    report a single-thread world (current tstate, no next).
//!
//! Static *data* symbols torch also binds (`PyCell_Type`,
//! `_PyWeakref_RefType` and the proxy types) live in [`crate::types`] /
//! [`crate::weakref_api`].

#![allow(clippy::missing_safety_doc)]

use core::ffi::{c_char, c_int, c_void};
use std::collections::HashMap;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use weavepy_vm::object::Object;

use crate::object::{clone_object, into_owned, PyObject, PySsizeT, Py_DecRef, IMMORTAL_REFCNT};
use crate::types::PyTypeObject;

// ---------------------------------------------------------------------------
// Integer conversion
// ---------------------------------------------------------------------------

/// `PyLong_AsSize_t(o)` — like `PyLong_AsSsize_t` but unsigned:
/// negative values raise `OverflowError` and report `(size_t)-1`.
#[no_mangle]
pub unsafe extern "C" fn PyLong_AsSize_t(o: *mut PyObject) -> usize {
    let v = unsafe { crate::numbers::PyLong_AsSsize_t(o) };
    if v == -1 && crate::errors::has_pending() {
        return usize::MAX;
    }
    if v < 0 {
        crate::errors::set_pending(
            Some(
                weavepy_vm::builtin_types::builtin_types()
                    .overflow_error
                    .clone(),
            ),
            Object::from_static("can't convert negative value to size_t"),
        );
        return usize::MAX;
    }
    v as usize
}

// ---------------------------------------------------------------------------
// Warnings
// ---------------------------------------------------------------------------

/// `PyErr_WarnExplicit(category, message, filename, lineno, module,
/// registry)` — the explicit-location variant. We route through the same
/// `warnings` machinery as `PyErr_WarnEx`; the explicit filename/lineno
/// are advisory (they only alter the rendered location, which the VM's
/// warning display derives from the live frame anyway).
#[no_mangle]
pub unsafe extern "C" fn PyErr_WarnExplicit(
    category: *mut PyObject,
    message: *const c_char,
    _filename: *const c_char,
    _lineno: c_int,
    _module: *const c_char,
    _registry: *mut PyObject,
) -> c_int {
    unsafe { crate::errors::PyErr_WarnEx(category, message, 1) }
}

// ---------------------------------------------------------------------------
// Frame / eval introspection
// ---------------------------------------------------------------------------

/// `PyEval_GetFrame()` — borrowed reference to the current frame. The VM
/// does not expose a stable `PyFrameObject` for running frames (see
/// `PyThreadState_GetFrame`), so report "no frame": NULL without error is
/// a documented, valid answer callers must handle.
#[no_mangle]
pub extern "C" fn PyEval_GetFrame() -> *mut PyObject {
    ptr::null_mut()
}

/// `PyEval_SetProfileAllThreads(func, arg)` — install a profile hook on
/// every thread. The C-level profile hook surface is not wired (Python-
/// level `sys.setprofile` is); accept and drop, like `PyEval_SetProfile`.
#[no_mangle]
pub extern "C" fn PyEval_SetProfileAllThreads(_func: *mut c_void, _arg: *mut PyObject) {}

/// `PyFrame_GetLasti(frame)` — last executed instruction offset. With no
/// faithful frame surface, -1 ("not executing") is the honest constant.
#[no_mangle]
pub extern "C" fn PyFrame_GetLasti(_frame: *mut PyObject) -> c_int {
    -1
}

// ---------------------------------------------------------------------------
// Function object getters (borrowed-reference contracts)
// ---------------------------------------------------------------------------

/// Identity-stable owned boxes for the `PyFunction_Get*` *borrowed*
/// reference contracts — one pinned box per (function, attribute), live
/// for the process (same discipline as `PyFunction_GetAnnotations`).
static FUNC_ATTR_CACHE: Mutex<Option<HashMap<(usize, &'static str), usize>>> = Mutex::new(None);

unsafe fn function_attr_borrowed(func: *mut PyObject, attr: &'static str) -> *mut PyObject {
    if let Ok(g) = FUNC_ATTR_CACHE.lock() {
        if let Some(m) = g.as_ref() {
            if let Some(&p) = m.get(&(func as usize, attr)) {
                return p as *mut PyObject;
            }
        }
    }
    let mut name = [0u8; 32];
    name[..attr.len()].copy_from_slice(attr.as_bytes());
    let p =
        unsafe { crate::abstract_::PyObject_GetAttrString(func, name.as_ptr() as *const c_char) };
    if p.is_null() {
        crate::errors::clear_thread_local();
        return ptr::null_mut();
    }
    // CPython returns NULL (no error) where the slot is empty; the
    // attribute surface spells "empty" as None.
    if matches!(unsafe { clone_object(p) }, Object::None) {
        unsafe { Py_DecRef(p) };
        return ptr::null_mut();
    }
    let mut g = FUNC_ATTR_CACHE.lock().unwrap();
    g.get_or_insert_with(HashMap::new)
        .insert((func as usize, attr), p as usize);
    p
}

/// `PyFunction_GetCode(func)` — borrowed reference to `__code__`.
#[no_mangle]
pub unsafe extern "C" fn PyFunction_GetCode(func: *mut PyObject) -> *mut PyObject {
    unsafe { function_attr_borrowed(func, "__code__") }
}

/// `PyFunction_GetClosure(func)` — borrowed `__closure__` tuple or NULL.
#[no_mangle]
pub unsafe extern "C" fn PyFunction_GetClosure(func: *mut PyObject) -> *mut PyObject {
    unsafe { function_attr_borrowed(func, "__closure__") }
}

/// `PyFunction_GetDefaults(func)` — borrowed `__defaults__` tuple or NULL.
#[no_mangle]
pub unsafe extern "C" fn PyFunction_GetDefaults(func: *mut PyObject) -> *mut PyObject {
    unsafe { function_attr_borrowed(func, "__defaults__") }
}

/// `PyFunction_GetKwDefaults(func)` — borrowed `__kwdefaults__` dict or NULL.
#[no_mangle]
pub unsafe extern "C" fn PyFunction_GetKwDefaults(func: *mut PyObject) -> *mut PyObject {
    unsafe { function_attr_borrowed(func, "__kwdefaults__") }
}

// ---------------------------------------------------------------------------
// Code object introspection
// ---------------------------------------------------------------------------

/// `PyCode_Addr2Line(code, addr)` — source line for a bytecode offset.
/// `-1` maps to `co_firstlineno` (CPython contract). Offsets index the
/// VM's per-instruction line table (instruction = 2 bytes in CPython's
/// addressing, which torch's profiler uses only for display).
#[no_mangle]
pub unsafe extern "C" fn PyCode_Addr2Line(code: *mut PyObject, addr: PySsizeT) -> c_int {
    let Object::Code(c) = (unsafe { clone_object(code) }) else {
        crate::errors::set_type_error("PyCode_Addr2Line: not a code object");
        return -1;
    };
    let first = c.linetable.first().copied().unwrap_or(1) as c_int;
    if addr < 0 {
        return first;
    }
    c.linetable
        .get(addr as usize / 2)
        .map(|&l| l as c_int)
        .unwrap_or(first)
}

/// `PyCode_GetVarnames(code)` — new reference to the `co_varnames` tuple.
#[no_mangle]
pub unsafe extern "C" fn PyCode_GetVarnames(code: *mut PyObject) -> *mut PyObject {
    let Object::Code(c) = (unsafe { clone_object(code) }) else {
        crate::errors::set_type_error("PyCode_GetVarnames: not a code object");
        return ptr::null_mut();
    };
    let items: Vec<Object> = c
        .varnames
        .iter()
        .map(|s| Object::from_str(s.clone()))
        .collect();
    into_owned(Object::Tuple(weavepy_vm::sync::Rc::from(items)))
}

// ---------------------------------------------------------------------------
// Dict watchers (torch.compile / dynamo guard invalidation)
// ---------------------------------------------------------------------------

/// Registered watcher callbacks (by ID). The VM's dict has no C-visible
/// mutation hook, so watchers never fire — dynamo compiles nothing under
/// WeavePy (gated upstream), and an installed-but-silent watcher is the
/// honest degradation for the profiler paths that register one.
static DICT_WATCHERS: Mutex<[usize; 8]> = Mutex::new([0; 8]);

/// `PyDict_AddWatcher(callback)` — allocate a watcher ID.
#[no_mangle]
pub extern "C" fn PyDict_AddWatcher(callback: *mut c_void) -> c_int {
    let mut slots = DICT_WATCHERS.lock().unwrap();
    for (i, slot) in slots.iter_mut().enumerate() {
        if *slot == 0 {
            *slot = callback as usize;
            return i as c_int;
        }
    }
    crate::errors::set_runtime_error("no more dict watcher IDs available");
    -1
}

/// `PyDict_Watch(watcher_id, dict)` — mark a dict watched.
#[no_mangle]
pub extern "C" fn PyDict_Watch(watcher_id: c_int, _dict: *mut PyObject) -> c_int {
    let valid =
        (0..8).contains(&watcher_id) && DICT_WATCHERS.lock().unwrap()[watcher_id as usize] != 0;
    if !valid {
        crate::errors::set_value_error("invalid dict watcher ID");
        return -1;
    }
    0
}

/// `PyDict_Unwatch(watcher_id, dict)` — stop watching a dict.
#[no_mangle]
pub extern "C" fn PyDict_Unwatch(watcher_id: c_int, _dict: *mut PyObject) -> c_int {
    let valid =
        (0..8).contains(&watcher_id) && DICT_WATCHERS.lock().unwrap()[watcher_id as usize] != 0;
    if !valid {
        crate::errors::set_value_error("invalid dict watcher ID");
        return -1;
    }
    0
}

// ---------------------------------------------------------------------------
// Code-extra slots (PEP 523 companion surface; dynamo cache keys)
// ---------------------------------------------------------------------------

static CODE_EXTRA_NEXT: AtomicUsize = AtomicUsize::new(0);
static CODE_EXTRA: Mutex<Option<HashMap<(usize, usize), usize>>> = Mutex::new(None);

/// `PyUnstable_Eval_RequestCodeExtraIndex(free)` — allocate an extra slot
/// index on code objects.
#[no_mangle]
pub extern "C" fn PyUnstable_Eval_RequestCodeExtraIndex(_free: *mut c_void) -> c_int {
    CODE_EXTRA_NEXT.fetch_add(1, Ordering::Relaxed) as c_int
}

/// `PyUnstable_Code_GetExtra(code, index, &extra)` — read an extra slot
/// (NULL when never set; 0 on success).
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_Code_GetExtra(
    code: *mut PyObject,
    index: PySsizeT,
    extra: *mut *mut c_void,
) -> c_int {
    let v = CODE_EXTRA
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .and_then(|m| m.get(&(code as usize, index as usize)).copied())
        })
        .unwrap_or(0);
    unsafe { *extra = v as *mut c_void };
    0
}

/// `PyUnstable_Code_SetExtra(code, index, extra)` — write an extra slot.
#[no_mangle]
pub unsafe extern "C" fn PyUnstable_Code_SetExtra(
    code: *mut PyObject,
    index: PySsizeT,
    extra: *mut c_void,
) -> c_int {
    let mut g = CODE_EXTRA.lock().unwrap();
    g.get_or_insert_with(HashMap::new)
        .insert((code as usize, index as usize), extra as usize);
    0
}

// ---------------------------------------------------------------------------
// Frame-eval hooks (PEP 523; torch.compile's entry point)
// ---------------------------------------------------------------------------

static EVAL_FRAME_FUNC: AtomicUsize = AtomicUsize::new(0);

/// `_PyEval_EvalFrameDefault(tstate, frame, throwflag)` — the default
/// frame evaluator. The VM's evaluator is not frame-pointer-driven, so a
/// C caller invoking this directly (only dynamo's replaced-frame path
/// does) gets a clean error instead of an interpreter it can't have.
#[no_mangle]
pub extern "C" fn _PyEval_EvalFrameDefault(
    _tstate: *mut c_void,
    _frame: *mut c_void,
    _throwflag: c_int,
) -> *mut PyObject {
    crate::errors::set_runtime_error(
        "_PyEval_EvalFrameDefault: WeavePy frames are not C-evaluable (torch.compile is unsupported)",
    );
    ptr::null_mut()
}

/// `_PyInterpreterState_GetEvalFrameFunc(interp)` — the installed frame
/// evaluator (default when none was set).
#[no_mangle]
pub extern "C" fn _PyInterpreterState_GetEvalFrameFunc(_interp: *mut c_void) -> *mut c_void {
    let f = EVAL_FRAME_FUNC.load(Ordering::Acquire);
    if f != 0 {
        f as *mut c_void
    } else {
        _PyEval_EvalFrameDefault as *mut c_void
    }
}

/// `_PyInterpreterState_SetEvalFrameFunc(interp, func)` — record the
/// caller's evaluator so Get round-trips; the VM never routes frames
/// through it (dynamo is inert, matching the unsupported-toolchain gate).
#[no_mangle]
pub extern "C" fn _PyInterpreterState_SetEvalFrameFunc(_interp: *mut c_void, func: *mut c_void) {
    EVAL_FRAME_FUNC.store(func as usize, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Thread walking (profiler stack sampling)
// ---------------------------------------------------------------------------

/// `PyInterpreterState_ThreadHead(interp)` — head of the interpreter's
/// thread-state list. We report a single-entry list: the calling
/// thread's faithful tstate body.
#[no_mangle]
pub extern "C" fn PyInterpreterState_ThreadHead(_interp: *mut c_void) -> *mut c_void {
    crate::pystate::current_threadstate() as *mut c_void
}

/// `PyThreadState_Next(tstate)` — next tstate in the list (end: NULL).
#[no_mangle]
pub extern "C" fn PyThreadState_Next(_tstate: *mut c_void) -> *mut c_void {
    ptr::null_mut()
}

/// `_PyThreadState_GetCurrent()` — the current thread state (internal
/// spelling of `PyThreadState_GetUnchecked`).
#[no_mangle]
pub extern "C" fn _PyThreadState_GetCurrent() -> *mut c_void {
    crate::pystate::current_threadstate() as *mut c_void
}

// ---------------------------------------------------------------------------
// Arena allocator introspection
// ---------------------------------------------------------------------------

/// `PyObject_GetArenaAllocator(allocator)` — obmalloc arena hooks. The VM
/// does not use CPython's arena allocator; report the all-NULL default
/// (a valid state CPython itself starts from).
#[no_mangle]
pub unsafe extern "C" fn PyObject_GetArenaAllocator(allocator: *mut c_void) {
    if !allocator.is_null() {
        // struct { void *ctx; void *(*alloc)(...); void (*free)(...); }
        unsafe { ptr::write_bytes(allocator as *mut u8, 0, 3 * std::mem::size_of::<usize>()) };
    }
}

// ---------------------------------------------------------------------------
// Struct sequences (torch.return_types.*)
// ---------------------------------------------------------------------------

/// Layout matching `PyStructSequence_Field` in `Python.h`.
#[repr(C)]
pub struct PyStructSequenceField {
    pub name: *const c_char,
    pub doc: *const c_char,
}

/// Layout matching `PyStructSequence_Desc` in `Python.h`.
#[repr(C)]
pub struct PyStructSequenceDesc {
    pub name: *const c_char,
    pub doc: *const c_char,
    pub fields: *mut PyStructSequenceField,
    pub n_in_sequence: c_int,
}

/// `n_in_sequence` per readied struct-sequence type, keyed by the
/// extension's static `PyTypeObject` address (CPython stashes this in
/// the type dict; a side table is equivalent for `New`'s allocation).
static STRUCTSEQ_NFIELDS: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);

/// `PyStructSequence_InitType(type, desc)` — initialise the extension's
/// *static* `PyTypeObject` as a named-tuple-like type: tuple-shaped
/// variable layout (`ob_item` right after `PyVarObject`), one readonly
/// `T_OBJECT_EX` member per named field, then the ordinary
/// [`crate::types::PyType_Ready`] bridge (which harvests `tp_members`
/// into VM-visible descriptors). torch mints every
/// `torch.return_types.*` through here at import.
#[no_mangle]
pub unsafe extern "C" fn PyStructSequence_InitType(
    ty: *mut PyTypeObject,
    desc: *mut PyStructSequenceDesc,
) {
    unsafe { PyStructSequence_InitType2(ty, desc) };
}

/// `PyStructSequence_InitType2` — the checked variant (0 / -1).
#[no_mangle]
pub unsafe extern "C" fn PyStructSequence_InitType2(
    ty: *mut PyTypeObject,
    desc: *mut PyStructSequenceDesc,
) -> c_int {
    if ty.is_null() || desc.is_null() {
        crate::errors::set_type_error("PyStructSequence_InitType: NULL");
        return -1;
    }
    crate::interp::ensure_initialised();
    let item_base = std::mem::size_of::<crate::layout::PyVarObject>();
    let ptr_size = std::mem::size_of::<usize>();

    // Named members from the desc's NULL-terminated field array. Unnamed
    // fields (`PyStructSequence_UnnamedField`) participate in the
    // sequence but get no attribute; torch doesn't use them.
    let mut members: Vec<crate::getset::PyMemberDef> = Vec::new();
    let mut n_fields = 0usize;
    unsafe {
        let mut f = (*desc).fields;
        while !f.is_null() && !(*f).name.is_null() {
            members.push(crate::getset::PyMemberDef {
                name: (*f).name,
                ty: crate::getset::member_types::T_OBJECT_EX,
                offset: (item_base + n_fields * ptr_size) as PySsizeT,
                flags: crate::getset::READONLY,
                doc: (*f).doc,
            });
            n_fields += 1;
            f = f.add(1);
        }
        members.push(crate::getset::PyMemberDef {
            name: ptr::null(),
            ty: 0,
            offset: 0,
            flags: 0,
            doc: ptr::null(),
        });

        let t = &mut *ty;
        t.head.ob_refcnt = IMMORTAL_REFCNT;
        t.head.ob_type = crate::types::PyType_Type.as_ptr();
        t.tp_name = (*desc).name;
        t.tp_doc = (*desc).doc;
        t.tp_basicsize = item_base as PySsizeT;
        t.tp_itemsize = ptr_size as PySsizeT;
        t.tp_flags = crate::layout::tpflags::DEFAULT;
        t.tp_members = Box::into_raw(members.into_boxed_slice()) as *mut c_void;
    }

    let visible = unsafe { (*desc).n_in_sequence.max(0) } as usize;
    STRUCTSEQ_NFIELDS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(ty as usize, n_fields.max(visible));

    unsafe { crate::types::PyType_Ready(ty) }
}

/// `PyStructSequence_New(type)` — a fresh, zero-filled instance sized
/// for the type's field count.
#[no_mangle]
pub unsafe extern "C" fn PyStructSequence_New(ty: *mut PyTypeObject) -> *mut PyObject {
    let n = STRUCTSEQ_NFIELDS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&(ty as usize)).copied()))
        .unwrap_or(0);
    unsafe { crate::genericalloc::PyType_GenericAlloc(ty, n as PySsizeT) }
}

/// `PyStructSequence_SetItem(op, i, v)` — write a field (steals `v`;
/// no bounds/error reporting, exactly the CPython macro contract).
#[no_mangle]
pub unsafe extern "C" fn PyStructSequence_SetItem(
    op: *mut PyObject,
    i: PySsizeT,
    v: *mut PyObject,
) {
    let base = unsafe { (op as *mut u8).add(std::mem::size_of::<crate::layout::PyVarObject>()) }
        as *mut *mut PyObject;
    unsafe { *base.offset(i) = v };
}

/// `PyStructSequence_GetItem(op, i)` — read a field (borrowed).
#[no_mangle]
pub unsafe extern "C" fn PyStructSequence_GetItem(op: *mut PyObject, i: PySsizeT) -> *mut PyObject {
    let base = unsafe { (op as *const u8).add(std::mem::size_of::<crate::layout::PyVarObject>()) }
        as *const *mut PyObject;
    unsafe { *base.offset(i) }
}
