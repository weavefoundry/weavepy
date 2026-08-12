//! The `mmap` module — RFC 0023, rebuilt against CPython's
//! `mmapmodule.c` for RFC 0057 WS9/WS10.
//!
//! On Unix the mapping is a raw `mmap(2)` region (so `flags`/`prot`/
//! `offset` behave exactly like CPython's), owned by an `MmapRegion`
//! that unmaps on drop. The full CPython surface is provided: two-phase
//! `__new__` construction (subclasses delegate `mmap.mmap.__new__(cls,
//! -1, …)`), `read`/`readline`/`read_byte`, `write`/`write_byte`,
//! `seek` (returning the new position)/`tell`/`seekable`, `size`
//! (fstat of the dup'ed fd)/`__len__`, `find`/`rfind` with
//! slice-notation `start`/`end`, `move`, `resize` (mremap on Linux;
//! `SystemError` elsewhere, as CPython), `flush(offset, size)`,
//! `madvise`, subscripting with extended slices, the `closed`
//! property, and CPython's `__repr__` format.

use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::builtins::{coerce_index_i64, seq_index_bound, try_coerce_index_i64};
use crate::error::{
    buffer_error, index_error, overflow_error, type_error, value_error, RuntimeError,
};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule, SharedMemBuffer};
use crate::types::{PyInstance, TypeFlags, TypeObject};

pub const ACCESS_DEFAULT: i64 = 0;
pub const ACCESS_READ: i64 = 1;
pub const ACCESS_WRITE: i64 = 2;
pub const ACCESS_COPY: i64 = 3;

/// Only the no-`mremap()` resize fallback raises SystemError; Linux
/// resizes in place and Windows raises a deferred-support OSError
/// (RFC 0063).
#[cfg(not(any(target_os = "linux", windows)))]
fn system_error(message: &str) -> RuntimeError {
    RuntimeError::PyException(crate::error::PyException::from_builtin(
        "SystemError",
        message,
    ))
}

/// `OSError` from the thread's current `errno`, with CPython's PEP 3151
/// subclass mapping (EACCES → `PermissionError`, …).
#[cfg(unix)]
fn errno_error() -> RuntimeError {
    crate::error::io_error_to_py(&std::io::Error::last_os_error())
}

fn closed_error() -> RuntimeError {
    value_error("mmap closed or invalid")
}

fn page_size() -> i64 {
    #[cfg(unix)]
    {
        let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if v > 0 {
            v as i64
        } else {
            4096
        }
    }
    #[cfg(not(unix))]
    {
        4096
    }
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("mmap"),
        );
        let mut consts: Vec<(&'static str, i64)> = vec![
            ("ACCESS_DEFAULT", ACCESS_DEFAULT),
            ("ACCESS_READ", ACCESS_READ),
            ("ACCESS_WRITE", ACCESS_WRITE),
            ("ACCESS_COPY", ACCESS_COPY),
        ];
        #[cfg(unix)]
        consts.extend([
            ("MAP_SHARED", i64::from(libc::MAP_SHARED)),
            ("MAP_PRIVATE", i64::from(libc::MAP_PRIVATE)),
            ("MAP_ANON", i64::from(libc::MAP_ANON)),
            ("MAP_ANONYMOUS", i64::from(libc::MAP_ANONYMOUS)),
            ("PROT_READ", i64::from(libc::PROT_READ)),
            ("PROT_WRITE", i64::from(libc::PROT_WRITE)),
            ("PROT_EXEC", i64::from(libc::PROT_EXEC)),
            ("MADV_NORMAL", i64::from(libc::MADV_NORMAL)),
            ("MADV_RANDOM", i64::from(libc::MADV_RANDOM)),
            ("MADV_SEQUENTIAL", i64::from(libc::MADV_SEQUENTIAL)),
            ("MADV_WILLNEED", i64::from(libc::MADV_WILLNEED)),
            ("MADV_DONTNEED", i64::from(libc::MADV_DONTNEED)),
            ("MADV_FREE", i64::from(libc::MADV_FREE)),
        ]);
        #[cfg(target_os = "linux")]
        consts.extend([
            ("MAP_DENYWRITE", i64::from(libc::MAP_DENYWRITE)),
            ("MAP_EXECUTABLE", i64::from(libc::MAP_EXECUTABLE)),
            ("MAP_POPULATE", i64::from(libc::MAP_POPULATE)),
            ("MAP_STACK", i64::from(libc::MAP_STACK)),
            ("MAP_NORESERVE", i64::from(libc::MAP_NORESERVE)),
            ("MADV_REMOVE", i64::from(libc::MADV_REMOVE)),
            ("MADV_DONTFORK", i64::from(libc::MADV_DONTFORK)),
            ("MADV_DOFORK", i64::from(libc::MADV_DOFORK)),
            ("MADV_MERGEABLE", i64::from(libc::MADV_MERGEABLE)),
            ("MADV_UNMERGEABLE", i64::from(libc::MADV_UNMERGEABLE)),
            ("MADV_HUGEPAGE", i64::from(libc::MADV_HUGEPAGE)),
            ("MADV_NOHUGEPAGE", i64::from(libc::MADV_NOHUGEPAGE)),
            ("MADV_DONTDUMP", i64::from(libc::MADV_DONTDUMP)),
            ("MADV_DODUMP", i64::from(libc::MADV_DODUMP)),
        ]);
        #[cfg(windows)]
        consts.extend([
            ("MAP_SHARED", 0x01),
            ("MAP_PRIVATE", 0x02),
            ("PROT_READ", 0x01),
            ("PROT_WRITE", 0x02),
            ("PROT_EXEC", 0x04),
        ]);
        for (n, v) in consts {
            d.insert(DictKey(Object::from_static(n)), Object::Int(v));
        }
        // `mmap.PAGESIZE`/`ALLOCATIONGRANULARITY`: the live system page size
        // (`multiprocessing.heap.Heap` uses it as its default arena size). On
        // POSIX the allocation granularity equals the page size.
        let pagesize = page_size();
        d.insert(
            DictKey(Object::from_static("PAGESIZE")),
            Object::Int(pagesize),
        );
        d.insert(
            DictKey(Object::from_static("ALLOCATIONGRANULARITY")),
            Object::Int(pagesize),
        );
        d.insert(
            DictKey(Object::from_static("mmap")),
            Object::Type(mmap_type()),
        );
        d.insert(
            DictKey(Object::from_static("error")),
            Object::Type(crate::builtin_types::builtin_types().os_error.clone()),
        );
    }
    Rc::new(PyModule {
        name: "mmap".to_owned(),
        filename: None,
        dict,
    })
}

fn m(name: &'static str, f: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: true,
        call: Box::new(f),
        call_kw: None,
    }))
}

fn mmap_type() -> Rc<TypeObject> {
    use crate::builtin_types::builtin_types;
    let bt = builtin_types();
    let mut td = DictData::default();
    for (name, fn_) in [
        (
            "read",
            mm_read as fn(&[Object]) -> Result<Object, RuntimeError>,
        ),
        ("read_byte", mm_read_byte),
        ("readline", mm_readline),
        ("write", mm_write),
        ("write_byte", mm_write_byte),
        ("seek", mm_seek),
        ("seekable", mm_seekable),
        ("tell", mm_tell),
        ("size", mm_size),
        ("flush", mm_flush),
        ("close", mm_close),
        ("find", mm_find),
        ("rfind", mm_rfind),
        ("move", mm_move),
        ("resize", mm_resize),
        ("__enter__", mm_enter),
        ("__exit__", mm_exit),
        ("__len__", mm_len),
        ("__repr__", mm_repr),
        ("__getitem__", mm_getitem),
        ("__setitem__", mm_setitem),
    ] {
        td.insert(DictKey(Object::from_static(name)), m(name, fn_));
    }
    #[cfg(unix)]
    td.insert(
        DictKey(Object::from_static("madvise")),
        m("madvise", mm_madvise),
    );
    // Read-only `closed` property, as CPython's getset.
    td.insert(
        DictKey(Object::from_static("closed")),
        Object::Property(Rc::new(crate::object::PyProperty::new(
            m("closed", mm_closed_get),
            Object::None,
            Object::None,
            Object::None,
        ))),
    );
    td.insert(
        DictKey(Object::from_static("__module__")),
        Object::from_static("mmap"),
    );
    // All construction lives in `__new__` (CPython's `new_mmap_object` is
    // the tp_new slot), so a subclass `__new__` can delegate
    // `mmap.mmap.__new__(cls, -1, *args)` and receive a fully-mapped
    // instance of `cls` (test_mmap.test_subclass). `__init__` is a
    // permissive no-op so the constructor arguments passing through
    // `type.__call__` don't trip object.__init__ arity checks.
    td.insert(
        DictKey(Object::from_static("__new__")),
        Object::StaticMethod(crate::object::MethodWrapper::new(Object::Builtin(Rc::new(
            BuiltinFn {
                name: "mmap.__new__",
                binds_instance: false,
                call: Box::new(|args| mm_new(args, &[])),
                call_kw: Some(Box::new(mm_new)),
            },
        )))),
    );
    td.insert(
        DictKey(Object::from_static("__init__")),
        Object::Builtin(Rc::new(BuiltinFn {
            name: "__init__",
            binds_instance: true,
            call: Box::new(|_args| Ok(Object::None)),
            call_kw: Some(Box::new(|_args, _kwargs| Ok(Object::None))),
        })),
    );
    TypeObject::new_with_flags(
        "mmap",
        vec![bt.object_.clone()],
        td,
        TypeFlags {
            is_exception: false,
            is_builtin: true,
        },
    )
    .expect("mmap.mmap")
}

/// The raw mapped region, shared (via `Rc` = `Arc`) between the `mmap`
/// object and any `memoryview` exported over it. A memory mapping never
/// moves (except through `resize`, which requires no extant exports),
/// so the base pointer stays valid for as long as this `Arc` is held —
/// which is exactly what lets a `memoryview` keep the mapping alive
/// past `mmap.close()`.
pub struct MmapRegion {
    ptr: AtomicPtr<u8>,
    len: AtomicUsize,
    /// Buffer-protocol export flag: `access == ACCESS_READ`.
    readonly: bool,
    /// Windows keeps the `memmap2` mapping alive here; Unix owns a raw
    /// region released in `Drop`.
    #[cfg(windows)]
    win_backing: Option<WinBacking>,
}

#[cfg(windows)]
enum WinBacking {
    Read(memmap2::Mmap),
    Write(memmap2::MmapMut),
}

impl std::fmt::Debug for MmapRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmapRegion")
            .field("len", &self.byte_len())
            .field("readonly", &self.readonly)
            .finish_non_exhaustive()
    }
}

impl Drop for MmapRegion {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let p = self.ptr.load(Ordering::Relaxed);
            let l = self.len.load(Ordering::Relaxed);
            if !p.is_null() && l > 0 {
                // SAFETY: on Unix the pointer/length always describe a live
                // mapping we own exclusively at drop time.
                unsafe {
                    libc::munmap(p.cast(), l);
                }
            }
        }
    }
}

impl MmapRegion {
    fn base(&self) -> *mut u8 {
        self.ptr.load(Ordering::Relaxed)
    }
    fn byte_len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }
    fn as_slice(&self) -> &[u8] {
        // SAFETY: the mapping is live for `&self`; the GIL serialises all
        // Python-level access so no concurrent `&mut` view exists.
        unsafe { std::slice::from_raw_parts(self.base(), self.byte_len()) }
    }
    /// SAFETY-by-convention: callers must have verified `access !=
    /// ACCESS_READ` (writes to a PROT_READ mapping fault). The GIL
    /// serialises access, so no concurrent borrow of the region exists.
    #[allow(clippy::mut_from_ref)]
    fn as_mut_slice(&self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.base(), self.byte_len()) }
    }
}

// SAFETY: the region is genuinely shared memory whose pointer is stable,
// and every mutation goes through the GIL.
impl SharedMemBuffer for MmapRegion {
    fn byte_len(&self) -> usize {
        self.byte_len()
    }
    fn data_ptr(&self) -> *mut u8 {
        self.base()
    }
    fn is_readonly(&self) -> bool {
        self.readonly
    }
}

struct MmapState {
    region: Rc<MmapRegion>,
    pos: usize,
    /// Final (derived) access mode: one of the `ACCESS_*` constants.
    access: i64,
    /// File offset the mapping starts at (repr / resize / size).
    offset: i64,
    /// The file descriptor (`-1` for anonymous or `trackfd=False`).
    /// On unix it is a dup the mapping owns (closed by `mm_close`); on
    /// Windows it is the caller's CRT fd, held non-owning so `size()`
    /// can re-derive a metadata view (RFC 0063 fd model).
    fd: i32,
    /// The `mmap(2)` flags actually used (only the Linux `resize` path
    /// consults it, for the shared-anonymous-grow guard).
    #[allow(dead_code)]
    flags: i64,
    /// Whether the constructor was allowed to keep an fd (`trackfd`).
    trackfd: bool,
}

/// Process-global mmap registry: an `mmap` created on one OS thread can be
/// used from another (a `multiprocessing` heap arena is allocated on the
/// main thread but read and written by Queue feeder / pool worker
/// threads). Access is serialised by the GIL; the `parking_lot::Mutex`
/// only guards the table itself.
fn registry() -> &'static parking_lot::Mutex<HashMap<usize, Rc<RefCell<MmapState>>>> {
    static REGISTRY: std::sync::OnceLock<
        parking_lot::Mutex<HashMap<usize, Rc<RefCell<MmapState>>>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn next_id() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn alloc_state(state: MmapState) -> usize {
    let id = next_id();
    registry().lock().insert(id, Rc::new(RefCell::new(state)));
    id
}

fn state_id(inst: &Rc<PyInstance>) -> Result<usize, RuntimeError> {
    match inst
        .dict
        .borrow()
        .get(&DictKey(Object::from_static("_id")))
        .cloned()
    {
        Some(Object::Int(i)) if i > 0 => Ok(i as usize),
        _ => Err(closed_error()),
    }
}

/// CPython's `CHECK_VALID`: hand back the live state cell or raise
/// "mmap closed or invalid". Callers take *short* borrows and must never
/// hold one across a VM re-entry (`__index__` coercion can close the map
/// — gh-103987).
fn state_cell(inst: &Rc<PyInstance>) -> Result<Rc<RefCell<MmapState>>, RuntimeError> {
    let id = state_id(inst)?;
    let map = registry().lock();
    map.get(&id).cloned().ok_or_else(closed_error)
}

/// Buffer-protocol export for `memoryview(mmap_obj)`: hands back the shared
/// region so the view writes straight through to the mapping (and, for a
/// `MAP_SHARED` file mapping, to every other process mapping it). Returns
/// `None` for a closed mapping.
pub fn shared_buffer(inst: &Rc<PyInstance>) -> Option<Rc<dyn SharedMemBuffer>> {
    let cell = state_cell(inst).ok()?;
    let region: Rc<dyn SharedMemBuffer> = cell.borrow().region.clone();
    Some(region)
}

fn self_arg(args: &[Object]) -> Result<Rc<PyInstance>, RuntimeError> {
    match args.first() {
        Some(Object::Instance(i)) => Ok(i.clone()),
        _ => Err(type_error("mmap method: missing self")),
    }
}

/// `y*`-style bytes-like extraction (str is *rejected*, as CPython).
fn bytes_like(o: Option<&Object>, func: &str) -> Result<Vec<u8>, RuntimeError> {
    match o {
        Some(Object::Bytes(b)) => Ok(b.to_vec()),
        Some(Object::ByteArray(b)) => Ok(b.borrow().clone()),
        Some(Object::MemoryView(mv)) => Ok(mv.to_bytes()),
        Some(other) => Err(type_error(format!(
            "{func}() argument must be a bytes-like object, not '{}'",
            other.type_name_owned()
        ))),
        None => Err(type_error(format!(
            "{func}() takes at least 1 argument (0 given)"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// `mmap.__new__(cls, fileno, length, flags=MAP_SHARED, prot=PROT_READ|
/// PROT_WRITE, access=ACCESS_DEFAULT, offset=0, *, trackfd=True)` — the
/// Unix signature of CPython's `new_mmap_object`.
fn mm_new(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    let Some(Object::Type(cls)) = args.first() else {
        return Err(type_error("mmap.__new__(X): X is not a type object"));
    };
    let pos = &args[1..];

    #[cfg(unix)]
    const NAMES: [&str; 6] = ["fileno", "length", "flags", "prot", "access", "offset"];
    #[cfg(windows)]
    const NAMES: [&str; 5] = ["fileno", "length", "tagname", "access", "offset"];

    if pos.len() > NAMES.len() {
        return Err(type_error(format!(
            "mmap() takes at most {} positional arguments ({} given)",
            NAMES.len(),
            pos.len()
        )));
    }
    let mut slots: Vec<Option<Object>> = vec![None; NAMES.len()];
    for (i, v) in pos.iter().enumerate() {
        slots[i] = Some(v.clone());
    }
    let mut trackfd = true;
    for (k, v) in kwargs {
        if cfg!(unix) && k == "trackfd" {
            trackfd = !matches!(v, Object::Bool(false) | Object::Int(0) | Object::None);
            continue;
        }
        match NAMES.iter().position(|n| n == k) {
            Some(idx) => {
                if slots[idx].is_some() {
                    return Err(type_error(format!(
                        "argument for mmap() given by name ('{k}') and position ({})",
                        idx + 1
                    )));
                }
                slots[idx] = Some(v.clone());
            }
            None => {
                return Err(type_error(format!(
                    "'{k}' is an invalid keyword argument for mmap()"
                )))
            }
        }
    }
    let fileno = match &slots[0] {
        Some(o) => coerce_index_i64(o)?,
        None => {
            return Err(type_error(
                "function missing required argument 'fileno' (pos 1)",
            ))
        }
    };
    let map_size = match &slots[1] {
        Some(o) => coerce_index_i64(o)?,
        None => {
            return Err(type_error(
                "function missing required argument 'length' (pos 2)",
            ))
        }
    };
    if map_size < 0 {
        return Err(overflow_error("memory mapped length must be positive"));
    }

    #[cfg(unix)]
    {
        let mut flags = match &slots[2] {
            Some(o) => coerce_index_i64(o)?,
            None => i64::from(libc::MAP_SHARED),
        };
        let mut prot = match &slots[3] {
            Some(o) => coerce_index_i64(o)?,
            None => i64::from(libc::PROT_READ | libc::PROT_WRITE),
        };
        let mut access = match &slots[4] {
            Some(o) => coerce_index_i64(o)?,
            None => ACCESS_DEFAULT,
        };
        let offset = match &slots[5] {
            Some(o) => coerce_index_i64(o)?,
            None => 0,
        };
        if offset < 0 {
            return Err(overflow_error("memory mapped offset must be positive"));
        }
        // PEP 578: `mmap.__new__(fileno, length, access, offset)`.
        crate::stdlib::sys::audit_event(
            "mmap.__new__",
            &[
                Object::Int(fileno),
                Object::Int(map_size),
                Object::Int(access),
                Object::Int(offset),
            ],
        )?;
        if access != ACCESS_DEFAULT
            && (flags != i64::from(libc::MAP_SHARED)
                || prot != i64::from(libc::PROT_READ | libc::PROT_WRITE))
        {
            return Err(value_error(
                "mmap can't specify both access and flags, prot.",
            ));
        }
        match access {
            ACCESS_READ => {
                flags = i64::from(libc::MAP_SHARED);
                prot = i64::from(libc::PROT_READ);
            }
            ACCESS_WRITE => {
                flags = i64::from(libc::MAP_SHARED);
                prot = i64::from(libc::PROT_READ | libc::PROT_WRITE);
            }
            ACCESS_COPY => {
                flags = i64::from(libc::MAP_PRIVATE);
                prot = i64::from(libc::PROT_READ | libc::PROT_WRITE);
            }
            ACCESS_DEFAULT => {
                // Map prot back to an access type (a read-only prot makes a
                // readonly map, so the write guards fire before a fault).
                let r = prot & i64::from(libc::PROT_READ) != 0;
                let w = prot & i64::from(libc::PROT_WRITE) != 0;
                if !(r && w) {
                    access = if w { ACCESS_WRITE } else { ACCESS_READ };
                }
            }
            _ => return Err(value_error("mmap invalid access parameter.")),
        }

        let fd = fileno as i32;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if fd != -1 {
            // Issue #11277: fsync(2) is not enough on OS X — the OS X
            // specific fcntl forces DISKSYNC and works around an mmap bug.
            unsafe {
                libc::fcntl(fd, libc::F_FULLFSYNC);
            }
        }

        let mut map_size = map_size;
        if fd != -1 {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            let fstat_ok = unsafe { libc::fstat(fd, &raw mut st) } == 0;
            if fstat_ok && (st.st_mode & libc::S_IFMT) == libc::S_IFREG {
                if map_size == 0 {
                    if st.st_size == 0 {
                        return Err(value_error("cannot mmap an empty file"));
                    }
                    if offset >= st.st_size {
                        return Err(value_error("mmap offset is greater than file size"));
                    }
                    map_size = st.st_size - offset;
                } else if offset > st.st_size || st.st_size - offset < map_size {
                    return Err(value_error("mmap length is greater than file size"));
                }
            }
        }

        let mut own_fd = -1;
        if fd == -1 {
            flags |= i64::from(libc::MAP_ANONYMOUS);
        } else if trackfd {
            own_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
            if own_fd == -1 {
                return Err(errno_error());
            }
        }

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_size as libc::size_t,
                prot as libc::c_int,
                flags as libc::c_int,
                fd,
                offset as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            let err = errno_error();
            if own_fd >= 0 {
                unsafe {
                    libc::close(own_fd);
                }
            }
            return Err(err);
        }

        let region = MmapRegion {
            ptr: AtomicPtr::new(ptr.cast()),
            len: AtomicUsize::new(map_size as usize),
            readonly: access == ACCESS_READ,
        };
        let id = alloc_state(MmapState {
            region: Rc::new(region),
            pos: 0,
            access,
            offset,
            fd: own_fd,
            flags,
            trackfd,
        });
        let inst = Rc::new(PyInstance::new(cls.clone()));
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("_id")), Object::Int(id as i64));
        Ok(Object::Instance(inst))
    }

    #[cfg(windows)]
    {
        // Windows arm (memmap2-backed): `tagname` is accepted for
        // signature parity only; non-zero `offset` is deferred along
        // with `resize` (RFC 0063 "mmap residuals").
        let access = match &slots[3] {
            Some(o) => coerce_index_i64(o)?,
            None => ACCESS_DEFAULT,
        };
        let offset = match &slots[4] {
            Some(o) => coerce_index_i64(o)?,
            None => 0,
        };
        if !(ACCESS_DEFAULT..=ACCESS_COPY).contains(&access) {
            return Err(value_error("mmap invalid access parameter."));
        }
        if offset < 0 {
            return Err(overflow_error("memory mapped offset must be positive"));
        }
        if offset != 0 {
            // CPython maps at any allocation-granularity offset via
            // CreateFileMapping/MapViewOfFile; deferred (RFC 0063).
            return Err(crate::error::os_error(
                "mmap: non-zero offset is not supported on Windows in WeavePy yet (RFC 0063)",
            ));
        }
        let _ = trackfd;
        let fd = fileno as i32;
        let mut map_size = map_size;
        let backing = if fd == -1 {
            let map = memmap2::MmapMut::map_anon(map_size as usize)
                .map_err(|e| crate::error::os_error(format!("mmap_anon: {e}")))?;
            WinBacking::Write(map)
        } else {
            // RFC 0063 fd model: the Python-visible integer is a CRT fd,
            // not a HANDLE (CPython's mmapmodule.c bridges through
            // `_get_osfhandle` the same way). `file_view_from_fd` hands
            // back a non-owning `ManuallyDrop<File>` view — the fd stays
            // the handle's sole owner, and the view is never dropped as
            // an owner, so no double close can occur. The view only has
            // to outlive the map*() calls themselves: CreateFileMapping
            // takes its own reference on the file handle, so the
            // resulting mapping stays valid independently of the fd.
            let file = crate::stdlib::nt_support::file_view_from_fd(fd)
                .map_err(|e| crate::error::io_error_to_py(&e))?;
            // CPython's `new_mmap_object` length rules: length 0 means
            // "the whole file" (empty files rejected). A length past
            // EOF — which CPython satisfies by growing the file — is
            // deferred along with `resize` (RFC 0063).
            let file_len = file
                .metadata()
                .map_err(|e| crate::error::io_error_to_py(&e))?
                .len();
            if map_size == 0 {
                if file_len == 0 {
                    return Err(value_error("cannot mmap an empty file"));
                }
                map_size = file_len as i64;
            } else if map_size as u64 > file_len {
                return Err(value_error("mmap length is greater than file size"));
            }
            let mut opts = memmap2::MmapOptions::new();
            opts.len(map_size as usize);
            let os_err = |e: std::io::Error| crate::error::io_error_to_py(&e);
            match access {
                ACCESS_READ => WinBacking::Read(unsafe { opts.map(&*file) }.map_err(os_err)?),
                // Copy-on-write, like the unix MAP_PRIVATE arm: writes
                // stay in the private copy, never reach the file.
                ACCESS_COPY => WinBacking::Write(unsafe { opts.map_copy(&*file) }.map_err(os_err)?),
                _ => WinBacking::Write(unsafe { opts.map_mut(&*file) }.map_err(os_err)?),
            }
        };
        let (ptr, len) = match &backing {
            WinBacking::Read(m) => (m.as_ptr().cast_mut(), m.len()),
            WinBacking::Write(m) => (m.as_ptr().cast_mut(), m.len()),
        };
        let region = MmapRegion {
            ptr: AtomicPtr::new(ptr),
            len: AtomicUsize::new(len),
            readonly: access == ACCESS_READ,
            win_backing: Some(backing),
        };
        let id = alloc_state(MmapState {
            region: Rc::new(region),
            pos: 0,
            access,
            offset: 0,
            // The CRT fd, kept (not dup'ed — the caller stays the owner,
            // unlike the unix `trackfd` dup) so `size()` can re-derive a
            // metadata view for file-backed maps.
            fd,
            flags: 0,
            trackfd: true,
        });
        let inst = Rc::new(PyInstance::new(cls.clone()));
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("_id")), Object::Int(id as i64));
        Ok(Object::Instance(inst))
    }
}

// ---------------------------------------------------------------------------
// I/O methods
// ---------------------------------------------------------------------------

fn mm_read(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    state_cell(&inst)?;
    // `read(None)` / no argument / a negative count all mean "the rest".
    // Coercion may re-enter the VM (and close the map — gh-103987), so it
    // happens outside any state borrow.
    let n: Option<i64> = match args.get(1) {
        None | Some(Object::None) => None,
        Some(o) => Some(coerce_index_i64(o)?),
    };
    let cell = state_cell(&inst)?;
    let mut st = cell.borrow_mut();
    let region = st.region.clone();
    let buf = region.as_slice();
    let remaining = buf.len().saturating_sub(st.pos);
    let num = match n {
        Some(k) if k >= 0 => (k as usize).min(remaining),
        _ => remaining,
    };
    let start = st.pos.min(buf.len());
    let out = buf[start..start + num].to_vec();
    st.pos = start + num;
    Ok(Object::Bytes(Rc::from(out.into_boxed_slice())))
}

fn mm_read_byte(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let cell = state_cell(&inst)?;
    let mut st = cell.borrow_mut();
    let region = st.region.clone();
    let buf = region.as_slice();
    if st.pos >= buf.len() {
        return Err(value_error("read byte out of range"));
    }
    let b = buf[st.pos];
    st.pos += 1;
    Ok(Object::Int(i64::from(b)))
}

fn mm_readline(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let cell = state_cell(&inst)?;
    let mut st = cell.borrow_mut();
    let region = st.region.clone();
    let buf = region.as_slice();
    let start = st.pos.min(buf.len());
    let mut end = start;
    while end < buf.len() {
        end += 1;
        if buf[end - 1] == b'\n' {
            break;
        }
    }
    let line = buf[start..end].to_vec();
    st.pos = end;
    Ok(Object::Bytes(Rc::from(line.into_boxed_slice())))
}

fn writable_or_err(access: i64) -> Result<(), RuntimeError> {
    if access == ACCESS_READ {
        return Err(type_error("mmap can't modify a readonly memory map."));
    }
    Ok(())
}

fn mm_write(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    state_cell(&inst)?;
    let data = bytes_like(args.get(1), "write")?;
    let cell = state_cell(&inst)?;
    let mut st = cell.borrow_mut();
    writable_or_err(st.access)?;
    let region = st.region.clone();
    let len = region.byte_len();
    if st.pos > len || len - st.pos < data.len() {
        return Err(value_error("data out of range"));
    }
    region.as_mut_slice()[st.pos..st.pos + data.len()].copy_from_slice(&data);
    st.pos += data.len();
    Ok(Object::Int(data.len() as i64))
}

fn mm_write_byte(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    state_cell(&inst)?;
    // The `b` format: `__index__`, then an unsigned-byte range check.
    let v = match args.get(1) {
        Some(o) => match try_coerce_index_i64(o) {
            Some(r) => r?,
            None => {
                return Err(type_error(format!(
                    "'{}' object cannot be interpreted as an integer",
                    o.type_name_owned()
                )))
            }
        },
        None => {
            return Err(type_error(
                "write_byte() takes exactly one argument (0 given)",
            ))
        }
    };
    if v < 0 {
        return Err(overflow_error("unsigned byte integer is less than minimum"));
    }
    if v > 255 {
        return Err(overflow_error(
            "unsigned byte integer is greater than maximum",
        ));
    }
    let cell = state_cell(&inst)?;
    let mut st = cell.borrow_mut();
    writable_or_err(st.access)?;
    let region = st.region.clone();
    if st.pos >= region.byte_len() {
        return Err(value_error("write byte out of range"));
    }
    region.as_mut_slice()[st.pos] = v as u8;
    st.pos += 1;
    Ok(Object::None)
}

fn mm_seek(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    state_cell(&inst)?;
    let dist = match args.get(1) {
        Some(o) => coerce_index_i64(o)?,
        None => return Err(type_error("seek() takes at least 1 argument (0 given)")),
    };
    let how = match args.get(2) {
        Some(o) => coerce_index_i64(o)?,
        None => 0,
    };
    let cell = state_cell(&inst)?;
    let mut st = cell.borrow_mut();
    let len = st.region.byte_len() as i64;
    let out_of_range = || value_error("seek out of range");
    let whence = match how {
        0 => dist,
        1 => (st.pos as i64).checked_add(dist).ok_or_else(out_of_range)?,
        2 => len.checked_add(dist).ok_or_else(out_of_range)?,
        _ => return Err(value_error("unknown seek type")),
    };
    if whence > len || whence < 0 {
        return Err(out_of_range());
    }
    st.pos = whence as usize;
    Ok(Object::Int(whence))
}

fn mm_seekable(args: &[Object]) -> Result<Object, RuntimeError> {
    let _ = self_arg(args)?;
    Ok(Object::Bool(true))
}

fn mm_tell(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let cell = state_cell(&inst)?;
    let pos = cell.borrow().pos;
    Ok(Object::Int(pos as i64))
}

/// `size()` — the *file* size via fstat of the dup'ed fd (an anonymous
/// mapping or one made with `trackfd=False` has fd `-1`, so this raises
/// EBADF, matching CPython's `_Py_fstat(-1)`).
fn mm_size(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let cell = state_cell(&inst)?;
    let st = cell.borrow();
    #[cfg(unix)]
    {
        if st.fd < 0 {
            return Err(crate::error::io_error_to_py(
                &std::io::Error::from_raw_os_error(libc::EBADF),
            ));
        }
        let mut status: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(st.fd, &raw mut status) } != 0 {
            return Err(errno_error());
        }
        Ok(Object::Int(status.st_size))
    }
    #[cfg(windows)]
    {
        // CPython's `mmap_size_method` Windows arm: file-backed maps
        // report the live *file* size (GetFileSizeEx on the backing
        // handle — Modules/mmapmodule.c); anonymous maps report the
        // region length. The metadata view is non-owning (RFC 0063 fd
        // model), so a stale fd surfaces as EBADF, like unix's fstat(-1).
        if st.fd >= 0 {
            let view = crate::stdlib::nt_support::file_view_from_fd(st.fd)
                .map_err(|e| crate::error::io_error_to_py(&e))?;
            let meta = view
                .metadata()
                .map_err(|e| crate::error::io_error_to_py(&e))?;
            return Ok(Object::Int(meta.len() as i64));
        }
        Ok(Object::Int(st.region.byte_len() as i64))
    }
}

fn mm_flush(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    state_cell(&inst)?;
    let offset = match args.get(1) {
        Some(o) => coerce_index_i64(o)?,
        None => 0,
    };
    let size_arg = match args.get(2) {
        Some(o) => Some(coerce_index_i64(o)?),
        None => None,
    };
    let cell = state_cell(&inst)?;
    let st = cell.borrow();
    let len = st.region.byte_len() as i64;
    let size = size_arg.unwrap_or(len);
    if size < 0 || offset < 0 || len - offset < size {
        return Err(value_error("flush values out of range"));
    }
    if st.access == ACCESS_READ || st.access == ACCESS_COPY {
        return Ok(Object::None);
    }
    #[cfg(unix)]
    {
        let ptr = st.region.base();
        // SAFETY: offset/size validated against the live mapping above.
        if unsafe {
            libc::msync(
                ptr.add(offset as usize).cast(),
                size as libc::size_t,
                libc::MS_SYNC,
            )
        } == -1
        {
            return Err(errno_error());
        }
    }
    #[cfg(windows)]
    {
        // memmap2's flush is exactly CPython's `mmap_flush_method` pair:
        // FlushViewOfFile on the requested range, then FlushFileBuffers
        // on the backing handle (Modules/mmapmodule.c). Read-only and
        // copy-on-write maps already returned above, so the remaining
        // backing is the shared-writable mapping.
        if let Some(WinBacking::Write(map)) = &st.region.win_backing {
            map.flush_range(offset as usize, size as usize)
                .map_err(|e| crate::error::io_error_to_py(&e))?;
        }
    }
    Ok(Object::None)
}

fn mm_close(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    if let Ok(id) = state_id(&inst) {
        // Drop the registry's reference. Any `memoryview` still exporting
        // the region holds its own `Arc`, so the mapping survives until
        // released.
        let removed = registry().lock().remove(&id);
        #[cfg(unix)]
        if let Some(cell) = removed {
            let fd = cell.borrow().fd;
            if fd >= 0 {
                unsafe {
                    libc::close(fd);
                }
            }
        }
        #[cfg(not(unix))]
        drop(removed);
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("_id")), Object::Int(0));
    }
    Ok(Object::None)
}

fn mm_closed_get(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    Ok(Object::Bool(state_cell(&inst).is_err()))
}

fn mm_enter(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    state_cell(&inst)?;
    Ok(args[0].clone())
}

fn mm_exit(args: &[Object]) -> Result<Object, RuntimeError> {
    mm_close(&args[..1])
}

fn mm_len(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let cell = state_cell(&inst)?;
    let len = cell.borrow().region.byte_len();
    Ok(Object::Int(len as i64))
}

fn mm_repr(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    let cls = inst.cls();
    let tp_name = if cls.name == "mmap" {
        "mmap.mmap".to_owned()
    } else {
        cls.name.clone()
    };
    let out = match state_cell(&inst) {
        Err(_) => format!("<{tp_name} closed=True>"),
        Ok(cell) => {
            let st = cell.borrow();
            let access_str = match st.access {
                ACCESS_READ => "ACCESS_READ",
                ACCESS_WRITE => "ACCESS_WRITE",
                ACCESS_COPY => "ACCESS_COPY",
                _ => "ACCESS_DEFAULT",
            };
            format!(
                "<{tp_name} closed=False, access={access_str}, length={}, pos={}, offset={}>",
                st.region.byte_len(),
                st.pos,
                st.offset
            )
        }
    };
    Ok(Object::from_str(out))
}

// ---------------------------------------------------------------------------
// find / rfind / move / madvise / resize
// ---------------------------------------------------------------------------

fn locate(hay: &[u8], needle: &[u8], base: i64, reverse: bool) -> i64 {
    let n = needle.len();
    let h = hay.len();
    if n > h {
        return -1;
    }
    if n == 0 {
        return base + if reverse { h as i64 } else { 0 };
    }
    if reverse {
        for i in (0..=h - n).rev() {
            if &hay[i..i + n] == needle {
                return base + i as i64;
            }
        }
    } else {
        for i in 0..=h - n {
            if &hay[i..i + n] == needle {
                return base + i as i64;
            }
        }
    }
    -1
}

fn mm_gfind(args: &[Object], reverse: bool) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    // Snapshot the defaults (start = current pos, end = size) while the
    // map is known-live, then coerce arguments — which can close it.
    let (def_start, def_end) = {
        let cell = state_cell(&inst)?;
        let st = cell.borrow();
        (st.pos as i64, st.region.byte_len() as i64)
    };
    let needle = bytes_like(args.get(1), if reverse { "rfind" } else { "find" })?;
    let mut start = match args.get(2) {
        Some(o) => coerce_index_i64(o)?,
        None => def_start,
    };
    let mut end = match args.get(3) {
        Some(o) => coerce_index_i64(o)?,
        None => def_end,
    };
    let cell = state_cell(&inst)?;
    let st = cell.borrow();
    let region = st.region.clone();
    let size = region.byte_len() as i64;
    if start < 0 {
        start += size;
    }
    start = start.clamp(0, size);
    if end < 0 {
        end += size;
    }
    end = end.clamp(0, size);
    if end < start {
        return Ok(Object::Int(-1));
    }
    let buf = region.as_slice();
    Ok(Object::Int(locate(
        &buf[start as usize..end as usize],
        &needle,
        start,
        reverse,
    )))
}

fn mm_find(args: &[Object]) -> Result<Object, RuntimeError> {
    mm_gfind(args, false)
}

fn mm_rfind(args: &[Object]) -> Result<Object, RuntimeError> {
    mm_gfind(args, true)
}

fn mm_move(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    {
        let cell = state_cell(&inst)?;
        writable_or_err(cell.borrow().access)?;
    }
    let mut vals = [0i64; 3];
    for (i, name) in ["dest", "src", "count"].iter().enumerate() {
        vals[i] = match args.get(i + 1) {
            Some(o) => coerce_index_i64(o)?,
            None => return Err(type_error(format!("move() missing argument '{name}'"))),
        };
    }
    let [dest, src, cnt] = vals;
    let cell = state_cell(&inst)?;
    let st = cell.borrow();
    let region = st.region.clone();
    let size = region.byte_len() as i64;
    if dest < 0 || src < 0 || cnt < 0 || size - dest < cnt || size - src < cnt {
        return Err(value_error("source, destination, or count out of range"));
    }
    region
        .as_mut_slice()
        .copy_within(src as usize..(src + cnt) as usize, dest as usize);
    Ok(Object::None)
}

#[cfg(unix)]
fn mm_madvise(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    state_cell(&inst)?;
    let option = match args.get(1) {
        Some(o) => coerce_index_i64(o)?,
        None => return Err(type_error("madvise() missing argument 'option'")),
    };
    let start = match args.get(2) {
        Some(o) => coerce_index_i64(o)?,
        None => 0,
    };
    let length_arg = match args.get(3) {
        Some(o) => Some(coerce_index_i64(o)?),
        None => None,
    };
    let cell = state_cell(&inst)?;
    let st = cell.borrow();
    let size = st.region.byte_len() as i64;
    let mut length = length_arg.unwrap_or(size);
    if start < 0 || start >= size {
        return Err(value_error("madvise start out of bounds"));
    }
    if length < 0 {
        return Err(value_error("madvise length invalid"));
    }
    if i64::MAX - start < length {
        return Err(overflow_error("madvise length too large"));
    }
    if start + length > size {
        length = size - start;
    }
    let ptr = st.region.base();
    // SAFETY: start/length validated against the live mapping above.
    if unsafe {
        libc::madvise(
            ptr.add(start as usize).cast(),
            length as libc::size_t,
            option as libc::c_int,
        )
    } != 0
    {
        return Err(errno_error());
    }
    Ok(Object::None)
}

fn mm_resize(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    state_cell(&inst)?;
    let new_size = match args.get(1) {
        Some(o) => coerce_index_i64(o)?,
        None => return Err(type_error("resize() missing argument 'newsize'")),
    };
    let cell = state_cell(&inst)?;
    let st = cell.borrow();
    // CPython's `is_resizeable`, in order: extant buffer exports →
    // BufferError; `trackfd=False` → ValueError; readonly / copy-on-write
    // → TypeError.
    if Rc::strong_count(&st.region) > 1 {
        return Err(buffer_error(
            "mmap can't resize with extant buffers exported.",
        ));
    }
    if !st.trackfd {
        return Err(value_error("mmap can't resize with trackfd=False."));
    }
    if st.access != ACCESS_WRITE && st.access != ACCESS_DEFAULT {
        return Err(type_error(
            "mmap can't resize a readonly or copy-on-write memory map.",
        ));
    }
    if new_size < 0 || i64::MAX - new_size < st.offset {
        return Err(value_error("new size out of range"));
    }
    #[cfg(target_os = "linux")]
    {
        let old_len = st.region.byte_len();
        // Linux mremap() refuses to grow a shared anonymous mapping
        // (kernel bug 8691) — reject it here, as CPython does. Reaching
        // this point with `fd == -1` means anonymous (`trackfd=False`
        // was rejected just above).
        if st.fd == -1
            && (st.flags & i64::from(libc::MAP_PRIVATE)) == 0
            && new_size as usize > old_len
        {
            return Err(value_error("mmap: can't expand a shared anonymous mapping"));
        }
        if st.fd != -1 && unsafe { libc::ftruncate(st.fd, st.offset + new_size) } == -1 {
            return Err(errno_error());
        }
        let old_ptr = st.region.base();
        let newmap = unsafe {
            libc::mremap(
                old_ptr.cast(),
                old_len,
                new_size as libc::size_t,
                libc::MREMAP_MAYMOVE,
            )
        };
        if newmap == libc::MAP_FAILED {
            return Err(errno_error());
        }
        st.region.ptr.store(newmap.cast(), Ordering::Relaxed);
        st.region.len.store(new_size as usize, Ordering::Relaxed);
        Ok(Object::None)
    }
    #[cfg(windows)]
    {
        let _ = new_size;
        drop(st);
        // CPython resizes Windows maps for real (UnmapViewOfFile +
        // SetFilePointer/SetEndOfFile + a fresh CreateFileMapping —
        // Modules/mmapmodule.c `mmap_resize_method`); WeavePy's
        // memmap2-backed map can't remap in place, so full support is
        // deferred (RFC 0063 "mmap residuals"). OSError — not the
        // no-mremap SystemError — so callers see a catchable,
        // documented failure rather than an internal-error shape.
        Err(crate::error::os_error(
            "mmap: resizing is not supported on Windows in WeavePy yet (RFC 0063)",
        ))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = new_size;
        drop(st);
        Err(system_error("mmap: resizing not available--no mremap()"))
    }
}

// ---------------------------------------------------------------------------
// Subscripting
// ---------------------------------------------------------------------------

struct AdjSlice {
    start: i64,
    step: i64,
    len: i64,
}

fn adjust_slice(len: i64, start: Option<i64>, stop: Option<i64>, step: i64) -> AdjSlice {
    let (lower, upper) = if step < 0 { (-1, len - 1) } else { (0, len) };
    let clamp = |v: Option<i64>, default: i64| -> i64 {
        match v {
            None => default,
            Some(mut v) => {
                if v < 0 {
                    v += len;
                    if v < lower {
                        v = lower;
                    }
                } else if v > upper {
                    v = upper;
                }
                v
            }
        }
    };
    let s = clamp(start, if step < 0 { upper } else { lower });
    let e = clamp(stop, if step < 0 { lower } else { upper });
    let slicelen = if step < 0 {
        if e < s {
            (s - e - 1) / (-step) + 1
        } else {
            0
        }
    } else if s < e {
        (e - s - 1) / step + 1
    } else {
        0
    };
    AdjSlice {
        start: s,
        step,
        len: slicelen,
    }
}

/// `PySlice_Unpack`: saturating `__index__` on each component; step 0 is
/// a ValueError. Components can re-enter the VM (gh-103987), so this runs
/// before any state borrow.
fn unpack_slice(
    sl: &crate::object::PySlice,
) -> Result<(Option<i64>, Option<i64>, i64), RuntimeError> {
    let step = match &sl.step {
        Object::None => 1,
        o => seq_index_bound(o)?,
    };
    if step == 0 {
        return Err(value_error("slice step cannot be zero"));
    }
    let start = match &sl.start {
        Object::None => None,
        o => Some(seq_index_bound(o)?),
    };
    let stop = match &sl.stop {
        Object::None => None,
        o => Some(seq_index_bound(o)?),
    };
    Ok((start, stop, step))
}

fn mm_getitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    state_cell(&inst)?;
    let key = args.get(1).cloned().unwrap_or(Object::None);
    if let Object::Slice(sl) = &key {
        let (start, stop, step) = unpack_slice(sl)?;
        let cell = state_cell(&inst)?;
        let st = cell.borrow();
        let region = st.region.clone();
        let buf = region.as_slice();
        let adj = adjust_slice(buf.len() as i64, start, stop, step);
        if adj.len <= 0 {
            return Ok(Object::Bytes(Rc::from(Vec::new().into_boxed_slice())));
        }
        if adj.step == 1 {
            let s = adj.start as usize;
            return Ok(Object::Bytes(Rc::from(
                buf[s..s + adj.len as usize].to_vec().into_boxed_slice(),
            )));
        }
        let mut out = Vec::with_capacity(adj.len as usize);
        let mut cur = adj.start;
        for _ in 0..adj.len {
            out.push(buf[cur as usize]);
            // Saturating: with `step = sys.maxsize` the last advance
            // overflows i64 but is never read again.
            cur = cur.saturating_add(adj.step);
        }
        return Ok(Object::Bytes(Rc::from(out.into_boxed_slice())));
    }
    match try_coerce_index_i64(&key) {
        None => Err(type_error("mmap indices must be integers")),
        Some(r) => {
            let mut i = r?;
            let cell = state_cell(&inst)?;
            let st = cell.borrow();
            let region = st.region.clone();
            let buf = region.as_slice();
            if i < 0 {
                i += buf.len() as i64;
            }
            if i < 0 || i >= buf.len() as i64 {
                return Err(index_error("mmap index out of range"));
            }
            Ok(Object::Int(i64::from(buf[i as usize])))
        }
    }
}

fn mm_setitem(args: &[Object]) -> Result<Object, RuntimeError> {
    let inst = self_arg(args)?;
    {
        let cell = state_cell(&inst)?;
        writable_or_err(cell.borrow().access)?;
    }
    let key = args.get(1).cloned().unwrap_or(Object::None);
    let value = args.get(2).cloned().unwrap_or(Object::None);
    if let Object::Slice(sl) = &key {
        let (start, stop, step) = unpack_slice(sl)?;
        let data = match &value {
            Object::Bytes(b) => b.to_vec(),
            Object::ByteArray(b) => b.borrow().clone(),
            Object::MemoryView(mv) => mv.to_bytes(),
            other => {
                return Err(type_error(format!(
                    "a bytes-like object is required, not '{}'",
                    other.type_name_owned()
                )))
            }
        };
        let cell = state_cell(&inst)?;
        let st = cell.borrow();
        let region = st.region.clone();
        let adj = adjust_slice(region.byte_len() as i64, start, stop, step);
        if data.len() as i64 != adj.len {
            return Err(index_error("mmap slice assignment is wrong size"));
        }
        if adj.len == 0 {
            return Ok(Object::None);
        }
        let buf = region.as_mut_slice();
        if adj.step == 1 {
            let s = adj.start as usize;
            buf[s..s + data.len()].copy_from_slice(&data);
        } else {
            let mut cur = adj.start;
            for &b in &data {
                buf[cur as usize] = b;
                cur = cur.saturating_add(adj.step);
            }
        }
        return Ok(Object::None);
    }
    match try_coerce_index_i64(&key) {
        None => Err(type_error("mmap indices must be integer")),
        Some(r) => {
            let mut i = r?;
            let cell = state_cell(&inst)?;
            let st = cell.borrow();
            let region = st.region.clone();
            let size = region.byte_len() as i64;
            if i < 0 {
                i += size;
            }
            if i < 0 || i >= size {
                return Err(index_error("mmap index out of range"));
            }
            let v = match try_coerce_index_i64(&value) {
                None => return Err(type_error("mmap item value must be an int")),
                Some(r) => r?,
            };
            if !(0..=255).contains(&v) {
                return Err(value_error("mmap item value must be in range(0, 256)"));
            }
            region.as_mut_slice()[i as usize] = v as u8;
            Ok(Object::None)
        }
    }
}
