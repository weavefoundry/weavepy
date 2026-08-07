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
/// `sys.implementation.cache_tag`. Mirrors CPython's `cpython-313`
/// shape: `<impl>-<major><minor>` with **no dot** in the tag itself
/// (PEP 3147's `<name>.<tag>.pyc` parsing — e.g. `source_from_cache`,
/// which `runpy`/`make_legacy_pyc` rely on — keys off the first dot, so
/// a dotted tag like `weavepy-3.13` would corrupt the recovered source
/// name). Distinct from CPython's `cpython-313` so the artifacts never
/// collide.
///
/// The trailing `-<rev>` is WeavePy's **bytecode-format revision**, our
/// only lever for invalidating stale `.pyc` on a compiler change: we pin
/// [`MAGIC`] to CPython 3.13's value (so `importlib.util.MAGIC_NUMBER` /
/// `_imp.get_magic()` match for `test_importlib.test_magic_number` and
/// external-tool interop), which means we *cannot* bump the magic the way
/// CPython does. Bumping the tag changes the `.pyc` *filename*, so both
/// readers that key off `cache_tag` — the native [`cache_path_for`] and
/// the frozen `importlib._bootstrap_external` — uniformly miss the old
/// artifact and recompile from source. Both hyphens keep the tag dotless,
/// so PEP 3147 source recovery still resolves `<name>.py`.
///
/// - rev `2`: WTF-8 string constants. Pre-rev `.pyc` compiled lone
///   surrogates in literals (`'\udfff'`) lossily to U+FFFD; rev 2 stores
///   them faithfully (`test_posixpath.test_realpath_invalid_paths`,
///   `test_os`/`test_tarfile` surrogate paths).
/// - rev `3`: `SETUP_ANNOTATIONS` opcode. Module/class bodies containing
///   annotated statements now bind `__annotations__` at block entry
///   (create-if-absent) instead of lazily at the first annotation.
/// - rev `4`: RFC 0051 PEP 695 lowering. Type parameters now capture
///   `*Ts`/`**P` kinds, bounds/constraints, and PEP 696 defaults via
///   the `__weavepy_typevar__` intrinsic family and append the
///   implicit `Generic[…]` base; pre-rev `.pyc` baked the name-only
///   placeholder lowering.
/// - rev `12`: RFC 0056 WS4 invalidation. Rev-11 artifacts in the wild
///   were written across intermediate compiler changes (notably module
///   `__doc__` binding) without a bump, so a stale `.pyc` could import a
///   module with `__doc__ = None` (doctest found no module-level
///   examples in `test_doctest2`). One bump flushes them all.
/// - rev `13`: PEP 657 column spans in the location table (long entry
///   form). Rev-12 `.pyc`s carried the no-column form only, so modules
///   imported from cache lost traceback caret underlines
///   (test_doctest's error-report examples compare them textually).
/// - rev `14`: PEP 488 `.opt-N` variants + `source_to_code(_optimize=)`
///   honoured. Rev-13 `.opt-1`/`.opt-2` artifacts (written by
///   `py_compile`/`compileall`) contain *unoptimized* code under the
///   optimized filename; one bump flushes them before the native
///   reader starts trusting the suffix.
/// - rev `15`: class bodies store `__static_attributes__` *after* the
///   body statements (CPython 3.13's emission order, observed by
///   `__prepare__` mappings — test_metaclass), and comprehension
///   `GET_ITER`/`FOR_ITER` carry the iterable expression's column span
///   (test_dictcomps/test_listcomps `test_exception_locations`).
///   Rev-14 artifacts bake the old ordering and whole-comprehension
///   spans.
/// - rev `16`: `*x` splats lower through `LIST_EXTEND` /
///   `LIST_TO_TUPLE` (CPython's shape; the errors carry CPython's
///   "Value after * must be an iterable" / func-prefixed wording —
///   test_extcall), replacing the old `tuple(x)`-by-name lowering.
///   Rev-15 artifacts still call the possibly-shadowed `tuple` builtin.
/// - rev `17`: RFC 0057 match-codegen rewrite changed three encoding
///   conventions: `COPY` carries its real depth (was hardcoded 1),
///   `UNPACK_EX` uses CPython's byte order (before-star count in the
///   low byte; ours had it in the high byte), and `BINARY_OP` encodes
///   in-place operators as `NB_INPLACE_*` indexes (the augmented flag
///   was previously dropped). Rev-16 artifacts decode incorrectly
///   under all three.
/// - rev `18`: RFC 0057 trace-fidelity work changed codegen shape:
///   jump threading with CPython's synthetic/same-line eligibility,
///   `pass` lowered to a located NOP, and per-site `return None`
///   copies gated by the same eligibility. Rev-17 artifacts bake the
///   over-threaded jumps (spurious/missing `'line'` trace events).
/// - rev `19`: `PUSH_EXC_INFO` persists its handler-body-end tag (the
///   unwinder's cue for discarding handled-exception entries when an
///   exception escapes a handler) as an absolute code-unit oparg.
///   Rev-18 artifacts decode the tag as 0 (untagged), which loosens
///   handled-exception unwinding and corrupts `__context__` chains
///   (test_contextlib_async `test_exit_exception_chaining_reference`).
pub const CACHE_TAG: &str = "weavepy-313-19";

const HEADER_LEN: usize = 16;

/// Resolve the `__pycache__/<name>.<tag>[.opt-N].pyc` companion for a
/// source file. CPython routes the cache to `<source_dir>/__pycache__/...`
/// unless `sys.pycache_prefix` redirects elsewhere; we follow the
/// same shape. `optimize` selects the PEP 488 suffix: level 0 is
/// untagged, `-O`/`-OO` read and write `.opt-1`/`.opt-2` variants (an
/// import under `-O` updates exactly the `opt-1` artifact —
/// `test_compileall.HardlinkDedupTests.test_import`).
pub fn cache_path_for(source: &Path, optimize: u8) -> Option<PathBuf> {
    let stem = source.file_stem()?.to_string_lossy().into_owned();
    let dir = source.parent()?;
    let cache_dir = dir.join("__pycache__");
    let name = if optimize == 0 {
        format!("{stem}.{CACHE_TAG}.pyc")
    } else {
        format!("{stem}.{CACHE_TAG}.opt-{optimize}.pyc")
    };
    Some(cache_dir.join(name))
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
pub fn try_load(source_path: &Path, optimize: u8) -> Option<CodeObject> {
    let cache_path = cache_path_for(source_path, optimize)?;
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
        Object::Code(c) => {
            let mut code = (*c).clone();
            // The cache may have been written under a different spelling of
            // the same file (a symlinked `sys.path` entry — e.g. a vendored
            // `Lib -> /opt/.../python3.13`). CPython re-imports record the
            // *current* path in `co_filename` (each interpreter writes its
            // own pyc from the path it used); a stale spelling here would
            // diverge from the module's `__file__` and break consumers that
            // bridge the two (`warnings.warn(stacklevel=)` filename checks).
            let current = source_path.to_string_lossy();
            if code.filename != current {
                rewrite_filenames(&mut code, &current);
            }
            Some(code)
        }
        _ => None,
    }
}

/// Recursively stamp `filename` on a code object and every nested code
/// constant (function/class bodies, comprehensions).
fn rewrite_filenames(code: &mut CodeObject, filename: &str) {
    code.filename = filename.to_owned();
    fn walk(c: &mut weavepy_compiler::Constant, filename: &str) {
        match c {
            weavepy_compiler::Constant::Code(inner) => rewrite_filenames(inner, filename),
            weavepy_compiler::Constant::Tuple(items) => {
                for it in items {
                    walk(it, filename);
                }
            }
            _ => {}
        }
    }
    for c in &mut code.constants {
        walk(c, filename);
    }
}

/// Persist the compiled code object alongside its source. Errors are
/// silently swallowed (matching CPython): a read-only filesystem or a
/// missing parent directory shouldn't fail the import.
pub fn try_write(source_path: &Path, code: &CodeObject, optimize: u8) {
    let Some(cache_path) = cache_path_for(source_path, optimize) else {
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
    // CPython's `_write_atomic` creates the file with the *source's*
    // permission bits (forced user-writable for later cache updates —
    // issue #6074) masked to 0o666, letting the umask apply at open
    // (test_import.FilePermissionTests).
    let tmp = cache_path.with_extension("pyc.tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mode = (meta.permissions().mode() | 0o200) & 0o666;
        let Ok(mut f) = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)
        else {
            return;
        };
        if f.write_all(&bytes).is_err() {
            drop(f);
            let _ = fs::remove_file(&tmp);
            return;
        }
    }
    #[cfg(not(unix))]
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
