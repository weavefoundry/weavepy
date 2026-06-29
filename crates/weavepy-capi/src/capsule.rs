//! `PyCapsule` — opaque pointer wrapper used by extensions to
//! publish C-level helpers to other extensions.
//!
//! ## Why we care
//!
//! `PyCapsule` is the lifeline by which a *consumer* extension
//! (e.g. `scipy._lib._ccallback`) reaches into a *producer*
//! extension's C surface (e.g. `numpy.core.multiarray`'s
//! `_ARRAY_API` table). Without it the entire numpy / scipy /
//! pandas / pyarrow / matplotlib stack falls apart because
//! every one of them publishes its low-level vtable as a
//! capsule named `pkg._sub._API`. See RFC 0029 §3.2 for the
//! larger picture.
//!
//! ## Public surface (CPython 3.13)
//!
//! - **Constructor**: [`PyCapsule_New`].
//! - **Predicates / accessors**: [`PyCapsule_IsValid`],
//!   [`PyCapsule_GetPointer`], [`PyCapsule_GetName`],
//!   [`PyCapsule_GetDestructor`], [`PyCapsule_GetContext`].
//! - **Mutators**: [`PyCapsule_SetPointer`],
//!   [`PyCapsule_SetName`], [`PyCapsule_SetDestructor`],
//!   [`PyCapsule_SetContext`].
//! - **Import**: [`PyCapsule_Import`] — the workhorse the
//!   consumer side uses. Re-implemented from CPython exactly to
//!   preserve the dotted-name → import-and-fetch behaviour
//!   numpy / scipy rely on.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::ptr;

use weavepy_vm::object::{Object, PyCapsuleSoul};
use weavepy_vm::sync::Rc;

use crate::object::{PyObject, PyObjectBox};

#[repr(C)]
struct CapsuleState {
    pointer: *mut std::ffi::c_void,
    name: Option<Box<[u8]>>,
    /// Opaque context pointer set via `PyCapsule_SetContext`.
    /// Used by some numpy ufuncs that stash a vtable + per-context
    /// state in the same capsule.
    context: *mut std::ffi::c_void,
    /// Capsule destructor (run when the refcount drops to zero).
    /// CPython lets capsules carry destructors so that producer
    /// modules can free state owned by the capsule when nothing
    /// references it any more.
    destructor: Option<CapsuleDestructor>,
}

type CapsuleDestructor = unsafe extern "C" fn(*mut PyObject);

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_New(
    pointer: *mut std::ffi::c_void,
    name: *const c_char,
    destructor: Option<CapsuleDestructor>,
) -> *mut PyObject {
    crate::interp::ensure_initialised();
    let name_owned: Option<Box<[u8]>> = if name.is_null() {
        None
    } else {
        let bytes: Vec<u8> = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul().to_vec();
        Some(bytes.into_boxed_slice())
    };
    let state = Box::new(CapsuleState {
        pointer,
        name: name_owned,
        context: ptr::null_mut(),
        destructor,
    });
    let user_data = Box::into_raw(state) as *mut std::ffi::c_void;
    let bx = Box::new(PyObjectBox {
        head: PyObject {
            ob_refcnt: 1,
            ob_type: crate::types::PyCapsule_Type.as_ptr(),
        },
        payload: crate::object::PayloadCell {
            obj: Object::None,
            user_data,
            destructor,
        },
    });
    let raw = Box::into_raw(bx) as *mut PyObject;
    crate::object::register_minted(raw);
    raw
}

fn capsule_state(p: *mut PyObject) -> Option<*mut CapsuleState> {
    if p.is_null() {
        return None;
    }
    let head = unsafe { &*p };
    if !std::ptr::eq(head.ob_type, crate::types::PyCapsule_Type.as_ptr()) {
        return None;
    }
    let bx = unsafe { &*(p as *const PyObjectBox) };
    Some(bx.payload.user_data as *mut CapsuleState)
}

// ---------------------------------------------------------------------------
// Capsule <-> VM round-trip (RFC 0045, wave 3)
//
// A capsule is a legacy `PyObjectBox` whose *identity* is the box pointer and
// whose state (the wrapped `void*`, name, …) lives in `user_data` — its
// `payload.obj` is `Object::None`. That made it collapse to `None` the moment
// it crossed into the VM (a module dict / attribute), breaking the load-bearing
// `import_array()` idiom: `PyModule_AddObject(m, "_API", capsule)` stored
// `None`, so a later `PyCapsule_Import(...)` fetched a non-capsule and failed.
//
// The fix mirrors wave 3's stable-instance-body design: the capsule keeps its
// box, but the VM holds an identity-stable [`Object::Capsule`] *soul* that maps
// back to the **same** box. The soul retains one C reference on the box for its
// whole life, hands that same pointer back out on each crossing, and releases
// the retain when the last soul drops (the [`capsule_soul_free`] hook).
// ---------------------------------------------------------------------------

thread_local! {
    /// Dedup map `capsule box pointer -> Weak<soul>`. Lets a capsule that
    /// crosses into the VM more than once resolve to the *same* soul (so
    /// `cap is cap` holds and exactly one VM-side retain is kept). Advisory:
    /// a missing/stale entry only costs an extra soul, never correctness —
    /// each soul independently balances its own retain.
    static CAPSULE_SOULS: RefCell<HashMap<usize, weavepy_vm::sync::Weak<PyCapsuleSoul>>> =
        RefCell::new(HashMap::new());
}

/// Install the VM hook that releases a capsule's retained box when its
/// VM-side [`PyCapsuleSoul`] drops (RFC 0045, wave 3). Idempotent; called
/// from [`crate::interp::ensure_initialised`].
pub fn install() {
    weavepy_vm::object::register_capsule_free(capsule_soul_free);
}

/// True if `p` is a live capsule box (its `ob_type` is `PyCapsule_Type`).
pub fn is_capsule(p: *mut PyObject) -> bool {
    capsule_state(p).is_some()
}

/// Resolve a capsule box crossing into the VM to its identity-stable
/// [`Object::Capsule`] soul (RFC 0045). On the **first** crossing the box
/// is retained once (the soul's lifelong reference) and registered; later
/// crossings of the same box return the same soul without a new retain.
///
/// # Safety
/// `p` must be a live capsule box ([`is_capsule`]).
pub unsafe fn capsule_soul(p: *mut PyObject) -> Object {
    let key = p as usize;
    if let Some(existing) = CAPSULE_SOULS.with(|m| {
        m.borrow()
            .get(&key)
            .and_then(weavepy_vm::sync::Weak::upgrade)
    }) {
        return Object::Capsule(existing);
    }
    // First crossing: read the name for `repr`, take the soul's lifelong
    // retain on the box, and register the soul for dedup.
    let name = capsule_state(p).and_then(|s| unsafe {
        (*s).name.as_deref().map(|bytes| {
            let text = CStr::from_bytes_with_nul(bytes)
                .ok()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
            weavepy_vm::sync::Rc::<str>::from(text.as_str())
        })
    });
    unsafe { crate::object::Py_IncRef(p) };
    let soul = Rc::new(PyCapsuleSoul { name, handle: key });
    CAPSULE_SOULS.with(|m| m.borrow_mut().insert(key, Rc::downgrade(&soul)));
    Object::Capsule(soul)
}

/// Hand a capsule soul back to C as its original box (RFC 0045): bump the
/// box's C refcount and return the same pointer. The box is guaranteed
/// live — the soul holds a retain on it for as long as it exists.
pub fn capsule_box_from_soul(soul: &Rc<PyCapsuleSoul>) -> *mut PyObject {
    let p = soul.handle as *mut PyObject;
    unsafe { crate::object::Py_IncRef(p) };
    p
}

/// VM hook (registered by [`install`]): the last [`PyCapsuleSoul`] for a
/// capsule has dropped, so release the lifelong retain its box was holding.
/// Drops the dedup entry first, then decrefs — the decref may reach zero and
/// free the box (running any `PyCapsule` destructor), which must not see a
/// borrow of [`CAPSULE_SOULS`] held.
fn capsule_soul_free(handle: usize) {
    if handle == 0 {
        return;
    }
    // Best-effort dedup cleanup. Advisory only: removing the wrong entry (a
    // pathological cross-thread reuse of the same address) costs at most an
    // extra soul later, never a refcount imbalance.
    CAPSULE_SOULS.with(|m| {
        m.borrow_mut().remove(&handle);
    });
    unsafe { crate::object::Py_DecRef(handle as *mut PyObject) };
}

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_GetPointer(
    capsule: *mut PyObject,
    name: *const c_char,
) -> *mut std::ffi::c_void {
    let Some(state_ptr) = capsule_state(capsule) else {
        crate::errors::set_value_error("PyCapsule_GetPointer: not a capsule");
        return ptr::null_mut();
    };
    let state = unsafe { &*state_ptr };
    if !name.is_null() {
        let want = unsafe { CStr::from_ptr(name) }.to_bytes();
        let have = state.name.as_deref().unwrap_or(&[]);
        if have.split_last().map(|(_, h)| h) != Some(want) {
            crate::errors::set_value_error("PyCapsule_GetPointer: name mismatch");
            return ptr::null_mut();
        }
    }
    state.pointer
}

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_GetName(capsule: *mut PyObject) -> *const c_char {
    let Some(state_ptr) = capsule_state(capsule) else {
        return ptr::null();
    };
    let state = unsafe { &*state_ptr };
    state
        .name
        .as_deref()
        .map_or(ptr::null(), |s| s.as_ptr() as *const c_char)
}

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_IsValid(capsule: *mut PyObject, name: *const c_char) -> c_int {
    let Some(state_ptr) = capsule_state(capsule) else {
        return 0;
    };
    if name.is_null() {
        return 1;
    }
    let state = unsafe { &*state_ptr };
    let want = unsafe { CStr::from_ptr(name) }.to_bytes();
    let have = state.name.as_deref().unwrap_or(&[]);
    i32::from(have.split_last().map(|(_, h)| h) == Some(want))
}

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_SetPointer(
    capsule: *mut PyObject,
    pointer: *mut std::ffi::c_void,
) -> c_int {
    let Some(state_ptr) = capsule_state(capsule) else {
        crate::errors::set_value_error("PyCapsule_SetPointer: not a capsule");
        return -1;
    };
    if pointer.is_null() {
        crate::errors::set_value_error("PyCapsule_SetPointer: pointer is NULL");
        return -1;
    }
    unsafe { (*state_ptr).pointer = pointer };
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_SetName(capsule: *mut PyObject, name: *const c_char) -> c_int {
    let Some(state_ptr) = capsule_state(capsule) else {
        crate::errors::set_value_error("PyCapsule_SetName: not a capsule");
        return -1;
    };
    let new_name: Option<Box<[u8]>> = if name.is_null() {
        None
    } else {
        let bytes: Vec<u8> = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul().to_vec();
        Some(bytes.into_boxed_slice())
    };
    unsafe { (*state_ptr).name = new_name };
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_GetDestructor(
    capsule: *mut PyObject,
) -> Option<CapsuleDestructor> {
    let state_ptr = capsule_state(capsule)?;
    unsafe { (*state_ptr).destructor }
}

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_SetDestructor(
    capsule: *mut PyObject,
    destructor: Option<CapsuleDestructor>,
) -> c_int {
    let Some(state_ptr) = capsule_state(capsule) else {
        crate::errors::set_value_error("PyCapsule_SetDestructor: not a capsule");
        return -1;
    };
    unsafe { (*state_ptr).destructor = destructor };
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_GetContext(capsule: *mut PyObject) -> *mut std::ffi::c_void {
    let Some(state_ptr) = capsule_state(capsule) else {
        crate::errors::set_value_error("PyCapsule_GetContext: not a capsule");
        return ptr::null_mut();
    };
    unsafe { (*state_ptr).context }
}

#[no_mangle]
pub unsafe extern "C" fn PyCapsule_SetContext(
    capsule: *mut PyObject,
    context: *mut std::ffi::c_void,
) -> c_int {
    let Some(state_ptr) = capsule_state(capsule) else {
        crate::errors::set_value_error("PyCapsule_SetContext: not a capsule");
        return -1;
    };
    unsafe { (*state_ptr).context = context };
    0
}

/// `PyCapsule_Import(name, no_block)` — fetch the named capsule.
///
/// CPython's implementation:
/// 1. Splits `name` into dotted components.
/// 2. Imports the head, then walks attribute lookups for each
///    subsequent component.
/// 3. Verifies the final attribute is a capsule whose **own** name
///    matches `name` (so extensions can't accidentally grab a
///    different capsule with the wrong layout).
/// 4. Returns the pointer.
///
/// We reproduce this exactly: numpy's
/// `import_array()` macro expands to `PyCapsule_Import("numpy.core.multiarray._ARRAY_API", 0)`
/// and won't accept any deviation.
///
/// `no_block` is accepted but ignored; the underlying lock is
/// per-process and stale ABIs are caught by the name check.
#[no_mangle]
pub unsafe extern "C" fn PyCapsule_Import(
    name: *const c_char,
    _no_block: c_int,
) -> *mut std::ffi::c_void {
    if name.is_null() {
        crate::errors::set_value_error("PyCapsule_Import: name is NULL");
        return ptr::null_mut();
    }
    let dotted = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => {
            crate::errors::set_value_error("PyCapsule_Import: invalid UTF-8 in name");
            return ptr::null_mut();
        }
    };
    let parts: Vec<&str> = dotted.split('.').collect();
    if parts.is_empty() {
        crate::errors::set_value_error("PyCapsule_Import: empty name");
        return ptr::null_mut();
    }

    // Step 1: walk longest-prefix module loads, then fall back to
    // attribute lookups for the remainder. This matches CPython's
    // implementation in `_PyImport_LookUpAttrFromName`.
    let mut object_ptr: *mut PyObject = ptr::null_mut();
    let mut consumed = 0usize;
    for i in (1..=parts.len()).rev() {
        let prefix = parts[..i].join(".");
        let c_prefix = match CString::new(prefix) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let module = unsafe { crate::module::PyImport_ImportModule(c_prefix.as_ptr()) };
        if !module.is_null() {
            object_ptr = module;
            consumed = i;
            break;
        }
        unsafe { crate::errors::PyErr_Clear() };
    }
    if object_ptr.is_null() {
        let msg = format!("PyCapsule_Import: could not import module \"{dotted}\"");
        crate::errors::set_pending(
            Some(
                weavepy_vm::builtin_types::builtin_types()
                    .import_error
                    .clone(),
            ),
            Object::from_str(msg),
        );
        return ptr::null_mut();
    }

    // Step 2: walk remaining attributes.
    for attr in &parts[consumed..] {
        let c_attr = match CString::new(*attr) {
            Ok(s) => s,
            Err(_) => {
                unsafe { crate::object::Py_DecRef(object_ptr) };
                crate::errors::set_attribute_error(format!(
                    "PyCapsule_Import: bad attribute name \"{attr}\""
                ));
                return ptr::null_mut();
            }
        };
        let next = unsafe { crate::abstract_::PyObject_GetAttrString(object_ptr, c_attr.as_ptr()) };
        if next.is_null() {
            // RFC 0029: built-in C-API capsules (e.g.
            // `datetime.datetime_CAPI`, `numpy.core.multiarray._ARRAY_API`)
            // are lazily installed onto their modules the first
            // time a downstream extension tries to import them.
            // Give the well-known list a shot before failing.
            unsafe { crate::errors::PyErr_Clear() };
            if let Some(c) = try_install_well_known_capsule(&dotted, object_ptr) {
                unsafe { crate::object::Py_DecRef(object_ptr) };
                object_ptr = c;
                continue;
            }
            unsafe { crate::object::Py_DecRef(object_ptr) };
            return ptr::null_mut();
        }
        unsafe { crate::object::Py_DecRef(object_ptr) };
        object_ptr = next;
    }

    // Step 3: verify the capsule's stored name matches and return.
    let cname = match CString::new(dotted.clone()) {
        Ok(s) => s,
        Err(_) => {
            unsafe { crate::object::Py_DecRef(object_ptr) };
            return ptr::null_mut();
        }
    };
    let p = unsafe { PyCapsule_GetPointer(object_ptr, cname.as_ptr()) };
    unsafe { crate::object::Py_DecRef(object_ptr) };
    p
}

/// Lazy registry of the canonical "shipped with WeavePy" capsules.
///
/// CPython initialises these in each owning module's `module_exec`
/// (e.g. `_datetime_exec` calls `PyModule_AddObject(m,
/// "datetime_CAPI", capsule)`). We don't run that init because our
/// `datetime` module is frozen Python on top of the `_datetime`
/// builtin, so we materialise the capsule the first time anyone
/// asks via [`PyCapsule_Import`].
///
/// Returns a *fresh owned* reference to the capsule (with the
/// caller responsible for decref'ing) or `None` when `dotted` is
/// not a known well-known capsule path. On success the capsule is
/// also stashed onto `parent_module`'s dict so subsequent imports
/// hit the fast path.
fn try_install_well_known_capsule(
    dotted: &str,
    parent_module: *mut PyObject,
) -> Option<*mut PyObject> {
    if dotted == "datetime.datetime_CAPI" {
        // RFC 0029 (wave 5): mint the faithful datetime types + dynamic
        // capsule table (size-correct type slots) before publishing, and
        // best-effort fill the `TimeZone_UTC` singleton. Falls back to
        // the static table (NULL type slots) if `datetime` can't be
        // located, which keeps the function-pointer constructors usable.
        crate::datetime_api::ensure_datetime_bridge();
        crate::datetime_api::fill_utc_singleton();
        let name = match CString::new("datetime.datetime_CAPI") {
            Ok(s) => s,
            Err(_) => return None,
        };
        let payload = crate::datetime_api::capi_table_void_ptr();
        let capsule = unsafe { PyCapsule_New(payload, name.as_ptr(), None) };
        if capsule.is_null() {
            return None;
        }
        // Publish on the module dict so we don't repeatedly
        // build new ones.
        let attr = match CString::new("datetime_CAPI") {
            Ok(s) => s,
            Err(_) => return Some(capsule),
        };
        let _ = unsafe {
            crate::abstract_::PyObject_SetAttrString(parent_module, attr.as_ptr(), capsule)
        };
        // Also publish the global pointer for the `PyDateTimeAPI`
        // macro in `Python.h` (the dynamic table when ready, else static).
        unsafe {
            crate::datetime_api::PyDateTimeAPI =
                payload as *mut crate::datetime_api::PyDateTimeCAPI;
        }
        return Some(capsule);
    }
    None
}

/// Force the symbol to remain in the binary even if no internal
/// Rust call site references it. Re-export ensures dynamic
/// extensions can find it via `dlsym`.
pub fn touch() -> [*const std::ffi::c_void; 4] {
    [
        PyCapsule_New as *const _,
        PyCapsule_GetPointer as *const _,
        PyCapsule_Import as *const _,
        PyCapsule_GetContext as *const _,
    ]
}
