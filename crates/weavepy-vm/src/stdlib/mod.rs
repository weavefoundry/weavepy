//! Built-in modules that ship with the WeavePy interpreter.
//!
//! Each submodule defines a factory `fn(&ModuleCache) -> Rc<PyModule>`
//! that constructs a fresh module value with all attributes installed.
//! The main entry point [`register_all`] registers every shipped
//! module into the import cache; the loader picks them up on demand.
//!
//! The set of modules here is intentionally small — RFC 0012 ships
//! `sys`, `math`, `os`, and `os.path`. Other stdlib equivalents live
//! in their own follow-up RFCs.

use crate::import::ModuleCache;

pub mod math;
pub mod os;
pub mod sys;

/// Register the built-in modules into `cache`. Called once at
/// interpreter startup.
pub fn register_all(cache: &ModuleCache) {
    cache.register_builtin("sys", sys::build);
    cache.register_builtin("math", math::build);
    cache.register_builtin("os", os::build);
    cache.register_builtin("os.path", os::build_path);
}
