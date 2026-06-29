//! `PyList_*`, `PyTuple_*`, `PyDict_*`, `PySet_*`, `PyFrozenSet_*`.
//!
//! Containers wrap WeavePy's native [`Object::List`], [`Object::Tuple`],
//! [`Object::Dict`], [`Object::Set`], [`Object::FrozenSet`] variants
//! through the same boxing machinery as scalars. Mutating operations
//! borrow the inner `RefCell` for the duration of the call.

use std::cell::RefCell as StdRefCell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use weavepy_vm::sync::Rc;
use weavepy_vm::sync::RefCell;

use weavepy_vm::object::{DictData, DictKey, Object, SetData};

use crate::object::{PyObject, PySsizeT};

thread_local! {
    /// Interned `*mut PyObject` cache for `PyTuple_GetItem` /
    /// `PyList_GetItem`'s "borrowed reference" contract. Without it
    /// we'd either leak fresh boxes on every call or hand callers a
    /// dangling pointer. The cache is keyed on the container's
    /// pointer + index so repeated `PyTuple_GetItem(t, 0)` calls
    /// return the same `*mut PyObject` (matching CPython).
    static BORROWED_ITEM_CACHE: StdRefCell<HashMap<(usize, isize), *mut PyObject>> =
        StdRefCell::new(HashMap::new());

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
    static DICT_BOX_CACHE: StdRefCell<HashMap<(usize, String), *mut PyObject>> =
        StdRefCell::new(HashMap::new());
}

/// A stable cache key for a dict key object (matches `==`-equal keys, the
/// dict contract numpy relies on for string / int / DType-class keys).
fn dict_key_id(key: &Object) -> String {
    match key {
        Object::Str(s) => format!("s\0{s}"),
        Object::Int(_) | Object::Long(_) | Object::Bool(_) => format!("i\0{}", key.to_str()),
        other => format!("r\0{}", other.repr()),
    }
}

/// Retain `value` (the *caller's* box) as `dict[key]`'s canonical C
/// reference, releasing any previous box for that slot. Increfs `value`
/// so it survives the caller's matching `Py_DECREF`.
fn dict_retain_value(dict: *mut PyObject, key: String, value: *mut PyObject) {
    if value.is_null() {
        return;
    }
    unsafe { crate::object::Py_IncRef(value) };
    let old = DICT_BOX_CACHE.with(|cell| cell.borrow_mut().insert((dict as usize, key), value));
    if let Some(old) = old {
        if old != value {
            unsafe { crate::object::Py_DecRef(old) };
        } else {
            // Same pointer re-stored: undo the extra incref.
            unsafe { crate::object::Py_DecRef(value) };
        }
    }
}

/// The canonical value box for `dict[key]`, if one was stored through a
/// C setter and is still live. A *borrowed* reference (not incref'd).
fn dict_cached_value(dict: *mut PyObject, key: &str) -> Option<*mut PyObject> {
    DICT_BOX_CACHE.with(|cell| cell.borrow().get(&(dict as usize, key.to_owned())).copied())
}

/// Drop every cached dict value box pinned to `container`.
fn invalidate_dict_box_cache(container: *mut PyObject) {
    let key = container as usize;
    let drained: Vec<*mut PyObject> = DICT_BOX_CACHE.with(|cell| {
        let mut map = cell.borrow_mut();
        let stale: Vec<(usize, String)> = map.keys().filter(|(c, _)| *c == key).cloned().collect();
        let mut out = Vec::with_capacity(stale.len());
        for k in stale {
            if let Some(p) = map.remove(&k) {
                out.push(p);
            }
        }
        out
    });
    for p in drained {
        unsafe { crate::object::Py_DecRef(p) };
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
    BORROWED_ITEM_CACHE.with(|cell| {
        let key = (container as usize, idx);
        let mut map = cell.borrow_mut();
        if let Some(&p) = map.get(&key) {
            return p;
        }
        let p = crate::object::into_owned(item);
        map.insert(key, p);
        p
    })
}

/// Drop every cached borrowed-reference entry pinned to `container`.
/// Called from `free_box` when the container's refcount hits zero
/// so a later allocation that lands at the same address starts
/// with a clean slate.
pub(crate) fn invalidate_borrowed_cache(container: *mut crate::object::PyObject) {
    let key = container as usize;
    // Collect-then-drop so we never hold the cache borrow while the
    // recursive `Py_DecRef` walks back into the cache (the freed
    // item itself might be a container with cached entries).
    let drained: Vec<*mut crate::object::PyObject> = BORROWED_ITEM_CACHE.with(|cell| {
        let mut map = cell.borrow_mut();
        let stale: Vec<(usize, isize)> = map.keys().filter(|(c, _)| *c == key).copied().collect();
        let mut out = Vec::with_capacity(stale.len());
        for k in stale {
            if let Some(p) = map.remove(&k) {
                out.push(p);
            }
        }
        out
    });
    for p in drained {
        unsafe { crate::object::Py_DecRef(p) };
    }
    invalidate_dict_box_cache(container);
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

#[no_mangle]
pub unsafe extern "C" fn PyList_Size(list: *mut PyObject) -> PySsizeT {
    if list.is_null() {
        return -1;
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
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let key = unsafe { crate::object::clone_object(k) };
            let val = unsafe { crate::object::clone_object(v) };
            let key_id = dict_key_id(&key);
            rc.borrow_mut().insert(DictKey(key), val);
            dict_retain_value(d, key_id, v);
            0
        }
        _ => {
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
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let val = unsafe { crate::object::clone_object(v) };
            let key_id = dict_key_id(&Object::from_str(key.clone()));
            rc.borrow_mut().insert(DictKey(Object::from_str(key)), val);
            dict_retain_value(d, key_id, v);
            0
        }
        Object::Module(m) => {
            // Convenience: PyDict_SetItemString on a module's dict
            // is a common idiom.
            let val = unsafe { crate::object::clone_object(v) };
            let key_id = dict_key_id(&Object::from_str(key.clone()));
            m.dict
                .borrow_mut()
                .insert(DictKey(Object::from_str(key)), val);
            dict_retain_value(d, key_id, v);
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
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let key = unsafe { crate::object::clone_object(k) };
            let key_id = dict_key_id(&key);
            let result = rc.borrow().get(&DictKey(key)).cloned();
            match result {
                Some(v) => dict_borrowed_box(d, key_id, v),
                None => ptr::null_mut(),
            }
        }
        _ => ptr::null_mut(),
    }
}

/// Return a *borrowed* (non-incref'd) box for a dict value: the canonical
/// box a C setter stored if one exists, otherwise a freshly minted box
/// retained in the dict cache so it lives as long as the dict (the
/// borrowed-reference contract). RFC 0046, wave 4.
fn dict_borrowed_box(dict: *mut PyObject, key_id: String, value: Object) -> *mut PyObject {
    if let Some(p) = dict_cached_value(dict, &key_id) {
        return p;
    }
    let p = crate::object::into_owned(value);
    // `dict_retain_value` increfs; balance the `into_owned` +1 so the
    // cache holds exactly one reference (released when the dict is freed).
    dict_retain_value(dict, key_id, p);
    unsafe { crate::object::Py_DecRef(p) };
    p
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
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let key = unsafe { crate::object::clone_object(k) };
            i32::from(rc.borrow().contains_key(&DictKey(key)))
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Size(d: *mut PyObject) -> PySsizeT {
    if d.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => rc.borrow().len() as PySsizeT,
        _ => -1,
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
    let dict = match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => rc,
        _ => return 0,
    };
    let pos = unsafe { *ppos };
    let dict_borrow = dict.borrow();
    if pos < 0 || pos >= dict_borrow.len() as PySsizeT {
        return 0;
    }
    let entry = dict_borrow.get_index(pos as usize);
    match entry {
        Some((k, v)) => {
            unsafe {
                *ppos = pos + 1;
                if !pkey.is_null() {
                    let p = crate::object::into_owned(k.0.clone());
                    crate::object::Py_DecRef(p);
                    *pkey = p;
                }
                if !pvalue.is_null() {
                    let p = crate::object::into_owned(v.clone());
                    crate::object::Py_DecRef(p);
                    *pvalue = p;
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
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            let new_d: DictData = rc.borrow().clone();
            crate::object::into_owned(Object::Dict(Rc::new(RefCell::new(new_d))))
        }
        _ => ptr::null_mut(),
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
    let dst = match unsafe { crate::object::clone_object(a) } {
        Object::Dict(rc) => rc,
        _ => return -1,
    };
    let src_dict = match unsafe { crate::object::clone_object(b) } {
        Object::Dict(rc) => rc,
        _ => return -1,
    };
    let src_snapshot = src_dict.borrow().clone();
    let mut dst_borrow = dst.borrow_mut();
    for (k, v) in src_snapshot {
        if override_ != 0 || !dst_borrow.contains_key(&k) {
            dst_borrow.insert(k, v);
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Clear(d: *mut PyObject) -> c_int {
    if d.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(d) } {
        Object::Dict(rc) => {
            rc.borrow_mut().clear();
            0
        }
        _ => -1,
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
    let mut data = SetData::new();
    if !iterable.is_null() {
        seed_set(&mut data, iterable);
    }
    crate::object::into_owned(Object::Set(Rc::new(RefCell::new(data))))
}

#[no_mangle]
pub unsafe extern "C" fn PyFrozenSet_New(iterable: *mut PyObject) -> *mut PyObject {
    let mut data = SetData::new();
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
        _ => {}
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Add(s: *mut PyObject, item: *mut PyObject) -> c_int {
    if s.is_null() || item.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(s) } {
        Object::Set(rc) => {
            rc.borrow_mut()
                .insert(DictKey(unsafe { crate::object::clone_object(item) }));
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Contains(s: *mut PyObject, item: *mut PyObject) -> c_int {
    if s.is_null() || item.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(s) } {
        Object::Set(rc) => i32::from(
            rc.borrow()
                .contains(&DictKey(unsafe { crate::object::clone_object(item) })),
        ),
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
    match unsafe { crate::object::clone_object(s) } {
        Object::Set(rc) => {
            rc.borrow_mut()
                .shift_remove(&DictKey(unsafe { crate::object::clone_object(item) }));
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Size(s: *mut PyObject) -> PySsizeT {
    if s.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(s) } {
        Object::Set(rc) => rc.borrow().len() as PySsizeT,
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
        (Object::Int(x), Object::Float(y)) | (Object::Float(y), Object::Int(x)) => {
            (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal)
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
                crate::object::into_owned(default_o)
            }
        }
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyDict_Pop(
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
            let popped = rc.borrow_mut().shift_remove(&key);
            match popped {
                Some(v) => crate::object::into_owned(v),
                None => {
                    if default.is_null() {
                        crate::errors::set_pending(
                            Some(weavepy_vm::builtin_types::builtin_types().key_error.clone()),
                            key.0,
                        );
                        ptr::null_mut()
                    } else {
                        unsafe { crate::object::Py_IncRef(default) };
                        default
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
        _ => {
            crate::errors::set_type_error("PyList_Extend: argument must be iterable");
            return -1;
        }
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
    match unsafe { crate::object::clone_object(s) } {
        Object::Set(rc) => {
            let mut set = rc.borrow_mut();
            let first = set.iter().next().cloned();
            match first {
                Some(k) => {
                    set.shift_remove(&k);
                    drop(set);
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
        _ => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn PySet_Clear(s: *mut PyObject) -> c_int {
    if s.is_null() {
        return -1;
    }
    match unsafe { crate::object::clone_object(s) } {
        Object::Set(rc) => {
            rc.borrow_mut().clear();
            0
        }
        _ => -1,
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
                _ => {
                    crate::errors::set_type_error(if msg.is_null() {
                        "expected list, tuple, or iterable".to_owned()
                    } else {
                        unsafe { CStr::from_ptr(msg) }
                            .to_string_lossy()
                            .into_owned()
                    });
                    ptr::null_mut()
                }
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
