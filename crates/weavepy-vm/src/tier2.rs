//! RFC 0032 — the VM side of the tier-2 Cranelift JIT.
//!
//! This module is compiled only with the `jit` feature. It owns a
//! per-thread [`weavepy_jit::JitEngine`] and a hot-counter cache keyed by
//! `CodeObject` identity, decides when a frame is hot enough to compile,
//! applies the entry type-guard, marshals locals into a
//! [`weavepy_jit::JitFrame`], enters the native code, and reconstructs
//! interpreter state on a deopt side exit.
//!
//! Everything here runs under the GIL on a single thread, so the engine,
//! cache, and the raw function pointers they hand out never cross thread
//! boundaries — hence the thread-local state and the plain [`StdRc`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc as StdRc;

use weavepy_compiler::CodeObject;
use weavepy_jit::{
    AttrSiteMeta, CallStatus, CompiledFrame, JitEngine, JitFrame, JitStatus, JitType,
    ResolvedGlobal, SlotTag,
};

use crate::error::RuntimeError;
use crate::object::{DictData, DictKey, Object, PyFunction, PyIterator, StrKey};
use crate::sync::{Rc, RefCell as GilRefCell};
use crate::types::TypeObject;

/// What happened when the VM offered a frame to the JIT.
pub(crate) enum JitEntry {
    /// The native frame ran to completion; this is its return value.
    Ran(Object),
    /// The native frame deopted; `frame.pc` / locals / stack have been
    /// rewritten and the interpreter should resume.
    Deopt,
    /// RFC 0059 WS3 — a native Python-to-Python call raised and no
    /// handler exists in the JIT subset. `frame.pc` / locals / stack
    /// have been rewritten to the post-`CALL` state; the caller routes
    /// this through the normal exception machinery.
    Raised(RuntimeError),
    /// The frame was not entered (cold, not JITable, or guard failed);
    /// run the interpreter as usual.
    Skip,
}

/// One burned-in Python callee (RFC 0059 WS3): the function object the
/// `CallPy` token resolves to, plus its `__code__` at compile time
/// (functions are code-rebindable, so identity of the function alone
/// does not pin the burned-in arity/return-lane assumptions).
type CalleeTable = Vec<(Object, Rc<CodeObject>)>;

/// RFC 0065 WS5 — the runtime guard fingerprint of one burned-in
/// attribute site, snapshotted right after compilation with the same
/// eligibility predicate the tier-1 inline caches use. The access
/// helpers re-validate it per access and deopt on any mismatch.
struct AttrGuard {
    /// The attribute name (the indexed dict hit must still carry it —
    /// a `del` of an earlier attribute shift-renumbers later slots).
    name: String,
    /// The value lane the site was compiled with.
    lane: JitType,
    /// `rc_id` of the receiver's class at compile time.
    type_id: u64,
    /// The class's `attr_version` at compile time (bumps on any class
    /// or MRO mutation, exactly like the tier-1 caches).
    ver: u32,
    /// Index of `name` in the instance dict at compile time.
    key_idx: u32,
    /// Pins the class object so `type_id` (an address) can't be reused.
    _class: Rc<TypeObject>,
}

/// A compiled frame plus the globals it burned in: `snapshot[i]` is the
/// object `guards[i].name` resolved to at compile time. Every entry
/// re-resolves each name against the entering frame's namespaces and
/// requires identity (`is_same`) with the snapshot (RFC 0058 WS4).
struct CompiledEntry {
    cf: StdRc<CompiledFrame>,
    guard_snapshot: StdRc<Vec<(String, Object)>>,
    callees: StdRc<CalleeTable>,
    /// RFC 0065 WS5 — one guard per burned-in attribute site, in
    /// `site`-token order (parallel to `cf.attr_sites`).
    attr_guards: StdRc<Vec<AttrGuard>>,
}

/// Per-`CodeObject` compilation state.
enum Tier {
    Cold,
    NotJitable,
    Compiled(
        StdRc<CompiledFrame>,
        StdRc<Vec<(String, Object)>>,
        StdRc<CalleeTable>,
        StdRc<Vec<AttrGuard>>,
    ),
}

struct CacheEntry {
    counter: u32,
    tier: Tier,
    /// Failed OSR validations (RFC 0059 WS3b). Mid-loop entry re-checks
    /// guards + locals on every back edge while it keeps failing, so a
    /// chronically unenterable loop stops polling after a budget.
    osr_failures: u32,
    /// Keeps the code object alive so its address can't be reused while
    /// this entry (and any compiled pointer keyed by it) is live.
    _code: Rc<CodeObject>,
}

/// Give up on OSR for a code object after this many failed validations.
const OSR_FAILURE_BUDGET: u32 = 64;

/// JIT counters surfaced through `WEAVEPY_VM_STATS`.
#[derive(Default, Clone)]
pub(crate) struct JitStats {
    pub frames_seen: u64,
    pub frames_compiled: u64,
    pub frames_notjitable: u64,
    pub native_entries: u64,
    pub deopts: u64,
    pub entry_guard_failures: u64,
    /// Mid-loop (OSR) native entries, a subset of `native_entries`
    /// (RFC 0059 WS3b).
    pub osr_entries: u64,
}

struct JitState {
    enabled: bool,
    threshold: u32,
    engine: Option<JitEngine>,
    cache: HashMap<*const CodeObject, CacheEntry>,
    stats: JitStats,
}

impl JitState {
    fn new() -> JitState {
        let enabled = match std::env::var("WEAVEPY_JIT") {
            Ok(v) => v != "0" && !v.eq_ignore_ascii_case("off") && !v.is_empty(),
            Err(_) => false,
        };
        let threshold = std::env::var("WEAVEPY_JIT_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(50);
        // RFC 0059 WS3 — must precede the first compile of a frame
        // containing calls. Registered unconditionally (it only stores a
        // fn pointer) so late enabling, e.g. via the test hook, works.
        weavepy_jit::register_call_py_helper(wpjit_call_py);
        // RFC 0061 WS5 — same for the pinned-list access helpers.
        weavepy_jit::register_list_helpers(wpjit_list_get, wpjit_list_set);
        // RFC 0065 WS5 — the length/append and attribute lanes.
        weavepy_jit::register_list_extra_helpers(wpjit_list_len, wpjit_list_append);
        weavepy_jit::register_attr_helpers(wpjit_attr_get, wpjit_attr_set);
        JitState {
            enabled,
            threshold,
            engine: None,
            cache: HashMap::new(),
            stats: JitStats::default(),
        }
    }

    /// Bump the hot counter for `code` and, once it crosses the
    /// threshold, attempt compilation. `resolve_obj` maps a
    /// `LOAD_GLOBAL` name to its current resolution in the requesting
    /// frame's namespaces (used both to classify globals for analysis
    /// and to snapshot the guard expectations); `ret_lane_of` reports a
    /// candidate Python callee's stable scalar return lane (RFC 0059
    /// WS3); `probe_list` reports a subscripted local's observed element
    /// lane in the requesting activation (RFC 0061 WS5). Returns the
    /// compiled frame + guard snapshot + callee table when one is
    /// available.
    fn get_compiled(
        &mut self,
        code: &Rc<CodeObject>,
        resolve_obj: &mut dyn FnMut(&str) -> Option<Object>,
        ret_lane_of: &mut dyn FnMut(&Rc<PyFunction>, &Rc<CodeObject>) -> Option<JitType>,
        probe_list: &mut dyn FnMut(u32) -> Option<JitType>,
        probe_attr: &mut dyn FnMut(u32, &str, bool) -> Option<JitType>,
        attr_guard_of: &mut dyn FnMut(&AttrSiteMeta) -> Option<AttrGuard>,
    ) -> Option<CompiledEntry> {
        let key = Rc::as_ptr(code).cast::<CodeObject>();
        {
            let entry = self.cache.entry(key).or_insert_with(|| CacheEntry {
                counter: 0,
                tier: Tier::Cold,
                osr_failures: 0,
                _code: code.clone(),
            });
            match &entry.tier {
                Tier::Compiled(cf, snap, callees, attrs) => {
                    return Some(CompiledEntry {
                        cf: cf.clone(),
                        guard_snapshot: snap.clone(),
                        callees: callees.clone(),
                        attr_guards: attrs.clone(),
                    })
                }
                Tier::NotJitable => return None,
                Tier::Cold => {
                    entry.counter += 1;
                    if entry.counter < self.threshold {
                        return None;
                    }
                }
            }
        }
        // Threshold reached: compile (engine + cache borrowed disjointly).
        if self.engine.is_none() {
            self.engine = JitEngine::new();
            if self.engine.is_none() {
                // Host ISA unavailable — disable so we stop retrying.
                self.enabled = false;
                return None;
            }
        }
        let engine = self.engine.as_mut()?;
        // RFC 0059 WS3 — classify each LOAD_GLOBAL. A plain Python
        // function becomes a `PyFunc` callee: it gets a token in the
        // callee table, and (for non-self callees) must have an
        // analyzable scalar return lane so the caller can type the call
        // result. The analyzer resolves each name exactly once, so the
        // token sequence here matches the compiled code's.
        let mut callees: CalleeTable = Vec::new();
        let mut classify = |name: &str| {
            let obj = resolve_obj(name);
            if let Some(Object::Function(f)) = obj.as_ref() {
                let fcode = f.code.borrow().clone();
                if !py_callee_ok(&fcode) {
                    return ResolvedGlobal::Opaque;
                }
                let is_self = Rc::ptr_eq(&fcode, code);
                let ret = if is_self {
                    None
                } else {
                    ret_lane_of(f, &fcode)
                };
                if !is_self && ret.is_none() {
                    return ResolvedGlobal::Opaque;
                }
                let token = callees.len() as u32;
                callees.push((obj.clone().expect("checked Some above"), fcode.clone()));
                return ResolvedGlobal::PyFunc {
                    token,
                    arg_count: fcode.arg_count,
                    is_self,
                    ret,
                };
            }
            classify_global(obj.as_ref())
        };
        let (tier, out) =
            match engine.compile_with_probes(code, &mut classify, probe_list, probe_attr) {
                Ok(cf) => {
                    self.stats.frames_compiled += 1;
                    // Snapshot the exact objects the guards must keep
                    // resolving to. Every guarded name resolved during
                    // analysis, so it resolves here too (nothing ran since
                    // — same thread, GIL held).
                    let snap: Vec<(String, Object)> = cf
                        .global_guards
                        .iter()
                        .filter_map(|g| resolve_obj(&g.name).map(|o| (g.name.clone(), o)))
                        .collect();
                    // RFC 0065 WS5 — snapshot one guard fingerprint per
                    // burned-in attribute site. Every site probed during
                    // analysis, so it probes here too.
                    let mut attr_guards: Vec<AttrGuard> = Vec::with_capacity(cf.attr_sites.len());
                    for site in &cf.attr_sites {
                        match attr_guard_of(site) {
                            Some(g) => attr_guards.push(g),
                            None => break,
                        }
                    }
                    if snap.len() != cf.global_guards.len()
                        || attr_guards.len() != cf.attr_sites.len()
                    {
                        self.stats.frames_notjitable += 1;
                        (Tier::NotJitable, None)
                    } else {
                        let rc = StdRc::new(cf);
                        let snap = StdRc::new(snap);
                        let callees = StdRc::new(callees);
                        let attr_guards = StdRc::new(attr_guards);
                        (
                            Tier::Compiled(
                                rc.clone(),
                                snap.clone(),
                                callees.clone(),
                                attr_guards.clone(),
                            ),
                            Some(CompiledEntry {
                                cf: rc,
                                guard_snapshot: snap,
                                callees,
                                attr_guards,
                            }),
                        )
                    }
                }
                Err(_) => {
                    self.stats.frames_notjitable += 1;
                    (Tier::NotJitable, None)
                }
            };
        if let Some(entry) = self.cache.get_mut(&key) {
            entry.tier = tier;
        }
        out
    }

    /// Bump the back-edge counter. Returns `true` when the code is hot
    /// enough (or already compiled) that the caller should attempt an
    /// OSR entry (RFC 0059 WS3b).
    fn note_backedge(&mut self, code: &Rc<CodeObject>) -> bool {
        if !self.enabled {
            return false;
        }
        let key = Rc::as_ptr(code).cast::<CodeObject>();
        let entry = self.cache.entry(key).or_insert_with(|| CacheEntry {
            counter: 0,
            tier: Tier::Cold,
            osr_failures: 0,
            _code: code.clone(),
        });
        match entry.tier {
            Tier::Cold => {
                entry.counter = entry.counter.saturating_add(1);
                entry.counter >= self.threshold
            }
            Tier::Compiled(..) => entry.osr_failures < OSR_FAILURE_BUDGET,
            Tier::NotJitable => false,
        }
    }

    fn note_osr_failure(&mut self, code: &Rc<CodeObject>) {
        let key = Rc::as_ptr(code).cast::<CodeObject>();
        if let Some(entry) = self.cache.get_mut(&key) {
            entry.osr_failures = entry.osr_failures.saturating_add(1);
        }
    }
}

/// Whether a function's code is shape-eligible as a burned-in `CallPy`
/// callee: plain positional signature (defaults are fine — the analyzer
/// only admits exact-arity call sites), not a generator family or class
/// body. The *call itself* goes through the full interpreter machinery,
/// so this is about keeping the burned-in arity/lane assumptions simple,
/// not about what could be called.
fn py_callee_ok(code: &CodeObject) -> bool {
    !code.is_generator
        && !code.is_coroutine
        && !code.is_async_generator
        && !code.is_class_body
        && !code.has_varargs
        && !code.has_varkeywords
        && code.kwonly_count == 0
}

thread_local! {
    static JIT: RefCell<JitState> = RefCell::new(JitState::new());

    /// Memoized callee return lanes (RFC 0059 WS3), keyed by code
    /// object identity. The `Rc<CodeObject>` pins the address against
    /// reuse. The lane is a *prediction* — the call helper re-checks
    /// the actual result's tag at runtime — so staleness (e.g. the
    /// callee's own globals changing what its analysis would say) costs
    /// a deopt, never correctness.
    static RET_LANE_CACHE: RefCell<HashMap<*const CodeObject, (Option<JitType>, Rc<CodeObject>)>> =
        RefCell::new(HashMap::new());
}

/// Infer a candidate callee's stable scalar return lane by running the
/// tier-2 analyzer over its body, resolving names in the *callee's* own
/// namespaces. Nested Python callees are only recognized when they are
/// the callee itself (self-recursion, e.g. `fib`); anything deeper stays
/// opaque, bounding the recursion at depth one.
fn callee_ret_lane(
    interp: &super::Interpreter,
    f: &Rc<PyFunction>,
    fcode: &Rc<CodeObject>,
) -> Option<JitType> {
    let key = Rc::as_ptr(fcode).cast::<CodeObject>();
    if let Some(lane) = RET_LANE_CACHE.with(|c| c.borrow().get(&key).map(|(lane, _)| *lane)) {
        return lane;
    }
    let resolve = |name: &str| resolve_plain_dicts(interp, &f.globals, &f.builtins, name);
    let mut classify = |name: &str| {
        let obj = resolve(name);
        if let Some(Object::Function(g)) = obj.as_ref() {
            let gcode = g.code.borrow().clone();
            if Rc::ptr_eq(&gcode, fcode) && py_callee_ok(&gcode) {
                return ResolvedGlobal::PyFunc {
                    token: 0,
                    arg_count: gcode.arg_count,
                    is_self: true,
                    ret: None,
                };
            }
            return ResolvedGlobal::Opaque;
        }
        classify_global(obj.as_ref())
    };
    let lane = weavepy_jit::analyze(fcode, &mut classify)
        .ok()
        .and_then(|tf| tf.ret_lane);
    RET_LANE_CACHE.with(|c| c.borrow_mut().insert(key, (lane, fcode.clone())));
    lane
}

/// One pinned object in an activation's pin table (RFC 0061/0065 WS5):
/// slot bits tagged [`SlotTag::ListPin`] / [`SlotTag::ObjPin`] index
/// this table, which keeps the object alive and reachable for the
/// access helpers and the deopt rebuild.
enum Pin {
    /// A pinned list plus the element lane the compile assumed.
    List(Rc<GilRefCell<Vec<Object>>>, JitType),
    /// A pinned instance receiver (RFC 0065 WS5).
    Obj(Object),
}

impl Pin {
    /// The real object this pin stands for.
    fn to_object(&self) -> Object {
        match self {
            Pin::List(l, _) => Object::List(l.clone()),
            Pin::Obj(o) => o.clone(),
        }
    }
}

/// One activation's pinned objects (RFC 0061/0065 WS5).
type PinTable = Vec<Pin>;

/// Reconstruct an [`Object`] from a `(bits, tag)` slot. `Boxed` never
/// appears in locals or ordinary spills (the parked result travels
/// through [`CallCtx::parked`]); map it defensively to `None`, likewise
/// a pin tag reaching a context without pin-table access.
fn unpack(bits: u64, tag: u32) -> Object {
    match SlotTag::from_raw(tag) {
        SlotTag::Int => Object::Int(bits as i64),
        SlotTag::Float => Object::Float(f64::from_bits(bits)),
        SlotTag::Bool => Object::Bool(bits != 0),
        SlotTag::Boxed | SlotTag::ListPin | SlotTag::ObjPin => Object::None,
    }
}

/// As [`unpack`] with the activation's pin table at hand, so a pin
/// slot rebuilds into its real object (RFC 0061/0065 WS5).
fn unpack_pins(bits: u64, tag: u32, pins: &PinTable) -> Object {
    match SlotTag::from_raw(tag) {
        SlotTag::ListPin | SlotTag::ObjPin => {
            pins.get(bits as usize).map_or(Object::None, Pin::to_object)
        }
        _ => unpack(bits, tag),
    }
}

/// Reconstruct an [`Object`] from a slot whose lane is statically known.
fn unpack_ty(bits: u64, ty: JitType, pins: &PinTable) -> Object {
    match ty {
        JitType::Int => Object::Int(bits as i64),
        JitType::Float => Object::Float(f64::from_bits(bits)),
        JitType::Bool => Object::Bool(bits != 0),
        JitType::ListInt | JitType::ListFloat | JitType::Obj => {
            pins.get(bits as usize).map_or(Object::None, Pin::to_object)
        }
        JitType::Unknown => Object::None,
    }
}

/// Pack a representable [`Object`] into its slot bits for `ty`, or `None`
/// if it doesn't match the expected lane.
fn pack(obj: &Object, ty: JitType) -> Option<u64> {
    match (ty, obj) {
        (JitType::Int, Object::Int(i)) => Some(*i as u64),
        (JitType::Bool, Object::Bool(b)) => Some(u64::from(*b)),
        (JitType::Float, Object::Float(f)) => Some(f.to_bits()),
        _ => None,
    }
}

/// Entry-guard check for one managed local (RFC 0061/0065 WS5): a
/// scalar lane must pack; a pinned-list lane must hold a `list` whose
/// *first* element matches the compiled element lane (an O(1) proxy
/// for the probe's full scan — the access helpers re-validate per
/// element, so a heterogeneous tail costs a deopt, never correctness);
/// a pinned-instance lane must hold an instance (the per-site class
/// guards re-validate the shape per access).
fn entry_local_ok(obj: &Object, ty: JitType) -> bool {
    if ty == JitType::Obj {
        return matches!(obj, Object::Instance(_));
    }
    let Some(elem) = ty.elem_lane() else {
        return pack(obj, ty).is_some();
    };
    let Object::List(l) = obj else {
        return false;
    };
    matches!(
        (l.borrow().first(), elem),
        (None, _) | (Some(Object::Int(_)), JitType::Int) | (Some(Object::Float(_)), JitType::Float)
    )
}

/// The compile-time shape probe (RFC 0061 WS5): report the element lane
/// of local `slot` when it currently holds a homogeneous non-empty
/// `int` or `float` list; `Some(Unknown)` for an *empty* list
/// (definitely a list, but with no lane evidence — RFC 0065 WS5 lets
/// `append`'s value lane pin it); `None` otherwise.
fn probe_list_lane(frame: &super::Frame, slot: u32) -> Option<JitType> {
    let locals = frame.locals.borrow();
    let Some(Object::List(l)) = locals.get(slot as usize) else {
        return None;
    };
    let items = l.borrow();
    if items.is_empty() {
        return Some(JitType::Unknown);
    }
    let mut lane: Option<JitType> = None;
    for it in items.iter() {
        let t = match it {
            Object::Int(_) => JitType::Int,
            Object::Float(_) => JitType::Float,
            _ => return None,
        };
        match lane {
            None => lane = Some(t),
            Some(cur) if cur == t => {}
            Some(_) => return None,
        }
    }
    lane
}

/// The scalar lane of an [`Object`], or `None` for anything else.
fn scalar_lane(obj: &Object) -> Option<JitType> {
    match obj {
        Object::Int(_) => Some(JitType::Int),
        Object::Float(_) => Some(JitType::Float),
        Object::Bool(_) => Some(JitType::Bool),
        _ => None,
    }
}

/// RFC 0065 WS5 — the compile-time attribute probe: report the scalar
/// value lane of `name` on the instance currently in local `slot`,
/// but only when the receiver shape matches the tier-1 inline-cache
/// eligibility (no `__getattr__`/`__getattribute__`, no shadowing data
/// descriptor, name present in the instance dict — exactly the
/// `LoadAttrInstance`/`StoreAttrInstance` shapes).
fn probe_attr_lane(frame: &super::Frame, slot: u32, name: &str, store: bool) -> Option<JitType> {
    attr_fingerprint(frame, slot, name, store).map(|(lane, ..)| lane)
}

/// RFC 0065 WS5 — snapshot the full guard fingerprint for one
/// attribute site right after compilation (nothing ran since the
/// probe — same thread, GIL held — so it succeeds iff the probe did).
fn attr_site_guard(frame: &super::Frame, site: &AttrSiteMeta) -> Option<AttrGuard> {
    let (lane, type_id, ver, key_idx, class) =
        attr_fingerprint(frame, site.slot, &site.name, site.store)?;
    if lane != site.lane {
        return None;
    }
    Some(AttrGuard {
        name: site.name.clone(),
        lane,
        type_id,
        ver,
        key_idx,
        _class: class,
    })
}

/// Shared probe body: classify the receiver with the tier-1
/// specialization predicate and read the current value's lane.
fn attr_fingerprint(
    frame: &super::Frame,
    slot: u32,
    name: &str,
    store: bool,
) -> Option<(JitType, u64, u32, u32, Rc<TypeObject>)> {
    use weavepy_compiler::InlineCache as IC;
    let locals = frame.locals.borrow();
    let obj = locals.get(slot as usize)?;
    let Object::Instance(inst) = obj else {
        return None;
    };
    let (type_id, key_idx, ver) = if store {
        match crate::specialize::attempt_specialize_store_attr(obj, name) {
            IC::StoreAttrInstance {
                type_id,
                key_idx,
                ver,
            } => (type_id, key_idx, ver),
            _ => return None,
        }
    } else {
        match crate::specialize::attempt_specialize_load_attr(obj, name) {
            IC::LoadAttrInstance {
                type_id,
                key_idx,
                ver,
            } => (type_id, key_idx, ver),
            _ => return None,
        }
    };
    let dict = inst.dict.borrow();
    let (_, v) = dict.get_index(key_idx as usize)?;
    let lane = scalar_lane(v)?;
    Some((lane, type_id, ver, key_idx, inst.cls()))
}

/// Bump the back-edge hot counter for a code object. Returns `true`
/// when the caller should attempt an OSR entry (RFC 0059 WS3b); always
/// `false` when the JIT is disabled.
pub(crate) fn note_backedge(code: &Rc<CodeObject>) -> bool {
    JIT.with(|cell| cell.borrow_mut().note_backedge(code))
}

/// Resolve a global name the way `LOAD_GLOBAL`'s happy path does —
/// globals then builtins, plain dict gets only. Returns `None` for a
/// dict-subclass globals mapping (whose `__missing__` hook the generic
/// path would consult), so such frames never take the burned-in fast
/// path.
fn resolve_plain_global(
    interp: &super::Interpreter,
    frame: &super::Frame,
    name: &str,
) -> Option<Object> {
    resolve_plain_dicts(interp, &frame.globals, &frame.builtins, name)
}

/// As [`resolve_plain_global`] but against explicit dicts (the call
/// helper and callee analysis have no `Frame` at hand).
fn resolve_plain_dicts(
    interp: &super::Interpreter,
    globals: &Rc<GilRefCell<DictData>>,
    builtins: &Rc<GilRefCell<DictData>>,
    name: &str,
) -> Option<Object> {
    if interp.globals_missing_owner(globals).is_some() {
        return None;
    }
    let key = StrKey(name);
    if let Some(v) = globals.borrow().get(&key) {
        return Some(v.clone());
    }
    builtins.borrow().get(&key).cloned()
}

/// Classify a resolved global for the analyzer (RFC 0058 WS4): the
/// canonical `range` becomes a counted-loop callee; scalar constants
/// burn in; everything else is opaque. `range` appears in two canonical
/// shapes — module globals hold the singleton `range` *type* object
/// (from `builtin_types().as_globals()`), while the `builtins` dict
/// holds the function-flavoured `BuiltinFn` — and both call through
/// `b_range`. Builtin types reject attribute mutation, so identity
/// implies unmodified call semantics.
fn classify_global(obj: Option<&Object>) -> ResolvedGlobal {
    match obj {
        Some(Object::Builtin(b)) if b.name == "range" => ResolvedGlobal::RangeBuiltin,
        Some(Object::Type(t)) if Rc::ptr_eq(t, &crate::builtin_types::builtin_types().range_) => {
            ResolvedGlobal::RangeBuiltin
        }
        // RFC 0065 WS5 — `len` on a pinned list lowers to `ListLen`.
        // Builtins reject attribute mutation, so identity (the entry
        // guard) implies unmodified call semantics.
        Some(Object::Builtin(b)) if b.name == "len" => ResolvedGlobal::LenBuiltin,
        Some(Object::Int(v)) => ResolvedGlobal::ConstInt(*v),
        Some(Object::Float(v)) => ResolvedGlobal::ConstFloat(v.to_bits()),
        Some(Object::Bool(v)) => ResolvedGlobal::ConstBool(*v),
        _ => ResolvedGlobal::Opaque,
    }
}

/// Per-native-activation context handed to [`wpjit_call_py`] through
/// [`JitFrame::ctx`] (RFC 0059 WS3). Lives on `enter_compiled`'s stack
/// for exactly the duration of one native call.
struct CallCtx {
    /// The live interpreter, as a raw pointer because the `&mut` that
    /// entered native code is dormant while the helper runs (the same
    /// re-entrancy pattern as `vm_singletons::publish_interpreter_ptr`).
    interp: *mut super::Interpreter,
    callees: StdRc<CalleeTable>,
    guard_snapshot: StdRc<Vec<(String, Object)>>,
    /// The caller frame's namespaces, for post-call guard revalidation
    /// (the caller `Frame` itself is mutably borrowed across the native
    /// call and must not be touched from here).
    globals: Rc<GilRefCell<DictData>>,
    builtins: Rc<GilRefCell<DictData>>,
    /// A completed call's unrepresentable (or guard-invalidated) result,
    /// parked for the deopt-after-call reconstruction.
    parked: Option<Object>,
    /// A raised callee's exception, parked for the `Raised` exit.
    raised: Option<RuntimeError>,
    /// RFC 0061/0065 WS5 — this activation's pinned objects, indexed
    /// by the pin bits native code carries in `ListPin`/`ObjPin` slots.
    pins: PinTable,
    /// RFC 0065 WS5 — per-site attribute guards, indexed by the `site`
    /// operand of `wpjit_attr_get`/`_set`.
    attr_guards: StdRc<Vec<AttrGuard>>,
}

/// `true` while every burned-in resolution still holds: each guarded
/// global resolves to the identical object, and each burned-in callee
/// still wears the `__code__` it was compiled against (functions are
/// code-rebindable; a swap invalidates arity/lane assumptions).
fn guards_hold(
    interp: &super::Interpreter,
    globals: &Rc<GilRefCell<DictData>>,
    builtins: &Rc<GilRefCell<DictData>>,
    guard_snapshot: &[(String, Object)],
    callees: &CalleeTable,
) -> bool {
    for (name, expected) in guard_snapshot {
        let ok = resolve_plain_dicts(interp, globals, builtins, name)
            .is_some_and(|cur| cur.is_same(expected));
        if !ok {
            return false;
        }
    }
    for (f, code_snap) in callees {
        let Object::Function(pf) = f else {
            return false;
        };
        if !Rc::ptr_eq(&pf.code.borrow(), code_snap) {
            return false;
        }
    }
    true
}

/// The `wpjit_call_py` helper (RFC 0059 WS3): native code calls this
/// with marshaled scalar arguments; it performs the full Python call
/// through the interpreter and reports how the caller should proceed.
///
/// # Safety
///
/// Called only from compiled frames entered by [`enter_compiled`], which
/// guarantees `frame` and its `ctx`/`call_args`/`call_tags` buffers are
/// live and exclusive for the duration of the native activation.
unsafe extern "C" fn wpjit_call_py(
    frame: *mut JitFrame,
    token: u32,
    argc: u32,
    expect_tag: u32,
) -> i64 {
    // SAFETY: per the function contract, `frame` and `ctx` are the live,
    // exclusively-owned buffers of the current native activation. The
    // `ctx` pointer was produced from a `&mut CallCtx` in `enter_compiled`
    // (erased to `*mut u8` for the C ABI), so the alignment the cast
    // reinstates is guaranteed by construction.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` that entered native code is dormant
    // while the helper runs; this is the only live path to it.
    let interp = unsafe { &mut *ctx.interp };

    let (callee, _code) = ctx.callees[token as usize].clone();
    let mut args: Vec<Object> = Vec::with_capacity(argc as usize);
    for j in 0..argc as usize {
        // SAFETY: native code wrote `argc` entries, and the buffers are
        // `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        args.push(unpack(bits, tag));
    }

    match interp.call(&callee, &args, &[], &ctx.globals) {
        Err(err) => {
            ctx.raised = Some(err);
            CallStatus::Raised as i64
        }
        Ok(v) => {
            // The callee may have rebound a burned-in global or a
            // callee's `__code__`; the *next* burned operation would
            // then be wrong, so the caller must deopt after this call.
            // (Everything up to and including this call used the
            // pre-rebinding values, which the entry guards validated.)
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
            );
            if still_valid {
                let expect = match SlotTag::from_raw(expect_tag) {
                    SlotTag::Int => JitType::Int,
                    SlotTag::Float => JitType::Float,
                    SlotTag::Bool => JitType::Bool,
                    // A pin-lane call result is rejected at emission;
                    // `Unknown` never packs, forcing the boxed path.
                    SlotTag::Boxed | SlotTag::ListPin | SlotTag::ObjPin => JitType::Unknown,
                };
                if let Some(bits) = pack(&v, expect) {
                    jf.ret_bits = bits;
                    jf.ret_tag = expect_tag;
                    return CallStatus::Ok as i64;
                }
            }
            ctx.parked = Some(v);
            CallStatus::Boxed as i64
        }
    }
}

/// The `wpjit_list_get` helper (RFC 0061 WS5): read one element of a
/// pinned list. Returns `0` with the element's bits in
/// [`JitFrame::ret_bits`], or non-zero to deopt — out of range, or the
/// element no longer matches the pinned lane (aliased mutation through
/// a callee). Never runs Python code and never drops a heap object.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_list_get(frame: *mut JitFrame, pin: i64, idx: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let items = list.borrow();
    let len = items.len() as i64;
    let i = if idx < 0 { idx + len } else { idx };
    if i < 0 || i >= len {
        return 1;
    }
    match (&items[i as usize], elem) {
        (Object::Int(v), JitType::Int) => {
            jf.ret_bits = *v as u64;
            0
        }
        (Object::Float(f), JitType::Float) => {
            jf.ret_bits = f.to_bits();
            0
        }
        _ => 1,
    }
}

/// The `wpjit_list_set` helper (RFC 0061 WS5): write one element of a
/// pinned list. The value's bits are pre-staged in
/// [`JitFrame::ret_bits`], interpreted per the pin's element lane.
/// Deopts (non-zero) when out of range or when the displaced element
/// is a heap object — replacing it here would drop it inside the
/// helper, and the drop-site machinery (prompt reap, parked
/// finalizers) belongs to the interpreter's store path.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_list_set(frame: *mut JitFrame, pin: i64, idx: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let v = match elem {
        JitType::Int => Object::Int(jf.ret_bits as i64),
        JitType::Float => Object::Float(f64::from_bits(jf.ret_bits)),
        _ => return 1,
    };
    let mut items = list.borrow_mut();
    let len = items.len() as i64;
    let i = if idx < 0 { idx + len } else { idx };
    if i < 0 || i >= len {
        return 1;
    }
    let dst = &mut items[i as usize];
    if !matches!(
        dst,
        Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
    ) {
        return 1;
    }
    *dst = v;
    0
}

/// The `wpjit_list_len` helper (RFC 0065 WS5): the length of a pinned
/// list, or `-1` on a pin-table miss (defensive — deopts). Never runs
/// Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_list_len(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::List(list, _)) = ctx.pins.get(pin as usize) else {
        return -1;
    };
    list.borrow().len() as i64
}

/// The `wpjit_list_append` helper (RFC 0065 WS5): append one value
/// (pre-staged in [`JitFrame::ret_bits`], interpreted per the pin's
/// element lane) to a pinned list. The analyzer guaranteed the value's
/// lane, so the append preserves the pinned shape; the non-zero paths
/// are defensive. Never runs Python code and never drops a heap
/// object (the appended value is a fresh scalar).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_list_append(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let v = match elem {
        JitType::Int => Object::Int(jf.ret_bits as i64),
        JitType::Float => Object::Float(f64::from_bits(jf.ret_bits)),
        _ => return 1,
    };
    list.borrow_mut().push(v);
    0
}

/// `true` when a dict key is the string `name` (the per-access name
/// re-check that makes an indexed hit safe against `del`-driven index
/// shifts, mirroring the tier-1 caches).
fn key_is(key: &DictKey, name: &str) -> bool {
    matches!(&key.0, Object::Str(s) if &**s == name)
}

/// The `wpjit_attr_get` helper (RFC 0065 WS5): read one scalar
/// attribute of a pinned instance through the burned-in site guard —
/// class identity + attr-version, indexed instance-dict hit with name
/// match, value lane. Any mismatch returns non-zero (deopt) and the
/// interpreter re-executes the `LOAD_ATTR` generically, so descriptor
/// or `__getattr__` semantics introduced *after* compilation stay
/// exact. Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_attr_get(frame: *mut JitFrame, pin: i64, site: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::Obj(Object::Instance(inst))) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let Some(g) = ctx.attr_guards.get(site as usize) else {
        return 1;
    };
    let guard_ok = {
        let cls = inst.class.borrow();
        crate::specialize::rc_id(&cls) == g.type_id && cls.attr_version.get() == g.ver
    };
    if !guard_ok {
        return 1;
    }
    let dict = inst.dict.borrow();
    match dict.get_index(g.key_idx as usize) {
        Some((k, v)) if key_is(k, &g.name) => match pack(v, g.lane) {
            Some(bits) => {
                jf.ret_bits = bits;
                0
            }
            None => 1,
        },
        _ => 1,
    }
}

/// The `wpjit_attr_set` helper (RFC 0065 WS5): overwrite one scalar
/// attribute of a pinned instance (value pre-staged in
/// [`JitFrame::ret_bits`], interpreted per the site's lane) under the
/// same guards as [`wpjit_attr_get`], plus one more: the *displaced*
/// value must itself be a scalar — dropping a heap object here would
/// bypass the interpreter's drop-site machinery (prompt reap, parked
/// finalizers), so that case deopts to the generic store instead.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_attr_set(frame: *mut JitFrame, pin: i64, site: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::Obj(Object::Instance(inst))) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let Some(g) = ctx.attr_guards.get(site as usize) else {
        return 1;
    };
    let v = match g.lane {
        JitType::Int => Object::Int(jf.ret_bits as i64),
        JitType::Float => Object::Float(f64::from_bits(jf.ret_bits)),
        JitType::Bool => Object::Bool(jf.ret_bits != 0),
        _ => return 1,
    };
    let guard_ok = {
        let cls = inst.class.borrow();
        crate::specialize::rc_id(&cls) == g.type_id && cls.attr_version.get() == g.ver
    };
    if !guard_ok {
        return 1;
    }
    let mut dict = inst.dict.borrow_mut();
    let Some((k, dst)) = dict.get_index_mut(g.key_idx as usize) else {
        return 1;
    };
    if !key_is(k, &g.name) {
        return 1;
    }
    if !matches!(
        dst,
        Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
    ) {
        return 1;
    }
    *dst = v;
    0
}

/// Offer a fresh frame (pc 0, empty stack) to the JIT. See [`JitEntry`].
pub(crate) fn try_enter(interp: &mut super::Interpreter, frame: &mut super::Frame) -> JitEntry {
    // Phase 1: counter + compilation, holding the state borrow briefly.
    let entry = JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.enabled {
            return None;
        }
        st.stats.frames_seen += 1;
        let interp_ref: &super::Interpreter = interp;
        let frame_ref: &super::Frame = frame;
        let mut resolve = |name: &str| resolve_plain_global(interp_ref, frame_ref, name);
        let mut ret_of = |f: &Rc<PyFunction>, c: &Rc<CodeObject>| callee_ret_lane(interp_ref, f, c);
        let mut probe = |slot: u32| probe_list_lane(frame_ref, slot);
        let mut probe_attr =
            |slot: u32, name: &str, store: bool| probe_attr_lane(frame_ref, slot, name, store);
        let mut attr_guard = |site: &AttrSiteMeta| attr_site_guard(frame_ref, site);
        st.get_compiled(
            &frame.code,
            &mut resolve,
            &mut ret_of,
            &mut probe,
            &mut probe_attr,
            &mut attr_guard,
        )
    });
    let Some(entry) = entry else {
        return JitEntry::Skip;
    };

    // Phase 2a: global identity + callee code guards.
    if !guards_hold(
        interp,
        &frame.globals,
        &frame.builtins,
        &entry.guard_snapshot,
        &entry.callees,
    ) {
        JIT.with(|cell| cell.borrow_mut().stats.entry_guard_failures += 1);
        return JitEntry::Skip;
    }

    // Phase 2b: entry type-guard on the live-in locals.
    {
        let locals = frame.locals.borrow();
        for &slot in &entry.cf.livein {
            let ty = match entry.cf.local_types.get(slot as usize).copied().flatten() {
                Some(t) => t,
                None => return JitEntry::Skip,
            };
            let ok = locals
                .get(slot as usize)
                .is_some_and(|o| entry_local_ok(o, ty));
            if !ok {
                JIT.with(|cell| cell.borrow_mut().stats.entry_guard_failures += 1);
                return JitEntry::Skip;
            }
        }
    }

    enter_compiled(interp, frame, &entry, 0, &[])
}

/// Attempt an on-stack replacement entry at a loop back-edge target
/// (RFC 0059 WS3b). `frame.pc` must already be the jump target. The
/// operand stack must consist of exactly the live rewritten-`range`
/// iterators for the loops enclosing that pc (decomposed into their
/// synthetic slots), and *every* JIT-managed local must currently hold
/// its stable lane (the native prologue loads them all).
pub(crate) fn try_enter_osr(interp: &mut super::Interpreter, frame: &mut super::Frame) -> JitEntry {
    let entry = JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.enabled {
            return None;
        }
        let interp_ref: &super::Interpreter = interp;
        let frame_ref: &super::Frame = frame;
        let mut resolve = |name: &str| resolve_plain_global(interp_ref, frame_ref, name);
        let mut ret_of = |f: &Rc<PyFunction>, c: &Rc<CodeObject>| callee_ret_lane(interp_ref, f, c);
        let mut probe = |slot: u32| probe_list_lane(frame_ref, slot);
        let mut probe_attr =
            |slot: u32, name: &str, store: bool| probe_attr_lane(frame_ref, slot, name, store);
        let mut attr_guard = |site: &AttrSiteMeta| attr_site_guard(frame_ref, site);
        st.get_compiled(
            &frame.code,
            &mut resolve,
            &mut ret_of,
            &mut probe,
            &mut probe_attr,
            &mut attr_guard,
        )
    });
    let Some(entry) = entry else {
        return JitEntry::Skip;
    };
    let fail = |code: &Rc<CodeObject>| {
        JIT.with(|cell| cell.borrow_mut().note_osr_failure(code));
        JitEntry::Skip
    };
    let cf = &entry.cf;
    let pc = frame.pc;
    if !cf.osr_entries.iter().any(|e| e.pc == pc) {
        return fail(&frame.code);
    }
    if !guards_hold(
        interp,
        &frame.globals,
        &frame.builtins,
        &entry.guard_snapshot,
        &entry.callees,
    ) {
        return fail(&frame.code);
    }
    // Every managed *real* local must hold its lane right now — unlike a
    // fresh entry there is no definite-assignment argument that the
    // native code writes before it reads.
    {
        let locals = frame.locals.borrow();
        let n_real = frame.code.varnames.len();
        for slot in 0..n_real {
            if let Some(ty) = cf.local_types.get(slot).copied().flatten() {
                if !locals.get(slot).is_some_and(|o| entry_local_ok(o, ty)) {
                    drop(locals);
                    return fail(&frame.code);
                }
            }
        }
    }
    // The interpreter stack at the loop header holds exactly the live
    // range iterators of the enclosing rewritten loops, outermost first.
    // Decompose them into the synthetic (cur, stop) slots the compiled
    // loop runs on; the `ForRange` header re-checks the bound on entry.
    let live: Vec<&weavepy_jit::RangeLoopMeta> = cf
        .range_loops
        .iter()
        .filter(|l| l.live_from <= pc && pc < l.live_to)
        .collect();
    if frame.stack.len() != live.len() {
        return fail(&frame.code);
    }
    let mut synth: Vec<(u32, u64)> = Vec::with_capacity(live.len() * 2);
    for (idx, lp) in live.iter().enumerate() {
        let Object::Iter(it) = &frame.stack[idx] else {
            return fail(&frame.code);
        };
        let decomposed = match &*it.borrow() {
            PyIterator::Range {
                current,
                stop,
                step: 1,
            } => Some((*current as u64, *stop as u64)),
            _ => None,
        };
        let Some((cur, stop)) = decomposed else {
            return fail(&frame.code);
        };
        synth.push((lp.cur_slot, cur));
        synth.push((lp.stop_slot, stop));
    }
    // The iterators are consumed by the decomposition: native code owns
    // the loops from here (a deopt reconstructs fresh iterators).
    frame.stack.clear();
    JIT.with(|cell| cell.borrow_mut().stats.osr_entries += 1);
    enter_compiled(interp, frame, &entry, pc, &synth)
}

/// Marshal locals, enter the compiled frame at `entry_pc`, and translate
/// the native exit back into interpreter state. Guards must already
/// hold, `frame.stack` must be empty, and `synth_init` seeds synthetic
/// slots for an OSR entry.
fn enter_compiled(
    interp: &mut super::Interpreter,
    frame: &mut super::Frame,
    entry: &CompiledEntry,
    entry_pc: u32,
    synth_init: &[(u32, u64)],
) -> JitEntry {
    let cf = &entry.cf;
    let n = cf.n_locals as usize;
    let mut locals_buf = vec![0u64; n];
    // RFC 0061/0065 WS5 — pin every list-/instance-lane local: the
    // slot carries an index into `pins`, and the table (not the slot)
    // keeps the object alive and reachable for the access helpers and
    // the deopt rebuild.
    let mut pins: PinTable = Vec::new();
    {
        let locals = frame.locals.borrow();
        for (slot, dst) in locals_buf.iter_mut().enumerate() {
            if let Some(ty) = cf.local_types[slot] {
                if let Some(elem) = ty.elem_lane() {
                    if let Some(Object::List(l)) = locals.get(slot) {
                        *dst = pins.len() as u64;
                        pins.push(Pin::List(l.clone(), elem));
                    }
                    continue;
                }
                if ty == JitType::Obj {
                    if let Some(o @ Object::Instance(_)) = locals.get(slot) {
                        *dst = pins.len() as u64;
                        pins.push(Pin::Obj(o.clone()));
                    }
                    continue;
                }
                *dst = locals.get(slot).and_then(|o| pack(o, ty)).unwrap_or(0);
            }
        }
    }
    for &(slot, bits) in synth_init {
        locals_buf[slot as usize] = bits;
    }
    let cap = cf.max_stack as usize + 1;
    let mut spill = vec![0u64; cap];
    let mut tags = vec![0u32; cap];
    let call_cap = (cf.max_call_args as usize).max(1);
    let mut call_args = vec![0u64; call_cap];
    let mut call_tags = vec![0u32; call_cap];
    let mut ctx = CallCtx {
        interp: std::ptr::from_mut(interp),
        callees: entry.callees.clone(),
        guard_snapshot: entry.guard_snapshot.clone(),
        globals: frame.globals.clone(),
        builtins: frame.builtins.clone(),
        parked: None,
        raised: None,
        pins,
        attr_guards: entry.attr_guards.clone(),
    };
    let mut jf = JitFrame {
        locals: locals_buf.as_mut_ptr(),
        n_locals: cf.n_locals,
        entry_pc,
        ret_bits: 0,
        ret_tag: 0,
        deopt_pc: 0,
        stack_spill: spill.as_mut_ptr(),
        stack_tags: tags.as_mut_ptr(),
        stack_len: 0,
        stack_cap: cap as u32,
        ctx: std::ptr::from_mut(&mut ctx).cast::<u8>(),
        call_args: call_args.as_mut_ptr(),
        call_tags: call_tags.as_mut_ptr(),
    };

    // SAFETY: `locals_buf` is `n_locals` wide, `spill`/`tags` are
    // `max_stack + 1` wide, and `call_args`/`call_tags` are
    // `max_call_args` wide, matching what the compiled frame was built
    // to address; the engine that backs `cf` lives in this thread's
    // `JIT` thread-local for the process lifetime; `ctx` outlives the
    // call and is only touched by the `wpjit_call_py` helper.
    let status = unsafe { cf.enter(&raw mut jf) };

    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        st.stats.native_entries += 1;
        if matches!(status, JitStatus::Deopt) {
            st.stats.deopts += 1;
        }
    });

    match status {
        JitStatus::Returned => JitEntry::Ran(unpack_pins(jf.ret_bits, jf.ret_tag, &ctx.pins)),
        JitStatus::Deopt | JitStatus::Raised => {
            // Write back managed locals (synthetic range slots have no
            // interpreter home — they feed the iterator rebuild below),
            // rebuild the operand stack from the spill, and resume at
            // the deopt pc.
            {
                let mut locals = frame.locals.borrow_mut();
                for (slot, &bits) in locals_buf.iter().enumerate() {
                    if let Some(ty) = cf.local_types[slot] {
                        if let Some(dst) = locals.get_mut(slot) {
                            *dst = unpack_ty(bits, ty, &ctx.pins);
                        }
                    }
                }
            }
            rebuild_stack(
                interp,
                frame,
                entry,
                &locals_buf,
                &spill,
                &tags,
                &jf,
                &ctx.pins,
            );
            if matches!(status, JitStatus::Raised) {
                // As though the CALL instruction just executed and
                // raised: pc points past it (`handle_exception` uses
                // `pc - 1` as the raise site).
                frame.pc = jf.deopt_pc + 1;
                let err = ctx.raised.take().unwrap_or_else(|| {
                    RuntimeError::Internal("JIT Raised exit without a parked exception".to_owned())
                });
                return JitEntry::Raised(err);
            }
            // A deopt-after-call carries the parked, already-computed
            // result: it goes on top of the rebuilt stack and the
            // interpreter resumes at the instruction after the call.
            if let Some(v) = ctx.parked.take() {
                frame.stack.push(v);
            }
            frame.pc = jf.deopt_pc;
            JitEntry::Deopt
        }
    }
}

/// Rebuild the interpreter operand stack after a native side exit: the
/// live range iterators of enclosing rewritten loops (bottom), then the
/// spilled temporaries with any *erased* callee objects re-inserted at
/// their recorded interpreter-stack depths (RFC 0059 WS3). RFC 0065
/// WS5 adds two more erasures: `len` builtins (re-inserted from the
/// guard snapshot, like callees) and `.append` bound-method receivers
/// (the spilled list pin is rebuilt as the *bound method* the
/// interpreter would hold there).
#[allow(clippy::too_many_arguments)]
fn rebuild_stack(
    interp: &mut super::Interpreter,
    frame: &mut super::Frame,
    entry: &CompiledEntry,
    locals_buf: &[u64],
    spill: &[u64],
    tags: &[u32],
    jf: &JitFrame,
    pins: &PinTable,
) {
    let cf = &entry.cf;
    // RFC 0058 WS4 — iterators from the synthetic slots, outermost first.
    for lp in &cf.range_loops {
        if lp.live_from <= jf.deopt_pc && jf.deopt_pc < lp.live_to {
            let current = locals_buf[lp.cur_slot as usize] as i64;
            let stop = locals_buf[lp.stop_slot as usize] as i64;
            frame
                .stack
                .push(Object::Iter(Rc::new(crate::sync::RefCell::new(
                    PyIterator::Range {
                        current,
                        stop,
                        step: 1,
                    },
                ))));
        }
    }
    // Erased objects to re-insert, by ascending interpreter depth:
    // callee spans open at the deopt pc (strictly between the erased
    // LOAD_GLOBAL and its CALL) and `len` spans likewise (their
    // `live_to` is already past the CALL).
    let mut inserts: Vec<(u32, Object)> = cf
        .callee_spans
        .iter()
        .filter(|s| s.live_from < jf.deopt_pc && jf.deopt_pc < s.live_to)
        .map(|s| (s.interp_depth, entry.callees[s.token as usize].0.clone()))
        .collect();
    if !cf.len_spans.is_empty() {
        let len_obj = entry
            .guard_snapshot
            .iter()
            .find(|(name, _)| name == "len")
            .map(|(_, o)| o.clone());
        for s in cf
            .len_spans
            .iter()
            .filter(|s| s.live_from < jf.deopt_pc && jf.deopt_pc < s.live_to)
        {
            inserts.push((s.interp_depth, len_obj.clone().unwrap_or(Object::None)));
        }
    }
    inserts.sort_unstable_by_key(|(depth, _)| *depth);
    // Open method spans: the spilled entry at `native_index` must
    // rebuild as the bound method, not the bare pinned list.
    let bound_recv: Vec<u32> = cf
        .method_spans
        .iter()
        .filter(|s| s.live_from < jf.deopt_pc && jf.deopt_pc < s.live_to)
        .map(|s| s.native_index)
        .collect();
    let mut next = 0usize;
    for i in 0..jf.stack_len as usize {
        while next < inserts.len() && inserts[next].0 as usize == frame.stack.len() {
            frame.stack.push(inserts[next].1.clone());
            next += 1;
        }
        let mut v = unpack_pins(spill[i], tags[i], pins);
        if bound_recv.contains(&(i as u32)) {
            // The receiver of an open `.append` span: what the
            // interpreter holds here is the *bound method*. `list`
            // always has `append`, so the load cannot fail; `None` is
            // an unreachable defensive fallback.
            v = interp
                .load_attr_public(&v, "append")
                .unwrap_or(Object::None);
        }
        frame.stack.push(v);
    }
    while next < inserts.len() {
        frame.stack.push(inserts[next].1.clone());
        next += 1;
    }
}

/// Test hook: force the JIT on for the current thread with a low
/// tier-up threshold, regardless of `WEAVEPY_JIT`. Compiled only in
/// test builds so it never reaches release binaries.
#[cfg(test)]
pub(crate) fn force_enable_for_test(threshold: u32) {
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        st.enabled = true;
        st.threshold = threshold.max(1);
    });
}

/// Test hook: `(frames_compiled, native_entries, deopts)` for the
/// current thread.
#[cfg(test)]
pub(crate) fn stats_for_test() -> (u64, u64, u64) {
    JIT.with(|cell| {
        let s = &cell.borrow().stats;
        (s.frames_compiled, s.native_entries, s.deopts)
    })
}

/// Test hook: OSR entry count for the current thread (RFC 0059 WS3b).
#[cfg(test)]
pub(crate) fn osr_stats_for_test() -> u64 {
    JIT.with(|cell| cell.borrow().stats.osr_entries)
}

/// Render the JIT counters as markdown rows, or `None` if the JIT was
/// never exercised on this thread.
pub(crate) fn format_stats_markdown() -> Option<String> {
    JIT.with(|cell| {
        let st = cell.borrow();
        let s = &st.stats;
        if s.frames_seen == 0 {
            return None;
        }
        Some(format!(
            "\n## Tier-2 JIT stats\n\n\
             - frames seen: **{}**\n\
             - frames compiled: **{}**\n\
             - frames not JITable: **{}**\n\
             - native entries: **{}**\n\
             - OSR entries: **{}**\n\
             - deopts: **{}**\n\
             - entry-guard failures: **{}**\n",
            s.frames_seen,
            s.frames_compiled,
            s.frames_notjitable,
            s.native_entries,
            s.osr_entries,
            s.deopts,
            s.entry_guard_failures,
        ))
    })
}
