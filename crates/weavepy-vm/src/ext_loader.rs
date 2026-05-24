//! Process-global hook for the C-extension loader (RFC 0022).
//!
//! The VM doesn't depend on `weavepy-capi` directly — that would
//! create a dependency cycle (the C-API depends on the VM for the
//! `Object` type, builtin types, etc.). Instead, the binary that
//! pulls both crates together registers a closure here at startup;
//! [`Interpreter::load_one`](crate::Interpreter) calls it during
//! the import walk.
//!
//! The callback returns:
//!
//! - `Ok(Some(module))` — the module was successfully loaded as a
//!   C extension; the loader caches it in `sys.modules`.
//! - `Ok(None)` — no extension exists for this name; the caller
//!   falls back to the source loader.
//! - `Err(err)` — the extension exists but failed to load; the
//!   caller propagates the error (so the user sees the reason
//!   rather than a misleading `ModuleNotFoundError`).
//!
//! The hook fires *before* the filesystem source loader, which
//! mirrors CPython's order: extensions take precedence over
//! same-name `.py` files because their search paths overlap.

use std::sync::Mutex;

use crate::error::RuntimeError;
use crate::object::Object;

/// Signature of a registered loader hook.
///
/// `interp` is the running interpreter; `full_name` is the
/// fully-qualified module name (e.g. `numpy.core._multiarray_umath`).
pub type ExtensionLoader =
    fn(interp: &mut crate::Interpreter, full_name: &str) -> Result<Option<Object>, RuntimeError>;

static REGISTRY: Mutex<Option<ExtensionLoader>> = Mutex::new(None);

/// Install a loader. Replaces any previously-registered hook.
/// Idempotent under repeated registration of the same callback.
pub fn install_extension_loader(loader: ExtensionLoader) {
    *REGISTRY.lock().unwrap() = Some(loader);
}

/// Remove the loader hook (if any). Used by tests that need to
/// isolate from extension side effects.
pub fn clear_extension_loader() {
    *REGISTRY.lock().unwrap() = None;
}

/// Read the currently-installed loader.
pub fn current_extension_loader() -> Option<ExtensionLoader> {
    *REGISTRY.lock().unwrap()
}
