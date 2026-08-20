//! RFC 0068 WS3 — native event dispatch for the C-API watcher families
//! (`PyDict_Watch` / `PyType_Watch` / `PyFunction_AddWatcher`), graded by
//! `test_capi.test_watchers`.
//!
//! The registry *bookkeeping* (slot allocation, the exact `ValueError`
//! messages, the per-kind fixture behaviours) lives in the frozen
//! `_weave_capi_misc.py` module, exactly where CPython keeps it in
//! `Modules/_testcapi/watchers.c`. What must be native is the **event
//! plumbing**: the VM's dict/type/function mutation chokepoints fire into
//! a Python-level dispatcher registered here at fixture-module import.
//!
//! Cost discipline: every hook is gated on a relaxed atomic flag that is
//! only raised while at least one object is actually watched (dicts,
//! types) or one watcher is registered (functions). Outside
//! `test_watchers` the flags stay `false` and each hook is a single
//! predictable-branch atomic load.

use crate::object::{DictData, Object};
use crate::sync::{Rc, RefCell};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Any dict currently watched?
static DICTS_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Any type currently watched?
static TYPES_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Any function watcher registered?
static FUNCS_ACTIVE: AtomicBool = AtomicBool::new(false);

/// A watched dict: identity pointer, the bitmask of watcher IDs watching
/// it, and a keep-check `Weak` so a stale entry (the dict died on a path
/// that skipped the dealloc probe) can never fire for an unrelated dict
/// that later reuses the allocation.
struct WatchedDict {
    ptr: usize,
    mask: u8,
    weak: crate::sync::Weak<RefCell<DictData>>,
}

/// A watched type. Holding the type strongly is fine: entries only exist
/// between `watch_type` and `unwatch/clear`, both driven by the fixture.
/// `armed` mirrors CPython's version-tag aggregation: an event fires only
/// while the tag is assigned; modification clears it, a lookup re-assigns.
struct WatchedType {
    obj: Object,
    mask: u8,
    armed: bool,
}

static WATCHED_DICTS: Mutex<Vec<WatchedDict>> = Mutex::new(Vec::new());
static WATCHED_TYPES: Mutex<Vec<WatchedType>> = Mutex::new(Vec::new());

/// Python-level dispatchers (functions in `_weave_capi_misc`), registered
/// once at fixture-module import via `_testinternalcapi`.
static DICT_DISPATCH: Mutex<Option<Object>> = Mutex::new(None);
static TYPE_DISPATCH: Mutex<Option<Object>> = Mutex::new(None);
static FUNC_DISPATCH: Mutex<Option<Object>> = Mutex::new(None);

thread_local! {
    /// Re-entrancy guard: a dispatcher is Python code; if it mutates a
    /// watched object itself we do not re-enter (CPython's C callbacks
    /// cannot re-enter the notify path either — they run after the
    /// version bump that disarms aggregation, and the test callbacks
    /// only append to a list).
    static IN_DISPATCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Raised across a `dict.update()` that fired a single CLONED event
    /// so the per-key inserts below it stay silent, mirroring CPython's
    /// `PyDict_Merge` fast path.
    static SUPPRESS_DICT_EVENTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn set_dispatchers(dict_cb: Object, type_cb: Object, func_cb: Object) {
    *DICT_DISPATCH.lock() = Some(dict_cb);
    *TYPE_DISPATCH.lock() = Some(type_cb);
    *FUNC_DISPATCH.lock() = Some(func_cb);
}

fn call_dispatcher(cb: &Object, args: Vec<Object>) {
    let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() else {
        return;
    };
    // SAFETY: published by an enclosing VM frame on this thread; the
    // same re-entry pattern weakref proxies and coroutine wrappers use.
    let interp = unsafe { &mut *ptr };
    let entered = IN_DISPATCH.with(|f| {
        if f.get() {
            true
        } else {
            f.set(true);
            false
        }
    });
    if entered {
        return;
    }
    let g = interp.builtins_dict();
    let _ = interp.call(cb, &args, &[], &g);
    IN_DISPATCH.with(|f| f.set(false));
}

// ---------------------------------------------------------------- dicts

#[inline(always)]
pub fn dicts_active() -> bool {
    DICTS_ACTIVE.load(Ordering::Relaxed)
}

pub fn watch_dict(watcher_id: u8, d: &Rc<RefCell<DictData>>) {
    let mut w = WATCHED_DICTS.lock();
    let ptr = Rc::as_ptr(d) as usize;
    if let Some(e) = w.iter_mut().find(|e| e.ptr == ptr) {
        e.mask |= 1 << watcher_id;
    } else {
        w.push(WatchedDict {
            ptr,
            mask: 1 << watcher_id,
            weak: Rc::downgrade(d),
        });
    }
    DICTS_ACTIVE.store(true, Ordering::Relaxed);
}

pub fn unwatch_dict(watcher_id: u8, d: &Rc<RefCell<DictData>>) {
    let mut w = WATCHED_DICTS.lock();
    let ptr = Rc::as_ptr(d) as usize;
    if let Some(e) = w.iter_mut().find(|e| e.ptr == ptr) {
        e.mask &= !(1 << watcher_id);
    }
    w.retain(|e| e.mask != 0 && e.weak.upgrade().is_some());
    if w.is_empty() {
        DICTS_ACTIVE.store(false, Ordering::Relaxed);
    }
}

/// A watcher slot was cleared: strip its bit from every watched dict
/// (CPython leaves stale bits and validates at fire time; we validate at
/// clear time so the active flag can drop back to zero).
pub fn clear_dict_watcher_slot(watcher_id: u8) {
    let mut w = WATCHED_DICTS.lock();
    for e in w.iter_mut() {
        e.mask &= !(1 << watcher_id);
    }
    w.retain(|e| e.mask != 0 && e.weak.upgrade().is_some());
    if w.is_empty() {
        DICTS_ACTIVE.store(false, Ordering::Relaxed);
    }
}

fn dict_mask(d: &RefCell<DictData>) -> u8 {
    let ptr = std::ptr::from_ref::<RefCell<DictData>>(d) as usize;
    let w = WATCHED_DICTS.lock();
    w.iter()
        .find(|e| {
            e.ptr == ptr
                && e.weak
                    .upgrade()
                    .is_some_and(|live| Rc::as_ptr(&live) as usize == ptr)
        })
        .map(|e| e.mask)
        .unwrap_or(0)
}

/// Fire a dict watcher event. `event` is CPython's `PyDict_WatchEvent`
/// name suffix: `ADDED`, `MODIFIED`, `DELETED`, `CLONED`, `CLEARED`,
/// `DEALLOCATED`. Call only after the mutation's borrows are released.
pub fn dict_event(
    event: &str,
    d: &RefCell<DictData>,
    key: Option<&Object>,
    value: Option<&Object>,
) {
    if !dicts_active() || SUPPRESS_DICT_EVENTS.with(|f| f.get()) {
        return;
    }
    let mask = dict_mask(d);
    if mask == 0 {
        return;
    }
    let cb = DICT_DISPATCH.lock().clone();
    let Some(cb) = cb else { return };
    let addr = std::ptr::from_ref::<RefCell<DictData>>(d) as usize;
    call_dispatcher(
        &cb,
        vec![
            Object::from_str(event.to_owned()),
            Object::Int(i64::from(mask)),
            Object::Int(addr as i64),
            key.cloned().unwrap_or(Object::None),
            value.cloned().unwrap_or(Object::None),
        ],
    );
}

/// `dict.update()` CLONED fast path: returns `true` (and fires a single
/// CLONED event) when the target is watched, was empty, and the source is
/// a plain dict — the caller must then suppress the per-key events.
#[derive(Debug)]
pub struct SuppressGuard(bool);
impl Drop for SuppressGuard {
    fn drop(&mut self) {
        if self.0 {
            SUPPRESS_DICT_EVENTS.with(|f| f.set(false));
        }
    }
}

pub fn dict_update_begin(target: &Rc<RefCell<DictData>>, target_was_empty: bool) -> SuppressGuard {
    if !dicts_active() {
        return SuppressGuard(false);
    }
    if target_was_empty && dict_mask(target) != 0 {
        dict_event("CLONED", target, None, None);
        SUPPRESS_DICT_EVENTS.with(|f| f.set(true));
        return SuppressGuard(true);
    }
    SuppressGuard(false)
}

/// Dealloc probe: `dropped` is about to lose its last live binding. If it
/// is a watched dict about to die, fire DEALLOCATED and drop the entry.
pub fn note_dropped(vm_dropped: &Object) {
    if let Object::Dict(d) = vm_dropped {
        if dicts_active() {
            let id = crate::weakref_registry::id_of(vm_dropped);
            let registry_clones = crate::weakref_registry::strong_clone_count(id);
            // The GC registry's `track` handle is a strong clone too.
            let gc_clone = usize::from(crate::gc_trace::is_tracked(id));
            if Rc::strong_count(d) <= 1 + registry_clones + gc_clone && dict_mask(d) != 0 {
                dict_event("DEALLOCATED", d, None, None);
                let ptr = Rc::as_ptr(d) as usize;
                let mut w = WATCHED_DICTS.lock();
                w.retain(|e| e.ptr != ptr);
                if w.is_empty() {
                    DICTS_ACTIVE.store(false, Ordering::Relaxed);
                }
            }
        }
    }
    if let Object::Function(f) = vm_dropped {
        if funcs_active() {
            let id = crate::weakref_registry::id_of(vm_dropped);
            let registry_clones = crate::weakref_registry::strong_clone_count(id);
            // The GC registry holds one strong clone from MAKE_FUNCTION's
            // `track`; a dying function is down to our handle + that one.
            let gc_clone = usize::from(crate::gc_trace::is_tracked(id));
            if Rc::strong_count(f) <= 1 + registry_clones + gc_clone {
                func_event("DESTROY", vm_dropped, &Object::None);
            }
        }
    }
}

// ---------------------------------------------------------------- types

#[inline(always)]
pub fn types_active() -> bool {
    TYPES_ACTIVE.load(Ordering::Relaxed)
}

fn type_ptr(t: &Object) -> usize {
    match t {
        Object::Type(ty) => Rc::as_ptr(ty) as usize,
        _ => 0,
    }
}

pub fn watch_type(watcher_id: u8, t: &Object) {
    let mut w = WATCHED_TYPES.lock();
    let ptr = type_ptr(t);
    if let Some(e) = w.iter_mut().find(|e| type_ptr(&e.obj) == ptr) {
        e.mask |= 1 << watcher_id;
        e.armed = true;
    } else {
        w.push(WatchedType {
            obj: t.clone(),
            mask: 1 << watcher_id,
            armed: true,
        });
    }
    TYPES_ACTIVE.store(true, Ordering::Relaxed);
}

pub fn unwatch_type(watcher_id: u8, t: &Object) {
    let mut w = WATCHED_TYPES.lock();
    let ptr = type_ptr(t);
    if let Some(e) = w.iter_mut().find(|e| type_ptr(&e.obj) == ptr) {
        e.mask &= !(1 << watcher_id);
    }
    w.retain(|e| e.mask != 0);
    if w.is_empty() {
        TYPES_ACTIVE.store(false, Ordering::Relaxed);
    }
}

pub fn clear_type_watcher_slot(watcher_id: u8) {
    let mut w = WATCHED_TYPES.lock();
    for e in w.iter_mut() {
        e.mask &= !(1 << watcher_id);
    }
    w.retain(|e| e.mask != 0);
    if w.is_empty() {
        TYPES_ACTIVE.store(false, Ordering::Relaxed);
    }
}

/// A type's attribute set changed (CPython `type_modified`). Fires for
/// every watched type whose MRO contains the modified type — modifying a
/// base invalidates the subclass version tags, which is what dispatches
/// a watched subclass (test_watch_type_subclass).
pub fn type_modified(modified: &crate::sync::Rc<crate::types::TypeObject>) {
    if !types_active() {
        return;
    }
    let modified_ptr = Rc::as_ptr(modified) as usize;
    let mut fired: Vec<(Object, u8)> = Vec::new();
    {
        let mut w = WATCHED_TYPES.lock();
        for e in w.iter_mut() {
            if !e.armed {
                continue;
            }
            let hits = match &e.obj {
                Object::Type(ty) => {
                    Rc::as_ptr(ty) as usize == modified_ptr
                        || ty
                            .mro
                            .borrow()
                            .iter()
                            .any(|b| Rc::as_ptr(b) as usize == modified_ptr)
                }
                _ => false,
            };
            if hits {
                e.armed = false;
                fired.push((e.obj.clone(), e.mask));
            }
        }
    }
    if fired.is_empty() {
        return;
    }
    let cb = TYPE_DISPATCH.lock().clone();
    let Some(cb) = cb else { return };
    for (t, mask) in fired {
        call_dispatcher(&cb, vec![Object::Int(i64::from(mask)), t]);
    }
}

/// A successful attribute lookup on a type re-assigns its version tag in
/// CPython, re-arming event aggregation for the next modification.
pub fn type_lookup_rearm(t: &crate::sync::Rc<crate::types::TypeObject>) {
    if !types_active() {
        return;
    }
    let ptr = Rc::as_ptr(t) as usize;
    let mut w = WATCHED_TYPES.lock();
    if let Some(e) = w.iter_mut().find(|e| type_ptr(&e.obj) == ptr) {
        e.armed = true;
    }
}

// ------------------------------------------------------------ functions

#[inline(always)]
pub fn funcs_active() -> bool {
    FUNCS_ACTIVE.load(Ordering::Relaxed)
}

pub fn set_funcs_active(active: bool) {
    FUNCS_ACTIVE.store(active, Ordering::Relaxed);
}

/// Fire a function watcher event. `event` is the `PyFunction_EVENT_` name
/// suffix: `CREATE`, `DESTROY`, `MODIFY_CODE`, `MODIFY_DEFAULTS`,
/// `MODIFY_KWDEFAULTS`.
pub fn func_event(event: &str, func: &Object, new_value: &Object) {
    if !funcs_active() {
        return;
    }
    let cb = FUNC_DISPATCH.lock().clone();
    let Some(cb) = cb else { return };
    call_dispatcher(
        &cb,
        vec![
            Object::from_str(event.to_owned()),
            func.clone(),
            new_value.clone(),
        ],
    );
}
