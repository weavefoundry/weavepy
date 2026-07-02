//! `PySlice_*` — slice object machinery.
//!
//! Three families:
//! * Constructors and predicates ([`PySlice_New`], [`PySlice_Check`]).
//! * Component accessors ([`PySlice_Unpack`]) — the modern surface.
//! * Adjustment helpers ([`PySlice_AdjustIndices`],
//!   [`PySlice_GetIndices`], [`PySlice_GetIndicesEx`]) — the legacy
//!   numpy / scipy surface that downstream extensions rely on to
//!   translate a (start, stop, step) triple into an iteration count
//!   bounded by the source-sequence length.

use num_traits::ToPrimitive;
use std::os::raw::c_int;
use weavepy_vm::sync::Rc;

use weavepy_vm::object::{Object, PySlice};

use crate::object::PyObject;

type PySsizeT = isize;

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

/// Decode a slice object into (start, stop, step) signed offsets,
/// returning -1 on failure. Mirrors CPython 3.6+ `PySlice_Unpack`.
#[no_mangle]
pub unsafe extern "C" fn PySlice_Unpack(
    slice: *mut PyObject,
    start: *mut PySsizeT,
    stop: *mut PySsizeT,
    step: *mut PySsizeT,
) -> c_int {
    if slice.is_null() || start.is_null() || stop.is_null() || step.is_null() {
        return -1;
    }
    let s = match unsafe { crate::object::clone_object(slice) } {
        Object::Slice(s) => s,
        _ => {
            crate::errors::set_type_error("PySlice_Unpack: expected a slice");
            return -1;
        }
    };
    let resolve = |o: &Object, default: PySsizeT| -> Option<PySsizeT> {
        match o {
            Object::None => Some(default),
            Object::Int(i) => Some(*i as PySsizeT),
            Object::Long(big) => big.to_isize(),
            Object::Bool(b) => Some(if *b { 1 } else { 0 }),
            // CPython's `_PyEval_SliceIndex` accepts any object exposing
            // `__index__` — a numpy `int64`/`intp` scalar, a pandas block
            // placement — not just a native `int`. Coerce it through
            // `PyNumber_Index`; on failure clear the error it set so the
            // caller reports the slice-specific message. (pandas' `melt`,
            // groupby `apply`, and MultiIndex `loc` all slice with np ints.)
            other => {
                let p = crate::object::into_owned(other.clone());
                if p.is_null() {
                    return None;
                }
                let idx = unsafe { crate::abstract_::PyNumber_Index(p) };
                unsafe { crate::object::Py_DecRef(p) };
                if idx.is_null() {
                    crate::errors::clear_thread_local();
                    return None;
                }
                let v = match unsafe { crate::object::clone_object(idx) } {
                    Object::Int(i) => Some(i as PySsizeT),
                    Object::Long(big) => big.to_isize(),
                    Object::Bool(b) => Some(if b { 1 } else { 0 }),
                    _ => None,
                };
                unsafe { crate::object::Py_DecRef(idx) };
                v
            }
        }
    };
    let step_v = match resolve(&s.step, 1) {
        Some(0) => {
            crate::errors::set_value_error("slice step cannot be zero");
            return -1;
        }
        Some(v) => v,
        None => {
            crate::errors::set_type_error("slice step must be an integer");
            return -1;
        }
    };
    let big_default_start: PySsizeT = if step_v < 0 { PySsizeT::MAX } else { 0 };
    let big_default_stop: PySsizeT = if step_v < 0 {
        PySsizeT::MIN
    } else {
        PySsizeT::MAX
    };
    let start_v = match resolve(&s.start, big_default_start) {
        Some(v) => v,
        None => {
            crate::errors::set_type_error("slice start must be an integer");
            return -1;
        }
    };
    let stop_v = match resolve(&s.stop, big_default_stop) {
        Some(v) => v,
        None => {
            crate::errors::set_type_error("slice stop must be an integer");
            return -1;
        }
    };
    unsafe {
        *start = start_v;
        *stop = stop_v;
        *step = step_v;
    }
    0
}

/// Clamp `(start, stop, step)` to the bounds set by `length` and
/// return the resulting iteration count. CPython publishes this
/// as a static inline helper but numpy's `_multiarray_umath`
/// relies on the symbol existing as a real function.
#[no_mangle]
pub unsafe extern "C" fn PySlice_AdjustIndices(
    length: PySsizeT,
    start: *mut PySsizeT,
    stop: *mut PySsizeT,
    step: PySsizeT,
) -> PySsizeT {
    if start.is_null() || stop.is_null() {
        return 0;
    }
    let mut s = unsafe { *start };
    let mut e = unsafe { *stop };
    if step == 0 {
        return 0;
    }
    if s < 0 {
        s += length;
        if s < 0 {
            s = if step < 0 { -1 } else { 0 };
        }
    } else if s >= length {
        s = if step < 0 { length - 1 } else { length };
    }
    if e < 0 {
        e += length;
        if e < 0 {
            e = if step < 0 { -1 } else { 0 };
        }
    } else if e >= length {
        e = if step < 0 { length - 1 } else { length };
    }
    unsafe {
        *start = s;
        *stop = e;
    }
    if step < 0 {
        if e < s {
            (s - e - 1) / (-step) + 1
        } else {
            0
        }
    } else if s < e {
        (e - s - 1) / step + 1
    } else {
        0
    }
}

/// Combined "unpack + adjust" surface. Returns the iteration count
/// (>= 0) on success, -1 on failure with a Python exception set.
#[no_mangle]
pub unsafe extern "C" fn PySlice_GetIndicesEx(
    slice: *mut PyObject,
    length: PySsizeT,
    start: *mut PySsizeT,
    stop: *mut PySsizeT,
    step: *mut PySsizeT,
    slicelength: *mut PySsizeT,
) -> c_int {
    if unsafe { PySlice_Unpack(slice, start, stop, step) } < 0 {
        return -1;
    }
    let n = unsafe { PySlice_AdjustIndices(length, start, stop, *step) };
    if !slicelength.is_null() {
        unsafe { *slicelength = n };
    }
    0
}

/// Legacy form that pre-dates the unpack/adjust split. Same as
/// `PySlice_GetIndicesEx` minus the length-clamping step.
#[no_mangle]
pub unsafe extern "C" fn PySlice_GetIndices(
    slice: *mut PyObject,
    length: PySsizeT,
    start: *mut PySsizeT,
    stop: *mut PySsizeT,
    step: *mut PySsizeT,
) -> c_int {
    let mut slicelen: PySsizeT = 0;
    unsafe { PySlice_GetIndicesEx(slice, length, start, stop, step, &raw mut slicelen) }
}

#[no_mangle]
pub unsafe extern "C" fn _WeavePy_LastResort() {
    // Placeholder export so tooling that scans for `_WeavePy_*`
    // symbols sees at least one even if the rest are stripped.
}
