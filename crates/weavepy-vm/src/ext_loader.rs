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

use std::collections::HashMap;
use std::path::PathBuf;
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

// ---------------------------------------------------------------------
// RFC 0029: explicit-path side-channel.
//
// When `_imp._load_dynamic(name, path)` is invoked from
// Python-level code, the explicit path is the source of truth —
// the loader's normal `sys.path` walk would re-discover it but
// in some cases (e.g. when the file is outside `sys.path`) we
// need to stash it. The side-channel below lets the C-API loader
// read the explicit path back when it dispatches by name.
// ---------------------------------------------------------------------

static EXPLICIT_PATHS: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);

/// Stash an explicit path for `name`. The C-API loader's
/// next-by-name lookup will see it and use it before falling
/// back to `sys.path` traversal.
pub fn stash_explicit_path(name: &str, path: PathBuf) {
    let mut guard = EXPLICIT_PATHS.lock().unwrap();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(name.to_owned(), path);
}

/// Consume the stashed path for `name`, if any.
pub fn take_explicit_path(name: &str) -> Option<PathBuf> {
    let mut guard = EXPLICIT_PATHS.lock().unwrap();
    guard.as_mut().and_then(|m| m.remove(name))
}

/// Peek at the stashed path for `name` without consuming it.
pub fn peek_explicit_path(name: &str) -> Option<PathBuf> {
    let guard = EXPLICIT_PATHS.lock().unwrap();
    guard.as_ref().and_then(|m| m.get(name).cloned())
}
