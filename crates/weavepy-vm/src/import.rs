//! Module loading and `sys.modules` cache.
//!
//! Implements the runtime half of RFC 0012:
//!
//! - [`ModuleCache`] owns `sys.modules` and `sys.path`, plus a
//!   registry of *built-in* (Rust-defined) module factories.
//! - The cache is consulted on every `IMPORT_NAME`; the loader walks
//!   the dotted name, asking the registry first, then the filesystem.
//! - Loaded modules are executed in their own globals dict so
//!   submodule globals don't leak into siblings.
//!
//! Anything user-visible — `Object::Module`, `__name__`,
//! attribute access — lives in `object.rs` and the dispatch loop.
//! This file is the *plumbing* that turns a dotted name into a
//! cached `Rc<PyModule>`.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::object::{DictData, DictKey, Object, PyModule};

/// Build a fresh built-in module given the live cache (so factories
/// can read `sys.argv` / `sys.path` at construction time).
pub type BuiltinModuleFactory = fn(&ModuleCache) -> Rc<PyModule>;

/// A frozen-Python module: source compiled at runtime and executed
/// inside the interpreter exactly like a real `.py` file, but without
/// touching the filesystem. Used to ship pure-Python stdlib modules
/// inside the binary.
#[derive(Clone, Copy, Debug)]
pub struct FrozenSource {
    pub name: &'static str,
    pub source: &'static str,
    pub is_package: bool,
}

/// Shared runtime state for the import machinery.
///
/// All three `Rc`-wrapped fields double as the corresponding `sys`
/// attributes so mutations from Python code (e.g.
/// `sys.path.append("…")`) flow straight back to the loader, and CLI
/// updates (`set_argv` / `add_to_path`) are immediately visible to
/// the running script.
#[allow(missing_debug_implementations)]
#[derive(Clone)]
pub struct ModuleCache {
    pub modules: Rc<RefCell<DictData>>,
    pub path: Rc<RefCell<Vec<Object>>>,
    pub argv: Rc<RefCell<Vec<Object>>>,
    pub builtins: Rc<RefCell<HashMap<&'static str, BuiltinModuleFactory>>>,
    /// Pure-Python modules baked into the binary. Looked up after
    /// Rust-defined built-ins so a host can override frozen stdlib by
    /// registering a builtin with the same name.
    pub frozen: Rc<RefCell<HashMap<&'static str, FrozenSource>>>,
}

impl Default for ModuleCache {
    fn default() -> Self {
        Self {
            modules: Rc::new(RefCell::new(DictData::new())),
            path: Rc::new(RefCell::new(Vec::new())),
            argv: Rc::new(RefCell::new(Vec::new())),
            builtins: Rc::new(RefCell::new(HashMap::new())),
            frozen: Rc::new(RefCell::new(HashMap::new())),
        }
    }
}

impl ModuleCache {
    pub fn register_builtin(&self, name: &'static str, factory: BuiltinModuleFactory) {
        self.builtins.borrow_mut().insert(name, factory);
    }

    /// Cache hit lookup. Returns `None` if not yet loaded.
    pub fn get(&self, full_name: &str) -> Option<Object> {
        let key = DictKey(Object::from_str(full_name));
        self.modules.borrow().get(&key).cloned()
    }

    /// Install a loaded module in the cache. CPython treats
    /// `sys.modules` writes as authoritative — subsequent imports
    /// of the same name return whatever is in the cache, regardless
    /// of whether the original loader has finished.
    pub fn insert(&self, full_name: &str, module: Object) {
        // CPython tracks module objects with the cycle GC (`gc.is_tracked(gc)`
        // is True); a module's globals routinely close cycles (a function
        // defined in the module captures the module's dict). Track on install.
        if matches!(module, Object::Module(_)) {
            crate::gc_trace::track(module.clone());
        }
        self.modules
            .borrow_mut()
            .insert(DictKey(Object::from_str(full_name)), module);
    }

    pub fn remove(&self, full_name: &str) {
        self.modules
            .borrow_mut()
            .shift_remove(&DictKey(Object::from_str(full_name)));
    }

    pub fn builtin_factory(&self, name: &str) -> Option<BuiltinModuleFactory> {
        self.builtins.borrow().get(name).copied()
    }

    /// Register a Python-source module that ships inside the binary.
    /// The loader compiles and executes it lazily on first import.
    pub fn register_frozen(&self, source: FrozenSource) {
        self.frozen.borrow_mut().insert(source.name, source);
    }

    pub fn frozen_source(&self, name: &str) -> Option<FrozenSource> {
        self.frozen.borrow().get(name).copied()
    }

    /// Snapshot the current `sys.path` as a list of `PathBuf`s,
    /// dropping entries that aren't strings. Cheap enough for the
    /// inner loop because `sys.path` is short.
    pub fn search_dirs(&self) -> Vec<PathBuf> {
        self.path
            .borrow()
            .iter()
            .filter_map(|o| match o {
                Object::Str(s) => Some(PathBuf::from(s.as_ref())),
                _ => None,
            })
            .collect()
    }

    /// Locate a module's source on disk by walking `sys.path`.
    ///
    /// Returns:
    /// - `Some((path, is_package))` where `is_package` is `true` for
    ///   `<dir>/<leaf>/__init__.py` matches.
    /// - `None` if the module is not present anywhere on the path.
    pub fn find_source(&self, full_name: &str) -> Option<(PathBuf, bool)> {
        let rel: PathBuf = full_name.split('.').collect();
        for dir in self.search_dirs() {
            let module_file = dir.join(&rel).with_extension("py");
            if module_file.is_file() {
                return Some((module_file, false));
            }
            let pkg_init = dir.join(&rel).join("__init__.py");
            if pkg_init.is_file() {
                return Some((pkg_init, true));
            }
        }
        None
    }

    /// Locate a PEP 420 namespace package — a directory on `sys.path`
    /// with the given name that does *not* contain an `__init__.py`.
    /// Returns the list of contributing directories (matching CPython's
    /// `_NamespacePath`), or empty if no candidates exist.
    pub fn find_namespace_package(&self, full_name: &str) -> Vec<PathBuf> {
        let rel: PathBuf = full_name.split('.').collect();
        let mut hits = Vec::new();
        for dir in self.search_dirs() {
            let pkg_dir = dir.join(&rel);
            if pkg_dir.is_dir() && !pkg_dir.join("__init__.py").is_file() {
                hits.push(pkg_dir);
            }
        }
        hits
    }
}

/// Resolution of a relative import (`from . import x` /
/// `from ..pkg import y`).
///
/// `level` is the count of leading dots; `name` is the explicit
/// suffix after the dots (may be empty for bare `from . import …`).
/// Returns the fully-qualified module name to load — the same string
/// CPython's `__import__` builds internally via `_bootstrap._resolve_name`.
pub fn resolve_relative(
    package: Option<&str>,
    name: &str,
    level: u32,
) -> Result<String, &'static str> {
    if level == 0 {
        return Ok(name.to_owned());
    }
    let pkg = match package {
        Some(p) if !p.is_empty() => p,
        _ => return Err("attempted relative import with no known parent package"),
    };
    // CPython equivalent:
    //   bits = package.rsplit('.', level - 1)
    //   if len(bits) < level: raise ImportError
    //   base = bits[0]
    //
    // Rust's `rsplitn(n, '.')` matches Python's `rsplit('.', n - 1)`:
    // it yields *at most* `n` parts, rightmost first. The leftmost
    // chunk (the base we want) is therefore the last element.
    let parts: Vec<&str> = pkg.rsplitn(level as usize, '.').collect();
    if parts.len() < level as usize {
        return Err("attempted relative import beyond top-level package");
    }
    let base = parts.last().copied().unwrap_or("");
    let resolved = if name.is_empty() {
        base.to_owned()
    } else if base.is_empty() {
        name.to_owned()
    } else {
        format!("{base}.{name}")
    };
    if resolved.is_empty() {
        Err("relative import produced empty module name")
    } else {
        Ok(resolved)
    }
}

/// `__path__` for a freshly loaded package — the directory the
/// `__init__.py` was found in, wrapped in a Python list.
pub fn package_search_path(init_file: &Path) -> Object {
    let dir = init_file.parent().map_or(PathBuf::new(), Path::to_path_buf);
    let lossy = dir.to_string_lossy().into_owned();
    Object::new_list(vec![Object::from_str(lossy)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_absolute_passes_through() {
        assert_eq!(
            resolve_relative(None, "os.path", 0).unwrap(),
            "os.path".to_owned()
        );
    }

    #[test]
    fn resolve_relative_one_level_drops_last_component() {
        // Inside package `pkg.sub`, `from . import x` reaches `pkg.sub.x`.
        assert_eq!(
            resolve_relative(Some("pkg.sub"), "x", 1).unwrap(),
            "pkg.sub.x".to_owned()
        );
    }

    #[test]
    fn resolve_relative_two_levels_drops_two() {
        // Inside `pkg.sub`, `from .. import y` reaches `pkg.y`.
        assert_eq!(
            resolve_relative(Some("pkg.sub"), "y", 2).unwrap(),
            "pkg.y".to_owned()
        );
    }

    #[test]
    fn resolve_relative_overshoot_errors() {
        let err = resolve_relative(Some("pkg"), "x", 2).unwrap_err();
        assert!(err.contains("beyond top-level"));
    }
}
