//! greenlet C-API surface (RFC 0072 WS1).
//!
//! Upstream greenlet exposes a 12-slot function-pointer table through a
//! capsule named `"greenlet._C_API"` (attached to the C extension module
//! `greenlet._greenlet` and re-exported as `greenlet._C_API`; the
//! `PyGreenlet_Import()` macro in `greenlet.h` imports the
//! backwards-compatible `greenlet._C_API` path). gevent's compiled Cython
//! modules bind that table — `PyGreenlet_GetCurrent`, `PyGreenlet_Switch`
//! — and additionally `__Pyx_ImportType("greenlet", "greenlet",
//! sizeof(PyGreenlet), CheckSize_Warn)` the Python-visible class, then
//! subclass it from C (`cdef class TrackedRawGreenlet(greenlet)`), placing
//! their own cdef fields at `sizeof(PyGreenlet)`.
//!
//! ## The shell type
//!
//! `sizeof(PyGreenlet)` is part of upstream's ABI:
//!
//! ```c
//! typedef struct _greenlet {
//!     PyObject_HEAD
//!     PyObject* weakreflist;
//!     PyObject* dict;
//!     implementation_ptr_t pimpl;
//! } PyGreenlet;    /* 40 bytes on LP64 */
//! ```
//!
//! A Cython subclass computes its field offsets from that struct, so the
//! generic identity-box mirror (whose `tp_basicsize` is the Rust
//! `PyObjectBox`) cannot be handed out — the subclass's fields would
//! overlap the box payload. Instead this module mints a **byte-faithful
//! shell** `PyTypeObject` (the RFC 0029/0066 datetime-shell discipline):
//! `tp_basicsize = 40`, bridged to the process-global VM `greenlet`
//! class, registered as an inline-instance type so VM greenlets crossing
//! into C get stable 40-byte bodies (the three pointer fields stay
//! zeroed — WeavePy serves `__dict__`/weakrefs through the VM instance,
//! and no consumer reads the opaque `pimpl`).
//!
//! Instance state (the stack, parent chain, status) lives entirely in
//! `weavepy_vm::stdlib::greenlet_native`'s per-thread registry keyed by
//! the `_greenlet_id` in the instance dict, so a C-allocated subclass
//! instance (Cython `tp_alloc`) becomes a real greenlet the moment the
//! chained `greenlet.__init__` runs — no C-side state to keep coherent.

use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use weavepy_vm::object::Object;
use weavepy_vm::stdlib::greenlet_native as green;
use weavepy_vm::sync::Rc;
use weavepy_vm::types::TypeObject;

use crate::layout::tpflags;
use crate::object::{PyObject, PySsizeT};
use crate::types::PyTypeObject;
use std::ffi::c_void;

/// `sizeof(PyGreenlet)` on LP64 per upstream `greenlet.h`:
/// `PyObject_HEAD` (16) + `weakreflist` + `dict` + `pimpl`.
const SIZE_GREENLET: PySsizeT = 40;

/// Slot count and order per upstream `greenlet.h` (`PyGreenlet_API_pointers`).
const API_POINTERS: usize = 12;

/// The minted `PyGreenlet` shell (as `usize`; 0 = not yet minted).
static PTR_GREENLET: AtomicUsize = AtomicUsize::new(0);
/// The leaked 12-slot `void*[]` the capsule wraps.
static API_TABLE: AtomicUsize = AtomicUsize::new(0);
/// Mint lock: at most one thread builds the shell + table.
static INIT_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------
// The shell type.
// ---------------------------------------------------------------------

/// `tp_new` for the shell: allocate through the generic path (which
/// mints the faithful inline body *and* the VM `PyInstance` soul). A
/// Cython subclass's generated `__pyx_tp_new` calls this slot directly
/// with the **subclass** type, so the allocation is subclass-sized and
/// the cdef fields at offset 40+ start zeroed. Initialisation is
/// `__init__`'s job (Cython chains `greenlet.__init__(self, ...)`,
/// which registers the `_greenlet_id` body) — greenlet has no C-visible
/// state bytes to bake here, unlike the datetime shells.
unsafe extern "C" fn greenlet_tp_new(
    type_: *mut PyTypeObject,
    _args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    unsafe { crate::genericalloc::PyType_GenericAlloc(type_, 0) }
}

/// Idempotently mint the `PyGreenlet` shell: faithful `tp_basicsize`,
/// `BASETYPE`, bridged to the process-global VM `greenlet` class (the
/// socket-pattern class in `greenlet_native` — one class per process,
/// so unlike the datetime shells no per-interpreter re-wiring is
/// needed). Registered in the heap-type registry (so `bridge_type` /
/// `find_type_ptr` resolve both directions) and as an inline-instance
/// type (so VM greenlets crossing into C get stable 40-byte bodies).
pub fn ensure_greenlet_type() -> *mut PyTypeObject {
    let existing = PTR_GREENLET.load(Ordering::Acquire);
    if existing != 0 {
        return existing as *mut PyTypeObject;
    }
    let _guard = INIT_LOCK.lock();
    let existing = PTR_GREENLET.load(Ordering::Acquire);
    if existing != 0 {
        return existing as *mut PyTypeObject;
    }
    let cname = CString::new("greenlet.greenlet").expect("static name");
    let mut ty = PyTypeObject::new_zeroed();
    ty.head.ob_type = crate::types::PyType_Type.as_ptr();
    ty.tp_name = cname.into_raw();
    ty.tp_basicsize = SIZE_GREENLET;
    ty.tp_itemsize = 0;
    ty.tp_dealloc = Some(crate::object::_PyWeavePy_Dealloc);
    ty.tp_flags = tpflags::DEFAULT | tpflags::BASETYPE | tpflags::READY;
    ty.tp_base = crate::types::PyBaseObject_Type.as_ptr();
    ty.tp_new = greenlet_tp_new as *mut c_void;
    ty.bridge = Box::into_raw(Box::new(green::capi_class()));
    let p = Box::into_raw(Box::new(ty));
    crate::types::register_heap_type(p);
    crate::types::maybe_register_inline_type(p);
    PTR_GREENLET.store(p as usize, Ordering::Release);
    p
}

/// The faithful C type for the VM `greenlet` class — [`crate::types`]'
/// `find_type_ptr` hook (the datetime `faithful_type_for_class`
/// discipline). Identity-checked against the process-global class, so a
/// user class that merely shares the name falls through to the generic
/// registry scan. Must win over the generic `install_user_type` mirror:
/// the mirror's `tp_basicsize` is the identity box, and a Cython
/// subclass computing field offsets from it would corrupt memory.
pub fn faithful_type_for_class(t: &Rc<TypeObject>) -> Option<*mut PyTypeObject> {
    if t.name != "greenlet" {
        return None;
    }
    if !Rc::ptr_eq(t, &green::capi_class()) {
        return None;
    }
    Some(ensure_greenlet_type())
}

// ---------------------------------------------------------------------
// The 12 table entries (upstream `CObjects.cpp` semantics).
// ---------------------------------------------------------------------

fn set_bad_argument() {
    crate::errors::set_type_error("bad argument type for built-in operation");
}

/// Decode a `PyGreenlet*` argument: a live greenlet instance (any
/// subclass, by MRO — the same test the Python-level methods use).
/// Errors like upstream's `PyGreenlet_Check` failure: `PyErr_BadArgument`.
unsafe fn greenlet_arg(g: *mut PyObject) -> Option<Object> {
    if g.is_null() {
        set_bad_argument();
        return None;
    }
    let o = unsafe { crate::object::clone_object(g) };
    if !green::capi_is_greenlet(&o) {
        set_bad_argument();
        return None;
    }
    Some(o)
}

fn out_object(res: Result<Object, weavepy_vm::error::RuntimeError>) -> *mut PyObject {
    match res {
        Ok(v) => crate::object::into_owned(v),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
    }
}

/// Slot 3: `PyGreenlet* PyGreenlet_New(PyObject* run, PyGreenlet* parent)`.
unsafe extern "C" fn c_greenlet_new(run: *mut PyObject, parent: *mut PyObject) -> *mut PyObject {
    let run_o = if run.is_null() {
        None
    } else {
        Some(unsafe { crate::object::clone_object(run) })
    };
    let parent_o = if parent.is_null() {
        None
    } else {
        Some(unsafe { crate::object::clone_object(parent) })
    };
    out_object(green::capi_new(run_o, parent_o))
}

/// Slot 4: `PyGreenlet* PyGreenlet_GetCurrent(void)`.
unsafe extern "C" fn c_greenlet_getcurrent() -> *mut PyObject {
    out_object(green::capi_getcurrent())
}

/// Slot 5: `PyObject* PyGreenlet_Throw(PyGreenlet*, typ, val, tb)`.
unsafe extern "C" fn c_greenlet_throw(
    g: *mut PyObject,
    typ: *mut PyObject,
    val: *mut PyObject,
    tb: *mut PyObject,
) -> *mut PyObject {
    let Some(target) = (unsafe { greenlet_arg(g) }) else {
        return ptr::null_mut();
    };
    let conv = |p: *mut PyObject| {
        if p.is_null() {
            Object::None
        } else {
            unsafe { crate::object::clone_object(p) }
        }
    };
    out_object(green::capi_throw(&target, conv(typ), conv(val), conv(tb)))
}

/// Slot 6: `PyObject* PyGreenlet_Switch(PyGreenlet*, args, kwargs)`.
/// Calls the **base** switch implementation regardless of subclass
/// overrides (upstream calls `green_switch` directly; gevent's
/// `SwitchOutGreenletWithLoop.switch` depends on this to avoid
/// re-entering itself).
unsafe extern "C" fn c_greenlet_switch(
    g: *mut PyObject,
    args: *mut PyObject,
    kwargs: *mut PyObject,
) -> *mut PyObject {
    let Some(target) = (unsafe { greenlet_arg(g) }) else {
        return ptr::null_mut();
    };
    let args_v: Vec<Object> = if args.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::object::clone_object(args) } {
            Object::Tuple(items) => items.iter().cloned().collect(),
            Object::None => Vec::new(),
            other => vec![other],
        }
    };
    // Upstream: a non-dict kwargs is treated as absent.
    let kw: Vec<(String, Object)> = if kwargs.is_null() {
        Vec::new()
    } else {
        match unsafe { crate::object::clone_object(kwargs) } {
            Object::Dict(d) => d
                .borrow()
                .iter()
                .filter_map(|(k, v)| match &k.0 {
                    Object::Str(s) => Some((s.to_string(), v.clone())),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    };
    out_object(green::capi_switch(&target, args_v, kw))
}

/// Slot 7: `int PyGreenlet_SetParent(PyGreenlet*, PyGreenlet* nparent)`.
unsafe extern "C" fn c_greenlet_setparent(g: *mut PyObject, nparent: *mut PyObject) -> c_int {
    let Some(target) = (unsafe { greenlet_arg(g) }) else {
        return -1;
    };
    let parent = if nparent.is_null() {
        Object::None
    } else {
        unsafe { crate::object::clone_object(nparent) }
    };
    match green::capi_set_parent(&target, parent) {
        Ok(()) => 0,
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            -1
        }
    }
}

/// Slots 8–10: the `MAIN`/`STARTED`/`ACTIVE` predicates (functions
/// through the table since greenlet 2.x; `-1` + `PyErr_BadArgument` on
/// a non-greenlet, matching upstream).
unsafe extern "C" fn c_greenlet_main(g: *mut PyObject) -> c_int {
    let Some(target) = (unsafe { greenlet_arg(g) }) else {
        return -1;
    };
    match green::capi_is_main(&target) {
        Ok(b) => c_int::from(b),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            -1
        }
    }
}

unsafe extern "C" fn c_greenlet_started(g: *mut PyObject) -> c_int {
    let Some(target) = (unsafe { greenlet_arg(g) }) else {
        return -1;
    };
    match green::capi_is_started(&target) {
        Ok(b) => c_int::from(b),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            -1
        }
    }
}

unsafe extern "C" fn c_greenlet_active(g: *mut PyObject) -> c_int {
    let Some(target) = (unsafe { greenlet_arg(g) }) else {
        return -1;
    };
    match green::capi_is_active(&target) {
        Ok(b) => c_int::from(b),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            -1
        }
    }
}

/// Slot 11: `PyGreenlet* PyGreenlet_GetParent(PyGreenlet*)` — a new
/// reference, or NULL **without an exception** when there is no parent
/// (main), exactly as upstream documents.
unsafe extern "C" fn c_greenlet_getparent(g: *mut PyObject) -> *mut PyObject {
    let Some(target) = (unsafe { greenlet_arg(g) }) else {
        return ptr::null_mut();
    };
    match green::capi_get_parent(&target) {
        Ok(Object::None) => ptr::null_mut(),
        Ok(v) => crate::object::into_owned(v),
        Err(e) => {
            crate::errors::set_pending_from_runtime(e);
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------
// The capsule payload.
// ---------------------------------------------------------------------

/// The `void**` the `greenlet._C_API` capsule wraps: the 12-slot table
/// in upstream index order, minted once and leaked (extensions keep the
/// pointer for the life of the process).
pub fn capi_table_ptr() -> *mut c_void {
    let existing = API_TABLE.load(Ordering::Acquire);
    if existing != 0 {
        return existing as *mut c_void;
    }
    let shell = ensure_greenlet_type();
    let _guard = INIT_LOCK.lock();
    let existing = API_TABLE.load(Ordering::Acquire);
    if existing != 0 {
        return existing as *mut c_void;
    }
    let exc_error = crate::object::into_owned(Object::Type(green::capi_error_class()));
    let exc_exit = crate::object::into_owned(Object::Type(green::capi_exit_class()));
    let table: Box<[*mut c_void; API_POINTERS]> = Box::new([
        shell as *mut c_void,                 // PyGreenlet_Type_NUM 0
        exc_error as *mut c_void,             // PyExc_GreenletError_NUM 1
        exc_exit as *mut c_void,              // PyExc_GreenletExit_NUM 2
        c_greenlet_new as *mut c_void,        // PyGreenlet_New_NUM 3
        c_greenlet_getcurrent as *mut c_void, // PyGreenlet_GetCurrent_NUM 4
        c_greenlet_throw as *mut c_void,      // PyGreenlet_Throw_NUM 5
        c_greenlet_switch as *mut c_void,     // PyGreenlet_Switch_NUM 6
        c_greenlet_setparent as *mut c_void,  // PyGreenlet_SetParent_NUM 7
        c_greenlet_main as *mut c_void,       // PyGreenlet_MAIN_NUM 8
        c_greenlet_started as *mut c_void,    // PyGreenlet_STARTED_NUM 9
        c_greenlet_active as *mut c_void,     // PyGreenlet_ACTIVE_NUM 10
        c_greenlet_getparent as *mut c_void,  // PyGreenlet_GET_PARENT_NUM 11
    ]);
    let p = Box::into_raw(table) as *mut c_void;
    API_TABLE.store(p as usize, Ordering::Release);
    p
}
