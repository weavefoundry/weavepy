//! `PySlice_New` / `PySlice_Check`.

use std::os::raw::c_int;
use std::rc::Rc;

use weavepy_vm::object::{Object, PySlice};

use crate::object::PyObject;

#[no_mangle]
pub unsafe extern "C" fn PySlice_New(
    start: *mut PyObject,
    stop: *mut PyObject,
    step: *mut PyObject,
) -> *mut PyObject {
    let load = |p: *mut PyObject| {
        if p.is_null() {
            Object::None
        } else {
            unsafe { crate::object::clone_object(p) }
        }
    };
    let s = PySlice {
        start: load(start),
        stop: load(stop),
        step: load(step),
    };
    crate::object::into_owned(Object::Slice(Rc::new(s)))
}

#[no_mangle]
pub unsafe extern "C" fn PySlice_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    matches!(unsafe { crate::object::clone_object(o) }, Object::Slice(_)).into()
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_LastResort() {
    // Placeholder export so tooling that scans for `_WeavePy_*`
    // symbols sees at least one even if the rest are stripped.
}
