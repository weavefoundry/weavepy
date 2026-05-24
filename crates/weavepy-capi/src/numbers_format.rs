//! Tiny float formatter used by [`crate::abstract_::PyObject_Repr`].
//!
//! CPython's `repr(float)` uses `repr(n)` which is implemented by
//! the `dtoa_short` shortest-decimal algorithm. We approximate with
//! Rust's default `{}` (which is also shortest-decimal for most
//! values) plus the `1.0` suffix rule that Python always renders.

pub fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "inf".to_owned()
        } else {
            "-inf".to_owned()
        };
    }
    let s = format!("{f}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}
