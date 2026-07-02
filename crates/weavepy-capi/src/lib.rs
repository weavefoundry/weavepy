//! The WeavePy C-API.
//!
//! This crate bridges WeavePy's native [`Object`](weavepy_vm::object::Object)
//! representation to the C-API surface CPython exposes through
//! `Python.h`, so that:
//!
//! 1. A C extension compiled against WeavePy's `Python.h` produces a
//!    shared library with the same `PyInit_<modname>` entry-point
//!    convention CPython uses.
//! 2. WeavePy's import machinery (RFC 0012) can `dlopen` such a
//!    shared library and call its init function to materialise a
//!    Python module.
//! 3. Calls between C and Rust round-trip values through
//!    [`PyObject *`](object::PyObject) handles whose lifetimes are
//!    tracked by reference counting on the C side and `Rc<…>` on
//!    the Rust side.
//!
//! ## Surface area
//!
//! The exposed surface tracks CPython 3.13's `Py_LIMITED_API`
//! subset, plus a few unstable helpers that idiomatic extensions
//! reach for in practice (`PyType_FromSpec`, `PyCapsule`, the
//! buffer protocol minimum). See [`docs/rfcs/0022-c-api-foundation.md`]
//! for the full specification.
//!
//! ## Module layout
//!
//! - [`object`] — `PyObject` layout and the Rust↔C handle bridge.
//! - [`singletons`] — static `Py_None` / `Py_True` / … cells.
//! - [`types`] — `PyTypeObject`, `PyType_FromSpec`, slot tables.
//! - [`module`] — `PyModule_Create2`, `PyMethodDef` plumbing,
//!   import helpers.
//! - [`numbers`], [`strings`], [`containers`] — concrete value
//!   constructors / accessors.
//! - [`abstract_`] — protocol-style helpers (`PyObject_*`,
//!   `PyNumber_*`, `PySequence_*`, `PyMapping_*`).
//! - [`errors`] — `PyErr_*` and the `PyExc_*` exception statics.
//! - [`memory`] — `PyMem_*` and `PyObject_Malloc/Free`.
//! - [`lifecycle`] — `Py_Initialize`, GIL stubs, version helpers.
//! - [`capsule`], [`buffer`], [`slice`] — auxiliary protocols.
//! - [`argparse`] — non-variadic core that the C shim invokes.
//! - [`loader`] — `dlopen` / `PyInit_*` invocation; the bridge that
//!   turns a `.so` into an [`Object::Module`].
//! - [`interp`] — thread-local handle to the running interpreter.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(unused_unsafe)]
#![allow(improper_ctypes_definitions)]
#![allow(improper_ctypes)]
#![allow(unreachable_pub)]
#![allow(dead_code)]
#![allow(missing_debug_implementations)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::ptr_as_ptr)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::ref_as_ptr)]
#![allow(clippy::borrow_as_ptr)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::bool_to_int_with_if)]
#![allow(clippy::manual_c_str_literals)]
#![allow(clippy::ptr_eq)]
#![allow(clippy::ptr_cast_constness)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::useless_transmute)]
#![allow(clippy::transmutes_expressible_as_ptr_casts)]
#![allow(clippy::checked_conversions)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::unnecessary_debug_formatting)]
#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::doc_overindented_list_items)]
#![allow(clippy::pub_underscore_fields)]
#![allow(clippy::used_underscore_items)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::float_cmp)]
#![allow(clippy::unnecessary_lazy_evaluations)]
#![allow(clippy::drop_non_drop)]
#![allow(clippy::double_ended_iterator_last)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::new_without_default)]
#![allow(clippy::overly_complex_bool_expr)]
#![allow(clippy::nonminimal_bool)]

pub mod abstract_;
pub mod argparse;
pub mod buffer;
pub mod buffer_format;
pub mod builtin_new;
pub mod builtin_slots;
pub mod capsule;
pub mod code_obj;
pub mod containers;
pub mod datetime_api;
pub mod dunder_shim;
pub mod errors;
pub mod ffi;
pub mod force_link_table;
pub mod foreign;
pub mod gc_bridge;
pub mod genericalloc;
pub mod getset;
pub mod inherit;
pub mod instance;
pub mod interp;
pub mod layout;
pub mod lifecycle;
pub mod loader;
pub mod memory;
pub mod memoryview;
pub mod mirror;
pub mod module;
pub mod monitoring;
pub mod numbers;
pub mod numbers_format;
pub mod object;
pub mod pystate;
pub mod singletons;
pub mod slice;
pub mod slottable;
pub mod strings;
pub mod types;
pub mod vectorcall;
pub mod wave4;
pub mod wave5;
pub mod wave5_pandas;

pub use interp::{enter_extension_call, with_active, ActiveContext};
pub use loader::{load_extension_module, LoadError};

/// Public re-export so embedders can manually walk every static
/// symbol that needs to be visible to dlopen'd extensions.
///
/// The `force_link()` call keeps the symbol from being
/// garbage-collected by the linker when nothing else in `weavepy`
/// references it. Without this, a release build of an embedder
/// (e.g. the `cargo test` binary) would silently strip almost all
/// of the C-API surface because no Rust call site references it.
pub fn force_link() {
    let _ = singletons::none_ptr();
    let _ = singletons::true_ptr();
    let _ = singletons::false_ptr();
    interp::ensure_initialised();
    force_link_table::touch();
    let _ = datetime_api::touch();
}
