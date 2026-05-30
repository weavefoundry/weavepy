//! `__pycache__` (PEP 3147) cache for compiled bytecode.
//!
//! On every import from source we try to read a sibling
//! `__pycache__/<name>.<cache_tag>.pyc` file. If its header is valid,
//! the embedded code object is unmarshaled and returned, skipping the
//! parser + compiler entirely. If the cache file is missing,
//! outdated, or malformed, we fall back to recompiling and write a
//! fresh cache file on the way out (subject to `-B` /
//! `PYTHONDONTWRITEBYTECODE`).
//!
//! ## File layout
//!
//! The 16-byte header mirrors CPython's PEP 552 timestamp-invalidation
//! mode, with WeavePy's own magic so CPython and WeavePy can coexist
//! in the same `__pycache__` directory without confusion:
//!
//! ```text
//! +----+----+----+----+----+----+----+----+----+----+----+----+----+----+----+----+
//! |  MAGIC (4)        |  FLAGS  (4) = 0   |  MTIME (4)        |  SIZE (4)         |
//! +----+----+----+----+----+----+----+----+----+----+----+----+----+----+----+----+
//! |  marshal.dumps(code) ...                                                       |
//! ```
//!
//! - **MAGIC**: 4 bytes. `b"WPY0"` for this format version. Bumped
//!   when the bytecode shape changes incompatibly.
//! - **FLAGS**: 4 bytes, little-endian. Reserved for the future
//!   PEP 552 hash-mode bit; today always `0`.
//! - **MTIME**: little-endian u32 source mtime in seconds (Unix epoch).
//! - **SIZE**: little-endian u32 source file size in bytes. Used as a
//!   cheap second-line check against in-place edits that preserve mtime.
//! - **Body**: the output of `marshal.dumps(code_object)`.

use crate::sync::Rc;
use crate::sync::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use weavepy_compiler::CodeObject;

use crate::object::{DictData, Object};
use crate::stdlib::marshal_mod;

/// Bytecode magic. RFC 0033 adopts CPython 3.13's value
/// (`b"\xf3\x0d\x0d\x0a"`, surfaced via `importlib.util.MAGIC_NUMBER`
/// and `_imp.get_magic()`). Collisions with CPython's own `.pyc`
/// files are avoided by the distinct [`CACHE_TAG`] in the filename,
/// so adopting the real magic costs nothing and buys tool interop.
pub const MAGIC: &[u8; 4] = b"\xf3\x0d\x0d\x0a";

/// Cache tag — appears in `__pycache__/<name>.<tag>.pyc` and on
/// `sys.implementation.cache_tag`. Mirrors CPython's `cpython-313`.
pub const CACHE_TAG: &str = "weavepy-3.13";

const HEADER_LEN: usize = 16;

/// Resolve the `__pycache__/<name>.<tag>.pyc` companion for a source
/// file. CPython routes the cache to `<source_dir>/__pycache__/...`
/// unless `sys.pycache_prefix` redirects elsewhere; we follow the
/// same shape.
pub fn cache_path_for(source: &Path) -> Option<PathBuf> {
    let stem = source.file_stem()?.to_string_lossy().into_owned();
    let dir = source.parent()?;
    let cache_dir = dir.join("__pycache__");
    Some(cache_dir.join(format!("{stem}.{CACHE_TAG}.pyc")))
}

/// Returns true when the user has asked us not to persist `.pyc`s.
/// Reads `sys.dont_write_bytecode` (set by the CLI or by user code).
pub fn dont_write_bytecode(sys_module: &Rc<RefCell<DictData>>) -> bool {
    let dict = sys_module.borrow();
    match dict.get(&crate::object::DictKey(Object::from_static(
        "dont_write_bytecode",
    ))) {
        Some(Object::Bool(b)) => *b,
        Some(Object::Int(i)) => *i != 0,
        _ => false,
    }
}

/// Try to load a cached code object for `source_path`. Returns
/// `Some(code)` on a healthy hit; returns `None` if the cache is
/// missing, stale, or malformed (so the caller falls back to source
/// compilation).
pub fn try_load(source_path: &Path) -> Option<CodeObject> {
    let cache_path = cache_path_for(source_path)?;
    let src_meta = fs::metadata(source_path).ok()?;
    let src_mtime = mtime_seconds(&src_meta);
    let src_size = u32::try_from(src_meta.len()).ok()?;
    let bytes = fs::read(&cache_path).ok()?;
    if bytes.len() < HEADER_LEN {
        return None;
    }
    if &bytes[0..4] != MAGIC {
        return None;
    }
    // FLAGS at [4..8] — reserved.
    let mtime_bytes: [u8; 4] = bytes[8..12].try_into().ok()?;
    let size_bytes: [u8; 4] = bytes[12..16].try_into().ok()?;
    let cache_mtime = u32::from_le_bytes(mtime_bytes);
    let cache_size = u32::from_le_bytes(size_bytes);
    if cache_mtime != src_mtime || cache_size != src_size {
        return None;
    }
    let body = &bytes[HEADER_LEN..];
    match marshal_mod::load_from_bytes(body).ok()? {
        Object::Code(c) => Some((*c).clone()),
        _ => None,
    }
}

/// Persist the compiled code object alongside its source. Errors are
/// silently swallowed (matching CPython): a read-only filesystem or a
/// missing parent directory shouldn't fail the import.
pub fn try_write(source_path: &Path, code: &CodeObject) {
    let Some(cache_path) = cache_path_for(source_path) else {
        return;
    };
    let Ok(meta) = fs::metadata(source_path) else {
        return;
    };
    let mtime = mtime_seconds(&meta);
    let Ok(size) = u32::try_from(meta.len()) else {
        return;
    };
    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut bytes = Vec::with_capacity(HEADER_LEN + 256);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&mtime.to_le_bytes());
    bytes.extend_from_slice(&size.to_le_bytes());
    let code_obj = Object::Code(Rc::new(code.clone()));
    let Ok(payload) = marshal_mod::b_dumps(&[code_obj]) else {
        return;
    };
    if let Object::Bytes(b) = payload {
        bytes.extend_from_slice(&b);
    } else {
        return;
    }
    // Atomic-ish write: write to a tempfile next door, then rename
    // so concurrent imports can't observe a half-written cache.
    let tmp = cache_path.with_extension("pyc.tmp");
    if fs::write(&tmp, &bytes).is_err() {
        return;
    }
    let _ = fs::rename(&tmp, &cache_path);
}

fn mtime_seconds(meta: &fs::Metadata) -> u32 {
    use std::time::UNIX_EPOCH;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| u32::try_from(d.as_secs() & u64::from(u32::MAX)).unwrap_or(0))
        .unwrap_or(0)
}
