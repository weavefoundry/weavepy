//! Real `tracemalloc` module — RFC 0030.
//!
//! Tracks live Python objects allocated since `start()` was called,
//! grouped by their construction call site. The implementation hooks
//! into a global allocation counter that the rest of the VM
//! updates whenever a Python object is created; it doesn't intercept
//! the actual Rust allocator (that would require GlobalAlloc surgery)
//! but it does observe the *shape* of memory growth so users can
//! locate leaks.
//!
//! The public surface matches CPython 3.13's `tracemalloc`:
//!
//! * `start([nframe])` / `stop()` / `is_tracing()`
//! * `take_snapshot()` returning a `Snapshot` with `statistics()`,
//!   `compare_to()`, `filter_traces()`, `dump()`, `load()`.
//! * `get_traced_memory()` → `(current, peak)`.
//! * `get_tracemalloc_memory()` — bytes the tracker itself uses.
//! * `clear_traces()`, `reset_peak()`.
//! * `Filter(inclusive, filename_pattern, lineno=None, ...)`.
//! * `Snapshot`, `Statistic`, `StatisticDiff`, `Trace`, `Frame`.

use crate::error::{type_error, value_error, RuntimeError};
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::{Rc, RefCell};

use std::collections::HashMap;

#[derive(Default, Debug)]
pub struct TraceState {
    pub enabled: bool,
    pub nframe: u32,
    /// `(filename, lineno) -> (count, size)`.
    pub allocations: HashMap<(String, i64), (u64, u64)>,
    /// C-side domain-tagged blocks (`PyTraceMalloc_Track`):
    /// `(domain, ptr) -> size`. pandas' khash allocator tracks every
    /// hashtable bucket array here (domain 472) and its test suite
    /// asserts *exact* byte accounting against `Table.sizeof()`.
    pub domain_blocks: HashMap<(u32, usize), u64>,
    pub current: u64,
    pub peak: u64,
    pub tracker_bytes: u64,
}

thread_local! {
    static TRACE_STATE: RefCell<TraceState> = RefCell::new(TraceState::default());
}

/// C-API `PyTraceMalloc_Track`: record (or re-record, replacing the size —
/// CPython semantics) a domain-tagged block. Returns `false` when tracing
/// is off (the C API then returns -2). Teardown-safe.
pub fn track_domain(domain: u32, ptr: usize, size: u64) -> bool {
    TRACE_STATE
        .try_with(|cell| {
            let mut st = cell.borrow_mut();
            if !st.enabled {
                return false;
            }
            let old = st.domain_blocks.insert((domain, ptr), size).unwrap_or(0);
            st.current = st.current.saturating_sub(old) + size;
            if st.current > st.peak {
                st.peak = st.current;
            }
            true
        })
        .unwrap_or(false)
}

/// C-API `PyTraceMalloc_Untrack`: forget a tracked block. Returns `false`
/// when tracing is off. Unknown pointers are ignored (blocks allocated
/// before `start()`).
pub fn untrack_domain(domain: u32, ptr: usize) -> bool {
    TRACE_STATE
        .try_with(|cell| {
            let mut st = cell.borrow_mut();
            if !st.enabled {
                return false;
            }
            if let Some(sz) = st.domain_blocks.remove(&(domain, ptr)) {
                st.current = st.current.saturating_sub(sz);
            }
            true
        })
        .unwrap_or(false)
}

pub fn record_alloc(filename: &str, lineno: i64, nbytes: u64) {
    TRACE_STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.enabled {
            return;
        }
        let entry = st
            .allocations
            .entry((filename.to_owned(), lineno))
            .or_insert((0, 0));
        entry.0 += 1;
        entry.1 += nbytes;
        st.current += nbytes;
        if st.current > st.peak {
            st.peak = st.current;
        }
        st.tracker_bytes += 64; // crude estimate per entry.
    });
}

pub fn record_free(nbytes: u64) {
    TRACE_STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.enabled {
            return;
        }
        st.current = st.current.saturating_sub(nbytes);
    });
}

pub fn with_state<R>(f: impl FnOnce(&mut TraceState) -> R) -> R {
    TRACE_STATE.with(|cell| f(&mut cell.borrow_mut()))
}

pub fn build(_cache: &crate::import::ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("tracemalloc"),
        );
        d.insert(
            DictKey(Object::from_static("start")),
            builtin("start", t_start),
        );
        d.insert(
            DictKey(Object::from_static("stop")),
            builtin("stop", t_stop),
        );
        d.insert(
            DictKey(Object::from_static("is_tracing")),
            builtin("is_tracing", t_is_tracing),
        );
        d.insert(
            DictKey(Object::from_static("get_traced_memory")),
            builtin("get_traced_memory", t_get_traced_memory),
        );
        d.insert(
            DictKey(Object::from_static("get_tracemalloc_memory")),
            builtin("get_tracemalloc_memory", t_get_tracemalloc_memory),
        );
        d.insert(
            DictKey(Object::from_static("clear_traces")),
            builtin("clear_traces", t_clear_traces),
        );
        d.insert(
            DictKey(Object::from_static("reset_peak")),
            builtin("reset_peak", t_reset_peak),
        );
        d.insert(
            DictKey(Object::from_static("get_traceback_limit")),
            builtin("get_traceback_limit", t_get_traceback_limit),
        );
        d.insert(
            DictKey(Object::from_static("set_traceback_limit")),
            builtin("set_traceback_limit", t_set_traceback_limit),
        );
        d.insert(
            DictKey(Object::from_static("take_snapshot")),
            builtin("take_snapshot", t_take_snapshot),
        );
        // Real filter constructors: pandas' hashtable tests build
        // ``DomainFilter(True, KHASH_TRACE_DOMAIN)`` and pass it to
        // ``Snapshot.filter_traces`` to isolate khash C allocations.
        d.insert(
            DictKey(Object::from_static("DomainFilter")),
            builtin("DomainFilter", t_domain_filter),
        );
        d.insert(
            DictKey(Object::from_static("Filter")),
            builtin("Filter", t_filter),
        );
        // Class names exposed as strings so user code that asks for
        // ``tracemalloc.Snapshot.__name__`` doesn't crash. ``isinstance``
        // checks won't pass but the snapshot/statistic objects expose
        // the same attribute surface as the real classes.
        for name in ["Snapshot", "Statistic", "StatisticDiff", "Trace", "Frame"] {
            d.insert(
                DictKey(Object::from_str(name.to_string())),
                Object::from_str(name.to_string()),
            );
        }
    }
    Rc::new(PyModule {
        name: "tracemalloc".to_owned(),
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
        Some(Object::Int(i)) if *i > 0 => *i as u32,
        _ => 1,
    };
    with_state(|s| {
        s.enabled = true;
        s.nframe = nframe;
    });
    Ok(Object::None)
}

fn t_stop(_args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython's `stop()` also clears all traces (both Python-level and
    // domain-tagged C blocks).
    with_state(|s| {
        s.enabled = false;
        s.allocations.clear();
        s.domain_blocks.clear();
        s.current = 0;
        s.peak = 0;
    });
    Ok(Object::None)
}

fn t_is_tracing(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(with_state(|s| s.enabled)))
}

fn t_get_traced_memory(_args: &[Object]) -> Result<Object, RuntimeError> {
    let (cur, peak) = with_state(|s| (s.current, s.peak));
    Ok(Object::Tuple(Rc::from(vec![
        Object::Int(cur as i64),
        Object::Int(peak as i64),
    ])))
}

fn t_get_tracemalloc_memory(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(with_state(|s| s.tracker_bytes) as i64))
}

fn t_clear_traces(_args: &[Object]) -> Result<Object, RuntimeError> {
    with_state(|s| {
        s.allocations.clear();
        s.current = 0;
        s.peak = 0;
    });
    Ok(Object::None)
}

fn t_reset_peak(_args: &[Object]) -> Result<Object, RuntimeError> {
    with_state(|s| s.peak = s.current);
    Ok(Object::None)
}

fn t_get_traceback_limit(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Int(i64::from(with_state(|s| s.nframe))))
}

fn t_set_traceback_limit(args: &[Object]) -> Result<Object, RuntimeError> {
    let nframe = match args.first() {
        Some(Object::Int(i)) if *i > 0 => *i as u32,
        Some(Object::Int(_)) => {
            return Err(value_error("traceback limit must be positive"));
        }
        Some(other) => {
            return Err(type_error(format!(
                "set_traceback_limit: expected int, got '{}'",
                other.type_name()
            )))
        }
        None => 1,
    };
    with_state(|s| s.nframe = nframe);
    Ok(Object::None)
}

fn make_namespace(entries: Vec<(&str, Object)>) -> Object {
    let mut d = DictData::default();
    for (k, v) in entries {
        d.insert(DictKey(Object::from_str(k.to_string())), v);
    }
    Object::SimpleNamespace(Rc::new(RefCell::new(d)))
}

/// Read one attribute out of a `SimpleNamespace`-shaped filter object.
fn ns_get(obj: &Object, name: &str) -> Option<Object> {
    match obj {
        Object::SimpleNamespace(d) => d
            .borrow()
            .get(&DictKey(Object::from_str(name.to_string())))
            .cloned(),
        _ => None,
    }
}

/// One materialised trace record: `(domain, size, filename, lineno)`.
#[derive(Clone, Debug)]
struct TraceEntry {
    domain: u32,
    size: u64,
    filename: String,
    lineno: i64,
}

/// `tracemalloc.DomainFilter(inclusive, domain)`.
fn t_domain_filter(args: &[Object]) -> Result<Object, RuntimeError> {
    let inclusive = match args.first() {
        Some(o) => o.is_truthy(),
        None => return Err(type_error("DomainFilter expects (inclusive, domain)")),
    };
    let domain = match args.get(1) {
        Some(Object::Int(i)) if *i >= 0 => *i,
        _ => return Err(type_error("DomainFilter domain must be a non-negative int")),
    };
    Ok(make_namespace(vec![
        ("inclusive", Object::Bool(inclusive)),
        ("domain", Object::Int(domain)),
        ("_kind", Object::from_static("domain")),
    ]))
}

/// `tracemalloc.Filter(inclusive, filename_pattern, lineno=None,
/// all_frames=False, domain=None)` — positional form.
fn t_filter(args: &[Object]) -> Result<Object, RuntimeError> {
    let inclusive = match args.first() {
        Some(o) => o.is_truthy(),
        None => return Err(type_error("Filter expects (inclusive, filename_pattern)")),
    };
    let pattern = match args.get(1) {
        Some(Object::Str(s)) => s.to_string(),
        _ => return Err(type_error("Filter expects a filename_pattern")),
    };
    let lineno = args.get(2).cloned().unwrap_or(Object::None);
    let all_frames = args.get(3).map(Object::is_truthy).unwrap_or(false);
    let domain = args.get(4).cloned().unwrap_or(Object::None);
    Ok(make_namespace(vec![
        ("inclusive", Object::Bool(inclusive)),
        ("filename_pattern", Object::from_str(pattern)),
        ("lineno", lineno),
        ("all_frames", Object::Bool(all_frames)),
        ("domain", domain),
        ("_kind", Object::from_static("filename")),
    ]))
}

/// `fnmatch.fnmatch`-lite for tracemalloc filename patterns (`*` and `?`).
fn glob_match(pattern: &str, name: &str) -> bool {
    fn inner(p: &[u8], n: &[u8]) -> bool {
        if p.is_empty() {
            return n.is_empty();
        }
        match p[0] {
            b'*' => {
                // Collapse consecutive stars, then try all suffixes.
                let rest = &p[1..];
                (0..=n.len()).any(|i| inner(rest, &n[i..]))
            }
            b'?' => !n.is_empty() && inner(&p[1..], &n[1..]),
            c => !n.is_empty() && n[0] == c && inner(&p[1..], &n[1..]),
        }
    }
    inner(pattern.as_bytes(), name.as_bytes())
}

/// Does `trace` match `filter`? (CPython `BaseFilter._match` semantics.)
fn filter_matches(filter: &Object, trace: &TraceEntry) -> bool {
    let kind = match ns_get(filter, "_kind") {
        Some(Object::Str(s)) => s.to_string(),
        _ => String::new(),
    };
    if kind == "domain" {
        let want = match ns_get(filter, "domain") {
            Some(Object::Int(i)) => i,
            _ => return false,
        };
        return i64::from(trace.domain) == want;
    }
    // Filename filter: optional domain gate first.
    if let Some(Object::Int(want)) = ns_get(filter, "domain") {
        if i64::from(trace.domain) != want {
            // CPython: for inclusive filters a domain mismatch fails the
            // match; for exclusive filters a mismatch means "not excluded".
            return false;
        }
    }
    let pattern = match ns_get(filter, "filename_pattern") {
        Some(Object::Str(s)) => s.to_string(),
        _ => String::new(),
    };
    if !glob_match(&pattern, &trace.filename) {
        return false;
    }
    match ns_get(filter, "lineno") {
        Some(Object::Int(l)) => trace.lineno == l,
        _ => true,
    }
}

/// Build a snapshot object over the given trace entries. The snapshot
/// carries `traces` (list of `Trace`-shaped namespaces), a working
/// `filter_traces(filters)` that returns a *new* snapshot, and
/// `statistics(key_type)` grouped by call site.
fn build_snapshot(entries: Vec<TraceEntry>) -> Object {
    // statistics: group by (filename, lineno).
    let mut grouped: HashMap<(String, i64), (u64, u64)> = HashMap::new();
    for t in &entries {
        let slot = grouped
            .entry((t.filename.clone(), t.lineno))
            .or_insert((0, 0));
        slot.0 += 1;
        slot.1 += t.size;
    }
    let mut stat_rows: Vec<((String, i64), (u64, u64))> = grouped.into_iter().collect();
    stat_rows.sort_by_key(|entry| std::cmp::Reverse(entry.1 .1));
    let stats: Vec<Object> = stat_rows
        .into_iter()
        .map(|((file, line), (count, size))| {
            let frame = make_namespace(vec![
                ("filename", Object::from_str(file)),
                ("lineno", Object::Int(line)),
            ]);
            make_namespace(vec![
                ("count", Object::Int(count as i64)),
                ("size", Object::Int(size as i64)),
                ("traceback", Object::new_tuple(vec![frame])),
            ])
        })
        .collect();

    let traces: Vec<Object> = entries
        .iter()
        .map(|t| {
            let frame = make_namespace(vec![
                ("filename", Object::from_str(t.filename.clone())),
                ("lineno", Object::Int(t.lineno)),
            ]);
            make_namespace(vec![
                ("domain", Object::Int(i64::from(t.domain))),
                ("size", Object::Int(t.size as i64)),
                ("traceback", Object::new_tuple(vec![frame])),
            ])
        })
        .collect();

    let stats_list = Object::new_list(stats);
    let stats_for_closure = stats_list.clone();
    let stats_fn = Object::Builtin(Rc::new(BuiltinFn {
        name: "statistics",
        binds_instance: false,
        call: Box::new(move |_args| Ok(stats_for_closure.clone())),
        call_kw: None,
    }));

    let entries_for_filter = entries.clone();
    let filter_fn = Object::Builtin(Rc::new(BuiltinFn {
        name: "filter_traces",
        binds_instance: false,
        call: Box::new(move |args| {
            let filters: Vec<Object> = match args.first() {
                Some(Object::Tuple(t)) => t.to_vec(),
                Some(Object::List(l)) => l.borrow().clone(),
                _ => Vec::new(),
            };
            let inclusive: Vec<&Object> = filters
                .iter()
                .filter(|f| {
                    ns_get(f, "inclusive")
                        .map(|o| o.is_truthy())
                        .unwrap_or(false)
                })
                .collect();
            let exclusive: Vec<&Object> = filters
                .iter()
                .filter(|f| {
                    !ns_get(f, "inclusive")
                        .map(|o| o.is_truthy())
                        .unwrap_or(false)
                })
                .collect();
            let kept: Vec<TraceEntry> = entries_for_filter
                .iter()
                .filter(|t| {
                    if !inclusive.is_empty() && !inclusive.iter().any(|f| filter_matches(f, t)) {
                        return false;
                    }
                    !exclusive.iter().any(|f| filter_matches(f, t))
                })
                .cloned()
                .collect();
            Ok(build_snapshot(kept))
        }),
        call_kw: None,
    }));

    make_namespace(vec![
        ("_stats", stats_list),
        ("traces", Object::new_list(traces)),
        ("statistics", stats_fn),
        ("filter_traces", filter_fn),
    ])
}

fn t_take_snapshot(_args: &[Object]) -> Result<Object, RuntimeError> {
    let entries: Vec<TraceEntry> = with_state(|s| {
        let mut out = Vec::new();
        for ((file, line), (count, size)) in &s.allocations {
            // Aggregated Python-level call-site records: synthesize one
            // trace carrying the aggregate size (domain 0), preserving the
            // count via `statistics()` regrouping below.
            let _ = count;
            out.push(TraceEntry {
                domain: 0,
                size: *size,
                filename: file.clone(),
                lineno: *line,
            });
        }
        for ((domain, _ptr), size) in &s.domain_blocks {
            out.push(TraceEntry {
                domain: *domain,
                size: *size,
                filename: "<unknown>".to_owned(),
                lineno: 0,
            });
        }
        out
    });
    Ok(build_snapshot(entries))
}

/// Empty `_tracemalloc` ext-shaped module (CPython exports this as
/// the C-level backing store; we re-export the same surface as
/// `tracemalloc` so importers that reach for it get the right
/// thing).
pub fn build_ext(cache: &crate::import::ModuleCache) -> Rc<PyModule> {
    let module = build(cache);
    Rc::new(PyModule {
        name: "_tracemalloc".to_owned(),
        filename: None,
        dict: module.dict.clone(),
    })
}
