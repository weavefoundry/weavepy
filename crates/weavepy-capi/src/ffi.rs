//! Re-exports of the C-visible types so consumers of this crate
//! can refer to them under a stable path (e.g.
//! `weavepy_capi::ffi::PyMethodDef`) without having to know which
//! sub-module they live in.

pub use crate::module::{PyMethodDef, PyModuleDef, PyModuleDef_Base, PyModuleDef_Slot};
pub use crate::object::{PyHashT, PyObject, PyObjectBox, PySsizeT};
pub use crate::types::{PyTypeObject, PyType_Slot, PyType_Spec};
