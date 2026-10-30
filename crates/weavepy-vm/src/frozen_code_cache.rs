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
            ((*code.filename).into(), bytes.into_boxed_slice()),
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
// with a 20-byte header — magic `WPYN`, payload length, source
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
    use std::io::Read;
    use weavepy_compiler::native_code;
    let mut file = std::fs::File::open(disk_path(name)?).ok()?;
    let size = usize::try_from(file.metadata().ok()?.len()).ok()?;
    let mut head = [0u8; FROZEN_HEADER_LEN];
    file.read_exact(&mut head).ok()?;
    if &head[0..4] != FROZEN_MAGIC {
        return None;
    }
    let len = u32::from_le_bytes(head[8..12].try_into().ok()?);
    let hash = u64::from_le_bytes(head[12..20].try_into().ok()?);
    // An embedded module's hash comes from the build: computing it here
    // would page in the whole source text the cached code stands for.
    if len as usize != source.len() || hash != crate::stdlib::frozen_source_hash(name, source) {
        return None;
    }
    // The reserved word holds the payload's length: only a complete artifact
    // of this format may be decoded lazily.
    let payload_len = u32::from_le_bytes(head[4..8].try_into().ok()?) as usize;
    if size.checked_sub(FROZEN_HEADER_LEN)? != payload_len {
        return None;
    }
    // Not mirrored into the in-memory cache: a later interpreter reads the
    // same artifact again, and a resident clone of every loaded module's
    // code would double its footprint in the (usual) single-interpreter
    // process.
    //
    // The file is mapped rather than read: most of what it holds (the bodies
    // of functions that never run, column spans, wire marks) is never read,
    // and pages never touched take no memory.
    //
    // SAFETY: the cache's writers replace an artifact by renaming a new
    // file over it, never by changing it in place, so the mapped bytes
    // keep the contents just validated (a replaced or removed file keeps
    // them while mapped).
    let map = unsafe { memmap2::MmapOptions::new().map(&file) }.ok()?;
    native_code::decode_lazily(
        weavepy_compiler::TableBuffer::new(Payload::new(map)),
        filename,
    )
}

/// An artifact's mapping, read as the payload after its header.
struct Payload {
    map: memmap2::Mmap,
    /// Tells this artifact's views apart (see [`view`]).
    #[cfg(target_os = "macos")]
    id: u64,
}

impl Payload {
    fn new(map: memmap2::Mmap) -> Self {
        #[cfg(target_os = "macos")]
        {
            static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            Payload {
                map,
                id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            }
        }
        #[cfg(not(target_os = "macos"))]
        Payload { map }
    }
}

impl weavepy_compiler::CodeBytes for Payload {
    fn bytes(&self) -> &[u8] {
        &self.map[FROZEN_HEADER_LEN..]
    }

    /// Reading part of a mapped file leaves its pages in the resident set,
    /// and over a run most of the file's pages hold something that gets read
    /// (a page holds several functions, and a module's identifiers are all
    /// in one place). What's read is copied out, so the pages can go.
    fn read(&self, read: &mut dyn FnMut(&[u8])) {
        #[cfg(target_os = "macos")]
        if view::read(self, read) {
            return;
        }
        read(weavepy_compiler::CodeBytes::bytes(self));
        #[cfg(target_os = "linux")]
        // SAFETY: a read-only file mapping: dropping its page table entries
        // loses nothing, and the next read faults the pages back in.
        unsafe {
            libc::madvise(
                self.map.as_ptr().cast_mut().cast(),
                self.map.len(),
                libc::MADV_DONTNEED,
            );
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for Payload {
    fn drop(&mut self) {
        view::close(self.id);
    }
}

/// `madvise` doesn't take a file's clean pages out of the resident set on
/// macOS, so reads go through a second mapping of the same pages, and
/// removing that does. Making one takes two system calls, and a run reads
/// most bodies in bursts from one module (its class bodies as it's imported),
/// so the last one stays open for the next read, up to [`MAX_READS`] of them.
#[cfg(target_os = "macos")]
mod view {
    #![allow(deprecated)] // (`libc::mach_task_self`)

    use super::{Payload, FROZEN_HEADER_LEN};
    use std::sync::{Mutex, PoisonError};

    /// The reads a view serves before its pages are let go.
    const MAX_READS: u32 = 64;

    struct Open {
        id: u64,
        address: u64,
        size: u64,
        reads: u32,
    }

    /// One at a time, since a read that decodes a body may not need another
    /// (nested bodies stay encoded), and only under the GIL in practice.
    static OPEN: Mutex<Option<Open>> = Mutex::new(None);

    extern "C" {
        fn mach_vm_remap(
            target_task: libc::mach_port_t,
            target_address: *mut u64,
            size: u64,
            mask: u64,
            flags: libc::c_int,
            src_task: libc::mach_port_t,
            src_address: u64,
            copy: libc::c_int,
            cur_protection: *mut libc::c_int,
            max_protection: *mut libc::c_int,
            inheritance: libc::c_uint,
        ) -> libc::c_int;
        fn mach_vm_deallocate(
            target_task: libc::mach_port_t,
            address: u64,
            size: u64,
        ) -> libc::c_int;
    }
    const VM_FLAGS_ANYWHERE: libc::c_int = 1;
    // (A forked child keeps the open view, as it does the rest of the state.)
    const VM_INHERIT_COPY: libc::c_uint = 1;

    fn remove(open: &Open) {
        // SAFETY: the view is ours, and the lock is held by the caller, so no
        // read is using it.
        unsafe { mach_vm_deallocate(libc::mach_task_self(), open.address, open.size) };
    }

    /// Run `read` on a view of `payload`; `false` if it couldn't have one.
    pub(super) fn read(payload: &Payload, read: &mut dyn FnMut(&[u8])) -> bool {
        let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(current) = open.as_mut() {
            if current.id == payload.id && current.reads < MAX_READS {
                current.reads += 1;
            } else {
                remove(current);
                *open = None;
            }
        }
        if open.is_none() {
            let (mut address, mut current, mut max) = (0u64, 0, 0);
            let size = payload.map.len() as u64;
            // SAFETY: the source range is a live mapping; the view is ours.
            let mapped = unsafe {
                mach_vm_remap(
                    libc::mach_task_self(),
                    &raw mut address,
                    size,
                    0,
                    VM_FLAGS_ANYWHERE,
                    libc::mach_task_self(),
                    payload.map.as_ptr() as u64,
                    0,
                    &raw mut current,
                    &raw mut max,
                    VM_INHERIT_COPY,
                )
            } == 0;
            if !mapped {
                return false;
            }
            *open = Some(Open {
                id: payload.id,
                address,
                size,
                reads: 1,
            });
        }
        let current = open.as_ref().expect("opened above");
        // SAFETY: the view maps `size` readable bytes while the lock is held
        // (only `remove`, under it, unmaps it). Nothing reads a nested code
        // object's body during this read, so the lock isn't taken again.
        let bytes = unsafe {
            std::slice::from_raw_parts(current.address as *const u8, current.size as usize)
        };
        read(&bytes[FROZEN_HEADER_LEN..]);
        true
    }

    /// Remove the view of the artifact `id`, if it's the open one.
    pub(super) fn close(id: u64) {
        let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
        if open.as_ref().is_some_and(|current| current.id == id) {
            if let Some(current) = open.take() {
                remove(&current);
            }
        }
    }
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
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
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
        code.name = "foo".into();
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
        code.name = "warmed".into();
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
