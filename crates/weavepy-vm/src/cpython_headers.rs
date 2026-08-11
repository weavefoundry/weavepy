//! RFC 0062 WS2 — the embedded CPython 3.13 header tree.
//!
//! The stock `Include/` install tree (vendored under
//! `crates/weavepy-capi/include/cpython313/`, PSF-licensed) plus the
//! per-OS generated `pyconfig.h`, embedded so
//! [`crate::stdlib_tree::materialize`] can write a real
//! `{prefix}/include/python3.13/` — the surface `pip install` of a
//! C-extension sdist compiles against. See the build script for the
//! generation step.

include!(concat!(env!("OUT_DIR"), "/cpython_headers.rs"));
