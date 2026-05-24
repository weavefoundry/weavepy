//! `PyList_*`, `PyTuple_*`, `PyDict_*`, `PySet_*`, `PyFrozenSet_*`.
//!
//! Containers wrap WeavePy's native [`Object::List`], [`Object::Tuple`],
//! [`Object::Dict`], [`Object::Set`], [`Object::FrozenSet`] variants
//! through the same boxing machinery as scalars. Mutating operations
//! borrow the inner `RefCell` for the duration of the call.

use std::cell::RefCell;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::rc::Rc;

use weavepy_vm::object::{DictData, DictKey, Object, SetData};

use crate::object::{PyObject, PySsizeT};

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
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => {
            let v = rc.borrow();
            if index < 0 || index >= v.len() as PySsizeT {
                crate::errors::set_value_error("list index out of range");
                ptr::null_mut()
            } else {
                let p = crate::object::into_owned(v[index as usize].clone());
                // PyList_GetItem returns a borrowed reference: undo
                // the +1 we just added.
                unsafe { crate::object::Py_DecRef(p) };
                p
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
    match unsafe { crate::object::clone_object(list) } {
        Object::List(rc) => {
            rc.borrow_mut().reverse();
            0
        }
        _ => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn PyList_Sort(_list: *mut PyObject) -> c_int {
    // Generic sort needs the VM's comparison machinery; for the
    // foundation we reject non-trivial sorts.
    crate::errors::set_runtime_error("PyList_Sort: not supported in WeavePy's C-API foundation");
    -1
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
    // We encode an in-flight tuple as a list; PyTuple_SetItem
    // mutates by index; the result is shipped as an Object::Tuple
    // by the time C extensions return it.
    let len = n.max(0) as usize;
    crate::object::into_owned(Object::new_list(vec![Object::None; len]))
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
    let result = match unsafe { crate::object::clone_object(tuple) } {
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
                let bx = &mut *(tuple as *mut crate::object::PyObjectBox);
                bx.payload.obj = Object::Tuple(Rc::from(v.into_boxed_slice()));
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
    let item = match unsafe { crate::object::clone_object(tuple) } {
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
    let p = crate::object::into_owned(item);
    unsafe { crate::object::Py_DecRef(p) };
    p
}

#[no_mangle]
pub unsafe extern "C" fn PyTuple_Size(tuple: *mut PyObject) -> PySsizeT {
    if tuple.is_null() {
        return -1;
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
            rc.borrow_mut().insert(DictKey(key), val);
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
            rc.borrow_mut().insert(DictKey(Object::from_str(key)), val);
            0
        }
        Object::Module(m) => {
            // Convenience: PyDict_SetItemString on a module's dict
            // is a common idiom.
            let val = unsafe { crate::object::clone_object(v) };
            m.dict
                .borrow_mut()
                .insert(DictKey(Object::from_str(key)), val);
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
            let result = rc.borrow().get(&DictKey(key)).cloned();
            match result {
                Some(v) => {
                    let p = crate::object::into_owned(v);
                    unsafe { crate::object::Py_DecRef(p) };
                    p
                }
                None => ptr::null_mut(),
            }
        }
        _ => ptr::null_mut(),
    }
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
    let result = dict.borrow().get(&DictKey(Object::from_str(key))).cloned();
    match result {
        Some(v) => {
            let p = crate::object::into_owned(v);
            unsafe { crate::object::Py_DecRef(p) };
            p
        }
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
            let values: Vec<Object> = rc.borrow().values().cloned().collect();
            crate::object::into_owned(Object::new_list(values))
        }
        _ => ptr::null_mut(),
    }
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
    crate::object::into_owned(Object::FrozenSet(Rc::new(data)))
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
