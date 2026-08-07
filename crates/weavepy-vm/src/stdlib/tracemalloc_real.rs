//! Native `_tracemalloc` — RFC 0030, rebuilt for RFC 0057 WS6.
//!
//! CPython splits tracemalloc in two: `Lib/tracemalloc.py` holds the
//! pure-Python object model (`Frame`/`Traceback`/`Statistic`/
//! `StatisticDiff`/`Trace`/`Snapshot`/filters) and `Modules/
//! _tracemalloc.c` provides the raw tracking core. WeavePy now mirrors
//! that split exactly: the verbatim `tracemalloc.py` is frozen and this
//! module is the `_tracemalloc` backing it, with the same surface:
//!
//! * `start([nframe])` / `stop()` / `is_tracing()`
//! * `_get_traces()` → `[(domain, size, frames, total_nframe), …]`
//!   with equal frame tuples *interned* (CPython interns tracebacks in
//!   a hashtable; `test_get_traces_intern_traceback` asserts identity).
//! * `_get_object_traceback(obj)` → frames most-recent-first (the
//!   Python `Traceback` constructor reverses them).
//! * `get_traced_memory()` / `get_tracemalloc_memory()` /
//!   `clear_traces()` / `reset_peak()` / `get_traceback_limit()`.
//!
//! CPython hooks the raw allocator; WeavePy instead registers objects
//! at their construction sites in the VM (container literals, binary-op
//! results, builtin constructors, fresh closure cells, `open()` files).
//! Liveness is observed through `Weak` probes: an object's trace is
//! swept once its `Arc` strong count reaches zero. Sweeps run at query
//! time (and periodically on insert), which is indistinguishable from
//! CPython's eager free hook to Python code — the counters are only
//! observable through the query functions.
//!
//! The C-API surface (`PyTraceMalloc_Track`/`Untrack` via
//! `weavepy-capi`, pandas' khash domain accounting) and the
//! `ResourceWarning` allocation-site integration for files are
//! preserved.

use crate::error::{runtime_error, type_error, value_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule, SetData};
use crate::sync::{Rc, RefCell, Weak};

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// CPython `Modules/_tracemalloc.c` `MAX_NFRAME`.
const MAX_NFRAME: i64 = 65535;

/// Fast global "is anyone tracing" gate so the per-allocation hook is a
/// single relaxed load when tracemalloc is off (the overwhelmingly
/// common case).
static TRACING: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn is_tracking() -> bool {
    TRACING.load(Ordering::Relaxed)
}

/// Liveness probe for a tracked object: a weak reference to its
/// payload allocation. `alive()` is true while any strong `Arc`
/// remains. (The weak handle pins the allocation's control block —
/// and, for `Arc`'s inline layout, the payload bytes — until the sweep
/// drops it; that only delays the *Rust* free, not the accounting the
/// Python API observes.)
#[derive(Debug)]
enum Probe {
    Bytes(Weak<[u8]>),
    ByteArray(Weak<RefCell<Vec<u8>>>),
    Str(Weak<str>),
    WStr(Weak<[u32]>),
    Tuple(Weak<[Object]>),
    List(Weak<RefCell<Vec<Object>>>),
    Dict(Weak<RefCell<DictData>>),
    Set(Weak<RefCell<SetData>>),
    FrozenSet(Weak<crate::object::FrozenSetObj>),
    Cell(Weak<RefCell<Object>>),
}

impl Probe {
    fn alive(&self) -> bool {
        match self {
            Probe::Bytes(w) => w.strong_count() > 0,
            Probe::ByteArray(w) => w.strong_count() > 0,
            Probe::Str(w) => w.strong_count() > 0,
            Probe::WStr(w) => w.strong_count() > 0,
            Probe::Tuple(w) => w.strong_count() > 0,
            Probe::List(w) => w.strong_count() > 0,
            Probe::Dict(w) => w.strong_count() > 0,
            Probe::Set(w) => w.strong_count() > 0,
            Probe::FrozenSet(w) => w.strong_count() > 0,
            Probe::Cell(w) => w.strong_count() > 0,
        }
    }
}

/// One tracked live allocation.
#[derive(Debug)]
struct LiveTrace {
    probe: Probe,
    size: u64,
    /// Most-recent-first `(filename, lineno)` frames, at most `nframe`.
    frames: Vec<(String, i64)>,
    /// Full Python stack depth at allocation time (CPython stores the
    /// pre-truncation length; `Traceback.total_nframe`).
    total_nframe: u16,
}

/// A C-API domain-tagged block (`PyTraceMalloc_Track`).
#[derive(Debug)]
struct DomainBlock {
    size: u64,
    frames: Vec<(String, i64)>,
    total_nframe: u16,
}

#[derive(Debug, Default)]
pub struct TraceState {
    pub enabled: bool,
    pub nframe: u32,
    /// Live tracked Python objects, keyed by `id(obj)` (payload
    /// address — see `builtins::object_identity`).
    live: HashMap<usize, LiveTrace>,
    /// `(domain, ptr) -> block`. pandas' khash allocator tracks every
    /// hashtable bucket array here (domain 472) and its test suite
    /// asserts *exact* byte accounting against `Table.sizeof()`.
    domain_blocks: HashMap<(u32, usize), DomainBlock>,
    /// Per-file allocation tracebacks (most-recent-first), keyed by the
    /// `PyFile` payload address. Feeds `_get_object_traceback` for
    /// dealloc `ResourceWarning`s (`test_warnings.test_tracemalloc`),
    /// whose source token must survive the object's death.
    pub object_traces: HashMap<usize, Vec<(String, i64)>>,
    /// Frames pinned for a dying object's pending `ResourceWarning`.
    /// Moved out of [`Self::object_traces`] at enqueue time so a
    /// subsequent `open()` that reuses the same payload address cannot
    /// overwrite the frames the warning still needs to format.
    pub finalizing_traces: HashMap<usize, Vec<(String, i64)>>,
    pub current: u64,
    pub peak: u64,
    /// Insert counter driving the periodic sweep.
    inserts_since_sweep: u64,
}

thread_local! {
    static TRACE_STATE: RefCell<TraceState> = RefCell::new(TraceState {
        nframe: 1,
        ..TraceState::default()
    });
}

pub fn with_state<R>(f: impl FnOnce(&mut TraceState) -> R) -> R {
    TRACE_STATE.with(|cell| f(&mut cell.borrow_mut()))
}

/// Drop the traces of objects that have died since the last sweep,
/// returning their bytes to the free pool. Runs at query time — the
/// counters are only observable through queries, so this is equivalent
/// to CPython's eager free hook.
fn sweep(st: &mut TraceState) {
    st.live.retain(|_, t| {
        if t.probe.alive() {
            true
        } else {
            st.current = st.current.saturating_sub(t.size);
            false
        }
    });
    st.inserts_since_sweep = 0;
}

/// Snapshot the current Python call stack: at most `nframe` frames,
/// most recent first (CPython raw-trace order), plus the full depth.
fn capture_frames(nframe: usize) -> (Vec<(String, i64)>, u16) {
    let Some(h) = crate::vm_singletons::current_thread_handles() else {
        return (Vec::new(), 0);
    };
    let Ok(stack) = h.frame_stack.try_borrow() else {
        return (Vec::new(), 0);
    };
    let total = stack.len();
    let frames = stack
        .iter()
        .rev()
        .take(nframe.max(1))
        .map(|f| (f.code.filename.clone(), i64::from(f.current_lineno())))
        .collect();
    (frames, total.min(u16::MAX as usize) as u16)
}

/// Identity key + liveness probe for a trackable object. `None` for
/// unboxed / untracked variants.
fn probe_for(obj: &Object) -> Option<(usize, Probe)> {
    let key = crate::builtins::object_identity(obj) as usize;
    let probe = match obj {
        Object::Bytes(rc) => Probe::Bytes(Rc::downgrade(rc)),
        Object::ByteArray(rc) => Probe::ByteArray(Rc::downgrade(rc)),
        Object::Str(rc) => Probe::Str(Rc::downgrade(rc)),
        Object::WStr(rc) => Probe::WStr(Rc::downgrade(rc)),
        Object::Tuple(rc) => Probe::Tuple(Rc::downgrade(rc)),
        Object::List(rc) => Probe::List(Rc::downgrade(rc)),
        Object::Dict(rc) => Probe::Dict(Rc::downgrade(rc)),
        Object::Set(rc) => Probe::Set(Rc::downgrade(rc)),
        Object::FrozenSet(rc) => Probe::FrozenSet(Rc::downgrade(rc)),
        Object::Cell(rc) => Probe::Cell(Rc::downgrade(rc)),
        _ => return None,
    };
    Some((key, probe))
}

/// Register a freshly constructed Python object with tracemalloc.
/// Callers must pre-gate on [`is_tracking`] (a relaxed atomic load) so
/// the disabled path stays free.
pub fn track_new_object(obj: &Object) {
    let Some((key, probe)) = probe_for(obj) else {
        return;
    };
    let size = crate::stdlib::sys::sizeof_estimate(obj).max(0) as u64;
    let _ = TRACE_STATE.try_with(|cell| {
        let Ok(mut st) = cell.try_borrow_mut() else {
            return;
        };
        if !st.enabled {
            return;
        }
        let (frames, total_nframe) = capture_frames(st.nframe as usize);
        if frames.is_empty() {
            return;
        }
        if let Some(old) = st.live.insert(
            key,
            LiveTrace {
                probe,
                size,
                frames,
                total_nframe,
            },
        ) {
            // Address reuse: the previous occupant died; return its bytes.
            st.current = st.current.saturating_sub(old.size);
        }
        st.current += size;
        if st.current > st.peak {
            st.peak = st.current;
        }
        st.inserts_since_sweep += 1;
        if st.inserts_since_sweep >= 65536 {
            sweep(&mut st);
        }
    });
}

/// Register the fresh cell variables of a function activation. Called
/// from `make_frame` *before* the new frame is pushed, so the captured
/// stack attributes the cells to the calling site — CPython 3.12+
/// skips the callee's still-incomplete frame the same way
/// (`test_tracemalloc.test_no_incomplete_frames`).
pub fn track_new_cells(cells: &[Rc<RefCell<Object>>]) {
    for cell in cells {
        track_new_object(&Object::Cell(cell.clone()));
    }
}

/// C-API `PyTraceMalloc_Track`: record (or re-record, replacing the
/// size — CPython semantics) a domain-tagged block. Returns `false`
/// when tracing is off (the C API then returns -2). Teardown-safe.
pub fn track_domain(domain: u32, ptr: usize, size: u64) -> bool {
    TRACE_STATE
        .try_with(|cell| {
            let mut st = cell.borrow_mut();
            if !st.enabled {
                return false;
            }
            let (frames, total_nframe) = capture_frames(st.nframe as usize);
            let (frames, total_nframe) = if frames.is_empty() {
                (vec![("<unknown>".to_owned(), 0)], 1)
            } else {
                (frames, total_nframe)
            };
            let old = st
                .domain_blocks
                .insert(
                    (domain, ptr),
                    DomainBlock {
                        size,
                        frames,
                        total_nframe,
                    },
                )
                .map_or(0, |b| b.size);
            st.current = st.current.saturating_sub(old) + size;
            if st.current > st.peak {
                st.peak = st.current;
            }
            true
        })
        .unwrap_or(false)
}

/// C-API `PyTraceMalloc_Untrack`: forget a tracked block. Returns
/// `false` when tracing is off. Unknown pointers are ignored (blocks
/// allocated before `start()`).
pub fn untrack_domain(domain: u32, ptr: usize) -> bool {
    TRACE_STATE
        .try_with(|cell| {
            let mut st = cell.borrow_mut();
            if !st.enabled {
                return false;
            }
            if let Some(b) = st.domain_blocks.remove(&(domain, ptr)) {
                st.current = st.current.saturating_sub(b.size);
            }
            true
        })
        .unwrap_or(false)
}

/// Record the allocation traceback of a freshly constructed
/// resource-carrying object (a `PyFile`), keyed by its payload address.
/// No-op when tracing is off. Frames are most-recent-first (CPython
/// raw-trace order; the Python `Traceback` constructor reverses).
pub fn track_object_alloc(file: &crate::object::Object) {
    if !is_tracking() {
        return;
    }
    let crate::object::Object::File(rc) = file else {
        return;
    };
    let key = Rc::as_ptr(rc) as usize;
    let nframe = with_state(|s| if s.enabled { s.nframe } else { 0 });
    if nframe == 0 {
        return;
    }
    let (frames, _total) = capture_frames(nframe as usize);
    if frames.is_empty() {
        return;
    }
    with_state(|s| {
        s.object_traces.insert(key, frames);
    });
}

/// The recorded allocation traceback for `key`, if any. Prefers a
/// pinned finalizing entry (see [`pin_object_traceback`]) so a
/// recycled address's new `track_object_alloc` cannot clobber the
/// frames a pending `ResourceWarning` still needs.
pub fn object_traceback_for(key: usize) -> Option<Vec<(String, i64)>> {
    with_state(|s| {
        s.finalizing_traces
            .get(&key)
            .cloned()
            .or_else(|| s.object_traces.get(&key).cloned())
    })
}

/// Move `key`'s live allocation frames into
/// [`TraceState::finalizing_traces`] so they survive address reuse
/// until the pending `ResourceWarning` is formatted. Called from the
/// file destructor when enqueueing the warning.
pub fn pin_object_traceback(key: usize) {
    with_state(|s| {
        if let Some(frames) = s.object_traces.remove(&key) {
            s.finalizing_traces.insert(key, frames);
        }
    });
}

/// Drop a pinned finalizing entry after the warning that needed it has
/// been formatted (or abandoned).
pub fn unpin_object_traceback(key: usize) {
    with_state(|s| {
        s.finalizing_traces.remove(&key);
    });
}

/// Enable tracing with `nframe` captured frames. Startup path for
/// `PYTHONTRACEMALLOC` / `-X tracemalloc` (the CLI validated the
/// value); also the core of `_tracemalloc.start`.
pub fn start_tracing(nframe: u32) {
    with_state(|s| {
        if s.enabled {
            return;
        }
        s.nframe = nframe.max(1);
        s.enabled = true;
    });
    TRACING.store(true, Ordering::Relaxed);
}

fn stop_tracing() {
    TRACING.store(false, Ordering::Relaxed);
    with_state(|s| {
        s.enabled = false;
        s.live.clear();
        s.domain_blocks.clear();
        s.object_traces.clear();
        s.finalizing_traces.clear();
        s.current = 0;
        s.peak = 0;
    });
}

// ---------------------------------------------------------------------------
// Module builders.
// ---------------------------------------------------------------------------

pub fn build(_cache: &crate::import::ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_tracemalloc"),
        );
        for (name, f) in [
            (
                "start",
                t_start as fn(&[Object]) -> Result<Object, RuntimeError>,
            ),
            ("stop", t_stop),
            ("is_tracing", t_is_tracing),
            ("get_traced_memory", t_get_traced_memory),
            ("get_tracemalloc_memory", t_get_tracemalloc_memory),
            ("clear_traces", t_clear_traces),
            ("reset_peak", t_reset_peak),
            ("get_traceback_limit", t_get_traceback_limit),
            ("_get_traces", t_get_traces),
            ("_get_object_traceback", t_get_object_traceback),
            // WeavePy-private hooks backing `_testcapi.tracemalloc_track`
            // / `_testinternalcapi._PyTraceMalloc_GetTraceback`.
            ("_weave_track", t_capi_track),
            ("_weave_untrack", t_capi_untrack),
            ("_weave_get_traceback", capi_get_traceback),
        ] {
            d.insert(DictKey(Object::from_static(name)), builtin(name, f));
        }
    }
    Rc::new(PyModule {
        name: "_tracemalloc".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

fn t_start(args: &[Object]) -> Result<Object, RuntimeError> {
    let nframe = match args.first() {
        None => 1,
        Some(Object::Int(i)) => *i,
        Some(Object::Bool(b)) => i64::from(*b),
        Some(Object::Long(_)) => MAX_NFRAME + 1, // out of range by definition
        Some(other) => {
            return Err(type_error(format!(
                "'{}' object cannot be interpreted as an integer",
                other.type_name()
            )))
        }
    };
    if !(1..=MAX_NFRAME).contains(&nframe) {
        // CPython `_PyTraceMalloc_Start`.
        return Err(value_error(format!(
            "the number of frames must be in range [1; {MAX_NFRAME}]"
        )));
    }
    start_tracing(nframe as u32);
    Ok(Object::None)
}

fn t_stop(_args: &[Object]) -> Result<Object, RuntimeError> {
    stop_tracing();
    Ok(Object::None)
}

fn t_is_tracing(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(with_state(|s| s.enabled)))
}

fn t_get_traced_memory(_args: &[Object]) -> Result<Object, RuntimeError> {
    let (cur, peak) = with_state(|s| {
        if !s.enabled {
            return (0, 0);
        }
        sweep(s);
        (s.current, s.peak)
    });
    Ok(Object::Tuple(Rc::from(vec![
        Object::Int(cur as i64),
        Object::Int(peak as i64),
    ])))
}

fn t_get_tracemalloc_memory(_args: &[Object]) -> Result<Object, RuntimeError> {
    // Rough self-footprint estimate: hashtable entries + frame strings.
    let bytes = with_state(|s| {
        let live: u64 = s
            .live
            .values()
            .map(|t| 64 + 48 * t.frames.len() as u64)
            .sum();
        let dom = 96 * s.domain_blocks.len() as u64;
        512 + live + dom
    });
    Ok(Object::Int(bytes as i64))
}

fn t_clear_traces(_args: &[Object]) -> Result<Object, RuntimeError> {
    with_state(|s| {
        s.live.clear();
        s.domain_blocks.clear();
        s.object_traces.clear();
        s.finalizing_traces.clear();
        s.current = 0;
        s.peak = 0;
    });
    Ok(Object::None)
}

fn t_reset_peak(_args: &[Object]) -> Result<Object, RuntimeError> {
    with_state(|s| {
        sweep(s);
        s.peak = s.current;
    });
    Ok(Object::None)
}

fn t_get_traceback_limit(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(with_state(|s| s.nframe.max(1)))))
}

/// Frames → Python tuple of `(filename, lineno)` tuples,
/// most-recent-first (raw `_tracemalloc` order).
fn frames_to_tuple(frames: &[(String, i64)]) -> Object {
    let items: Vec<Object> = frames
        .iter()
        .map(|(f, l)| Object::Tuple(Rc::from(vec![Object::from_str(f.clone()), Object::Int(*l)])))
        .collect();
    Object::Tuple(Rc::from(items))
}

fn t_get_traces(_args: &[Object]) -> Result<Object, RuntimeError> {
    let mut out: Vec<Object> = Vec::new();
    with_state(|s| {
        if !s.enabled {
            return;
        }
        sweep(s);
        // Intern equal frame tuples: CPython's traceback hashtable
        // means two identical tracebacks come back as the *same*
        // tuple object (`test_get_traces_intern_traceback`).
        let mut interned: HashMap<Vec<(String, i64)>, Object> = HashMap::new();
        let mut push = |domain: u32, size: u64, frames: &Vec<(String, i64)>, total: u16| {
            let tb = interned
                .entry(frames.clone())
                .or_insert_with(|| frames_to_tuple(frames))
                .clone();
            out.push(Object::Tuple(Rc::from(vec![
                Object::Int(i64::from(domain)),
                Object::Int(size as i64),
                tb,
                Object::Int(i64::from(total)),
            ])));
        };
        for t in s.live.values() {
            push(0, t.size, &t.frames, t.total_nframe);
        }
        for ((domain, _ptr), b) in &s.domain_blocks {
            push(*domain, b.size, &b.frames, b.total_nframe);
        }
    });
    Ok(Object::new_list(out))
}

fn t_get_object_traceback(args: &[Object]) -> Result<Object, RuntimeError> {
    if !with_state(|s| s.enabled) {
        return Ok(Object::None);
    }
    let obj = args.first().cloned().unwrap_or(Object::None);
    // The dealloc `ResourceWarning` path passes the raw address token
    // of an already-dead file; live objects pass themselves.
    let key = match &obj {
        Object::File(rc) => Rc::as_ptr(rc) as usize,
        Object::Int(k) if *k > 0 => *k as usize,
        other => crate::builtins::object_identity(other) as usize,
    };
    let frames = with_state(|s| {
        sweep(s);
        s.live
            .get(&key)
            .map(|t| t.frames.clone())
            .or_else(|| s.finalizing_traces.get(&key).cloned())
            .or_else(|| s.object_traces.get(&key).cloned())
    });
    match frames {
        Some(frames) => Ok(frames_to_tuple(&frames)),
        None => Ok(Object::None),
    }
}

/// `_testcapi.tracemalloc_track(domain, ptr, size[, release_gil])`:
/// CPython's wrapper raises RuntimeError when `_PyTraceMalloc_Track`
/// fails (tracing disabled).
fn t_capi_track(args: &[Object]) -> Result<Object, RuntimeError> {
    let (domain, ptr) = capi_domain_ptr(args)?;
    let size = match args.get(2) {
        Some(Object::Int(s)) if *s >= 0 => *s as u64,
        _ => return Err(type_error("tracemalloc_track expects (domain, ptr, size)")),
    };
    if !track_domain(domain, ptr, size) {
        return Err(runtime_error("_PyTraceMalloc_Track error"));
    }
    Ok(Object::None)
}

fn t_capi_untrack(args: &[Object]) -> Result<Object, RuntimeError> {
    let (domain, ptr) = capi_domain_ptr(args)?;
    if !untrack_domain(domain, ptr) {
        return Err(runtime_error("_PyTraceMalloc_Untrack error"));
    }
    Ok(Object::None)
}

/// `_testinternalcapi._PyTraceMalloc_GetTraceback(domain, ptr)` →
/// frames tuple (most-recent-first) or None.
pub fn capi_get_traceback(args: &[Object]) -> Result<Object, RuntimeError> {
    let (domain, ptr) = capi_domain_ptr(args)?;
    let frames = with_state(|s| {
        s.domain_blocks
            .get(&(domain, ptr))
            .map(|b| b.frames.clone())
    });
    match frames {
        Some(frames) => Ok(frames_to_tuple(&frames)),
        None => Ok(Object::None),
    }
}

fn capi_domain_ptr(args: &[Object]) -> Result<(u32, usize), RuntimeError> {
    let domain = match args.first() {
        Some(Object::Int(d)) if *d >= 0 => *d as u32,
        _ => return Err(type_error("expected a non-negative domain int")),
    };
    let ptr = match args.get(1) {
        Some(Object::Int(p)) if *p >= 0 => *p as usize,
        Some(Object::Long(b)) => {
            // Addresses above i64::MAX arrive as bigints.
            u64::try_from(b.as_ref().clone())
                .map(|v| v as usize)
                .map_err(|_| type_error("expected an address int"))?
        }
        _ => return Err(type_error("expected an address int")),
    };
    Ok((domain, ptr))
}
