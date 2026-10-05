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
//!   across interpreters. Each copy of the cached code starts
//!   with a fresh, empty cache table because the type fingerprints
//!   one interpreter recorded would be invalid in another (the
//!   `Rc::as_ptr` addresses change).
//! - The cache keeps each module in the compact `native_code` form
//!   the disk layer uses, not as a [`CodeObject`]: a resident copy of
//!   every compiled module would double the memory its code takes in
//!   the (usual) single-interpreter process, which never reads it.
//!
//! ## Threading
//!
//! Today WeavePy is single-threaded, so a `RefCell` is enough.
//! The free-threaded build (RFC 0010 candidate) will replace this
//! with a `Mutex` or a shard'd cache.

use crate::sync::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use weavepy_compiler::CodeObject;

thread_local! {
    /// Each module's code in `native_code` form, with the filename it
    /// was compiled under.
    static CACHE: RefCell<HashMap<&'static str, (Box<str>, Box<[u8]>)>> =
        RefCell::new(HashMap::new());
}

/// Look up a previously-compiled frozen module by its static
/// name. Returns a fresh copy of the cached [`CodeObject`] —
/// callers want their own copy because the inline-cache
/// side-table needs to start fresh per-interpreter.
pub fn get(name: &str) -> Option<CodeObject> {
    CACHE.with(|c| {
        let map = c.borrow();
        let (filename, bytes) = map.get(name)?;
        weavepy_compiler::native_code::decode(bytes, filename)
    })
}

/// Install a freshly-compiled frozen module into the cache.
/// Keyed on the module's `&'static` name (which the frozen
/// loader carries through; we don't allocate a new `String`).
/// A code object the native form doesn't carry just isn't cached.
pub fn insert(name: &str, code: &CodeObject) {
    if CACHE.with(|c| c.borrow().contains_key(name)) {
        return;
    }
    let Some(bytes) = weavepy_compiler::native_code::encode(code) else {
        return;
    };
    // Look up the static name from the registered frozen sources
    // — the borrow-checker doesn't let us hash on a `&str`-into-
    // `&'static str` upgrade directly. We use `Box::leak` of the
    // owned `String` for new entries, which is a one-time-only
    // cost per module name and irrelevant against the compile
    // savings.
    let static_name: &'static str = Box::leak(name.to_owned().into_boxed_str());
    CACHE.with(|c| {
        c.borrow_mut().insert(
            static_name,
            (code.filename.as_str().into(), bytes.into_boxed_slice()),
        );
    });
}

// ---------------------------------------------------------------------
// On-disk layer (RFC 0059 WS5a).
//
// The in-memory cache above only helps the *second* interpreter in a
// process; a fresh `weavepy -c pass` still re-parses + re-compiles the
// whole `site` import chain (~13ms of a ~46ms startup). This layer
// persists the marshalled `CodeObject`s in a per-user cache directory
// so warm process starts skip parse + compile entirely.
//
// Artifact layout: `<cache_dir>/weavepy/frozen-<CACHE_TAG>-n<VERSION>/<name>`
// with a 20-byte header — magic `WPYN`, reserved flags word, source
// length, and a 64-bit source hash — followed by the code in
// `weavepy_compiler::native_code` form, which loads without the CPython
// bytecode transcoding a `marshal` payload needs. The `CACHE_TAG` and
// native-format `VERSION` in the directory name invalidate on bytecode or
// layout revisions (and keep binaries of different formats from
// rewriting each other's artifacts); the length + hash pair invalidates
// when the embedded source itself changes (a rebuilt binary with edited
// stdlib). Corrupt or mismatched artifacts are treated as misses.

/// Header magic for frozen-cache artifacts (distinct from `.pyc`'s
/// CPython magic: these files are WeavePy-internal).
const FROZEN_MAGIC: &[u8; 4] = b"WPYN";
const FROZEN_HEADER_LEN: usize = 20;

/// The per-user frozen-cache directory, resolved once. `None` disables
/// the disk layer (no resolvable cache dir, or `WEAVEPY_FROZEN_CACHE=0`).
/// `WEAVEPY_FROZEN_CACHE=<dir>` redirects it (useful for tests and
/// sandboxed environments).
fn disk_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = match std::env::var_os("WEAVEPY_FROZEN_CACHE") {
            Some(v) if v == "0" || v.is_empty() => return None,
            Some(v) => PathBuf::from(v),
            None => {
                #[cfg(target_os = "macos")]
                let base = std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join("Library").join("Caches"))?;
                #[cfg(all(unix, not(target_os = "macos")))]
                let base = std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))
                    })?;
                #[cfg(not(unix))]
                let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
                base
            }
        };
        Some(base.join("weavepy").join(format!(
            "frozen-{}-n{}",
            crate::pycache::CACHE_TAG,
            weavepy_compiler::native_code::VERSION
        )))
    })
    .as_ref()
}

/// The artifact path for a frozen module. Frozen names are dotted
/// identifiers (`os.path`), safe as single filename components.
fn disk_path(name: &str) -> Option<PathBuf> {
    Some(disk_dir()?.join(name))
}

/// Try the on-disk cache for a frozen module. On a healthy hit the
/// code is stamped with `filename` (the process's materialized-tree
/// path may differ from the writer's), installed into the in-memory
/// cache, and returned. Any mismatch or decode failure is a miss.
pub fn get_disk(name: &str, source: &str, filename: &str) -> Option<CodeObject> {
    let bytes = std::fs::read(disk_path(name)?).ok()?;
    if bytes.len() < FROZEN_HEADER_LEN || &bytes[0..4] != FROZEN_MAGIC {
        return None;
    }
    let len = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let hash = u64::from_le_bytes(bytes[12..20].try_into().ok()?);
    // An embedded module's hash comes from the build: computing it here
    // would page in the whole source text the cached code stands for.
    if len as usize != source.len() || hash != crate::stdlib::frozen_source_hash(name, source) {
        return None;
    }
    // Not mirrored into the in-memory cache: a later interpreter reads the
    // same artifact again, and a resident clone of every loaded module's
    // code would double its footprint in the (usual) single-interpreter
    // process.
    weavepy_compiler::native_code::decode(&bytes[FROZEN_HEADER_LEN..], filename)
}

/// Persist a freshly-compiled frozen module to the disk cache.
/// Best-effort: any I/O or marshal failure is silently ignored (a
/// read-only cache dir must not fail the import).
pub fn write_disk(name: &str, source: &str, code: &CodeObject) {
    let Some(path) = disk_path(name) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // A code object the native form doesn't carry just isn't cached.
    let Some(payload) = weavepy_compiler::native_code::encode(code) else {
        return;
    };
    let mut bytes = Vec::with_capacity(FROZEN_HEADER_LEN + payload.len());
    bytes.extend_from_slice(FROZEN_MAGIC);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&(source.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&crate::stdlib::frozen_source_hash(name, source).to_le_bytes());
    bytes.extend_from_slice(&payload);
    // Atomic-ish: temp + rename so concurrent starts never observe a
    // half-written artifact.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
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
