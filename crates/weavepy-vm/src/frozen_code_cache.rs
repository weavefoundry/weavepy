//! RFC 0021 — process-global cache of compiled frozen-stdlib
//! [`weavepy_compiler::CodeObject`]s.
//!
//! ## Why
//!
//! Every `Interpreter::new()` ships with the same set of frozen
//! Python modules — `collections`, `functools`, `argparse`, etc.
//! Without this cache, each interpreter re-parses + re-compiles
//! all of them on first import, paying ~25K LOC of compilation
//! cost per VM. With this cache, the *first* interpreter in a
//! process eats the cost; subsequent interpreters reuse the
//! [`CodeObject`] directly.
//!
//! Tests, the REPL, the bench harness, and any host that builds
//! up an [`crate::Interpreter`] more than once all benefit.
//!
//! ## Caveats
//!
//! - The cache holds *only* compiled code, not running modules.
//!   Each interpreter still executes the module body to populate
//!   its own `sys.modules`, build its own `__dict__`, and run any
//!   side-effects.
//! - The cached code is per-source. Frozen modules carry
//!   `&'static str` source so the cache key is the module name;
//!   if the source ever varied at runtime (it doesn't) we'd hash
//!   the source instead.
//! - Inline caches inside the [`CodeObject`] are *not* shared
//!   across interpreters. Each clone of the cached code starts
//!   with a fresh, empty cache table because the type fingerprints
//!   one interpreter recorded would be invalid in another (the
//!   `Rc::as_ptr` addresses change).
//!
//! ## Threading
//!
//! Today WeavePy is single-threaded, so a `RefCell` is enough.
//! The free-threaded build (RFC 0010 candidate) will replace this
//! with a `Mutex` or a shard'd cache.

use std::cell::RefCell;
use std::collections::HashMap;

use weavepy_compiler::CodeObject;

thread_local! {
    static CACHE: RefCell<HashMap<&'static str, CodeObject>> = RefCell::new(HashMap::new());
}

/// Look up a previously-compiled frozen module by its static
/// name. Returns a fresh clone of the cached [`CodeObject`] —
/// callers want their own copy because the inline-cache
/// side-table needs to start fresh per-interpreter.
pub fn get(name: &str) -> Option<CodeObject> {
    CACHE.with(|c| {
        let map = c.borrow();
        map.get(name).map(|code| {
            let clone = code.clone();
            // Reset every cache slot to `Empty` — see module docs.
            clone.caches.clear();
            clone
        })
    })
}

/// Install a freshly-compiled frozen module into the cache.
/// Keyed on the module's `&'static` name (which the frozen
/// loader carries through; we don't allocate a new `String`).
pub fn insert(name: &str, code: &CodeObject) {
    // Look up the static name from the registered frozen sources
    // — the borrow-checker doesn't let us hash on a `&str`-into-
    // `&'static str` upgrade directly. We use `Box::leak` of the
    // owned `String` for new entries, which is a one-time-only
    // cost per module name and irrelevant against the compile
    // savings.
    let static_name: &'static str = Box::leak(name.to_owned().into_boxed_str());
    CACHE.with(|c| {
        let mut map = c.borrow_mut();
        if !map.contains_key(static_name) {
            map.insert(static_name, code.clone());
        }
    });
}

/// Number of frozen modules currently cached. Used by tests.
#[allow(dead_code)]
pub fn len() -> usize {
    CACHE.with(|c| c.borrow().len())
}

/// Drop every cached entry. Used by tests that want a clean
/// baseline; production paths leave the cache to grow.
#[allow(dead_code)]
pub fn clear() {
    CACHE.with(|c| c.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_returns_fresh_copies() {
        clear();
        let mut code = CodeObject::default();
        code.name = "foo".to_owned();
        insert("foo", &code);
        let got = get("foo").expect("hit");
        assert_eq!(got.name, "foo");
        assert!(get("missing").is_none());
    }

    #[test]
    fn cache_clears_inline_caches_on_clone() {
        use weavepy_compiler::{CacheTable, InlineCache};
        clear();
        let mut code = CodeObject::default();
        code.name = "warmed".to_owned();
        code.caches = CacheTable::with_len(2);
        code.caches.set(0, InlineCache::BinOpAddInt);
        insert("warmed", &code);
        let got = get("warmed").expect("hit");
        // The cloned code's cache must start empty so this
        // interpreter's specializer gets to record fresh
        // fingerprints.
        assert_eq!(got.caches.get(0), InlineCache::Empty);
    }
}
