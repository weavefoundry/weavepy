//! `PyCapsule` — opaque pointer wrapper used by extensions to
//! publish C-level helpers to other extensions.

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::ptr;

use weavepy_vm::object::Object;

use crate::object::{PyObject, PyObjectBox};

#[repr(C)]
struct CapsuleState {
    pointer: *mut std::ffi::c_void,
    name: Option<Box<[u8]>>,
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
    Box::into_raw(bx) as *mut PyObject
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
        return -1;
    };
    unsafe { (*state_ptr).pointer = pointer };
    0
}
