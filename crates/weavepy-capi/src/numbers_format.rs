//! Tiny float formatter used by [`crate::abstract_::PyObject_Repr`].
//!
//! Delegates to the VM's CPython-faithful `float_repr` (shortest
//! round-trip digits with CPython's fixed/exponential switchover at
//! `decpt <= -4 || decpt > 16`). Rust's plain `{}` never switches to
//! exponential form, so `repr(1e-05)` through the C bridge printed
//! `0.00001` — pandas' `assert_almost_equal` message (built by Cython
//! via `PyFloat_Type.tp_repr`) then failed the test's regex.

pub fn format_float(f: f64) -> String {
    weavepy_vm::object::float_repr(f)
}
