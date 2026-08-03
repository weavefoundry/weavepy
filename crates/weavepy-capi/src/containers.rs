//! `PyList_*`, `PyTuple_*`, `PyDict_*`, `PySet_*`, `PyFrozenSet_*`.
//!
//! Containers wrap WeavePy's native [`Object::List`], [`Object::Tuple`],
//! [`Object::Dict`], [`Object::Set`], [`Object::FrozenSet`] variants
//! through the same boxing machinery as scalars. Mutating operations
//! borrow the inner `RefCell` for the duration of the call.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::sync::Mutex;
use weavepy_vm::fasthash::FxHashMap;
use weavepy_vm::sync::Rc;
use weavepy_vm::sync::RefCell;

use weavepy_vm::object::{DictData, DictKey, Object, SetData};

use crate::object::{PyHashT, PyObject, PySsizeT};

/// Interned `*mut PyObject` cache for `PyTuple_GetItem` /
/// `PyList_GetItem`'s "borrowed reference" contract. Without it
/// we'd either leak fresh boxes on every call or hand callers a
/// dangling pointer. The cache is keyed on the container's
/// pointer + index so repeated `PyTuple_GetItem(t, 0)` calls
/// return the same `*mut PyObject` (matching CPython).
///
/// Process-global (RFC 0047, wave 5): a container built on the main
/// thread is read from `threading.Thread` workers; a per-thread cache
/// would mint divergent "borrowed" pointers per thread and leak the
/// pinned boxes on thread exit. Stored as raw addresses in `usize`
/// (both key and value) so the `static` is `Send`.
///
/// Two-level (container → slot → box): [`crate::object::free_box`] runs
/// [`invalidate_borrowed_cache`] on *every* box free, so eviction must be
/// an O(1) `remove(&container)` — a flat `(container, idx)`-keyed map
/// forced a full-cache scan per free, which under pandas (thousands of
/// live entries) made every object free O(cache) and dominated entire
/// test-suite runs (84% of CPU in `free_box`).
static BORROWED_ITEM_CACHE: Mutex<Option<FxHashMap<usize, FxHashMap<isize, usize>>>> =
    Mutex::new(None);

/// Per-dict value-box cache (RFC 0046, wave 4). A C extension that
/// `PyDict_New()`s a dict, stores it under a key, then `Py_DECREF`s its
/// own reference relies on the *parent* dict keeping the value's
/// `PyObject` alive — and frequently retains the raw pointer (numpy
/// stashes module sub-dicts in `npy_static_pydata`). WeavePy's parent
/// dict only retains the *native* value, so the freshly-minted value
/// box would be freed by that `Py_DECREF`, dangling the extension's
/// pointer. Keyed on `(dict box ptr, key repr)`, this holds one
/// reference to each stored value box for as long as the dict lives, so
/// `PyDict_GetItem*` round-trips the *same* pointer the setter stored.
/// Drained by [`invalidate_borrowed_cache`] when the dict is freed.
///
/// The cached value carries the *native* [`Object`] it was minted from
/// alongside the box. `PyDict_GetItem*` return a borrowed reference to
/// the value *currently* stored, so on a read we compare the live value
/// against this snapshot (`Object::is_same`): if the slot was reassigned
/// (a monkeypatch, or any post-init global rebind — the exact idiom
/// `pytest.monkeypatch`/`mock.patch` use on a compiled module, which
/// Cython reads back via `__Pyx_GetModuleGlobalName` →
/// `_PyDict_GetItem_KnownHash`), we mint a fresh box for the new value
/// instead of handing back the stale one. Storing the `Object` (rather
/// than re-deriving it from the box) keeps identity stable for unchanged
/// mirrored builtins (str/int round-trip to a *fresh* Rc), so repeated
/// reads of an unchanged value keep returning the same pointer.
///
/// Process-global for the same cross-thread reasons as
/// [`BORROWED_ITEM_CACHE`]; the box pointer is stored as `usize`.
/// Two-level (dict → key → box) for the same O(1)-eviction reason.
static DICT_BOX_CACHE: Mutex<Option<FxHashMap<usize, FxHashMap<String, (usize, Object)>>>> =
    Mutex::new(None);

/// A stable cache key for a dict key object (matches `==`-equal keys, the
/// dict contract numpy relies on for string / int / DType-class keys).
fn dict_key_id(key: &Object) -> String {
    match key {
        Object::Str(s) => format!("s\0{s}"),
        // Python dict keys unify across the numeric tower (`d[2]`,
        // `d[2.0]`, and `d[True]`/`d[1]` all address one slot), so the
        // borrowed-box cache must too: pandas' `index.pyx` builds a
        // position dict with an `int` key and reads it back with the
        // equal `float` — two ids would pin two diverging mirrors of the
        // same list value. Canonical form: the integer's decimal digits.
        Object::Bool(b) => format!("i\0{}", i64::from(*b)),
        Object::Int(_) | Object::Long(_) => format!("i\0{}", key.to_str()),
        // An integral float shares the slot of the equal int; `{:.0}`
        // prints the exact integer value of any integral f64 (they are
        // all exactly representable), matching the int/Long rendering.
        // Normalise `-0.0` to `0` (equal and hash-equal to `0`).
        Object::Float(f) if f.is_finite() && f.fract() == 0.0 => {
            if *f == 0.0 {
                "i\x000".to_owned()
            } else {
                format!("i\0{f:.0}")
            }
        }
        // A foreign key is identified by its C pointer. Calling `.repr()`
        // here would re-enter the C side (`fwd_repr` → `PyObject_Repr` →
        // the type's `tp_repr`) while a `PyDict_GetItem` C frame is already
        // live — and when the resolved slot is our own `synth_tp_repr`
        // VM-forwarding bridge the round trip never terminates (numpy's
        // `discover_dtype_from_pyobject` keys its cache dict by *type
        // object*, which crashed `np.asarray(memoryview(...))` with a stack
        // overflow). Identity keying matches dict semantics for type
        // objects and any other foreign key that hashes by pointer.
        Object::Foreign(soul) => format!("f\0{:x}", soul.ptr),
        other => format!("r\0{}", other.repr()),
    }
}

/// Retain `value` (the *caller's* box) as `dict[key]`'s canonical C
/// reference, releasing any previous box for that slot. Increfs `value`
/// so it survives the caller's matching `Py_DECREF`. `value_obj` is the
/// native [`Object`] `value` boxes; it is stashed so a later read can tell
/// whether the slot still holds the same value (see [`dict_borrowed_box`]).
fn dict_retain_value(dict: *mut PyObject, key: String, value: *mut PyObject, value_obj: Object) {
    if value.is_null() {
        return;
    }
    unsafe { crate::object::Py_IncRef(value) };
    let old = DICT_BOX_CACHE.lock().ok().and_then(|mut g| {
        g.get_or_insert_with(FxHashMap::default)
            .entry(dict as usize)
            .or_default()
            .insert(key, (value as usize, value_obj))
    });
    if let Some((old, _)) = old {
        let old = old as *mut PyObject;
        if old != value {
            unsafe { crate::object::Py_DecRef(old) };
        } else {
            // Same pointer re-stored: undo the extra incref.
            unsafe { crate::object::Py_DecRef(value) };
        }
    }
}

/// The canonical value box for `dict[key]`, if one was stored through a
/// C setter and is still live, paired with the native [`Object`] it was
/// minted from. A *borrowed* reference (not incref'd).
fn dict_cached_value(dict: *mut PyObject, key: &str) -> Option<(*mut PyObject, Object)> {
    DICT_BOX_CACHE
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .and_then(|m| m.get(&(dict as usize)))
                .and_then(|slots| slots.get(key).cloned())
        })
        .map(|(p, obj)| (p as *mut PyObject, obj))
}

/// Diagnostic: log `_engine`/`_cache` dict operations (RFC 0045 body-reuse
/// debugging). Gated on `WEAVEPY_BODY_TRACE`.
fn engine_dict_trace(
    tag: &str,
    dict: *mut PyObject,
    key_id: &str,
    found: bool,
    boxp: *mut PyObject,
) {
    if !crate::mirror::body_trace_enabled() {
        return;
    }
    if !key_id.contains("_engine") {
        return;
    }
    let ty = if boxp.is_null() {
        "<null>".to_string()
    } else {
        unsafe { crate::object::debug_type_name(boxp) }
    };
    let cached = dict_cached_value(dict, key_id)
        .map(|(p, _)| p as usize)
        .unwrap_or(0);
    eprintln!(
        "[EDICT] {} dict=0x{:x} key={} found={} box=0x{:x} boxtype={} cached=0x{:x}",
        tag, dict as usize, key_id, found, boxp as usize, ty, cached,
    );
}

/// Drop every cached dict value box pinned to `container`.
fn invalidate_dict_box_cache(container: *mut PyObject) {
    let key = container as usize;
    // O(1) eviction: remove the container's whole slot map. Collect-then-
    // drop so `Py_DecRef` never runs under the cache lock.
    let drained: Option<FxHashMap<String, (usize, Object)>> = DICT_BOX_CACHE
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().and_then(|map| map.remove(&key)));
    if let Some(slots) = drained {
        for (_, (p, _)) in slots {
            unsafe { crate::object::Py_DecRef(p as *mut PyObject) };
        }
    }
}

/// Install or reuse the interned borrowed-reference pointer for the
/// `(container, index)` slot. Subsequent calls with the same
/// container pointer + index return the same `*mut PyObject`.
pub(crate) fn intern_borrowed_item(container: *mut PyObject, item: Object) -> *mut PyObject {
    intern_borrowed_at(container, isize::MIN /* sentinel */, item)
}

pub(crate) fn intern_borrowed_at(
    container: *mut PyObject,
    idx: isize,
    item: Object,
) -> *mut PyObject {
    let ckey = container as usize;
    if let Some(p) = BORROWED_ITEM_CACHE.lock().ok().and_then(|g| {
        g.as_ref()
            .and_then(|m| m.get(&ckey))
            .and_then(|slots| slots.get(&idx).copied())
    }) {
        return p as *mut PyObject;
    }
    // Mint outside the lock: `into_owned` can re-enter the C-API (and
    // this cache) through mirror seeding / dunder shims.
    let p = crate::object::into_owned(item);
    if let Ok(mut g) = BORROWED_ITEM_CACHE.lock() {
        // A racing thread may have minted its own box first; keep the
        // existing entry (callers already hold `p` for this call) so the
        // borrowed-pointer contract stays single-valued per slot.
        let slots = g
            .get_or_insert_with(FxHashMap::default)
            .entry(ckey)
            .or_default();
        if let Some(&existing) = slots.get(&idx) {
            drop(g);
            unsafe { crate::object::Py_DecRef(p) };
            return existing as *mut PyObject;
        }
        slots.insert(idx, p as usize);
    }
    p
}

/// Drop every cached borrowed-reference entry pinned to `container`.
/// Called from `free_box` when the container's refcount hits zero
/// so a later allocation that lands at the same address starts
/// with a clean slate.
pub(crate) fn invalidate_borrowed_cache(container: *mut crate::object::PyObject) {
    let key = container as usize;
    // O(1) eviction (see the cache doc comment): remove the container's
    // whole slot map. Collect-then-drop so we never hold the cache lock
    // while the recursive `Py_DecRef` walks back into the cache (the
    // freed item itself might be a container with cached entries).
    let drained: Option<FxHashMap<isize, usize>> = BORROWED_ITEM_CACHE
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().and_then(|map| map.remove(&key)));
    if let Some(slots) = drained {
        for (_, p) in slots {
            unsafe { crate::object::Py_DecRef(p as *mut crate::object::PyObject) };
        }
    }
    invalidate_dict_box_cache(container);
}

/// True iff the container caches are still reachable. The caches are
/// process `static`s now (RFC 0047: containers cross threads), so this
/// only reports mutex poisoning — a panic mid-mutation, after which
/// [`crate::object::free_box`] leaks rather than risks a double-free.
pub(crate) fn caches_alive() -> bool {
    BORROWED_ITEM_CACHE.lock().is_ok() && DICT_BOX_CACHE.lock().is_ok()
}

// ----------------------------------------------------------------
// PyList.
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PyList_New(size: PySsizeT) -> *mut PyObject {
    let n = size.max(0) as usize;
    crate::object::into_owned(Object::new_list(vec![Object::None; n]))
}

#[no_mangle]
pub unsafe extern "C" fn PyList_Append(list: *mut PyObject, item: *mut PyObject) -> c_int {
    if list.is_null() || item.is_null() {
        return -1;
    }
    // RFC 0046 (wave 4): a faithful list stores its elements in the inline
    // `ob_item` buffer (the source of truth every read-back — including a
    // stock `PyList_GET_ITEM` macro — consults), so the append must land
    // there, not on the now-vestigial staged native list.
    if unsafe { crate::mirror::is_faithful_list(list) } {
        unsafe { crate::mirror::list_append(list, item) };
        return 0;
    }
    // Defensive fallback for a (today unreachable) non-mirror list box.
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => {
            rc.borrow_mut()
                .push(unsafe { crate::object::clone_object(item) });
            0
        }
        _ => {
            crate::errors::set_type_error("expected list");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyList_Insert(
    list: *mut PyObject,
    index: PySsizeT,
    item: *mut PyObject,
) -> c_int {
    if list.is_null() || item.is_null() {
        return -1;
    }
    if unsafe { crate::mirror::is_faithful_list(list) } {
        unsafe { crate::mirror::list_insert(list, index, item) };
        return 0;
    }
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => {
            let mut v = rc.borrow_mut();
            let pos = index.clamp(0, v.len() as PySsizeT) as usize;
            v.insert(pos, unsafe { crate::object::clone_object(item) });
            0
        }
        _ => {
            crate::errors::set_type_error("expected list");
            -1
        }
    }
}

/// `PyList_SetItem` *steals* `item`'s reference (CPython convention).
#[no_mangle]
pub unsafe extern "C" fn PyList_SetItem(
    list: *mut PyObject,
    index: PySsizeT,
    item: *mut PyObject,
) -> c_int {
    if list.is_null() {
        return -1;
    }
    // RFC 0046 (wave 4): a faithful list stores elements inline; steal
    // `item` straight into the `ob_item` slot (CPython `PyList_SetItem`
    // takes ownership), releasing the prior occupant. This is the write
    // that keeps the inline buffer — the source of truth — coherent.
    if unsafe { crate::mirror::is_faithful_list(list) } {
        if unsafe { crate::mirror::list_store(list, index, item) } {
            return 0;
        }
        if !item.is_null() {
            unsafe { crate::object::Py_DecRef(item) };
        }
        crate::errors::set_value_error("list assignment index out of range");
        return -1;
    }
    let result = match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => {
            let mut v = rc.borrow_mut();
            if index < 0 || index >= v.len() as PySsizeT {
                drop(v);
                if !item.is_null() {
                    unsafe { crate::object::Py_DecRef(item) };
                }
                crate::errors::set_value_error("list assignment index out of range");
                return -1;
            }
            v[index as usize] = unsafe { crate::object::clone_object(item) };
            0
        }
        _ => {
            crate::errors::set_type_error("expected list");
            -1
        }
    };
    if !item.is_null() {
        unsafe { crate::object::Py_DecRef(item) };
    }
    result
}

#[no_mangle]
pub unsafe extern "C" fn PyList_GetItem(list: *mut PyObject, index: PySsizeT) -> *mut PyObject {
    if list.is_null() {
        return ptr::null_mut();
    }
    // RFC 0046 (wave 4): hand back the actual `ob_item` slot (a borrowed
    // reference, per `PyList_GetItem`'s contract) so the pointer is the
    // exact one a prior `PyList_SetItem` / `PyList_Append` stored — stock
    // code compares list elements by identity.
    if unsafe { crate::mirror::is_faithful_list(list) } {
        let n = unsafe { crate::mirror::list_size(list) };
        if index < 0 || index >= n {
            crate::errors::set_value_error("list index out of range");
            return ptr::null_mut();
        }
        return match unsafe { crate::mirror::list_slot(list, index) } {
            Some(slot) if !slot.is_null() => slot,
            // A NULL placeholder (`PyList_New(n)` slot never filled) reads
            // as the immortal `None`, matching CPython's NULL-slot handling.
            _ => crate::singletons::none_ptr(),
        };
    }
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => {
            let v = rc.borrow();
            if index < 0 || index >= v.len() as PySsizeT {
                crate::errors::set_value_error("list index out of range");
                ptr::null_mut()
            } else {
                // Borrowed reference: intern a stable pointer keyed
                // on the list pointer + index so callers get the
                // same `*mut PyObject` for repeated lookups.
                intern_borrowed_at(list, index as isize, v[index as usize].clone())
            }
        }
        _ => ptr::null_mut(),
    }
}

/// `PyList_GetItemRef` (3.13+) — like `PyList_GetItem` but returns a *new*
/// (strong) reference and sets IndexError on a bad index. numpy < 2.5 links
/// it directly in `PyUFunc_AddLoop`'s duplicate-loop scan; the missing
/// export left a NULL dyld stub and importing numpy 2.3.x/2.4.x segfaulted.
#[no_mangle]
pub unsafe extern "C" fn PyList_GetItemRef(list: *mut PyObject, index: PySsizeT) -> *mut PyObject {
    let p = unsafe { PyList_GetItem(list, index) };
    if !p.is_null() {
        unsafe { crate::object::Py_IncRef(p) };
    }
    p
}

#[no_mangle]
pub unsafe extern "C" fn PyList_Size(list: *mut PyObject) -> PySsizeT {
    if list.is_null() {
        return -1;
    }
    // Read `ob_size` straight off a faithful list — which since RFC 0047
    // (wave 5) includes list-subclass container bodies (`FrozenList`),
    // whose `clone_object` is an `Object::Instance` the match below would
    // reject.
    if unsafe { crate::mirror::is_faithful_list(list) } {
        return unsafe { crate::mirror::list_size(list) };
    }
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => rc.borrow().len() as PySsizeT,
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyList_AsTuple(list: *mut PyObject) -> *mut PyObject {
    if list.is_null() {
        return ptr::null_mut();
    }
    // A list-subclass container body (RFC 0047, wave 5) reads back as an
    // `Object::Instance`; build the tuple from its authoritative `ob_item`
    // buffer instead.
    if unsafe { crate::mirror::is_faithful_list(list) } {
        if let Object::List(rc) = unsafe { crate::mirror::read_list(list) } {
            return crate::object::into_owned(Object::new_tuple(rc.borrow().clone()));
        }
    }
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => crate::object::into_owned(Object::new_tuple(rc.borrow().clone())),
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyList_Reverse(list: *mut PyObject) -> c_int {
    if list.is_null() {
        return -1;
    }
    // RFC 0046 (wave 4): permute the inline `ob_item` pointers in place
    // (a pure reordering — no refcount change) so the source of truth is
    // reversed, not a throwaway reconstruction.
    if unsafe { crate::mirror::is_faithful_list(list) } {
        let mut ptrs = unsafe { crate::mirror::list_ptrs(list) };
        ptrs.reverse();
        unsafe { crate::mirror::list_permute(list, &ptrs) };
        return 0;
    }
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => {
            rc.borrow_mut().reverse();
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyList_Sort(list: *mut PyObject) -> c_int {
    if list.is_null() {
        return -1;
    }
    // RFC 0046 (wave 4): sort the inline `ob_item` pointers by their
    // resolved values, then write the permutation back — keeping every
    // element's identity (the same `PyObject*`) and the inline buffer
    // authoritative.
    if unsafe { crate::mirror::is_faithful_list(list) } {
        let mut ptrs = unsafe { crate::mirror::list_ptrs(list) };
        ptrs.sort_by(|&a, &b| {
            let oa = unsafe { crate::object::clone_object(a) };
            let ob = unsafe { crate::object::clone_object(b) };
            natural_cmp(&oa, &ob)
        });
        unsafe { crate::mirror::list_permute(list, &ptrs) };
        return 0;
    }
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => {
            let mut items = rc.borrow_mut();
            items.sort_by(|a, b| natural_cmp(a, b));
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyList_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::List(_)).into()
}

// ----------------------------------------------------------------
// PyTuple.
// ----------------------------------------------------------------
//
// Tuples are immutable, so we model an "in-flight" tuple as an
// `Object::List` until `PyTuple_SetItem` has finished initialising
// the slots, then convert at access time. This matches CPython's
// "tuples returned by `PyTuple_New(n)` start uninitialised, you
// must `PyTuple_SetItem` every slot before exposing them" rule.

#[no_mangle]
pub unsafe extern "C" fn PyTuple_New(n: PySsizeT) -> *mut PyObject {
    // RFC 0046 (wave 4): mint a faithful tuple mirror whose inline
    // `ob_item` array is `n` immortal-`None` placeholders. A stock
    // extension fills it with the `PyTuple_SET_ITEM` macro — a direct
    // write into that inline array — and reads it back with
    // `PyTuple_GET_ITEM`; both touch the C body, so it must be a real
    // `PyTupleObject` (not the old `List`-staged stand-in, whose
    // out-of-line `ob_item` pointer sits exactly where the macro would
    // scribble element 0). `clone_object` reconstructs the native tuple
    // from this inline array on read.
    let len = n.max(0) as usize;
    crate::object::into_owned(Object::new_tuple(vec![Object::None; len]))
}

#[no_mangle]
pub unsafe extern "C" fn PyTuple_SetItem(
    tuple: *mut PyObject,
    pos: PySsizeT,
    item: *mut PyObject,
) -> c_int {
    if tuple.is_null() {
        return -1;
    }
    // RFC 0046 (wave 4): a faithful tuple stores its elements inline; steal
    // `item` into the slot (CPython's `PyTuple_SetItem` takes ownership)
    // and release the prior occupant. This is also what keeps the inline
    // array — the source of truth for every read — in sync.
    if unsafe { crate::mirror::is_faithful_tuple(tuple) } {
        if unsafe { crate::mirror::tuple_store(tuple, pos, item) } {
            return 0;
        }
        if !item.is_null() {
            unsafe { crate::object::Py_DecRef(item) };
        }
        crate::errors::set_value_error("tuple assignment index out of range");
        return -1;
    }
    // Use the raw payload here (not `clone_object`) so the
    // staged-list-with-PyTuple_Type backing isn't frozen mid-fill.
    let raw = unsafe { crate::object::raw_payload(tuple) };
    let Some(raw) = raw else {
        return -1;
    };
    let result = match raw {
        Object::List(rc) => {
            let mut v = rc.borrow_mut();
            if pos < 0 || pos >= v.len() as PySsizeT {
                drop(v);
                if !item.is_null() {
                    unsafe { crate::object::Py_DecRef(item) };
                }
                crate::errors::set_value_error("tuple assignment index out of range");
                return -1;
            }
            v[pos as usize] = unsafe { crate::object::clone_object(item) };
            0
        }
        Object::Tuple(items) => {
            // The tuple is immutable; build a new one and rewrite
            // the box's payload.
            let mut v: Vec<Object> = items.iter().cloned().collect();
            if pos < 0 || pos >= v.len() as PySsizeT {
                if !item.is_null() {
                    unsafe { crate::object::Py_DecRef(item) };
                }
                crate::errors::set_value_error("tuple assignment index out of range");
                return -1;
            }
            v[pos as usize] = unsafe { crate::object::clone_object(item) };
            unsafe {
                crate::object::set_payload(tuple, Object::Tuple(Rc::from(v.into_boxed_slice())));
            }
            0
        }
        _ => {
            if !item.is_null() {
                unsafe { crate::object::Py_DecRef(item) };
            }
            crate::errors::set_type_error("expected tuple");
            return -1;
        }
    };
    if !item.is_null() {
        unsafe { crate::object::Py_DecRef(item) };
    }
    result
}

#[no_mangle]
pub unsafe extern "C" fn PyTuple_GetItem(tuple: *mut PyObject, pos: PySsizeT) -> *mut PyObject {
    if tuple.is_null() {
        return ptr::null_mut();
    }
    // RFC 0046 (wave 4): a faithful tuple's inline `ob_item` is the source
    // of truth; return the borrowed slot directly (as CPython does) so the
    // pointer numpy stored with `PyTuple_SET_ITEM` round-trips by identity.
    if unsafe { crate::mirror::is_faithful_tuple(tuple) } {
        return match unsafe { crate::mirror::tuple_slot(tuple, pos) } {
            Some(p) => p,
            None => {
                crate::errors::set_value_error("tuple index out of range");
                ptr::null_mut()
            }
        };
    }
    // Use the raw payload so a staged-list-backed tuple still works
    // when read mid-fill.
    let raw = match unsafe { crate::object::raw_payload(tuple) } {
        Some(r) => r,
        None => return ptr::null_mut(),
    };
    let item = match raw {
        Object::Tuple(items) => {
            if pos < 0 || pos >= items.len() as PySsizeT {
                None
            } else {
                Some(items[pos as usize].clone())
            }
        }
        Object::List(rc) => {
            let v = rc.borrow();
            if pos < 0 || pos >= v.len() as PySsizeT {
                None
            } else {
                Some(v[pos as usize].clone())
            }
        }
        _ => None,
    };
    let Some(item) = item else {
        crate::errors::set_value_error("tuple index out of range");
        return ptr::null_mut();
    };
    // CPython's `PyTuple_GetItem` returns a *borrowed* reference. We
    // don't have stable item pointers, so we materialise a fresh
    // box and intern it on the tuple's pointer so its lifetime
    // matches the tuple itself.
    intern_borrowed_at(tuple, pos as isize, item)
}

#[no_mangle]
pub unsafe extern "C" fn PyTuple_Size(tuple: *mut PyObject) -> PySsizeT {
    if tuple.is_null() {
        return -1;
    }
    // RFC 0046 (wave 4): read `ob_size` straight off a faithful tuple so we
    // don't materialise (and incref/decref) every element just to count.
    if unsafe { crate::mirror::is_faithful_tuple(tuple) } {
        let vo = tuple as *const crate::layout::PyVarObject;
        return unsafe { (*vo).ob_size };
    }
    match unsafe { crate::object::clone_object(tuple) } {
        Object::Tuple(items) => items.len() as PySsizeT,
        Object::List(rc) => rc.borrow().len() as PySsizeT,
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyTuple_GetSlice(
    tuple: *mut PyObject,
    lo: PySsizeT,
    hi: PySsizeT,
) -> *mut PyObject {
    if tuple.is_null() {
        return ptr::null_mut();
    }
    let items = match unsafe { crate::object::clone_object(tuple) } {
        Object::Tuple(items) => items.iter().cloned().collect::<Vec<_>>(),
        Object::List(rc) => rc.borrow().clone(),
        _ => return ptr::null_mut(),
    };
    let lo = lo.clamp(0, items.len() as PySsizeT) as usize;
    let hi = hi.clamp(lo as PySsizeT, items.len() as PySsizeT) as usize;
    crate::object::into_owned(Object::new_tuple(items[lo..hi].to_vec()))
}

#[no_mangle]
pub unsafe extern "C" fn PyTuple_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Tuple(_)).into()
}

// ----------------------------------------------------------------
// PyDict.
// ----------------------------------------------------------------

/// The concrete dict payload behind `o`: a plain `dict`, or the native
/// payload of a **dict-subclass instance** (RFC 0047, wave 5). CPython's
/// concrete `PyDict_*` API operates on subclasses too (the instance *is*
/// a `PyDictObject`), and pandas' ujson iterates dict subclasses through
/// `PyDict_Next` — without the unwrap they serialized as `{}`.
fn as_dict_rc(o: &Object) -> Option<Rc<RefCell<DictData>>> {
    match o {
        Object::Dict(rc) => Some(rc.clone()),
        Object::Instance(inst) => match inst.native.get() {
            Some(Object::Dict(rc)) => Some(rc.clone()),
            _ => None,
        },
        _ => None,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_New() -> *mut PyObject {
    crate::object::into_owned(Object::new_dict())
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_SetItem(
    d: *mut PyObject,
    k: *mut PyObject,
    v: *mut PyObject,
) -> c_int {
    if d.is_null() || k.is_null() || v.is_null() {
        return -1;
    }
    match as_dict_rc(&unsafe { crate::object::clone_object(d) }) {
        Some(rc) => {
            let key = unsafe { crate::object::clone_object(k) };
            let val = unsafe { crate::object::clone_object(v) };
            let key_id = dict_key_id(&key);
            if crate::mirror::listsync_trace_enabled() {
                if let Object::List(lrc) = &val {
                    eprintln!(
                        "[LISTSYNC] PyDict_SetItem dict={:p} key={} rc=0x{:x} len={}",
                        d,
                        key_id.escape_debug(),
                        weavepy_vm::sync::Rc::as_ptr(lrc) as usize,
                        lrc.borrow().len()
                    );
                }
            }
            rc.borrow_mut().insert(DictKey(key), val.clone());
            dict_retain_value(d, key_id.clone(), v, val);
            engine_dict_trace("SET", d, &key_id, true, v);
            unsafe { crate::mirror::sync_dict_ma_used(d) };
            0
        }
        None => {
            crate::errors::set_type_error("expected dict");
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_SetItemString(
    d: *mut PyObject,
    k: *const c_char,
    v: *mut PyObject,
) -> c_int {
    if d.is_null() || k.is_null() || v.is_null() {
        return -1;
    }
    let key = unsafe { CStr::from_ptr(k) }.to_string_lossy().into_owned();
    let obj = unsafe { crate::object::clone_object(d) };
    if let Some(rc) = as_dict_rc(&obj) {
        let val = unsafe { crate::object::clone_object(v) };
        let key_id = dict_key_id(&Object::from_str(key.clone()));
        rc.borrow_mut()
            .insert(DictKey(Object::from_str(key)), val.clone());
        dict_retain_value(d, key_id, v, val);
        unsafe { crate::mirror::sync_dict_ma_used(d) };
        return 0;
    }
    match obj {
        Object::Module(m) => {
            // Convenience: PyDict_SetItemString on a module's dict
            // is a common idiom.
            let val = unsafe { crate::object::clone_object(v) };
            let key_id = dict_key_id(&Object::from_str(key.clone()));
            m.dict
                .borrow_mut()
                .insert(DictKey(Object::from_str(key)), val.clone());
            dict_retain_value(d, key_id, v, val);
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_GetItem(d: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    if d.is_null() || k.is_null() {
        return ptr::null_mut();
    }
    match as_dict_rc(&unsafe { crate::object::clone_object(d) }) {
        Some(rc) => {
            let key = unsafe { crate::object::clone_object(k) };
            let key_id = dict_key_id(&key);
            let result = rc.borrow().get(&DictKey(key)).cloned();
            match result {
                Some(v) => {
                    let bx = dict_borrowed_box(d, key_id.clone(), v);
                    engine_dict_trace("GET", d, &key_id, true, bx);
                    bx
                }
                None => {
                    engine_dict_trace("GET", d, &key_id, false, ptr::null_mut());
                    ptr::null_mut()
                }
            }
        }
        None => ptr::null_mut(),
    }
}

/// Return a *borrowed* (non-incref'd) box for a dict value: the canonical
/// box a C setter stored if one exists, otherwise a freshly minted box
/// retained in the dict cache so it lives as long as the dict (the
/// borrowed-reference contract). RFC 0046, wave 4.
fn dict_borrowed_box(dict: *mut PyObject, key_id: String, value: Object) -> *mut PyObject {
    if let Some((p, cached_obj)) = dict_cached_value(dict, &key_id) {
        // Reuse the pinned box only while it still represents the value the
        // slot *currently* holds. CPython's `PyDict_GetItem*` return a
        // borrowed reference to the live value, so once a slot is rebound
        // (e.g. a test monkeypatches a compiled module's global — the VM
        // writes straight into `module.dict`, bypassing the C setter that
        // seeded this cache) the old box is stale and must not be returned:
        // Cython would otherwise keep reading the pre-patch value through
        // `__Pyx_GetModuleGlobalName` → `_PyDict_GetItem_KnownHash`.
        if cached_obj.is_same(&value) {
            // A faithful-list box can be *stale*: several boxes share one
            // native `Rc`, and an append routed through a sibling box only
            // rewrote that sibling's `ob_item` (plus the shared `Rc`) — the
            // VM→C flush that would repair this one only runs at a VM→C
            // boundary, not within a single C call. A stock reader then
            // consumes the short buffer through the `PyList_GET_ITEM`
            // macro (pandas' `IndexEngine.get_indexer_non_unique` dropped
            // the trailing duplicate positions). Re-publish from the
            // shared `Rc` before handing the box back; the fingerprint
            // gate makes the unmutated case cheap.
            unsafe { crate::mirror::sync_list_ob_item(p) };
            return p;
        }
    }
    let value_obj = value.clone();
    if crate::mirror::listsync_trace_enabled() {
        if let Object::List(rc) = &value_obj {
            eprintln!(
                "[LISTSYNC] dict_borrowed_box MISS dict={:p} key={} rc=0x{:x} len={}",
                dict,
                key_id.escape_debug(),
                weavepy_vm::sync::Rc::as_ptr(rc) as usize,
                rc.borrow().len()
            );
        }
    }
    let p = crate::object::into_owned(value);
    // `dict_retain_value` increfs; balance the `into_owned` +1 so the
    // cache holds exactly one reference (released when the dict is freed).
    // Replacing a stale entry drops the previous box's cache reference.
    dict_retain_value(dict, key_id, p, value_obj);
    unsafe { crate::object::Py_DecRef(p) };
    p
}

/// A *borrowed* box for a dict *key*, pinned for the dict's lifetime like
/// [`dict_borrowed_box`] but under a private namespace so a key box never
/// collides with the value box stored under the same `key_id`. Used by
/// [`PyDict_Next`], whose key reference is borrowed and which Cython's
/// vectorcall kwargs path immediately increfs.
fn dict_borrowed_key_box(dict: *mut PyObject, key_id: String, key: Object) -> *mut PyObject {
    dict_borrowed_box(dict, format!("\u{0}__key__\u{0}{key_id}"), key)
}

/// `_PyDict_GetItem_KnownHash(dict, key, hash)` — a private dict lookup
/// that accepts a precomputed `key` hash to skip rehashing. WeavePy's dict
/// hashes the key internally, so the supplied hash is advisory; we delegate
/// to [`PyDict_GetItem`] (same borrowed-reference, error-suppressing
/// contract). Cython's `__Pyx_PyDict_GetItem` fast path links this.
#[no_mangle]
pub unsafe extern "C" fn _PyDict_GetItem_KnownHash(
    d: *mut PyObject,
    k: *mut PyObject,
    _hash: PyHashT,
) -> *mut PyObject {
    unsafe { PyDict_GetItem(d, k) }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_GetItemString(d: *mut PyObject, k: *const c_char) -> *mut PyObject {
    if d.is_null() || k.is_null() {
        return ptr::null_mut();
    }
    let key = unsafe { CStr::from_ptr(k) }.to_string_lossy().into_owned();
    let dict = match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => rc,
        Object::Module(m) => m.dict.clone(),
        _ => return ptr::null_mut(),
    };
    let key_id = dict_key_id(&Object::from_str(key.clone()));
    let result = dict.borrow().get(&DictKey(Object::from_str(key))).cloned();
    match result {
        Some(v) => dict_borrowed_box(d, key_id, v),
        None => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_DelItem(d: *mut PyObject, k: *mut PyObject) -> c_int {
    if d.is_null() || k.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let key = unsafe { crate::object::clone_object(k) };
            if rc.borrow_mut().shift_remove(&DictKey(key)).is_some() {
                unsafe { crate::mirror::sync_dict_ma_used(d) };
                0
            } else {
                crate::errors::set_value_error("KeyError");
                -1
            }
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_DelItemString(d: *mut PyObject, k: *const c_char) -> c_int {
    if d.is_null() || k.is_null() {
        return -1;
    }
    let key = unsafe { CStr::from_ptr(k) }.to_string_lossy().into_owned();
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            if rc
                .borrow_mut()
                .shift_remove(&DictKey(Object::from_str(key)))
                .is_some()
            {
                unsafe { crate::mirror::sync_dict_ma_used(d) };
                0
            } else {
                -1
            }
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Contains(d: *mut PyObject, k: *mut PyObject) -> c_int {
    if d.is_null() || k.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(d) };
    match as_dict_rc(&obj) {
        Some(rc) => {
            let key = unsafe { crate::object::clone_object(k) };
            let key_id = dict_key_id(&key);
            let has = rc.borrow().contains_key(&DictKey(key));
            engine_dict_trace("CONTAINS", d, &key_id, has, ptr::null_mut());
            i32::from(has)
        }
        None => {
            if std::env::var_os("WEAVEPY_TRACE_NULL").is_some() {
                eprintln!(
                    "[WEAVEPY_TRACE_NULL] PyDict_Contains: d is not a dict, got {}",
                    obj.type_name()
                );
            }
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Size(d: *mut PyObject) -> PySsizeT {
    if d.is_null() {
        return -1;
    }
    match as_dict_rc(&unsafe { crate::object::clone_object(d) }) {
        Some(rc) => rc.borrow().len() as PySsizeT,
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Next(
    d: *mut PyObject,
    ppos: *mut PySsizeT,
    pkey: *mut *mut PyObject,
    pvalue: *mut *mut PyObject,
) -> c_int {
    if d.is_null() || ppos.is_null() {
        return 0;
    }
    let dict = match as_dict_rc(&unsafe { crate::object::clone_object(d) }) {
        Some(rc) => rc,
        None => return 0,
    };
    let pos = unsafe { *ppos };
    // Clone the entry out and *drop the dict borrow* before minting any
    // boxes: `dict_borrowed_box` can re-enter (into_owned / cache ops),
    // and we must not hold the RefCell borrow across that.
    let entry = {
        let dict_borrow = dict.borrow();
        if pos < 0 || pos >= dict_borrow.len() as PySsizeT {
            return 0;
        }
        dict_borrow
            .get_index(pos as usize)
            .map(|(k, v)| (k.0.clone(), v.clone()))
    };
    match entry {
        Some((key_obj, val_obj)) => {
            // CPython's `PyDict_Next` hands back *borrowed* references the
            // dict keeps alive — the caller may then `Py_INCREF` them (as
            // Cython's `__Pyx_PyVectorcall_FastCallDict_kw` does for the
            // `size=` kwarg). Mint each box once and pin it in the dict's
            // borrowed-box cache for the dict's lifetime; never hand back a
            // box we immediately free (that was a use-after-free that
            // crashed `Generator.integers`).
            let key_id = dict_key_id(&key_obj);
            unsafe {
                *ppos = pos + 1;
                if !pkey.is_null() {
                    *pkey = dict_borrowed_key_box(d, key_id.clone(), key_obj);
                }
                if !pvalue.is_null() {
                    *pvalue = dict_borrowed_box(d, key_id, val_obj);
                }
            }
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Keys(d: *mut PyObject) -> *mut PyObject {
    if d.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let keys: Vec<Object> = rc.borrow().keys().map(|k| k.0.clone()).collect();
            crate::object::into_owned(Object::new_list(keys))
        }
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Values(d: *mut PyObject) -> *mut PyObject {
    if d.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            // RFC 0046 (wave 4): hand back *new references to the dict's
            // canonical value boxes* (the very pointers `PyDict_GetItem`
            // returns, pinned in `DICT_BOX_CACHE` for the dict's lifetime) —
            // never freshly-minted throwaway boxes. CPython's `PyDict_Values`
            // returns new refs to the same objects the dict still owns, and
            // numpy's `resolve_implementation_info` leans on that: it borrows
            // an element of `PyDict_Values(ufunc->_loops)` into `*out_info`,
            // then `Py_DECREF`s the values list. A throwaway box owned solely
            // by that list is freed by the decref and the borrowed pointer
            // dangles — a use-after-free that surfaces as a NULL `ob_type`
            // read deep in ufunc dispatch (`promote_and_get_info_and_ufuncimpl`).
            // The cache keeps each box alive until the dict itself is freed.
            let pairs: Vec<(String, Object)> = rc
                .borrow()
                .iter()
                .map(|(k, v)| (dict_key_id(&k.0), v.clone()))
                .collect();
            let boxes: Vec<*mut PyObject> = pairs
                .into_iter()
                .map(|(kid, v)| {
                    let b = dict_borrowed_box(d, kid, v);
                    unsafe { crate::object::Py_IncRef(b) };
                    b
                })
                .collect();
            unsafe { list_owning_boxes(boxes) }
        }
        _ => ptr::null_mut(),
    }
}

/// Build a faithful list that *owns* the supplied boxes outright: each box
/// is written straight into the list's `ob_item` buffer — the buffer a
/// stock `PyList_GET_ITEM` / `PySequence_Fast_GET_ITEM` reads and that
/// WeavePy's `read_list` treats as authoritative — so the elements keep
/// their exact pointer identity (no per-crossing rebox). One reference per
/// box is consumed; `free_mirror` releases them when the list dies.
///
/// # Safety
/// Each pointer in `boxes` must be a live owned reference the caller hands
/// over.
unsafe fn list_owning_boxes(boxes: Vec<*mut PyObject>) -> *mut PyObject {
    let n = boxes.len();
    let list = unsafe { PyList_New(n as PySsizeT) };
    let ok = !list.is_null() && unsafe { crate::mirror::is_faithful_list(list) };
    let base = if ok {
        let lo = list as *mut crate::layout::PyListObject;
        unsafe { (*lo).ob_item }
    } else {
        ptr::null_mut()
    };
    if base.is_null() {
        for b in boxes {
            if !b.is_null() {
                unsafe { crate::object::Py_DecRef(b) };
            }
        }
        return list;
    }
    for (i, b) in boxes.into_iter().enumerate() {
        // The slot currently holds an immortal `None` placeholder from
        // `PyList_New` (no decref needed — `None` is immortal); overwrite it
        // so the buffer owns exactly the boxes handed in.
        unsafe { *base.add(i) = b };
    }
    list
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Items(d: *mut PyObject) -> *mut PyObject {
    if d.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let items: Vec<Object> = rc
                .borrow()
                .iter()
                .map(|(k, v)| Object::new_tuple(vec![k.0.clone(), v.clone()]))
                .collect();
            crate::object::into_owned(Object::new_list(items))
        }
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Copy(d: *mut PyObject) -> *mut PyObject {
    if d.is_null() {
        return ptr::null_mut();
    }
    match as_dict_rc(&unsafe { crate::object::clone_object(d) }) {
        Some(rc) => {
            let new_d: DictData = rc.borrow().clone();
            crate::object::into_owned(Object::Dict(Rc::new(RefCell::new(new_d))))
        }
        None => {
            crate::errors::set_type_error("expected dict");
            ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Update(d: *mut PyObject, other: *mut PyObject) -> c_int {
    unsafe { PyDict_Merge(d, other, 1) }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Merge(
    a: *mut PyObject,
    b: *mut PyObject,
    override_: c_int,
) -> c_int {
    if a.is_null() || b.is_null() {
        return -1;
    }
    let dst = match as_dict_rc(&unsafe { crate::object::clone_object(a) }) {
        Some(rc) => rc,
        None => {
            crate::errors::set_type_error("expected dict");
            return -1;
        }
    };
    let src_dict = match as_dict_rc(&unsafe { crate::object::clone_object(b) }) {
        Some(rc) => rc,
        None => {
            crate::errors::set_type_error("expected dict");
            return -1;
        }
    };
    let src_snapshot = src_dict.borrow().clone();
    {
        let mut dst_borrow = dst.borrow_mut();
        for (k, v) in src_snapshot {
            if override_ != 0 || !dst_borrow.contains_key(&k) {
                dst_borrow.insert(k, v);
            }
        }
    }
    unsafe { crate::mirror::sync_dict_ma_used(a) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Clear(d: *mut PyObject) -> c_int {
    if d.is_null() {
        return -1;
    }
    match as_dict_rc(&unsafe { crate::object::clone_object(d) }) {
        Some(rc) => {
            rc.borrow_mut().clear();
            unsafe { crate::mirror::sync_dict_ma_used(d) };
            0
        }
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Dict(_)).into()
}

// ----------------------------------------------------------------
// PySet / PyFrozenSet.
// ----------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn PySet_New(iterable: *mut PyObject) -> *mut PyObject {
    let mut data = SetData::default();
    if !iterable.is_null() {
        seed_set(&mut data, iterable);
    }
    crate::object::into_owned(Object::Set(Rc::new(RefCell::new(data))))
}

#[no_mangle]
pub unsafe extern "C" fn PyFrozenSet_New(iterable: *mut PyObject) -> *mut PyObject {
    let mut data = SetData::default();
    if !iterable.is_null() {
        seed_set(&mut data, iterable);
    }
    crate::object::into_owned(Object::FrozenSet(Rc::new(
        weavepy_vm::object::FrozenSetObj::new(data),
    )))
}

fn seed_set(data: &mut SetData, iterable: *mut PyObject) {
    match unsafe { crate::object::clone_object(iterable) } {
        Object::List(rc) => {
            for item in rc.borrow().iter() {
                data.insert(DictKey(item.clone()));
            }
        }
        Object::Tuple(items) => {
            for item in items.iter() {
                data.insert(DictKey(item.clone()));
            }
        }
        // Any other iterable — a `dict` (its *keys*), `set`/`frozenset`, `str`,
        // `range`, a generator, or a foreign extension iterable — is drained
        // through the iteration protocol, exactly as CPython's
        // `set_update_internal` does for the non-list/tuple case. The prior
        // `_ => {}` silently produced an *empty* set, so a Cython `set(x)`
        // (which Cython compiles to `PySet_New(x)`) over anything but a
        // list/tuple came back empty — e.g. pandas' `Timedelta(days=1)` kwarg
        // validation does `set(kwargs)` over a dict.
        other => {
            if std::env::var_os("WEAVEPY_TRACE_SETSEED").is_some() {
                eprintln!(
                    "[WEAVEPY_TRACE_SETSEED] seed_set general-iter over {}",
                    other.type_name()
                );
            }
            let it = unsafe { crate::abstract_::PyObject_GetIter(iterable) };
            if it.is_null() {
                return;
            }
            loop {
                let item = unsafe { crate::abstract_::PyIter_Next(it) };
                if item.is_null() {
                    break;
                }
                data.insert(DictKey(unsafe { crate::object::clone_object(item) }));
                unsafe { crate::object::Py_DecRef(item) };
            }
            unsafe { crate::object::Py_DecRef(it) };
        }
    }
}

/// The concrete set payload behind `o`: a plain `set`, or the native
/// payload of a **set-subclass instance**. CPython's concrete `PySet_*`
/// API operates on subclasses too (the instance *is* a `PySetObject`) —
/// sqlalchemy's Cython `OrderedSet(set)` calls `PySet_Add(self, …)`.
fn as_set_rc(o: &Object) -> Option<Rc<RefCell<SetData>>> {
    match o {
        Object::Set(rc) => Some(rc.clone()),
        Object::Instance(inst) => match inst.native.get() {
            Some(Object::Set(rc)) => Some(rc.clone()),
            _ => None,
        },
        _ => None,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Add(s: *mut PyObject, item: *mut PyObject) -> c_int {
    if s.is_null() || item.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(s) };
    if let Some(rc) = as_set_rc(&obj) {
        rc.borrow_mut()
            .insert(DictKey(unsafe { crate::object::clone_object(item) }));
        unsafe { crate::mirror::sync_set_used(s) };
        return 0;
    }
    match obj {
        // CPython's `PySet_Add` explicitly accepts a *frozenset* too — the
        // documented "fill it before it's exposed" idiom. mypyc's
        // `CPyStatics_Initialize` builds every frozenset literal with
        // `PyFrozenSet_New(NULL)` + `PySet_Add` (RFC 0055 WS5). The
        // payload is immutable, so rewrite the box with an extended copy.
        Object::FrozenSet(fs) => {
            let mut data: SetData = SetData::clone(&fs);
            data.insert(DictKey(unsafe { crate::object::clone_object(item) }));
            unsafe {
                crate::object::set_payload(
                    s,
                    Object::FrozenSet(Rc::new(weavepy_vm::object::FrozenSetObj::new(data))),
                );
                crate::mirror::sync_set_used(s);
            }
            0
        }
        other => {
            crate::errors::set_type_error(format!(
                "PySet_Add: expected set, got {}",
                other.type_name()
            ));
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Contains(s: *mut PyObject, item: *mut PyObject) -> c_int {
    if s.is_null() || item.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(s) };
    if let Some(rc) = as_set_rc(&obj) {
        return i32::from(
            rc.borrow()
                .contains(&DictKey(unsafe { crate::object::clone_object(item) })),
        );
    }
    match obj {
        Object::FrozenSet(s) => {
            i32::from(s.contains(&DictKey(unsafe { crate::object::clone_object(item) })))
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Discard(s: *mut PyObject, item: *mut PyObject) -> c_int {
    if s.is_null() || item.is_null() {
        return -1;
    }
    match as_set_rc(&unsafe { crate::object::clone_object(s) }) {
        Some(rc) => {
            rc.borrow_mut()
                .shift_remove(&DictKey(unsafe { crate::object::clone_object(item) }));
            unsafe { crate::mirror::sync_set_used(s) };
            0
        }
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Size(s: *mut PyObject) -> PySsizeT {
    if s.is_null() {
        return -1;
    }
    let obj = unsafe { crate::object::clone_object(s) };
    if let Some(rc) = as_set_rc(&obj) {
        return rc.borrow().len() as PySsizeT;
    }
    match obj {
        Object::FrozenSet(s) => s.len() as PySsizeT,
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Set(_)).into()
}

#[no_mangle]
pub unsafe extern "C" fn PyFrozenSet_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(
        unsafe { crate::object::clone_object(o) },
        Object::FrozenSet(_)
    )
    .into()
}

/// `PyTuple_Pack(n, …)` — variadic helper supplied by the C shim.
/// We expose a non-variadic Rust core that the shim invokes with
/// the args already collected into a slice.
#[no_mangle]
pub unsafe extern "C" fn _WeavePy_TuplePackFromArray(
    n: PySsizeT,
    items: *const *mut PyObject,
) -> *mut PyObject {
    if n < 0 {
        return ptr::null_mut();
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let p = unsafe { *items.add(i as usize) };
        out.push(if p.is_null() {
            Object::None
        } else {
            unsafe { crate::object::clone_object(p) }
        });
    }
    crate::object::into_owned(Object::new_tuple(out))
}

// ----------------------------------------------------------------
// RFC 0029 — additional `PyDict_*` / `PyList_*` / `PyTuple_*` /
// `PySet_*` surface.
// ----------------------------------------------------------------

/// Total-order compare helper for the new `PyList_Sort`.
/// Falls back to comparing repr strings for values whose
/// ordering Python would consider incomparable; this differs
/// from CPython (which would raise TypeError) but yields a
/// stable, panic-free sort.
fn natural_cmp(a: &Object, b: &Object) -> std::cmp::Ordering {
    use num_traits::ToPrimitive;
    use std::cmp::Ordering;
    match (a, b) {
        (Object::Int(x), Object::Int(y)) => x.cmp(y),
        (Object::Float(x), Object::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        // Separate arms: an or-pattern here would erase operand order and
        // answer `2.0 <=> 1` as `1 <=> 2.0`.
        (Object::Int(x), Object::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (Object::Float(x), Object::Int(y)) => {
            x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal)
        }
        (Object::Bool(x), Object::Bool(y)) => x.cmp(y),
        (Object::Str(x), Object::Str(y)) => x.cmp(y),
        (Object::Bytes(x), Object::Bytes(y)) => x.cmp(y),
        (Object::Long(x), Object::Long(y)) => x.cmp(y),
        (Object::Long(x), Object::Int(y)) => x.to_i64().map_or(Ordering::Greater, |v| v.cmp(y)),
        (Object::Int(x), Object::Long(y)) => {
            y.to_i64().map_or(Ordering::Less, |v| x.cmp(&v)).reverse()
        }
        _ => {
            // Fall back to repr; not Python-faithful but stable.
            a.repr().cmp(&b.repr())
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_SetDefault(
    d: *mut PyObject,
    k: *mut PyObject,
    default: *mut PyObject,
) -> *mut PyObject {
    if d.is_null() || k.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let key = DictKey(unsafe { crate::object::clone_object(k) });
            let mut map = rc.borrow_mut();
            if let Some(v) = map.get(&key) {
                let v = v.clone();
                drop(map);
                crate::object::into_owned(v)
            } else {
                let default_o = if default.is_null() {
                    Object::None
                } else {
                    unsafe { crate::object::clone_object(default) }
                };
                map.insert(key, default_o.clone());
                drop(map);
                unsafe { crate::mirror::sync_dict_ma_used(d) };
                crate::object::into_owned(default_o)
            }
        }
        _ => ptr::null_mut(),
    }
}

/// Public CPython 3.13 API — `int PyDict_Pop(dict, key, PyObject **result)`.
///
/// Removes `key` from `dict`. On success writes the removed value to
/// `*result` (ownership transferred to the caller) and returns `1`; if the
/// key is absent writes `NULL` to `*result` and returns `0`; on error writes
/// `NULL` and returns `-1`. `result` may be `NULL`, in which case the popped
/// value is simply released.
///
/// The 3rd parameter is an **out-pointer**, *not* a default value — that is
/// the (older, private) `_PyDict_Pop` contract, exposed separately below.
/// Cython's keyword parser (`__Pyx_ParseKeywordDictToDict`) calls this
/// function with `&value` and reads back `*result`; getting the signature
/// wrong left every `**kwds` argument uninitialised (garbage), which crashed
/// e.g. `pandas.offsets.DateOffset(n=…)` on the following `self.__init__`.
///
/// # Safety
/// `result`, if non-null, must be a writable `*mut PyObject` slot.
#[no_mangle]
pub unsafe extern "C" fn PyDict_Pop(
    d: *mut PyObject,
    k: *mut PyObject,
    result: *mut *mut PyObject,
) -> c_int {
    unsafe {
        if !result.is_null() {
            *result = ptr::null_mut();
        }
    }
    if d.is_null() || k.is_null() {
        crate::errors::set_type_error("PyDict_Pop: NULL argument");
        return -1;
    }
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let key = DictKey(unsafe { crate::object::clone_object(k) });
            let popped = rc.borrow_mut().shift_remove(&key);
            match popped {
                Some(v) => {
                    unsafe { crate::mirror::sync_dict_ma_used(d) };
                    let p = crate::object::into_owned(v);
                    unsafe {
                        if result.is_null() {
                            crate::object::Py_DecRef(p);
                        } else {
                            *result = p;
                        }
                    }
                    1
                }
                None => 0,
            }
        }
        _ => {
            crate::errors::set_type_error("PyDict_Pop: not a dict");
            -1
        }
    }
}

/// Public CPython 3.13 API — `int PyDict_PopString(dict, const char *key,
/// PyObject **result)`. Identical to [`PyDict_Pop`] but takes the key as a
/// UTF-8 C string.
///
/// # Safety
/// `key` must be a valid NUL-terminated C string; `result`, if non-null,
/// must be a writable `*mut PyObject` slot.
#[no_mangle]
pub unsafe extern "C" fn PyDict_PopString(
    d: *mut PyObject,
    key: *const c_char,
    result: *mut *mut PyObject,
) -> c_int {
    unsafe {
        if !result.is_null() {
            *result = ptr::null_mut();
        }
    }
    if key.is_null() {
        crate::errors::set_type_error("PyDict_PopString: NULL key");
        return -1;
    }
    let s = match unsafe { CStr::from_ptr(key) }.to_str() {
        Ok(s) => s.to_owned(),
        Err(_) => {
            crate::errors::set_type_error("PyDict_PopString: key is not valid UTF-8");
            return -1;
        }
    };
    let key_obj = crate::object::into_owned(Object::from_str(s));
    let r = unsafe { PyDict_Pop(d, key_obj, result) };
    unsafe { crate::object::Py_DecRef(key_obj) };
    r
}

/// Private CPython API — `PyObject *_PyDict_Pop(dict, key, default_value)`.
///
/// Removes `key` and returns its value (ownership transferred). If the key is
/// absent, returns a new reference to `default_value` when it is non-null, or
/// sets `KeyError` and returns `NULL` when it is null. This is the historical
/// signature WeavePy exposed under the `PyDict_Pop` name before 3.13 promoted
/// the out-pointer form to public API.
///
/// # Safety
/// `d`/`k` must be valid `*mut PyObject`; `default_value` may be null.
#[no_mangle]
pub unsafe extern "C" fn _PyDict_Pop(
    d: *mut PyObject,
    k: *mut PyObject,
    default_value: *mut PyObject,
) -> *mut PyObject {
    if d.is_null() || k.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let key = DictKey(unsafe { crate::object::clone_object(k) });
            let popped = rc.borrow_mut().shift_remove(&key);
            match popped {
                Some(v) => {
                    unsafe { crate::mirror::sync_dict_ma_used(d) };
                    crate::object::into_owned(v)
                }
                None => {
                    if default_value.is_null() {
                        crate::errors::set_pending(
                            Some(weavepy_vm::builtin_types::builtin_types().key_error.clone()),
                            key.0,
                        );
                        ptr::null_mut()
                    } else {
                        unsafe { crate::object::Py_IncRef(default_value) };
                        default_value
                    }
                }
            }
        }
        _ => ptr::null_mut(),
    }
}

// ----- PyList expanded -----

#[no_mangle]
pub unsafe extern "C" fn PyList_Extend(list: *mut PyObject, iterable: *mut PyObject) -> c_int {
    if list.is_null() || iterable.is_null() {
        return -1;
    }
    let mut new_items: Vec<Object> = match unsafe { crate::object::clone_object(iterable) } {
        Object::List(rc) => rc.borrow().clone(),
        Object::Tuple(items) => items.iter().cloned().collect(),
        // Any other iterable drains through the iterator protocol, exactly
        // like CPython's `list_extend` (Cython's fused-signature dispatch
        // extends a list with a `dict_keys` view, generators appear in
        // `list.extend` fast paths, …). `collect_iterable` leaves the
        // TypeError pending when the object isn't iterable at all.
        _ => match unsafe { crate::abstract_::collect_iterable(iterable) } {
            Some(items) => items,
            None => return -1,
        },
    };
    // RFC 0046 (wave 4): append each element to the inline `ob_item`
    // buffer (the source of truth), materialising it as an owned C
    // reference and handing the list its own reference.
    if unsafe { crate::mirror::is_faithful_list(list) } {
        for item in new_items {
            let p = crate::object::into_owned(item);
            unsafe { crate::mirror::list_append(list, p) };
            unsafe { crate::object::Py_DecRef(p) };
        }
        return 0;
    }
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => {
            rc.borrow_mut().append(&mut new_items);
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn _PyList_Extend(list: *mut PyObject, iterable: *mut PyObject) -> c_int {
    unsafe { PyList_Extend(list, iterable) }
}

// ----- PyTuple expanded -----

#[no_mangle]
pub unsafe extern "C" fn _PyTuple_Resize(_t: *mut *mut PyObject, _new_size: PySsizeT) -> c_int {
    // Tuples are immutable; the only legal case is shrinking a
    // tuple the caller still has a unique reference to. We
    // approximate by allocating a fresh truncated tuple and
    // letting the caller replace its pointer.
    -1
}

// ----- PySet expanded -----

#[no_mangle]
pub unsafe extern "C" fn PySet_Pop(s: *mut PyObject) -> *mut PyObject {
    if s.is_null() {
        return ptr::null_mut();
    }
    match as_set_rc(&unsafe { crate::object::clone_object(s) }) {
        Some(rc) => {
            let mut set = rc.borrow_mut();
            let first = set.iter().next().cloned();
            match first {
                Some(k) => {
                    set.shift_remove(&k);
                    drop(set);
                    unsafe { crate::mirror::sync_set_used(s) };
                    crate::object::into_owned(k.0)
                }
                None => {
                    crate::errors::set_pending(
                        Some(weavepy_vm::builtin_types::builtin_types().key_error.clone()),
                        Object::from_static("pop from an empty set"),
                    );
                    ptr::null_mut()
                }
            }
        }
        None => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Clear(s: *mut PyObject) -> c_int {
    if s.is_null() {
        return -1;
    }
    match as_set_rc(&unsafe { crate::object::clone_object(s) }) {
        Some(rc) => {
            rc.borrow_mut().clear();
            unsafe { crate::mirror::sync_set_used(s) };
            0
        }
        None => -1,
    }
}

// ----- PySequence_Fast helpers -----
//
// CPython's `PySequence_Fast(o, msg)` returns an *owned reference*
// to a list/tuple "view" over `o`. Callers then call
// `PySequence_Fast_GET_ITEM` (a macro) and
// `PySequence_Fast_GET_SIZE` (also a macro) without needing
// further borrow-tracking. We expose function-shaped versions
// because macros don't bind to dlopen'd symbols.

#[no_mangle]
pub unsafe extern "C" fn PySequence_Fast(o: *mut PyObject, msg: *const c_char) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::List(_) | Object::Tuple(_) => unsafe {
            crate::object::Py_IncRef(o);
            o
        },
        Object::Str(_) => {
            crate::errors::set_type_error(if msg.is_null() {
                "expected list or tuple".to_owned()
            } else {
                unsafe { CStr::from_ptr(msg) }
                    .to_string_lossy()
                    .into_owned()
            });
            ptr::null_mut()
        }
        _ => {
            // Try to coerce iterables into a list.
            match unsafe { crate::object::clone_object(o) } {
                Object::Set(rc) => {
                    let items: Vec<Object> = rc.borrow().iter().map(|k| k.0.clone()).collect();
                    crate::object::into_owned(Object::new_list(items))
                }
                Object::FrozenSet(s) => {
                    let items: Vec<Object> = s.iter().map(|k| k.0.clone()).collect();
                    crate::object::into_owned(Object::new_list(items))
                }
                Object::Dict(rc) => {
                    let items: Vec<Object> = rc.borrow().keys().map(|k| k.0.clone()).collect();
                    crate::object::into_owned(Object::new_list(items))
                }
                // Any other iterable (a `cdef class` instance, a foreign
                // extension object with `__iter__`, a generator, …) is
                // coerced through its iterator protocol, matching CPython's
                // `PySequence_Fast` (which calls `PySequence_List` for the
                // non-fast path). The previous hard error broke
                // `PySequence_Fast(cdef_instance)`.
                _ => match unsafe { crate::abstract_::collect_iterable(o) } {
                    Some(items) => crate::object::into_owned(Object::new_list(items)),
                    None => {
                        if !msg.is_null() && crate::errors::pending().is_some() {
                            // Replace the generic iterator TypeError with the
                            // caller-supplied context message, as CPython does.
                            crate::errors::clear_thread_local();
                            crate::errors::set_type_error(
                                unsafe { CStr::from_ptr(msg) }
                                    .to_string_lossy()
                                    .into_owned(),
                            );
                        }
                        ptr::null_mut()
                    }
                },
            }
        }
    }
}

/// `PySequence_Fast_GET_SIZE` — sized accessor companion.
#[no_mangle]
pub unsafe extern "C" fn PySequence_Fast_GET_SIZE(o: *mut PyObject) -> PySsizeT {
    if o.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(o) } {
        Object::List(rc) => rc.borrow().len() as PySsizeT,
        Object::Tuple(items) => items.len() as PySsizeT,
        _ => -1,
    }
}

/// `PySequence_Fast_GET_ITEM` — borrow accessor companion.
#[no_mangle]
pub unsafe extern "C" fn PySequence_Fast_GET_ITEM(
    o: *mut PyObject,
    idx: PySsizeT,
) -> *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    let item = match unsafe { crate::object::clone_object(o) } {
        Object::List(rc) => rc.borrow().get(idx as usize).cloned(),
        Object::Tuple(items) => items.get(idx as usize).cloned(),
        _ => None,
    };
    match item {
        Some(v) => intern_borrowed_at(o, idx, v),
        None => ptr::null_mut(),
    }
}

/// `PySequence_Fast_ITEMS` — return a pointer to the items
/// array. Caller treats this as borrowed.
#[no_mangle]
pub unsafe extern "C" fn PySequence_Fast_ITEMS(o: *mut PyObject) -> *mut *mut PyObject {
    if o.is_null() {
        return ptr::null_mut();
    }
    // We can't safely hand out a pointer to our heap-stored
    // Object array. Return NULL — callers should fall back to
    // `PySequence_Fast_GET_ITEM(o, i)`.
    ptr::null_mut()
}

// ----- PyList_GET_ITEM / PyList_SET_ITEM / PyTuple_GET_ITEM /
// PyTuple_SET_ITEM as function exports. CPython exposes these
// as macros; we mirror the function-call ABI so dlopen'd
// extensions that #include <Python.h> see something to call.

#[no_mangle]
pub unsafe extern "C" fn _PyList_GET_ITEM(list: *mut PyObject, idx: PySsizeT) -> *mut PyObject {
    unsafe { PyList_GetItem(list, idx) }
}

#[no_mangle]
pub unsafe extern "C" fn _PyList_SET_ITEM(
    list: *mut PyObject,
    idx: PySsizeT,
    item: *mut PyObject,
) -> c_int {
    unsafe { PyList_SetItem(list, idx, item) }
}

#[no_mangle]
pub unsafe extern "C" fn _PyTuple_GET_ITEM(t: *mut PyObject, idx: PySsizeT) -> *mut PyObject {
    unsafe { PyTuple_GetItem(t, idx) }
}

#[no_mangle]
pub unsafe extern "C" fn _PyTuple_SET_ITEM(
    t: *mut PyObject,
    idx: PySsizeT,
    item: *mut PyObject,
) -> c_int {
    unsafe { PyTuple_SetItem(t, idx, item) }
}
