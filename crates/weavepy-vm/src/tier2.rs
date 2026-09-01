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
    AttrSiteMeta, CallStatus, CompiledFrame, CtorFieldSrc, JitEngine, JitFrame, JitStatus, JitType,
    MethodResolution, MethodRet, Probes, ResolvedGlobal, SlotTag,
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
    /// RFC 0073 WS4 — a compiled generator body yielded this value and
    /// its whole native activation was *parked* on the frame
    /// ([`super::Frame::parked_native`]): no locals writeback, no
    /// stack rebuild. `frame.pc` sits at the yield's continuation, so
    /// the frame looks exactly like an interpreted suspension to the
    /// generator machinery — except its truth lives in the box until
    /// the next native resume or [`materialize_parked`].
    Yielded(Object),
    /// The frame was not entered (cold, not JITable, or guard failed);
    /// run the interpreter as usual.
    Skip,
}

/// One burned-in Python callee (RFC 0059 WS3): the function object the
/// `CallPy` token resolves to, plus its `__code__` at compile time
/// (functions are code-rebindable, so identity of the function alone
/// does not pin the burned-in arity/return-lane assumptions).
type CalleeTable = Vec<(Object, Rc<CodeObject>)>;

/// How a burned-in attribute site reaches its storage (RFC 0070 WS3 /
/// RFC 0071 WS2) — the classification the tier-1 inline caches make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttrStorage {
    /// An indexed instance-dict hit (the tier-1
    /// `LoadAttrInstance`/`StoreAttrInstance` shapes).
    Indexed(u32),
    /// A `__slots__` member, read/written through the slot side table
    /// by name (the tier-1 `LoadAttrSlot`/`StoreAttrSlot` shapes).
    Slot,
    /// RFC 0071 WS2 — the constructor-pattern store: the key is not
    /// present yet, so the write is a single-probe insert-or-replace
    /// (the tier-1 `StoreAttrNewKey` shape). Store sites only.
    NewKey,
}

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
    /// How the access reaches its storage.
    storage: AttrStorage,
    /// Pins the class object so `type_id` (an address) can't be reused.
    _class: Rc<TypeObject>,
}

/// RFC 0069 WS1 — one burned-in method-site resolution: the class-
/// resolved plain Python function the `(slot, name)` probe found, plus
/// the guard fingerprint (class identity + attr-version) and its
/// `__code__` at compile time. `wpjit_call_method` re-validates all of
/// it per call and rejects (deopts) on any mismatch, so class mutation,
/// instance-dict shadowing, and `__code__` rebinding introduced after
/// compilation stay exact.
struct MethodEntry {
    func: Rc<PyFunction>,
    /// The function's `__code__` at compile time (rebindable).
    code: Rc<CodeObject>,
    /// The method name (for the shadow check and the span rebuild).
    name: String,
    /// Positional arity, `self` included.
    arg_count: u32,
    /// Arity minus trailing defaults, `self` included.
    min_args: u32,
    /// The burned-in result typing.
    ret: MethodRet,
    /// `rc_id` of the receiver's class at compile time.
    type_id: u64,
    /// The class's `attr_version` at compile time.
    ver: u32,
    /// Pins the class object so `type_id` (an address) can't be reused.
    _class: Rc<TypeObject>,
}

/// One slot per method token (parallel to `cf.method_sites`).
type MethodTable = Vec<MethodEntry>;

/// RFC 0069 WS2 — the snapshot of one burned-in math intrinsic:
/// `(global name, attr, function object)`. The entry check and the
/// per-stride poll re-resolve `name.attr` and require identity with
/// the snapshotted function (module dicts are mutable).
type MathTable = Vec<(String, String, Object)>;

/// A compiled frame plus the globals it burned in: `snapshot[i]` is the
/// object `guards[i].name` resolved to at compile time. Every entry
/// re-resolves each name against the entering frame's namespaces and
/// requires identity (`is_same`) with the snapshot (RFC 0058 WS4).
struct CompiledEntry {
    cf: StdRc<CompiledFrame>,
    guard_snapshot: StdRc<Vec<(String, Object)>>,
    callees: StdRc<CalleeTable>,
    /// RFC 0074 WS1 — the obj-global table: `obj_globals[token]` is
    /// the identity-guarded object `PushGlobalObj { token }` pins
    /// (parallel to the analyzer's first-probe token order).
    obj_globals: StdRc<Vec<Object>>,
    /// RFC 0065 WS5 — one guard per burned-in attribute site, in
    /// `site`-token order (parallel to `cf.attr_sites`).
    attr_guards: StdRc<Vec<AttrGuard>>,
    /// RFC 0069 WS1 — per-token method resolutions (parallel to
    /// `cf.method_sites`; may carry trailing entries whose probe
    /// tokens no surviving site uses).
    methods: StdRc<MethodTable>,
    /// RFC 0069 WS2 — per-guard math-intrinsic snapshots (parallel to
    /// `cf.math_guards`).
    math: StdRc<MathTable>,
    /// RFC 0067 WS1 — the per-token native-callee resolution (`None`
    /// per token whose callee isn't natively enterable), snapshotted
    /// at the current compile generation.
    native: Option<StdRc<NativeTable>>,
    /// RFC 0069 WS1 — the per-method-token native resolution (parallel
    /// to [`Self::methods`]), same generation discipline.
    method_native: Option<StdRc<NativeTable>>,
    /// RFC 0073 WS4 — the process-unique id of the compilation these
    /// artifacts came from (see [`Artifacts::compile_id`]). A parked
    /// native activation stores it so a later resume only reuses its
    /// raw buffers against the *exact* compilation that laid them out.
    compile_id: u64,
}

/// RFC 0067 WS1 — one *natively enterable* burned-in callee: its
/// compiled frame, its own guard snapshot / callee table (namespaces
/// for validation and for its nested calls), and the function object
/// whose `globals`/`builtins` the guards resolve against. Resolved
/// per compile generation from the tier cache; a `None` slot keeps
/// using the interpreter call path.
struct NativeCallee {
    cf: StdRc<CompiledFrame>,
    snap: StdRc<Vec<(String, Object)>>,
    callees: StdRc<CalleeTable>,
    /// RFC 0074 WS1 — the callee's own obj-global table.
    obj_globals: StdRc<Vec<Object>>,
    attr_guards: StdRc<Vec<AttrGuard>>,
    methods: StdRc<MethodTable>,
    math: StdRc<MathTable>,
    func: Rc<PyFunction>,
    code: Rc<CodeObject>,
    /// RFC 0071 WS2 — `Some(cls)` when this callee is a *class
    /// constructor*: `cf`/`func`/`code` describe the compiled
    /// `__init__` (method shape, the fresh instance as pin 0), and the
    /// call site's value is the allocated instance, not `__init__`'s
    /// `None`.
    ctor: Option<Rc<TypeObject>>,
}

/// One slot per callee-table token (parallel to [`CalleeTable`]).
type NativeTable = Vec<Option<NativeCallee>>;

/// RFC 0069 WS3b — everything a frameless interpreter→native call
/// needs, resolved once per compile generation and handed out as a
/// single `Rc` clone per call (the per-call lookup cost is what makes
/// or breaks a ~100ns call).
struct DirectEntry {
    art: Artifacts,
    native: Option<StdRc<NativeTable>>,
    method_native: Option<StdRc<NativeTable>>,
    /// `true` = receiver-in-slot-0 shape ([`native_method_callable`]);
    /// `false` = all-scalar parameters ([`native_callable`]).
    method_shape: bool,
}

/// Everything one successful compile produced (RFC 0069 — the pieces
/// outgrew a tuple): the native frame plus the guard snapshots and
/// resolution tables its entries validate against.
#[derive(Clone)]
struct Artifacts {
    cf: StdRc<CompiledFrame>,
    snap: StdRc<Vec<(String, Object)>>,
    callees: StdRc<CalleeTable>,
    /// RFC 0074 WS1 — `obj_globals[token]` is the snapshotted object
    /// behind each `PushGlobalObj` token (identity-guarded through
    /// the ordinary guard snapshot; the helper pins it on demand).
    obj_globals: StdRc<Vec<Object>>,
    attr_guards: StdRc<Vec<AttrGuard>>,
    methods: StdRc<MethodTable>,
    math: StdRc<MathTable>,
    /// RFC 0073 WS4 — process-unique compilation id. JIT caches are
    /// thread-local (per-thread `compile_gen` counters can collide
    /// across threads), but a parked activation's box travels with its
    /// generator — possibly to another thread — so buffer-layout
    /// identity needs a process-wide id.
    compile_id: u64,
}

/// RFC 0073 WS4 — source of [`Artifacts::compile_id`].
static NEXT_COMPILE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Per-`CodeObject` compilation state.
enum Tier {
    Cold,
    NotJitable,
    Compiled(Artifacts),
}

struct CacheEntry {
    counter: u32,
    tier: Tier,
    /// Failed OSR validations (RFC 0059 WS3b). Mid-loop entry re-checks
    /// guards + locals on every back edge while it keeps failing, so a
    /// chronically unenterable loop stops polling after a budget.
    osr_failures: u32,
    /// RFC 0073 WS1 — entry pcs whose compile attempt failed with a
    /// retriable [`JitVerdict::ProbeMiss`] (a receiver local unbound
    /// in the triggering activation). The same pc never re-attempts
    /// (it would observe the same frame state); other entries still
    /// do, with their own live values.
    probe_misses: Vec<u32>,
    /// Native side exits taken by this code's compiled frame. Healthy
    /// compiled code exits by *returning* (deopt is exceptional — a
    /// type-lane surprise or invalidated guard), so a frame that keeps
    /// deopting is paying marshal-in + native entry + frame
    /// materialization on every activation for nothing. Past
    /// [`DEOPT_BUDGET`] the code is retired to [`Tier::NotJitable`]
    /// (and its `jit_hint` set) exactly as if the analyzer had
    /// rejected it.
    deopts: u32,
    /// Framed native entries of this code (saturating), the
    /// denominator of the generic-call retirement ratio below.
    native_entries: u32,
    /// RFC 0076 WS7 follow-up — generic `wpjit_call_dyn` legs taken by
    /// this code's compiled activations: calls that fell through
    /// [`try_dyn_native`] into the full interpreter round-trip
    /// (activation shell + `guards_hold` re-validation per call). A
    /// frame *dominated* by these is a net loss against tier-1 — the
    /// wave-11 escaping-callee lane admits call-shaped frames whose
    /// callees aren't compiled, and each such call pays the
    /// native→interpreter transition the interpreter wouldn't. Past
    /// [`GENERIC_CALL_RETIRE_RATIO`] per entry (after
    /// [`GENERIC_RETIRE_MIN_ENTRIES`]) the code is retired exactly
    /// like the deopt budget does (measured on `deltablue`: the
    /// compiled kernel ran 25% *slower* than tier-1 before this
    /// backoff).
    generic_dyn_calls: u32,
    /// RFC 0067 WS1 — the resolved native-callee table, stamped with
    /// the compile generation it was resolved at. A later compile
    /// (which may flip a `None` slot to `Some`) invalidates it by
    /// bumping [`JitState::compile_gen`].
    native: Option<(u64, StdRc<NativeTable>)>,
    /// RFC 0069 WS1 — the resolved native *method* table (parallel to
    /// the compiled entry's method table), same generation stamp.
    method_native: Option<(u64, StdRc<NativeTable>)>,
    /// RFC 0069 WS3b — the memoized frameless-direct-call bundle
    /// (artifacts + resolved tables + entry shape), same generation
    /// stamp; `Some((g, None))` memoizes *ineligibility* so a hot
    /// never-eligible callee costs one lookup, not a shape re-check.
    direct: Option<(u64, Option<StdRc<DirectEntry>>)>,
    /// Keeps the code object alive so its address can't be reused while
    /// this entry (and any compiled pointer keyed by it) is live. Also
    /// read by [`evict_dead_entries`]: strong_count == 1 means the JIT
    /// is the sole owner and the entry is evictable.
    code: Rc<CodeObject>,
}

/// Give up on OSR for a code object after this many failed validations.
const OSR_FAILURE_BUDGET: u32 = 64;

/// Retire a compiled code object after this many native side exits.
/// Sized like [`OSR_FAILURE_BUDGET`]: far above anything a legitimate
/// phase change produces (a guard invalidation deopts each active
/// frame *once*, then recompilation or the interpreter takes over),
/// far below the thousands of exits a shape-unstable hot function
/// (deltablue's method-heavy kernel) racks up when every native entry
/// ends in a materializing bail-out.
pub(crate) const DEOPT_BUDGET: u32 = 64;

/// RFC 0076 WS7 follow-up — generic-call backoff. A compiled frame
/// averaging this many generic interpreter round-trips
/// (`CacheEntry::generic_dyn_calls`) per framed native entry is
/// call-shaped, not loop-shaped: the native code is a thin driver
/// around interpreter calls, each paying activation-shell setup plus a
/// full `guards_hold` snapshot re-validation the interpreter wouldn't.
/// Retire it to tier-1.
pub(crate) const GENERIC_CALL_RETIRE_RATIO: u32 = 4;

/// Minimum framed entries before the generic-call ratio is judged —
/// avoids retiring on a cold first activation (e.g. a setup call that
/// makes a burst of generic calls once and then loops natively).
pub(crate) const GENERIC_RETIRE_MIN_ENTRIES: u32 = 64;

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
    /// RFC 0070 WS2 — `Yielded` exits from compiled generator bodies
    /// (healthy suspensions, excluded from the deopt budget).
    pub yields: u64,
    /// RFC 0071 WS5 — native generator *resume* entries (a subset of
    /// `native_entries`, sibling to `osr_entries`).
    pub gen_resumes: u64,
    /// RFC 0073 WS4 — `Yielded` exits that parked the whole native
    /// activation on the frame (no writeback, no stack rebuild).
    pub gen_parks: u64,
    /// RFC 0073 WS4 — native resumes served straight from a parked
    /// activation's buffers (a subset of `gen_resumes`).
    pub gen_parked_resumes: u64,
    /// RFC 0073 WS4 — parked activations written back into interpreter
    /// state (observer access, guard failure, JIT off, cross-thread
    /// resume).
    pub gen_materialized: u64,
    /// RFC 0076 WS7 follow-up — generic `wpjit_call_dyn` legs: calls
    /// from compiled code that `try_dyn_native` refused, each a full
    /// interpreter round-trip (activation shell + `guards_hold`).
    pub dyn_generic_calls: u64,
    /// Codes retired to `NotJitable` by the generic-call backoff
    /// ([`GENERIC_CALL_RETIRE_RATIO`]).
    pub generic_retires: u64,
}

/// RFC 0067 WS1 — call fast-path counters, kept in plain `Cell`s (one
/// increment is on the hottest path in a call-recursive program) and
/// merged into the [`JitStats`] report at render time.
#[derive(Default)]
struct NativeCallStats {
    /// Fast-path native-to-native call entries.
    calls: std::cell::Cell<u64>,
    /// RFC 0069 WS3b — frameless interpreter→native calls (the tier-1
    /// call fast path entered compiled code directly from the argument
    /// objects, skipping `Frame` construction). Counted separately
    /// from `JitStats::native_entries` (framed entries).
    direct_calls: std::cell::Cell<u64>,
    /// Eligible token, fast path refused (pending work, observers,
    /// argument-lane mismatch, callee guard failure, recursion limit).
    fallbacks: std::cell::Cell<u64>,
    /// RFC 0069 WS1 — `wpjit_call_method` invocations (any path).
    method_calls: std::cell::Cell<u64>,
    /// Method calls completed through the interpreter (callee not
    /// compiled / not enterable) with the caller's loop surviving.
    method_call_fallbacks: std::cell::Cell<u64>,
    /// Method guard misses (class version, instance-dict shadow, or
    /// `__code__` rebind) — each one is a Reject deopt at the call pc.
    method_guard_misses: std::cell::Cell<u64>,
    /// Nested native callee deopted or raised mid-call and was
    /// materialized into an interpreter frame.
    deopts: std::cell::Cell<u64>,
}

thread_local! {
    static NATIVE_CALL_STATS: NativeCallStats = NativeCallStats::default();
}

struct JitState {
    enabled: bool,
    threshold: u32,
    engine: Option<JitEngine>,
    cache: HashMap<*const CodeObject, CacheEntry>,
    stats: JitStats,
    /// RFC 0067 WS1 — bumped on every successful compile; stale
    /// native-callee tables (stamped with an older generation) are
    /// re-resolved so a newly compiled callee graduates from the
    /// interpreter call path to the native one.
    compile_gen: u64,
}

impl JitState {
    fn new() -> JitState {
        // RFC 0067 WS3 — the tier-2 JIT is on by default; `WEAVEPY_JIT=0`
        // (or `off`, or an empty value) restores the pure interpreter.
        let enabled = match std::env::var("WEAVEPY_JIT") {
            Ok(v) => v != "0" && !v.eq_ignore_ascii_case("off") && !v.is_empty(),
            Err(_) => true,
        };
        // RFC 0076 WS11 — tier-2 native code assumes the GIL's
        // single-writer discipline (unsynchronized inline-cache and
        // guard-table reads); the free-threaded mode pins execution
        // to tiers 0/1 for the whole run, even if an extension later
        // re-enables the GIL.
        let enabled = enabled && !crate::gil::free_threading_requested();
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
        // RFC 0076 WS6 — the closure-cell access helpers.
        weavepy_jit::register_cell_helpers(wpjit_cell_get, wpjit_cell_set);
        // RFC 0065 WS5 — the length/append and attribute lanes.
        weavepy_jit::register_list_extra_helpers(wpjit_list_len, wpjit_list_append);
        // RFC 0071 WS4 — the list-loop step helper.
        weavepy_jit::register_list_next_helper(wpjit_list_next);
        // RFC 0071 WS4 — the opaque-iterator capture/step and the
        // list-construction helpers.
        weavepy_jit::register_iter_helpers(
            wpjit_get_iter,
            wpjit_iter_next,
            wpjit_build_list,
            wpjit_build_tuple,
            wpjit_list_repeat,
            wpjit_list_slice,
        );
        // RFC 0071 WS6 — the string/bytes read helpers.
        weavepy_jit::register_str_helpers(
            wpjit_str_eq,
            wpjit_str_len,
            wpjit_bytes_len,
            wpjit_bytes_get,
        );
        weavepy_jit::register_attr_helpers(wpjit_attr_get, wpjit_attr_set);
        // RFC 0073 WS2 — the dict-lane helpers.
        weavepy_jit::register_dict_helpers(
            wpjit_dict_get,
            wpjit_dict_set,
            wpjit_dict_contains,
            wpjit_dict_len,
        );
        weavepy_jit::register_build_map_helper(wpjit_build_map);
        weavepy_jit::register_const_str_helper(wpjit_const_str);
        weavepy_jit::register_dict_iter_helper(wpjit_dict_iter_new);
        // RFC 0073 WS3 — the string write lanes.
        weavepy_jit::register_str_write_helpers(
            wpjit_str_concat,
            wpjit_str_get,
            wpjit_build_string,
        );
        // RFC 0067 WS2 — the eval-breaker poll for native loop headers.
        weavepy_jit::register_poll_helper(wpjit_poll);
        // RFC 0069 WS1 — the guarded method-call lane.
        weavepy_jit::register_call_method_helper(wpjit_call_method);
        // RFC 0073 WS3 — the native `str`-method lane.
        weavepy_jit::register_str_method_helper(wpjit_str_method);
        // RFC 0069 WS2 — the libm sin/cos intrinsics and the Python-
        // semantics float floor-div / mod.
        weavepy_jit::register_math_helpers(
            wpjit_math_sin,
            wpjit_math_cos,
            wpjit_float_floordiv,
            wpjit_float_mod,
        );
        // RFC 0074 — the frame-coverage lanes: obj globals, the
        // opaque-call lane, dynamic attributes, generic/pair
        // iteration, str %-format and slice.
        weavepy_jit::register_global_obj_helper(wpjit_global_obj);
        weavepy_jit::register_call_dyn_helper(wpjit_call_dyn);
        weavepy_jit::register_dyn_attr_helpers(wpjit_dyn_attr_get, wpjit_dyn_attr_set);
        // RFC 0076 WS8 — object-lane truthiness, generic membership,
        // and set literals.
        weavepy_jit::register_truth_helper(wpjit_truth);
        weavepy_jit::register_contains_dyn_helper(wpjit_contains_dyn);
        weavepy_jit::register_build_set_helper(wpjit_build_set);
        weavepy_jit::register_iter_new_helper(wpjit_iter_new);
        weavepy_jit::register_iter_next_pair_helper(wpjit_iter_next_pair);
        weavepy_jit::register_str_format_helpers(wpjit_str_mod, wpjit_str_slice);
        JitState {
            enabled,
            threshold,
            engine: None,
            cache: HashMap::new(),
            stats: JitStats::default(),
            compile_gen: 0,
        }
    }

    /// Bump the hot counter for `code` and, once it crosses the
    /// threshold, attempt compilation with the embedder probes in
    /// `probes` (see [`VmProbes`]). Returns the compiled frame + guard
    /// snapshots + resolution tables when one is available.
    fn get_compiled(
        &mut self,
        code: &Rc<CodeObject>,
        entry_pc: u32,
        probes: &mut VmProbes<'_>,
    ) -> Option<CompiledEntry> {
        let key = Rc::as_ptr(code).cast::<CodeObject>();
        {
            let entry = self.cache.entry(key).or_insert_with(|| CacheEntry {
                counter: 0,
                tier: Tier::Cold,
                osr_failures: 0,
                deopts: 0,
                native_entries: 0,
                generic_dyn_calls: 0,
                probe_misses: Vec::new(),
                native: None,
                method_native: None,
                direct: None,
                code: code.clone(),
            });
            match &entry.tier {
                Tier::Compiled(a) => {
                    let out = CompiledEntry {
                        cf: a.cf.clone(),
                        guard_snapshot: a.snap.clone(),
                        callees: a.callees.clone(),
                        obj_globals: a.obj_globals.clone(),
                        attr_guards: a.attr_guards.clone(),
                        methods: a.methods.clone(),
                        math: a.math.clone(),
                        native: None,
                        method_native: None,
                        compile_id: a.compile_id,
                    };
                    let native = self.native_table_for(key);
                    let method_native = self.method_native_table_for(key);
                    return Some(CompiledEntry {
                        native,
                        method_native,
                        ..out
                    });
                }
                Tier::NotJitable => return None,
                Tier::Cold => {
                    entry.counter += 1;
                    if entry.counter < self.threshold {
                        return None;
                    }
                    // RFC 0073 WS1 — a probe-miss rejection is
                    // *environmental* (a receiver local was unbound in
                    // the activation that triggered the compile), so
                    // it never retires the code object; but retrying
                    // from the same entry pc would observe the same
                    // frame state and fail identically, so each pc
                    // pays for the analysis at most once. A different
                    // entry (a later loop's OSR, a fresh call) re-
                    // attempts with its own live values.
                    if entry.probe_misses.contains(&entry_pc) {
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
        let VmProbes {
            resolve_obj,
            ret_lane_of,
            list,
            dict,
            attr,
            attr_guard_of,
            method,
            math_attr,
            param,
            class_ctor,
            ctor_field,
            cell,
            obj_live,
        } = probes;
        // RFC 0071 WS1 — an already-compiled callee's *actual* return
        // lane, from the code cache. The static re-analysis in
        // `callee_ret_info` runs probe-less, so a body whose typing
        // needs live values (object-lane parameters, attribute reads)
        // fails it even though the callee compiled fine from its own
        // activations. The lane is still just a prediction — the call
        // helpers re-check the actual result at runtime.
        let cache_ref = &self.cache;
        let compiled_ret = |fcode: &Rc<CodeObject>| -> Option<JitType> {
            let k = Rc::as_ptr(fcode).cast::<CodeObject>();
            match &cache_ref.get(&k)?.tier {
                Tier::Compiled(a) => a.cf.ret_lane.filter(|t| marshalable_lane_ty(*t)),
                _ => None,
            }
        };
        // RFC 0069 WS3 — one analysis attempt. The token tables
        // (callees, methods) are built fresh per attempt because the
        // analyzer's token sequence restarts with it. `seed_params`
        // gates the parameter-lane probe: the first attempt runs
        // unseeded (identical to the pre-seeding behavior); only a
        // `TypeUnknown` failure triggers a seeded retry, so shapes the
        // fixpoint can type on its own never pick up extra entry
        // guards or seed-vs-assignment conflicts.
        // RFC 0074 WS1 — `resolve_obj` is shared between `classify` and
        // the obj-global probe (sibling `&mut` closures alive across
        // the same `compile_frame`), so it rides a `RefCell`; every
        // call site's borrow is transient.
        let resolve_cell: std::cell::RefCell<&mut dyn FnMut(&str) -> Option<Object>> =
            std::cell::RefCell::new(&mut **resolve_obj);
        let mut run = |seed_params: bool| -> (
            Result<weavepy_jit::CompiledFrame, weavepy_jit::JitVerdict>,
            CalleeTable,
            MethodTable,
            Vec<String>,
        ) {
            // RFC 0059 WS3 — classify each LOAD_GLOBAL. A plain Python
            // function becomes a `PyFunc` callee: it gets a token in the
            // callee table, and (for non-self callees) must have an
            // analyzable scalar return lane so the caller can type the call
            // result. The analyzer resolves each name exactly once, so the
            // token sequence here matches the compiled code's.
            // A `RefCell` because the keyword-slot probe below reads
            // the table while `classify` (a sibling `&mut` closure)
            // grows it — both live across the same `compile_frame`.
            let callees: std::cell::RefCell<CalleeTable> = std::cell::RefCell::new(Vec::new());
            let mut classify = |name: &str| {
                let obj = (resolve_cell.borrow_mut())(name);
                if let Some(Object::Function(f)) = obj.as_ref() {
                    let fcode = f.code.borrow().clone();
                    if !py_callee_ok(&fcode) {
                        return ResolvedGlobal::Opaque;
                    }
                    let is_self = Rc::ptr_eq(&fcode, code);
                    let ret = if is_self {
                        None
                    } else {
                        compiled_ret(&fcode).or_else(|| ret_lane_of(f, &fcode))
                    };
                    if !is_self && ret.is_none() {
                        return ResolvedGlobal::Opaque;
                    }
                    let mut callees = callees.borrow_mut();
                    let token = callees.len() as u32;
                    callees.push((obj.clone().expect("checked Some above"), fcode.clone()));
                    // RFC 0069 WS3 — trailing defaults widen the admitted
                    // call-site arity range; the interpreter call binds
                    // them (the native fast path requires full arity).
                    let min_args = fcode
                        .arg_count
                        .saturating_sub(u32::try_from(f.defaults.len()).unwrap_or(u32::MAX));
                    return ResolvedGlobal::PyFunc {
                        token,
                        arg_count: fcode.arg_count,
                        min_args,
                        is_self,
                        ret,
                        ctor: false,
                    };
                }
                // RFC 0071 WS2 — a plain user class with the default
                // construction pipeline becomes a callable constructor:
                // the call itself runs through the interpreter
                // (`instantiate` + `__init__`), but the site types
                // natively as an object-lane producer with `__init__`'s
                // arity. The class object is the callee-table guard
                // subject; the `__init__` code is its snapshot.
                if let Some(Object::Type(t)) = obj.as_ref() {
                    if let Some(cc) = class_ctor(t) {
                        let mut callees = callees.borrow_mut();
                        let token = callees.len() as u32;
                        callees.push((obj.clone().expect("checked Some above"), cc.init_code));
                        return ResolvedGlobal::PyFunc {
                            token,
                            arg_count: cc.arg_count,
                            min_args: cc.min_args,
                            is_self: false,
                            ret: Some(JitType::Obj),
                            ctor: true,
                        };
                    }
                }
                // RFC 0069 WS2 — a module named `math`: the intrinsic
                // probe decides per attribute whether the pair is
                // burnable; a mis-shaped module simply fails every probe.
                if let Some(Object::Module(m)) = obj.as_ref() {
                    if m.name == "math" {
                        return ResolvedGlobal::MathModule;
                    }
                }
                classify_global(obj.as_ref())
            };
            // RFC 0069 WS1 — the method probe with token assignment: the
            // first resolution of a `(slot, path, name)` triple appends
            // to the table; repeated probes (the analyzer probes during
            // both inference and emission) reuse the token, keeping the
            // table parallel to the compiled `method_sites`.
            let mut methods: MethodTable = Vec::new();
            let mut method_tokens: HashMap<(u32, Vec<String>, String), u32> = HashMap::new();
            let mut probe_method =
                |slot: u32, path: &[String], name: &str| -> Option<MethodResolution> {
                    if let Some(&token) = method_tokens.get(&(slot, path.to_vec(), name.to_owned()))
                    {
                        let e = &methods[token as usize];
                        return Some(MethodResolution {
                            token,
                            arg_count: e.arg_count,
                            min_args: e.min_args,
                            ret: e.ret,
                        });
                    }
                    let e = method(slot, path, name)?;
                    let token = methods.len() as u32;
                    method_tokens.insert((slot, path.to_vec(), name.to_owned()), token);
                    let res = MethodResolution {
                        token,
                        arg_count: e.arg_count,
                        min_args: e.min_args,
                        ret: e.ret,
                    };
                    methods.push(e);
                    Some(res)
                };
            // RFC 0069 WS2 — the math probe reports eligibility only; the
            // guard snapshot below re-resolves each burned pair.
            let mut probe_math = |name: &str, attr_name: &str| math_attr(name, attr_name).is_some();
            // RFC 0069 WS3 — parameter-lane seeding, active on retry only.
            let mut probe_param = |slot: u32| if seed_params { param(slot) } else { None };
            // RFC 0073 WS5 — keyword-name → parameter-slot resolution
            // against the callee table `classify` built. Unknown
            // names, positional-only parameters, and constructor
            // callees refuse (those keyword sites stay interpreted);
            // the per-call code-identity guard keeps a rebound
            // `__code__` with renamed parameters from ever reaching
            // the burned permutation.
            let mut probe_kw_slot = |token: u32, name: &str| -> Option<u32> {
                let tbl = callees.borrow();
                let (obj, fcode) = tbl.get(token as usize)?;
                if !matches!(obj, Object::Function(_)) {
                    return None;
                }
                let total = fcode.arg_count as usize;
                let slot = fcode
                    .varnames
                    .get(..total)?
                    .iter()
                    .position(|v| v.as_str() == name)?;
                if slot < fcode.posonly_count as usize {
                    return None;
                }
                u32::try_from(slot).ok()
            };
            // RFC 0074 WS1 — the obj-global token table: names in
            // first-probe order, memoized per name (the analyzer
            // probes during both passes), graded once per name. The
            // object table snapshots from these names on success.
            let obj_names: std::cell::RefCell<Vec<(String, JitType)>> =
                std::cell::RefCell::new(Vec::new());
            let mut probe_obj_global = |name: &str| -> Option<(u32, JitType)> {
                {
                    let tbl = obj_names.borrow();
                    if let Some(i) = tbl.iter().position(|(n, _)| n == name) {
                        return Some((i as u32, tbl[i].1));
                    }
                }
                let obj = (resolve_cell.borrow_mut())(name)?;
                let lane = grade_obj_global(&obj);
                let mut tbl = obj_names.borrow_mut();
                let token = tbl.len() as u32;
                tbl.push((name.to_owned(), lane));
                Some((token, lane))
            };
            let mut path_arena = weavepy_jit::PathArena::default();
            let mut jit_probes = Probes {
                list: &mut **list,
                dict: &mut **dict,
                attr: &mut **attr,
                method: &mut probe_method,
                math: &mut probe_math,
                ctor_field: &mut **ctor_field,
                param: &mut probe_param,
                kw_slot: &mut probe_kw_slot,
                obj_global: &mut probe_obj_global,
                cell: &mut **cell,
                obj: &mut **obj_live,
                paths: &mut path_arena,
            };
            let r = engine.compile_frame(code, &mut classify, &mut jit_probes);
            let obj_names = obj_names.into_inner().into_iter().map(|(n, _)| n).collect();
            (r, callees.into_inner(), methods, obj_names)
        };
        let (res, callees, methods, obj_names) = {
            let first = run(false);
            if matches!(first.0, Err(weavepy_jit::JitVerdict::TypeUnknown)) {
                run(true)
            } else {
                first
            }
        };
        let (tier, out) = match res {
            Ok(cf) => {
                self.stats.frames_compiled += 1;
                // Snapshot the exact objects the guards must keep
                // resolving to. Every guarded name resolved during
                // analysis, so it resolves here too (nothing ran since
                // — same thread, GIL held).
                let snap: Vec<(String, Object)> = cf
                    .global_guards
                    .iter()
                    .filter_map(|g| {
                        (resolve_cell.borrow_mut())(&g.name).map(|o| (g.name.clone(), o))
                    })
                    .collect();
                // RFC 0074 WS1 — the obj-global object table, in token
                // order. Every probed name resolved during analysis,
                // so it resolves here too; each is identity-guarded
                // through the ordinary guard snapshot above.
                let obj_globals: Vec<Object> = obj_names
                    .iter()
                    .map(|n| (resolve_cell.borrow_mut())(n).unwrap_or(Object::None))
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
                // RFC 0069 WS2 — snapshot the resolved function per
                // math guard. Every pair probed during analysis, so it
                // resolves here too.
                let mut math_tbl: MathTable = Vec::with_capacity(cf.math_guards.len());
                for g in &cf.math_guards {
                    match math_attr(&g.name, &g.attr) {
                        Some(f) => math_tbl.push((g.name.clone(), g.attr.clone(), f)),
                        None => break,
                    }
                }
                if snap.len() != cf.global_guards.len()
                    || attr_guards.len() != cf.attr_sites.len()
                    || math_tbl.len() != cf.math_guards.len()
                    || methods.len() < cf.method_sites.len()
                {
                    self.stats.frames_notjitable += 1;
                    (Tier::NotJitable, None)
                } else {
                    if std::env::var_os("WEAVEPY_JIT_TRACE").is_some() {
                        eprintln!("jit compile {:?} (entry pc {entry_pc})", code.name);
                    }
                    let artifacts = Artifacts {
                        cf: StdRc::new(cf),
                        snap: StdRc::new(snap),
                        callees: StdRc::new(callees),
                        obj_globals: StdRc::new(obj_globals),
                        attr_guards: StdRc::new(attr_guards),
                        methods: StdRc::new(methods),
                        math: StdRc::new(math_tbl),
                        compile_id: NEXT_COMPILE_ID
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    };
                    // RFC 0067 WS1 — a fresh compile can flip a
                    // `None` native-callee slot in *other* frames'
                    // tables to `Some`; the generation bump makes
                    // every stale table re-resolve on next entry.
                    self.compile_gen += 1;
                    let entry = CompiledEntry {
                        cf: artifacts.cf.clone(),
                        guard_snapshot: artifacts.snap.clone(),
                        callees: artifacts.callees.clone(),
                        obj_globals: artifacts.obj_globals.clone(),
                        attr_guards: artifacts.attr_guards.clone(),
                        methods: artifacts.methods.clone(),
                        math: artifacts.math.clone(),
                        native: None,
                        method_native: None,
                        compile_id: artifacts.compile_id,
                    };
                    (Tier::Compiled(artifacts), Some(entry))
                }
            }
            Err(v) => {
                if std::env::var_os("WEAVEPY_JIT_TRACE").is_some() {
                    eprintln!("jit reject {:?} (entry pc {entry_pc}): {v:?}", code.name);
                }
                if matches!(v, weavepy_jit::JitVerdict::ProbeMiss(_)) {
                    // RFC 0073 WS1 — retriable: stay Cold, charge this
                    // entry pc so only *other* entries re-attempt.
                    if let Some(entry) = self.cache.get_mut(&key) {
                        if !entry.probe_misses.contains(&entry_pc) {
                            entry.probe_misses.push(entry_pc);
                        }
                    }
                    return None;
                }
                self.stats.frames_notjitable += 1;
                (Tier::NotJitable, None)
            }
        };
        if matches!(tier, Tier::NotJitable) {
            // RFC 0067 — denormalize the rejection onto the code
            // object so every later activation skips tier-up on one
            // relaxed load (see `JitHint`).
            code.jit_hint.mark_not_jitable();
        }
        if let Some(entry) = self.cache.get_mut(&key) {
            entry.tier = tier;
        }
        out.map(|entry| {
            let native = self.native_table_for(key);
            let method_native = self.method_native_table_for(key);
            CompiledEntry {
                native,
                method_native,
                ..entry
            }
        })
    }

    /// RFC 0073 WS4 — the compiled entry for a *parked* generator
    /// resume: a pure cache read (no counters, no probes, no compile
    /// attempt) that must hand back the exact compilation the parked
    /// buffers were laid out for, identified by `compile_id`. `None`
    /// (evicted, recompiled, or a different thread's cache) sends the
    /// caller down the materialize-and-interpret path.
    fn parked_entry(&mut self, key: *const CodeObject, compile_id: u64) -> Option<CompiledEntry> {
        let art = match &self.cache.get(&key)?.tier {
            Tier::Compiled(a) if a.compile_id == compile_id => a.clone(),
            _ => return None,
        };
        let native = self.native_table_for(key);
        let method_native = self.method_native_table_for(key);
        Some(CompiledEntry {
            cf: art.cf,
            guard_snapshot: art.snap,
            callees: art.callees,
            obj_globals: art.obj_globals,
            attr_guards: art.attr_guards,
            methods: art.methods,
            math: art.math,
            native,
            method_native,
            compile_id: art.compile_id,
        })
    }

    /// RFC 0067 WS1 — the resolved native-callee table for a compiled
    /// code object, re-resolving when the compile generation moved.
    /// `None` when the code isn't compiled (or has no callees worth a
    /// table — an all-`None` table is still cached to keep the lookup
    /// O(1)).
    fn native_table_for(&mut self, key: *const CodeObject) -> Option<StdRc<NativeTable>> {
        let gen = self.compile_gen;
        let callees = {
            let entry = self.cache.get(&key)?;
            if let Some((g, tbl)) = &entry.native {
                if *g == gen {
                    return Some(tbl.clone());
                }
            }
            let Tier::Compiled(a) = &entry.tier else {
                return None;
            };
            a.callees.clone()
        };
        let table: NativeTable = callees
            .iter()
            .map(|(obj, fcode)| self.resolve_native_callee(obj, fcode))
            .collect();
        let tbl = StdRc::new(table);
        if let Some(entry) = self.cache.get_mut(&key) {
            entry.native = Some((gen, tbl.clone()));
        }
        Some(tbl)
    }

    /// RFC 0069 WS1 — like [`Self::native_table_for`] but for the
    /// method table: one slot per method token, `Some` when the
    /// resolved method's own body is compiled and shape-eligible for
    /// a direct native entry (receiver passed as slot 0).
    fn method_native_table_for(&mut self, key: *const CodeObject) -> Option<StdRc<NativeTable>> {
        let gen = self.compile_gen;
        let methods = {
            let entry = self.cache.get(&key)?;
            if let Some((g, tbl)) = &entry.method_native {
                if *g == gen {
                    return Some(tbl.clone());
                }
            }
            let Tier::Compiled(a) = &entry.tier else {
                return None;
            };
            a.methods.clone()
        };
        let table: NativeTable = methods
            .iter()
            .map(|m| self.resolve_native_func(&m.func, &m.code, true))
            .collect();
        let tbl = StdRc::new(table);
        if let Some(entry) = self.cache.get_mut(&key) {
            entry.method_native = Some((gen, tbl.clone()));
        }
        Some(tbl)
    }

    /// RFC 0069 WS3b — the memoized frameless-direct-call bundle for a
    /// code object: artifacts, resolved native tables, and the entry
    /// shape, re-resolved when the compile generation moved. `None`
    /// when the code isn't compiled or neither entry shape is
    /// eligible (also memoized).
    fn direct_entry_for(&mut self, key: *const CodeObject) -> Option<StdRc<DirectEntry>> {
        let gen = self.compile_gen;
        let (art, code) = {
            let ce = self.cache.get(&key)?;
            if let Some((g, d)) = &ce.direct {
                if *g == gen {
                    return d.clone();
                }
            }
            let Tier::Compiled(a) = &ce.tier else {
                return None;
            };
            (a.clone(), ce.code.clone())
        };
        let method_shape = if native_callable(&art.cf, &code) {
            Some(false)
        } else if native_method_callable(&art.cf, &code) {
            Some(true)
        } else {
            None
        };
        let out = method_shape.map(|method_shape| {
            StdRc::new(DirectEntry {
                native: self.native_table_for(key),
                method_native: self.method_native_table_for(key),
                art,
                method_shape,
            })
        });
        if let Some(ce) = self.cache.get_mut(&key) {
            ce.direct = Some((gen, out.clone()));
        }
        out
    }

    /// Resolve one burned-in callee to its native entry, when its code
    /// is compiled and shape-eligible for a direct native call.
    fn resolve_native_callee(&self, obj: &Object, fcode: &Rc<CodeObject>) -> Option<NativeCallee> {
        match obj {
            Object::Function(pf) => self.resolve_native_func(pf, fcode, false),
            // RFC 0071 WS2 — a class-constructor callee resolves its
            // *`__init__`* body as a method-shaped native entry (the
            // fresh instance rides as pin 0). The memoised plan must
            // still be current and carry the snapshotted `__init__`
            // code; a stale plan (or one not yet rebuilt) simply takes
            // the interpreter path, and `guards_hold` — which re-probes
            // the full construction shape — remains the semantic guard.
            Object::Type(t) => {
                let init = {
                    let cached = t.instance_plan.borrow();
                    let (ver, plan) = cached.as_ref()?.clone();
                    if ver != t.attr_version.get() {
                        return None;
                    }
                    match plan.init_fn.as_ref() {
                        Some(Object::Function(f)) => f.clone(),
                        _ => return None,
                    }
                };
                if !Rc::ptr_eq(&init.code.borrow(), fcode) {
                    return None;
                }
                let nc = self.resolve_native_func(&init, fcode, true)?;
                Some(NativeCallee {
                    ctor: Some(t.clone()),
                    ..nc
                })
            }
            _ => None,
        }
    }

    /// The function-object form of [`Self::resolve_native_callee`]
    /// (method entries store the resolved `Rc<PyFunction>` directly).
    /// `method` selects the receiver-in-slot-0 eligibility shape.
    fn resolve_native_func(
        &self,
        pf: &Rc<PyFunction>,
        fcode: &Rc<CodeObject>,
        method: bool,
    ) -> Option<NativeCallee> {
        let ckey = Rc::as_ptr(fcode).cast::<CodeObject>();
        let entry = self.cache.get(&ckey)?;
        let Tier::Compiled(a) = &entry.tier else {
            return None;
        };
        let eligible = if method {
            native_method_callable(&a.cf, fcode)
        } else {
            native_callable(&a.cf, fcode)
        };
        if !eligible {
            return None;
        }
        Some(NativeCallee {
            cf: a.cf.clone(),
            snap: a.snap.clone(),
            callees: a.callees.clone(),
            obj_globals: a.obj_globals.clone(),
            attr_guards: a.attr_guards.clone(),
            methods: a.methods.clone(),
            math: a.math.clone(),
            func: pf.clone(),
            code: fcode.clone(),
            ctor: None,
        })
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
            deopts: 0,
            native_entries: 0,
            generic_dyn_calls: 0,
            probe_misses: Vec::new(),
            native: None,
            method_native: None,
            direct: None,
            code: code.clone(),
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

/// The embedder probes one compile consults (RFC 0059/0061/0065/0069),
/// bundled because the list outgrew a parameter row. Each borrows the
/// requesting frame and interpreter for the duration of one
/// [`JitState::get_compiled`] call.
struct VmProbes<'a> {
    /// A `LOAD_GLOBAL` name → its current resolution in the requesting
    /// frame's namespaces (classification and guard snapshots).
    resolve_obj: &'a mut dyn FnMut(&str) -> Option<Object>,
    /// A candidate Python callee's stable scalar return lane (RFC 0059
    /// WS3).
    ret_lane_of: &'a mut dyn FnMut(&Rc<PyFunction>, &Rc<CodeObject>) -> Option<JitType>,
    /// A subscripted local's observed element lane (RFC 0061 WS5).
    list: &'a mut dyn FnMut(u32) -> Option<JitType>,
    /// RFC 0073 WS2 — a subscripted local's observed dict key/value
    /// lanes.
    dict: &'a mut dyn FnMut(u32) -> Option<(JitType, JitType)>,
    /// An instance attribute's observed lane on the object reached by
    /// walking a path from a local (RFC 0065 WS5 / RFC 0071 WS3).
    attr: &'a mut dyn FnMut(u32, &[String], &str, bool) -> Option<JitType>,
    /// The post-compile guard fingerprint of one attribute site.
    attr_guard_of: &'a mut dyn FnMut(&AttrSiteMeta) -> Option<AttrGuard>,
    /// RFC 0069 WS1 — the class-resolved method on the instance
    /// reached by walking a path from a local, when the shape is
    /// eligible (RFC 0071 WS3).
    method: &'a mut dyn FnMut(u32, &[String], &str) -> Option<MethodEntry>,
    /// RFC 0069 WS2 — the canonical intrinsic function `name.attr`
    /// currently resolves to, when the pair is burnable.
    math_attr: &'a mut dyn FnMut(&str, &str) -> Option<Object>,
    /// RFC 0069 WS3 — the observed scalar lane of a parameter slot in
    /// the requesting activation (used only on the seeded retry after
    /// an unseeded analysis fails with `TypeUnknown`).
    param: &'a mut dyn FnMut(u32) -> Option<JitType>,
    /// RFC 0071 WS2 — a `LOAD_GLOBAL` class's constructor shape, when
    /// the class constructs through the default pipeline with a
    /// plain-Python `__init__` (the call site then types as an
    /// object-lane producer with `__init__`'s arity).
    class_ctor: &'a mut dyn FnMut(&Rc<TypeObject>) -> Option<ClassCtorEntry>,
    /// RFC 0073 WS1 — `(class global name, attr)` → the field's index
    /// in that class's post-construction canonical shape plus its
    /// value source, for attribute sites whose receiver local has no
    /// live value to probe (it is bound from the class's burned-in
    /// constructor call).
    ctor_field: &'a mut dyn FnMut(&str, &str) -> Option<(u32, CtorFieldSrc)>,
    /// RFC 0076 WS6 — the observed lane of closure cell `idx`
    /// (`cellvars` ++ `freevars` layout) in the requesting activation:
    /// a scalar lane, or the nullable object lane for any other bound
    /// payload. `None` = unbound or no live activation.
    cell: &'a mut dyn FnMut(u32) -> Option<JitType>,
    /// RFC 0076 WS8 — whether local `slot` holds *some* live value in
    /// the requesting activation (no grading), for the analyzer's
    /// generic-attribute probe-miss fallback.
    obj_live: &'a mut dyn FnMut(u32) -> bool,
}

/// RFC 0071 WS2 — one constructible class's burned-in call shape: the
/// `__init__` code snapshot (the callee-table guard object) and the
/// caller-visible arity derived from it (`self` excluded, trailing
/// defaults widening the admitted range).
struct ClassCtorEntry {
    init_code: Rc<CodeObject>,
    arg_count: u32,
    min_args: u32,
    /// RFC 0073 WS1 — the class's post-construction canonical shape:
    /// the attribute names `__init__` stores on `self`, in insertion
    /// order, with each value's source (a caller positional argument
    /// or a constant lane). Non-empty only when `__init__` is the
    /// *pure store prologue* (straight-line `self.a = <param|const>`
    /// stores, then `return None`) — the only shape whose dict-key
    /// order is statically knowable.
    fields: Vec<(String, CtorFieldSrc)>,
}

/// RFC 0073 WS1 — scan an eligible `__init__` for the pure store
/// prologue and derive the canonical field list. Any instruction
/// outside the recognized alphabet (a value load, `self` load,
/// `STORE_ATTR`, and the trailing `return None`) yields `None`: the
/// class still types as a constructor, but attribute sites get no
/// shape fallback. Duplicate stores keep the *first* index (dict
/// insertion order) but the *last* source (the surviving value).
fn ctor_field_plan(icode: &CodeObject) -> Option<Vec<(String, CtorFieldSrc)>> {
    use weavepy_compiler::OpCode;
    let ins = &icode.instructions;
    let mut fields: Vec<(String, CtorFieldSrc)> = Vec::new();
    let mut i = 0usize;
    while i < ins.len() {
        match ins[i].op {
            OpCode::Nop | OpCode::Resume => {
                i += 1;
            }
            // `return None` tail: LOAD_CONST None; RETURN_VALUE.
            OpCode::LoadConst
                if matches!(
                    icode.constants.get(ins[i].arg as usize),
                    Some(weavepy_compiler::Constant::None)
                ) && ins.get(i + 1).is_some_and(|n| n.op == OpCode::ReturnValue) =>
            {
                // Everything after the return is unreachable filler.
                return Some(fields);
            }
            // `self.<name> = <param or const>`: value load, self load,
            // STORE_ATTR.
            OpCode::LoadFast | OpCode::LoadConst => {
                let src = match ins[i].op {
                    OpCode::LoadFast => {
                        let slot = ins[i].arg;
                        if slot == 0 {
                            return None; // `self` as a *value* — aliasing.
                        }
                        if slot >= icode.arg_count {
                            return None; // not a parameter.
                        }
                        CtorFieldSrc::Param(slot - 1)
                    }
                    _ => {
                        let lane = match icode.constants.get(ins[i].arg as usize)? {
                            weavepy_compiler::Constant::None => JitType::Obj,
                            weavepy_compiler::Constant::Bool(_) => JitType::Bool,
                            weavepy_compiler::Constant::Int(_) => JitType::Int,
                            weavepy_compiler::Constant::Float(_) => JitType::Float,
                            weavepy_compiler::Constant::Str(_) => JitType::Str,
                            _ => return None,
                        };
                        CtorFieldSrc::Lane(lane)
                    }
                };
                let recv = ins.get(i + 1)?;
                let store = ins.get(i + 2)?;
                if recv.op != OpCode::LoadFast || recv.arg != 0 || store.op != OpCode::StoreAttr {
                    return None;
                }
                let name = icode.names.get(store.arg as usize)?.clone();
                match fields.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) => slot.1 = src,
                    None => fields.push((name, src)),
                }
                i += 3;
            }
            _ => return None,
        }
    }
    // Fell off the end without the `return None` tail — malformed.
    None
}

/// RFC 0071 WS2 — probe whether `cls` is a class whose call the JIT
/// can type as "construct an instance, run the plain-Python
/// `__init__`, return the object lane". The *call itself* always runs
/// through the interpreter (`Interpreter::call` on the class object),
/// so this predicate — like [`py_callee_ok`] — only protects the
/// burned arity/lane assumptions:
///
/// - the metaclass is exactly `type` (a custom metaclass `__call__`
///   can return anything with any signature);
/// - the default construction pipeline applies: no user `__new__`, no
///   native payload, not abstract, not an exception class;
/// - `__init__` resolves to a plain-Python function with a
///   [`py_callee_ok`] signature (its arity, minus `self`, becomes the
///   call site's).
///
/// The same probe re-runs as the guard predicate (memoised via
/// [`TypeObject::instance_plan`]'s `attr_version` key, so revalidation
/// is a version check in the common case).
fn probe_class_ctor(interp: &super::Interpreter, cls: &Rc<TypeObject>) -> Option<ClassCtorEntry> {
    let bt = crate::builtin_types::builtin_types();
    // `type` subclasses (metaclasses) construct *classes* through the
    // three-argument form, never plain instances.
    if cls.flags.is_builtin
        || cls.is_subclass_of(&bt.type_)
        || !Rc::ptr_eq(&cls.metaclass_or_type(), &bt.type_)
    {
        return None;
    }
    let plan = interp.instance_plan(cls);
    if plan.abstract_error.is_some()
        || plan.user_new.is_some()
        || !plan.is_object_new
        || !matches!(plan.native, crate::types::NativeKind::Plain)
        || plan.init_from_object
        || plan.seeds_exception_args
    {
        return None;
    }
    let Some(Object::Function(init)) = plan.init_fn.as_ref() else {
        return None;
    };
    let icode = init.code.borrow().clone();
    if !py_callee_ok(&icode) || icode.arg_count == 0 {
        return None;
    }
    let arg_count = icode.arg_count - 1;
    let min_args = arg_count.saturating_sub(u32::try_from(init.defaults.len()).unwrap_or(u32::MAX));
    // RFC 0073 WS1 — the canonical shape additionally requires the
    // instance to actually keep a dict for the indexed fingerprint.
    let fields = if cls.forbids_dict || cls.declares_slots.get() {
        Vec::new()
    } else {
        ctor_field_plan(&icode).unwrap_or_default()
    };
    Some(ClassCtorEntry {
        init_code: icode,
        arg_count,
        min_args,
        fields,
    })
}

/// RFC 0073 WS1 — the constructor-shape fallback probe: resolve the
/// class global by name in the requesting frame's namespaces, derive
/// its canonical field list, and look up the named field's index and
/// value source. Every result is a prediction — the guard snapshot
/// re-resolves and the runtime helpers re-validate per access.
fn probe_ctor_field(
    interp: &super::Interpreter,
    frame: &super::Frame,
    cls_name: &str,
    attr: &str,
) -> Option<(u32, CtorFieldSrc)> {
    let Object::Type(t) = resolve_plain_global(interp, frame, cls_name)? else {
        return None;
    };
    let cc = probe_class_ctor(interp, &t)?;
    let idx = cc.fields.iter().position(|(n, _)| n == attr)?;
    Some((idx as u32, cc.fields[idx].1))
}

/// `true` for the three lanes a native call can marshal by value.
fn scalar_lane_ty(t: JitType) -> bool {
    matches!(t, JitType::Int | JitType::Float | JitType::Bool)
}

/// RFC 0071 WS1 — `true` for the lanes a native call can marshal:
/// scalars by value, plus the nullable object lane (the argument
/// travels as an `ObjPin` entry that the callee re-pins in its own
/// table).
fn marshalable_lane_ty(t: JitType) -> bool {
    scalar_lane_ty(t) || t == JitType::Obj
}

/// RFC 0067 WS1 — whether a *compiled* callee can be entered directly
/// from native code:
///
/// - every parameter slot is a marshalable lane (scalar, or — RFC 0071
///   WS1 — the object lane, re-pinned on the callee side), so the
///   marshaled `(bits, tag)` arguments map 1:1 onto the leading locals
///   and a deopt write-back can't misinterpret an untouched argument;
/// - every live-in slot is a parameter (the analyzer admits only
///   exact-arity call sites, so exactly these slots are definitely
///   assigned at entry);
/// - non-parameter pin lanes are allowed (RFC 0071 WS1): they are
///   defined by native code (attribute loads, calls) before any use,
///   exactly as in an interpreter-frame entry;
/// - no cells (the analyzer rejects cell opcodes, so this is
///   defensive).
fn native_callable(cf: &CompiledFrame, code: &CodeObject) -> bool {
    // RFC 0070 WS2 — a compiled *generator* body is OSR-only: a call
    // must create the generator object, never run the body.
    if code.is_generator {
        return false;
    }
    let argc = code.arg_count as usize;
    for j in 0..argc {
        match cf.local_types.get(j).copied().flatten() {
            Some(t) if marshalable_lane_ty(t) => {}
            _ => return false,
        }
    }
    if !cf.livein.iter().all(|&s| (s as usize) < argc) {
        return false;
    }
    code.cellvars.is_empty() && code.freevars.is_empty()
}

/// RFC 0069 WS1 — the method-body variant of [`native_callable`]: the
/// receiver occupies slot 0 as an object-pin lane (the caller seeds it
/// as pin 0 of the callee's pin table), and every *other* parameter is
/// a marshalable lane (RFC 0071 WS1 admits object-lane arguments and
/// non-parameter pin lanes, which native code defines before use).
fn native_method_callable(cf: &CompiledFrame, code: &CodeObject) -> bool {
    // RFC 0070 WS2 — generator bodies are OSR-only (see
    // [`native_callable`]).
    if code.is_generator {
        return false;
    }
    let argc = code.arg_count as usize;
    if argc == 0 {
        return false;
    }
    if cf.local_types.first().copied().flatten() != Some(JitType::Obj) {
        return false;
    }
    for j in 1..argc {
        match cf.local_types.get(j).copied().flatten() {
            Some(t) if marshalable_lane_ty(t) => {}
            _ => return false,
        }
    }
    if !cf.livein.iter().all(|&s| (s as usize) < argc) {
        return false;
    }
    code.cellvars.is_empty() && code.freevars.is_empty()
}

thread_local! {
    static JIT: RefCell<JitState> = RefCell::new(JitState::new());

    /// Memoized callee return typing (RFC 0059 WS3 / RFC 0069 WS1):
    /// `(scalar lane, provably-returns-None)`, keyed by code object
    /// identity. The `Rc<CodeObject>` pins the address against reuse.
    /// Both are *predictions* — the call helpers re-check the actual
    /// result at runtime — so staleness (e.g. the callee's own globals
    /// changing what its analysis would say) costs a deopt, never
    /// correctness.
    static RET_LANE_CACHE: RefCell<
        HashMap<*const CodeObject, (Option<JitType>, bool, Rc<CodeObject>)>,
    > = RefCell::new(HashMap::new());
}

/// Infer a candidate callee's return typing — its stable scalar return
/// lane, plus whether it provably returns `None` from every site — by
/// running the tier-2 analyzer over its body, resolving names in the
/// *callee's* own namespaces. Nested Python callees are only recognized
/// when they are the callee itself (self-recursion, e.g. `fib`);
/// anything deeper stays opaque, bounding the recursion at depth one.
/// A body the analyzer rejects can still be recognized as a procedure
/// by the syntactic `return None` scan (RFC 0069 WS1) — that shape is
/// what method-heavy programs (deltablue) are made of.
fn callee_ret_info(
    interp: &super::Interpreter,
    f: &Rc<PyFunction>,
    fcode: &Rc<CodeObject>,
) -> (Option<JitType>, bool) {
    let key = Rc::as_ptr(fcode).cast::<CodeObject>();
    if let Some(hit) =
        RET_LANE_CACHE.with(|c| c.borrow().get(&key).map(|(lane, none, _)| (*lane, *none)))
    {
        return hit;
    }
    let resolve = |name: &str| resolve_plain_dicts(interp, &f.globals, &f.builtins, name);
    let mut classify = |name: &str| {
        let obj = resolve(name);
        if let Some(Object::Function(g)) = obj.as_ref() {
            let gcode = g.code.borrow().clone();
            if Rc::ptr_eq(&gcode, fcode) && py_callee_ok(&gcode) {
                let min_args = gcode
                    .arg_count
                    .saturating_sub(u32::try_from(g.defaults.len()).unwrap_or(u32::MAX));
                return ResolvedGlobal::PyFunc {
                    token: 0,
                    arg_count: gcode.arg_count,
                    min_args,
                    is_self: true,
                    ret: None,
                    ctor: false,
                };
            }
            return ResolvedGlobal::Opaque;
        }
        classify_global(obj.as_ref())
    };
    let (lane, ret_none) = match weavepy_jit::analyze(fcode, &mut classify) {
        Ok(tf) => (tf.ret_lane, tf.ret_none),
        Err(_) => (None, weavepy_jit::returns_none_syntactically(fcode)),
    };
    RET_LANE_CACHE.with(|c| c.borrow_mut().insert(key, (lane, ret_none, fcode.clone())));
    (lane, ret_none)
}

/// The RFC 0059 WS3 lane-only view of [`callee_ret_info`] (feeds the
/// `PyFunc` classification, which has no `None` lane).
fn callee_ret_lane(
    interp: &super::Interpreter,
    f: &Rc<PyFunction>,
    fcode: &Rc<CodeObject>,
) -> Option<JitType> {
    callee_ret_info(interp, f, fcode).0
}

/// RFC 0069 WS1 — [`callee_ret_info`] for a *method* body, with the
/// caller's live receiver standing in for `self` (local slot 0): the
/// analyzer's attribute probes resolve against it, which is what makes
/// `return self.x * self.y`-shaped bodies typable. Uncached — the
/// shared ret cache has no receiver in its key, and a method body is
/// analyzed at most once per `(slot, name)` site per compile.
fn method_ret_info(
    interp: &super::Interpreter,
    f: &Rc<PyFunction>,
    fcode: &Rc<CodeObject>,
    recv: &Object,
) -> (Option<JitType>, bool) {
    let resolve = |name: &str| resolve_plain_dicts(interp, &f.globals, &f.builtins, name);
    let mut classify = |name: &str| {
        let obj = resolve(name);
        if let Some(Object::Function(g)) = obj.as_ref() {
            let gcode = g.code.borrow().clone();
            if Rc::ptr_eq(&gcode, fcode) && py_callee_ok(&gcode) {
                let min_args = gcode
                    .arg_count
                    .saturating_sub(u32::try_from(g.defaults.len()).unwrap_or(u32::MAX));
                return ResolvedGlobal::PyFunc {
                    token: 0,
                    arg_count: gcode.arg_count,
                    min_args,
                    is_self: true,
                    ret: None,
                    ctor: false,
                };
            }
            return ResolvedGlobal::Opaque;
        }
        classify_global(obj.as_ref())
    };
    let mut list = |_: u32| None;
    let mut attr = |slot: u32, path: &[String], name: &str, store: bool| -> Option<JitType> {
        if slot != 0 {
            return None;
        }
        // RFC 0071 WS3 — chains walk from the caller's live receiver.
        let mut cur = recv.clone();
        for link in path {
            cur = attr_chain_step(&cur, link)?;
        }
        attr_fingerprint_obj(&cur, name, store).map(|(lane, ..)| lane)
    };
    // Depth bound: nested method resolution stays opaque, like nested
    // callees in `callee_ret_info`.
    let mut method = |_: u32, _: &[String], _: &str| None;
    let mut math = |_: &str, _: &str| false;
    // No live callee activation to observe parameter values from —
    // seeding stays off (RFC 0069 WS3).
    let mut param = |_: u32| None;
    // Depth bound: no constructor-shape fallback in the nested view.
    let mut ctor_field = |_: &str, _: &str| None;
    let mut path_arena = weavepy_jit::PathArena::default();
    let mut probes = Probes {
        list: &mut list,
        dict: &mut |_| None,
        attr: &mut attr,
        method: &mut method,
        math: &mut math,
        ctor_field: &mut ctor_field,
        param: &mut param,
        // Depth bound: no keyword-call recognition in the nested view.
        kw_slot: &mut |_, _| None,
        // Depth bound: no obj-global burning in the nested view (the
        // caller only wants the return lane; a body that needs the
        // frame-coverage lanes types through its own compilation).
        obj_global: &mut |_| None,
        // Depth bound: no live callee activation, so no cells to
        // observe — and `py_callee_ok` already excludes cell-bearing
        // callees from the native call lanes.
        cell: &mut |_| None,
        // Depth bound: no live locals to observe in the nested view.
        obj: &mut |_| false,
        paths: &mut path_arena,
    };
    match weavepy_jit::analyze_frame(fcode, &mut classify, &mut probes) {
        Ok(tf) => (tf.ret_lane, tf.ret_none),
        Err(_) => {
            if weavepy_jit::returns_none_syntactically(fcode) {
                return (None, true);
            }
            // RFC 0073 WS1 — the fluent shape (`return self` on every
            // path): predict the object-lane return even though the
            // body itself did not analyze (commonly it reads
            // attributes off a non-`self` parameter this nested view
            // has no live value for). Sound unconditionally — any
            // result pins into the caller's activation.
            let fluent = weavepy_jit::returns_self_syntactically(fcode);
            (fluent.then_some(JitType::Obj), false)
        }
    }
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

/// RFC 0070 WS1 — hard cap on an activation's pin table. Object-lane
/// attribute loads append a pin per access (a list traversal appends
/// one per node), so an unbounded loop needs a bound: at the cap the
/// access deopts, the activation exits, and the re-entry (usually OSR
/// at the loop header) starts over with a fresh table.
const RUNTIME_PIN_CAP: usize = 1 << 16;

/// RFC 0070 WS1 — drop the runtime pins appended after `base` (the
/// entry-time table size), running the interpreter's prompt-reap
/// cascade on any pinned object that looks like a dying temporary —
/// the same treatment interpreter locals get at frame exit. Runtime
/// pins mirror popped stack temporaries: a pinned object usually
/// outlives the activation through its container, but one *detached*
/// during the loop (`node.next = None`) dies here, and its `__del__`
/// must run promptly. Returns `true` when the cascade ran Python (the
/// caller must re-validate burned-in resolutions).
fn drain_runtime_pins(interp: &mut super::Interpreter, pins: &mut PinTable, base: usize) -> bool {
    let mut dirty = false;
    for p in pins.drain(base..) {
        let Pin::Obj(o) = p else { continue };
        if super::Interpreter::local_needs_prompt_reap(&o)
            && super::Interpreter::looks_reapable_temporary(&o)
        {
            dirty = true;
            interp.maybe_prompt_reap_replaced(o);
        }
    }
    dirty
}

/// Reconstruct an [`Object`] from a `(bits, tag)` slot. `Boxed` never
/// appears in locals or ordinary spills (the parked result travels
/// through [`CallCtx::parked`]); map it defensively to `None`, likewise
/// a pin tag reaching a context without pin-table access.
fn unpack(bits: u64, tag: u32) -> Object {
    match SlotTag::from_raw(tag) {
        SlotTag::Int => Object::Int(bits as i64),
        SlotTag::Float => Object::Float(f64::from_bits(bits)),
        SlotTag::Bool => Object::Bool(bits != 0),
        // RFC 0069 WS1 — the `None` singleton (a `ReturnNone` exit).
        SlotTag::None => Object::None,
        SlotTag::Boxed | SlotTag::ListPin | SlotTag::ObjPin => Object::None,
    }
}

/// As [`unpack`] with the activation's pin table at hand, so a pin
/// slot rebuilds into its real object (RFC 0061/0065 WS5).
fn unpack_pins(bits: u64, tag: u32, pins: &PinTable) -> Object {
    match SlotTag::from_raw(tag) {
        // RFC 0070 WS1 — an `ObjPin` slot holding `-1` is the nullable
        // lane's `None` (also what `pins.get` would fall back to, but
        // the mapping is a contract, not a defensive default).
        SlotTag::ObjPin if bits == u64::MAX => Object::None,
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
        // RFC 0070 WS1 — the nullable object lane's `None` (`-1`).
        JitType::Obj if bits == u64::MAX => Object::None,
        JitType::ListInt
        | JitType::ListFloat
        | JitType::ListObj
        | JitType::Obj
        | JitType::Str
        | JitType::Bytes
        | JitType::Dict => pins.get(bits as usize).map_or(Object::None, Pin::to_object),
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
/// a pinned-instance lane must hold an instance — or, RFC 0070 WS1,
/// the `None` singleton (the lane is nullable; `None` packs as the
/// machine value `-1` and every access helper deopts on it).
fn entry_local_ok(obj: &Object, ty: JitType) -> bool {
    if ty == JitType::Obj {
        // RFC 0076 WS8 — the object lane admits *any* bound value:
        // every access helper re-validates its own shape per access
        // and deopts on surprise, and the generic lanes (attributes,
        // membership, opaque calls, opaque iteration) serve the rest
        // through the interpreter core. Only an unbound slot refuses —
        // native code cannot model `UnboundLocalError`.
        return !matches!(obj, Object::Unbound);
    }
    // RFC 0071 WS6 — the exact-`str`/`bytes` read lanes (subclasses
    // are `Object::Instance` and never match).
    if ty == JitType::Str {
        return matches!(obj, Object::Str(_));
    }
    if ty == JitType::Bytes {
        return matches!(obj, Object::Bytes(_));
    }
    // RFC 0073 WS2 — the exact-`dict` lane (subclasses are
    // `Object::Instance` and never match). Key/value lanes are per-site
    // and re-validated by every helper, so entry checks only dict-ness.
    if ty == JitType::Dict {
        return matches!(obj, Object::Dict(_));
    }
    let Some(elem) = ty.elem_lane() else {
        return pack(obj, ty).is_some();
    };
    let Object::List(l) = obj else {
        return false;
    };
    matches!(
        (l.borrow().first(), elem),
        (None, _)
            | (Some(Object::Int(_)), JitType::Int)
            | (Some(Object::Float(_)), JitType::Float)
            | (Some(Object::Instance(_) | Object::None), JitType::Obj)
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
            // RFC 0071 WS4 — instances (and `None`) ride the object
            // element lane; the access helpers re-validate per element.
            Object::Instance(_) | Object::None => JitType::Obj,
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

/// RFC 0076 WS8 — the live-value probe: whether local `slot` holds
/// *some* bound value in the requesting activation (no grading). The
/// analyzer's generic-attribute fallback uses it to separate an
/// ungradable-but-live receiver (rides the object lane and the eager
/// generic helper) from an unbound slot (keeps the retriable
/// probe-miss verdict).
fn probe_obj_live(frame: &super::Frame, slot: u32) -> bool {
    let locals = frame.locals.borrow();
    locals
        .get(slot as usize)
        .is_some_and(|o| !matches!(o, Object::Unbound))
}

/// RFC 0073 WS2 — the dict-lane probe: `(key lane, value lane)` of
/// local `slot` when it currently holds an *exact* `dict` whose
/// sampled keys are uniformly exact-`str` (never `WStr` — surrogate-
/// bearing keys stay interpreted) or `int`, and whose sampled values
/// are uniformly `Int`/`Float` or object-lane (instances and `None`).
/// `Some((Unknown, Unknown))` for an *empty* dict (definitely a dict,
/// no lane evidence). The sample is bounded; a heterogeneous tail is
/// caught by the per-access re-validation in the helpers.
fn probe_dict_lane(frame: &super::Frame, slot: u32) -> Option<(JitType, JitType)> {
    let locals = frame.locals.borrow();
    let Some(Object::Dict(d)) = locals.get(slot as usize) else {
        return None;
    };
    let map = d.borrow();
    if map.is_empty() {
        return Some((JitType::Unknown, JitType::Unknown));
    }
    let mut key: Option<JitType> = None;
    let mut val: Option<JitType> = None;
    for (k, v) in map.iter().take(64) {
        let kt = match &k.0 {
            Object::Str(_) => JitType::Str,
            Object::Int(_) => JitType::Int,
            _ => return None,
        };
        let vt = match v {
            Object::Int(_) => JitType::Int,
            Object::Float(_) => JitType::Float,
            Object::Instance(_) | Object::None => JitType::Obj,
            _ => return None,
        };
        match key {
            None => key = Some(kt),
            Some(cur) if cur == kt => {}
            Some(_) => return None,
        }
        match val {
            None => val = Some(vt),
            Some(cur) if cur == vt => {}
            Some(_) => return None,
        }
    }
    Some((key?, val?))
}

/// RFC 0069 WS3 — the parameter-lane probe: the observed lane of the
/// argument currently bound in local `slot` of the requesting
/// activation. RFC 0071 WS1 adds the object lane for instance-valued
/// arguments. Only a prediction — every seeded slot is entry-guarded,
/// so a later call with a differently-typed argument falls back to
/// the interpreter.
fn probe_param_lane(frame: &super::Frame, slot: u32) -> Option<JitType> {
    let locals = frame.locals.borrow();
    let obj = locals.get(slot as usize)?;
    scalar_lane(obj).or_else(|| match obj {
        // RFC 0071 WS6 — exact `str`/`bytes` parameters ride the
        // pinned read lanes.
        Object::Str(_) => Some(JitType::Str),
        Object::Bytes(_) => Some(JitType::Bytes),
        // Unbound refuses (native code cannot model the
        // `UnboundLocalError` a read would raise); lists and dicts
        // stay untyped here so the fixpoint's own container probes
        // can pin their *specialized* lanes at the use sites — an
        // eager `Obj` seed would conflict with them.
        Object::Unbound | Object::List(_) | Object::Dict(_) => None,
        // RFC 0071 WS4 / RFC 0076 WS8 — everything else (instances,
        // identity iterables, sets, tuples, modules, …) rides the
        // object lane: the entry guard admits any bound value and
        // every access helper re-validates per access.
        _ => Some(JitType::Obj),
    })
}

/// RFC 0076 WS6 — the observed lane of closure cell `idx` in the
/// requesting frame (`cellvars` ++ `freevars` layout). Scalars ride
/// their unboxed lanes; any other bound payload rides the nullable
/// object lane, re-read (and freshly pinned) per access — no burn-in,
/// because closures exist to be mutated. An unbound cell refuses —
/// the compiled access would deopt on every execution.
fn probe_cell_lane(frame: &super::Frame, idx: u32) -> Option<JitType> {
    let cell = frame.cells.get(idx as usize)?;
    let payload = cell.borrow();
    scalar_lane(&payload).or_else(|| match &*payload {
        Object::Unbound => None,
        _ => Some(JitType::Obj),
    })
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

/// RFC 0071 WS3 — one link of an attribute-chain walk: read `name`
/// off `obj` under exactly the load-fingerprint discipline (eligible
/// shape, object-lane value) and return the reached *instance*. A
/// `None` mid-chain value (the lane is nullable) or any ineligible
/// shape ends the walk.
fn attr_chain_step(obj: &Object, name: &str) -> Option<Object> {
    let (lane, _, _, storage, _) = attr_fingerprint_obj(obj, name, false)?;
    if lane != JitType::Obj {
        return None;
    }
    let Object::Instance(inst) = obj else {
        return None;
    };
    let v = match storage {
        AttrStorage::Slot => inst.slot_get(name)?,
        AttrStorage::Indexed(key_idx) => inst.dict.borrow().get_index(key_idx as usize)?.1.clone(),
        AttrStorage::NewKey => return None,
    };
    matches!(v, Object::Instance(_)).then_some(v)
}

/// RFC 0071 WS3 — resolve the live object an attribute chain reaches:
/// the local in `slot`, walked through `path` one fingerprinted load
/// at a time. RFC 0073 WS1 — a [`weavepy_jit::ELEM_SENTINEL`] segment
/// steps into an *exemplar element* of a list instead (the receiver
/// residue for locals bound from a live list's elements: the probe
/// predicts from a representative, and the burned fingerprints
/// re-validate per access).
fn walk_attr_path(frame: &super::Frame, slot: u32, path: &[String]) -> Option<Object> {
    let mut cur = {
        let locals = frame.locals.borrow();
        locals.get(slot as usize)?.clone()
    };
    for name in path {
        if name == weavepy_jit::ELEM_SENTINEL {
            cur = exemplar_element(&cur)?;
            continue;
        }
        cur = attr_chain_step(&cur, name)?;
    }
    Some(cur)
}

/// RFC 0073 WS1 — a representative instance element of a live list,
/// for the element-residue probe. The scan is bounded: exemplars are
/// only needed while the receiver local is unbound (typically an OSR
/// compile mid-fill), and by then the list's head holds real
/// elements; a list with no instance in its first stretch simply
/// fails the probe (retriable).
fn exemplar_element(obj: &Object) -> Option<Object> {
    let Object::List(l) = obj else {
        return None;
    };
    let items = l.borrow();
    items
        .iter()
        .take(64)
        .find(|o| matches!(o, Object::Instance(_)))
        .cloned()
}

/// RFC 0065 WS5 — the compile-time attribute probe: report the value
/// lane of `name` on the object reached by walking `path` from local
/// `slot` (RFC 0071 WS3), but only when the receiver shape matches
/// the tier-1 inline-cache eligibility (no `__getattr__`/
/// `__getattribute__`, no shadowing data descriptor, name present in
/// the instance dict — exactly the `LoadAttrInstance`/
/// `StoreAttrInstance` shapes — or, RFC 0071 WS2, the store-only
/// new-key shape reported as `Unknown`).
fn probe_attr_lane(
    frame: &super::Frame,
    slot: u32,
    path: &[String],
    name: &str,
    store: bool,
) -> Option<JitType> {
    let recv = walk_attr_path(frame, slot, path)?;
    attr_fingerprint_obj(&recv, name, store).map(|(lane, ..)| lane)
}

/// RFC 0065 WS5 — snapshot the full guard fingerprint for one
/// attribute site right after compilation (nothing ran since the
/// probe — same thread, GIL held — so it succeeds iff the probe did).
fn attr_site_guard(
    interp: &super::Interpreter,
    frame: &super::Frame,
    site: &AttrSiteMeta,
) -> Option<AttrGuard> {
    // RFC 0073 WS1 — a constructor-resolved site has no live receiver
    // to fingerprint: burn the indexed guard from the class's
    // post-construction canonical shape instead. It is exactly the
    // fingerprint the site would learn from a live instance one call
    // later, and the runtime helpers re-validate `(type_id, ver,
    // key-at-index, lane)` per access all the same.
    if let Some((cls_name, field_idx)) = &site.ctor {
        let Object::Type(cls) = resolve_plain_global(interp, frame, cls_name)? else {
            return None;
        };
        let cc = probe_class_ctor(interp, &cls)?;
        let (fname, _) = cc.fields.get(*field_idx as usize)?;
        if fname != &site.name {
            return None;
        }
        return Some(AttrGuard {
            name: site.name.clone(),
            lane: site.lane,
            type_id: crate::specialize::rc_id(&cls),
            ver: cls.attr_version.get(),
            storage: AttrStorage::Indexed(*field_idx),
            _class: cls,
        });
    }
    // RFC 0073 WS1 — the *self-body* residue: the receiver is live but
    // mid-construction (its dict lacks the key), and the load follows
    // this body's own new-key stores. New-key *store* eligibility on
    // the live receiver (same class-override and data-descriptor
    // predicate) stands in for the load probe; the burned index is the
    // body's store order. A body entered with a non-empty dict fails
    // the runtime key-at-index check and deopts.
    if let Some(field_idx) = site.self_ctor {
        use weavepy_compiler::InlineCache as IC;
        let recv = walk_attr_path(frame, site.slot, &site.path)?;
        let IC::StoreAttrNewKey { type_id, ver } =
            crate::specialize::attempt_specialize_store_attr(&recv, &site.name)
        else {
            return None;
        };
        let Object::Instance(inst) = &recv else {
            return None;
        };
        return Some(AttrGuard {
            name: site.name.clone(),
            lane: site.lane,
            type_id,
            ver,
            storage: AttrStorage::Indexed(field_idx),
            _class: inst.cls(),
        });
    }
    let recv = walk_attr_path(frame, site.slot, &site.path)?;
    let (lane, type_id, ver, storage, class) = attr_fingerprint_obj(&recv, &site.name, site.store)?;
    // RFC 0071 WS2 — a new-key site has no current value, so its lane
    // came from the stored value; the storage modes must agree.
    if site.new_key {
        if storage != AttrStorage::NewKey {
            return None;
        }
    } else if storage == AttrStorage::NewKey || lane != site.lane {
        return None;
    }
    Some(AttrGuard {
        name: site.name.clone(),
        lane: site.lane,
        type_id,
        ver,
        storage,
        _class: class,
    })
}

/// RFC 0069 WS1 — the compile-time method probe: resolve `name` on the
/// class of the instance currently in local `slot`, when the shape is
/// eligible for a burned-in method call:
///
/// - the receiver is an instance whose class has no attribute-lookup
///   override (`__getattr__` / non-default `__getattribute__`), so the
///   class-version guard captures the full lookup semantics;
/// - `name` is not shadowed by an instance attribute (instance dict
///   beats a non-data descriptor) — re-checked per call;
/// - the MRO hit is a plain Python function with a burnable signature
///   (positional-only, no cells) taking at least `self`;
/// - its return typing is known: a stable scalar lane, or the provable
///   `return None` procedure shape.
///
/// The resolution is a prediction pinned by the returned fingerprint;
/// `wpjit_call_method` re-validates it per call.
fn probe_method_entry(
    interp: &super::Interpreter,
    frame: &super::Frame,
    slot: u32,
    path: &[String],
    name: &str,
) -> Option<MethodEntry> {
    let recv = walk_attr_path(frame, slot, path)?;
    let Object::Instance(inst) = &recv else {
        return None;
    };
    let cls = inst.cls();
    if crate::specialize::type_has_attr_override(&cls) {
        return None;
    }
    if inst.dict.borrow().get(&StrKey(name)).is_some() {
        return None;
    }
    let Some(Object::Function(f)) = cls.lookup(name) else {
        return None;
    };
    let fcode = f.code.borrow().clone();
    if !py_callee_ok(&fcode) || fcode.arg_count == 0 {
        return None;
    }
    let n_defaults = u32::try_from(f.defaults.len()).ok()?;
    let min_args = fcode.arg_count.checked_sub(n_defaults)?;
    if min_args == 0 {
        // A default for `self` is nonsense the interpreter would
        // still bind; keep such shapes on the generic path.
        return None;
    }
    let (lane, ret_none) = method_ret_info(interp, &f, &fcode, &recv);
    let ret = if ret_none {
        MethodRet::None
    } else {
        // RFC 0071 WS1 — object-lane returns cross the boundary as a
        // fresh caller pin, so they are admissible alongside scalars.
        MethodRet::Scalar(lane.filter(|t| marshalable_lane_ty(*t))?)
    };
    let type_id = crate::specialize::rc_id(&cls);
    let ver = cls.attr_version.get();
    let arg_count = fcode.arg_count;
    Some(MethodEntry {
        func: f,
        code: fcode,
        name: name.to_owned(),
        arg_count,
        min_args,
        ret,
        type_id,
        ver,
        _class: cls,
    })
}

/// RFC 0069 WS2 — the compile-time math-intrinsic probe: the function
/// object `name.attr` currently resolves to, when the pair is burnable
/// — `name` resolves to a module, its `attr` entry is a Rust builtin
/// wearing the same name, and a smoke call on `0.0` returns a plain
/// `float` (which tells `math.sin` apart from `cmath.sin`: the only
/// other builtins wearing these names are complex-valued). Builtins
/// are pure Rust — the smoke call can't run Python or observe state.
/// The returned object is snapshotted per guard; the entry check and
/// per-stride poll re-require identity.
fn math_attr_object(
    interp: &super::Interpreter,
    frame: &super::Frame,
    name: &str,
    attr: &str,
) -> Option<Object> {
    let Some(Object::Module(m)) = resolve_plain_global(interp, frame, name) else {
        return None;
    };
    let obj = m.dict.borrow().get(&StrKey(attr)).cloned()?;
    math_builtin_ok(&obj, attr).then_some(obj)
}

/// [`math_attr_object`]'s intrinsic-shape check: a builtin named
/// `attr` whose smoke call on `0.0` yields a plain `float`.
fn math_builtin_ok(obj: &Object, attr: &str) -> bool {
    let Object::Builtin(b) = obj else {
        return false;
    };
    if b.name != attr {
        return false;
    }
    matches!((b.call)(&[Object::Float(0.0)]), Ok(Object::Float(_)))
}

/// The shared fingerprint body against an explicit receiver: classify
/// with the tier-1 specialization predicate and read the current
/// value's lane (the method return-typing analysis probes the caller's
/// live receiver, which has no frame slot of its own — RFC 0069 WS1).
fn attr_fingerprint_obj(
    obj: &Object,
    name: &str,
    store: bool,
) -> Option<(JitType, u64, u32, AttrStorage, Rc<TypeObject>)> {
    use weavepy_compiler::InlineCache as IC;
    let Object::Instance(inst) = obj else {
        return None;
    };
    // RFC 0070 WS3 / RFC 0071 WS2 — the tier-1 predicate classifies
    // the storage: an indexed instance-dict hit, a `__slots__` member
    // (read and written through the slot side table by name), or the
    // new-key insert shape (stores only).
    let (type_id, ver, storage) = if store {
        match crate::specialize::attempt_specialize_store_attr(obj, name) {
            IC::StoreAttrInstance {
                type_id,
                key_idx,
                ver,
            } => (type_id, ver, AttrStorage::Indexed(key_idx)),
            IC::StoreAttrSlot { type_id, ver } => (type_id, ver, AttrStorage::Slot),
            IC::StoreAttrNewKey { type_id, ver } => (type_id, ver, AttrStorage::NewKey),
            _ => return None,
        }
    } else {
        match crate::specialize::attempt_specialize_load_attr(obj, name) {
            IC::LoadAttrInstance {
                type_id,
                key_idx,
                ver,
            } => (type_id, ver, AttrStorage::Indexed(key_idx)),
            IC::LoadAttrSlot { type_id, ver } => (type_id, ver, AttrStorage::Slot),
            _ => return None,
        }
    };
    // The current value pins the lane. An unset slot has no lane
    // evidence (and a load would raise), so it stays uncompiled.
    // RFC 0071 WS2 — a new-key store has no current value by
    // definition: the `Unknown` lane tells the analyzer to type the
    // site from the stored value instead.
    let slot_val;
    let dict;
    let v: &Object = match storage {
        AttrStorage::NewKey => return Some((JitType::Unknown, type_id, ver, storage, inst.cls())),
        AttrStorage::Slot => {
            slot_val = inst.slot_get(name)?;
            &slot_val
        }
        AttrStorage::Indexed(key_idx) => {
            dict = inst.dict.borrow();
            let (_, v) = dict.get_index(key_idx as usize)?;
            v
        }
    };
    // RFC 0070 WS1 — instance- or `None`-valued attributes take the
    // nullable object lane (loads pin the value at runtime; stores
    // resolve the staged pin); RFC 0071 WS6 — exact `str`/`bytes`
    // values take the read lanes; anything else must be a scalar.
    let lane = match v {
        Object::Instance(_) | Object::None => JitType::Obj,
        Object::Str(_) => JitType::Str,
        Object::Bytes(_) => JitType::Bytes,
        _ => scalar_lane(v)?,
    };
    Some((lane, type_id, ver, storage, inst.cls()))
}

/// RFC 0068 — drop tier-cache and ret-lane entries whose code object
/// the JIT is the *sole* owner of (`Rc::strong_count == 1`). The cache
/// pins code objects so pointer keys stay valid, which would otherwise
/// make every executed code object immortal (observable through
/// `weakref` on `__code__`). Called from `gc.collect()`. Eviction is
/// always safe: a live code object re-enters the cache through the
/// normal hot path. Runs to a fixpoint because an evicted entry can
/// release the last strong reference to another cached code object
/// (e.g. a nested function's code held via `co_consts`).
pub(crate) fn gc_sweep() {
    loop {
        let mut removed = false;
        JIT.with(|cell| {
            let mut st = cell.borrow_mut();
            let dead: Vec<*const CodeObject> = st
                .cache
                .iter()
                .filter(|(_, e)| Rc::strong_count(&e.code) == 1)
                .map(|(k, _)| *k)
                .collect();
            for k in dead {
                st.cache.remove(&k);
                removed = true;
            }
        });
        RET_LANE_CACHE.with(|c| {
            let mut m = c.borrow_mut();
            let dead: Vec<*const CodeObject> = m
                .iter()
                .filter(|(_, (_, _, code))| Rc::strong_count(code) == 1)
                .map(|(k, _)| *k)
                .collect();
            for k in dead {
                m.remove(&k);
                removed = true;
            }
        });
        if !removed {
            break;
        }
    }
}

/// Bump the back-edge hot counter for a code object. Returns `true`
/// when the caller should attempt an OSR entry (RFC 0059 WS3b); always
/// `false` when the JIT is disabled.
pub(crate) fn note_backedge(code: &Rc<CodeObject>) -> bool {
    // RFC 0067 — same fast-out as `try_enter`: rejected code pays one
    // relaxed load per back edge, not a thread-local + map lookup.
    if code.jit_hint.is_not_jitable() {
        return false;
    }
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
/// RFC 0074 WS1 — grade an obj-global's compiled lane. Exact `str`s
/// ride the `str` lane (the read/write/method lanes apply to them);
/// everything else rides the generic object lane, where the dynamic
/// ops (`CallDyn`, `DynAttrGet`/`Set`, iterator capture) apply and
/// every other access helper deopts on the lane surprise. The identity
/// guard makes the grade stable for the compilation's whole life.
fn grade_obj_global(obj: &Object) -> JitType {
    match obj {
        Object::Str(_) => JitType::Str,
        _ => JitType::Obj,
    }
}

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
        // RFC 0074 WS3 — canonical `enumerate` (builtins hold the
        // function flavour; module globals may hold the type object).
        // Certifies the tuple-target recognizer's lane training; the
        // burn itself rides the ordinary obj-global machinery.
        Some(Object::Builtin(b)) if b.name == "enumerate" => ResolvedGlobal::EnumerateBuiltin,
        Some(Object::Type(t))
            if Rc::ptr_eq(t, &crate::builtin_types::builtin_types().enumerate_) =>
        {
            ResolvedGlobal::EnumerateBuiltin
        }
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
    /// RFC 0076 WS6 — the activation's closure-cell array (`cellvars`
    /// then `freevars`, shared with the interpreter `Frame`), read and
    /// written live by `wpjit_cell_get`/`_set`. Empty for frameless
    /// entries (the native call lanes exclude cell-bearing callees).
    cells: Rc<Vec<Rc<GilRefCell<Object>>>>,
    /// A completed call's unrepresentable (or guard-invalidated) result,
    /// parked for the deopt-after-call reconstruction.
    parked: Option<Object>,
    /// A raised callee's exception, parked for the `Raised` exit.
    raised: Option<RuntimeError>,
    /// RFC 0073 WS2 — memoized `str`-constant pins: `(constant index,
    /// pin bits)`, appended by `wpjit_const_str` on first use so a
    /// loop re-executing a `LOAD_CONST` reuses one pin. Small linear
    /// scan — real functions carry a handful of hot str constants.
    const_pins: Vec<(u32, u64)>,
    /// RFC 0074 WS1 — the compile-time obj-global table (`token` →
    /// snapshotted object), read by `wpjit_global_obj`.
    obj_globals: StdRc<Vec<Object>>,
    /// RFC 0074 WS1 — memoized obj-global pins (`token`, pin bits),
    /// the `const_pins` discipline: one pin per token per activation.
    obj_global_pins: Vec<(u32, u64)>,
    /// RFC 0061/0065 WS5 — this activation's pinned objects, indexed
    /// by the pin bits native code carries in `ListPin`/`ObjPin` slots.
    pins: PinTable,
    /// RFC 0065 WS5 — per-site attribute guards, indexed by the `site`
    /// operand of `wpjit_attr_get`/`_set`.
    attr_guards: StdRc<Vec<AttrGuard>>,
    /// RFC 0069 WS1 — per-token method resolutions, indexed by the
    /// `token` operand of `wpjit_call_method`.
    methods: StdRc<MethodTable>,
    /// RFC 0069 WS2 — per-guard math-intrinsic snapshots, re-validated
    /// alongside the global guards.
    math: StdRc<MathTable>,
    /// RFC 0067 WS1 — `true` once arbitrary Python ran on behalf of
    /// this activation (an interpreter-path call, or a materialized
    /// deopt inside a nested native call). Burned-in resolutions are
    /// revalidated after a call *only* when it was dirty; a pure-native
    /// call tree can't rebind anything, so clean calls skip the guard
    /// lookups entirely.
    dirty: bool,
    /// RFC 0067 WS1 — identity of this activation's code object, so a
    /// self-recursive fast call can reuse [`Self::native`] without a
    /// cache lookup.
    code_ptr: *const CodeObject,
    /// RFC 0067 WS1 — the per-token native-callee table (parallel to
    /// [`Self::callees`]).
    native: Option<StdRc<NativeTable>>,
    /// RFC 0069 WS1 — the per-token native *method* table (parallel to
    /// [`Self::methods`]).
    method_native: Option<StdRc<NativeTable>>,
    /// RFC 0073 WS1 — the compile generation [`Self::native`] /
    /// [`Self::method_native`] were resolved at. A long-running
    /// activation (typically an OSR entry mid-warmup, before its
    /// callees' bodies compiled) re-resolves the tables *in place*
    /// when a call falls back and the generation moved, instead of
    /// paying the interpreter path until it happens to re-enter.
    table_gen: u64,
    /// RFC 0076 — the activation's code object when it runs
    /// *frameless* (the native call lanes and the direct
    /// interpreter→native entry push no interpreter `Frame`). The
    /// interpreter-fallback call helpers push a spine shell for it so
    /// a callee that walks the stack (`sys._getframe`,
    /// `traceback.walk_stack`, `warnings`' stacklevel) still observes
    /// this activation (test_asyncio's `test_timer_repr_debug` asserts
    /// the exact chain). `None` for a framed entry — its `Frame`'s
    /// shell is already on the spine.
    frameless_code: Option<Rc<CodeObject>>,
}

impl CallCtx {
    /// RFC 0073 WS1 — re-resolve this activation's native-callee
    /// tables if the compile generation moved since they were
    /// resolved. Returns `true` when the tables were refreshed (the
    /// caller should retry its native fast path).
    fn refresh_tables(&mut self) -> bool {
        let gen = JIT.with(|cell| cell.borrow().compile_gen);
        if gen == self.table_gen {
            return false;
        }
        self.table_gen = gen;
        self.native = resolved_native_table(self.code_ptr);
        self.method_native = resolved_method_native_table(self.code_ptr);
        true
    }
}

/// `true` while every burned-in resolution still holds: each guarded
/// global resolves to the identical object, each burned-in callee
/// still wears the `__code__` it was compiled against (functions are
/// code-rebindable; a swap invalidates arity/lane assumptions), and
/// each burned-in math intrinsic's `name.attr` still resolves to the
/// snapshotted function (RFC 0069 WS2 — module dicts are mutable).
fn guards_hold(
    interp: &super::Interpreter,
    globals: &Rc<GilRefCell<DictData>>,
    builtins: &Rc<GilRefCell<DictData>>,
    guard_snapshot: &[(String, Object)],
    callees: &CalleeTable,
    math: &MathTable,
) -> bool {
    for (name, expected) in guard_snapshot {
        let ok = resolve_plain_dicts(interp, globals, builtins, name)
            .is_some_and(|cur| cur.is_same(expected));
        if !ok {
            return false;
        }
    }
    for (f, code_snap) in callees {
        match f {
            Object::Function(pf) => {
                if !Rc::ptr_eq(&pf.code.borrow(), code_snap) {
                    return false;
                }
            }
            // RFC 0071 WS2 — a burned-in class constructor: the class
            // must still construct through the default pipeline with
            // the identical plain-Python `__init__` code (metaclass
            // swaps, `__new__` overrides, and `__init__` rebinding all
            // invalidate the burned arity/lane assumptions). The probe
            // is memoised on `attr_version`, so an unchanged class
            // revalidates with a version compare.
            Object::Type(t) => {
                let ok = matches!(
                    probe_class_ctor(interp, t),
                    Some(cc) if Rc::ptr_eq(&cc.init_code, code_snap)
                );
                if !ok {
                    return false;
                }
            }
            _ => return false,
        }
    }
    for (name, attr, expected) in math {
        let ok = match resolve_plain_dicts(interp, globals, builtins, name) {
            Some(Object::Module(m)) => m
                .dict
                .borrow()
                .get(&StrKey(attr))
                .is_some_and(|cur| cur.is_same(expected)),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// RFC 0067 WS1 — pooled exchange buffers for nested native entries
/// (and the top-level `enter_compiled`), so a call-recursive program
/// doesn't `malloc` five vectors per call.
#[derive(Default)]
struct JitBufs {
    u64s: Vec<Vec<u64>>,
    u32s: Vec<Vec<u32>>,
}

const JIT_BUF_POOL_CAP: usize = 64;

thread_local! {
    static JIT_BUFS: RefCell<JitBufs> = RefCell::new(JitBufs::default());
}

/// A pooled `u64` buffer of exactly `n` zeroed entries.
fn take_u64(n: usize) -> Vec<u64> {
    let mut v = JIT_BUFS
        .with(|p| p.borrow_mut().u64s.pop())
        .unwrap_or_default();
    v.clear();
    v.resize(n, 0);
    v
}

/// A pooled `u32` buffer of exactly `n` zeroed entries.
fn take_u32(n: usize) -> Vec<u32> {
    let mut v = JIT_BUFS
        .with(|p| p.borrow_mut().u32s.pop())
        .unwrap_or_default();
    v.clear();
    v.resize(n, 0);
    v
}

fn put_u64(v: Vec<u64>) {
    JIT_BUFS.with(|p| {
        let mut p = p.borrow_mut();
        if p.u64s.len() < JIT_BUF_POOL_CAP {
            p.u64s.push(v);
        }
    });
}

fn put_u32(v: Vec<u32>) {
    JIT_BUFS.with(|p| {
        let mut p = p.borrow_mut();
        if p.u32s.len() < JIT_BUF_POOL_CAP {
            p.u32s.push(v);
        }
    });
}

/// The `wpjit_poll` helper (RFC 0067 WS2): native loop headers call
/// this every `JIT_POLL_STRIDE` iterations. The GIL hand-off happens
/// inline (it needs no interpreter state — another thread runs, we
/// resume and continue natively); the return value is non-zero iff
/// the loop must deopt at its header:
///
/// - pending work that *requires* the interpreter — signals, parked
///   finalizers, C-extension drops, async exceptions, finalization
///   (the `hot_gates` word) or a freshly installed observer — which
///   the interpreter's prologue then handles with full fidelity; or
/// - a burned-in resolution that no longer holds. Same-thread rebinds
///   are caught by the post-call guard recheck (nothing else inside
///   the subset can store a global), but *another thread* can rebind
///   a guarded global mid-loop — the classic spin-on-a-flag idiom —
///   and a burned constant would otherwise never observe it. The
///   per-stride recheck bounds that staleness to one stride.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`], except this helper never runs
/// Python code and never touches the frame's exchange buffers.
unsafe extern "C" fn wpjit_poll(frame: *mut JitFrame) -> i64 {
    crate::gil::yield_checkpoint();
    if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
        return 1;
    }
    // SAFETY: see wpjit_call_py — same live-buffer contract; `ctx` is
    // null only for a frame compiled without an embedder context
    // (never the VM's own entries, but kept defensive).
    let jf = unsafe { &mut *frame };
    if !jf.ctx.is_null() {
        #[allow(clippy::cast_ptr_alignment)]
        let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
        // SAFETY: the `&mut Interpreter` that entered native code is
        // dormant while the helper runs.
        let interp = unsafe { &mut *ctx.interp };
        if !guards_hold(
            interp,
            &ctx.globals,
            &ctx.builtins,
            &ctx.guard_snapshot,
            &ctx.callees,
            &ctx.math,
        ) {
            return 1;
        }
    }
    0
}

/// The [`SlotTag`] raw value a marshalable lane travels as, or
/// `u32::MAX` for lanes that never cross a call boundary (which never
/// match an argument tag).
fn lane_tag(t: JitType) -> u32 {
    match t {
        JitType::Int => SlotTag::Int as u32,
        JitType::Float => SlotTag::Float as u32,
        JitType::Bool => SlotTag::Bool as u32,
        // RFC 0071 WS1 — the nullable object lane crosses as a pin.
        JitType::Obj => SlotTag::ObjPin as u32,
        _ => u32::MAX,
    }
}

/// RFC 0071 WS1 — pack a call result for an `ObjPin` return lane: the
/// `None` singleton is the nullable lane's `-1`; an instance pins into
/// the caller's table (capped). Anything else can't ride the lane.
fn obj_ret_bits(v: &Object, pins: &mut PinTable) -> Option<u64> {
    match v {
        Object::None => Some(u64::MAX),
        Object::Instance(_) if pins.len() < RUNTIME_PIN_CAP => {
            pins.push(Pin::Obj(v.clone()));
            Some((pins.len() - 1) as u64)
        }
        _ => None,
    }
}

/// RFC 0067 WS1 — the resolved native-callee table for a compiled
/// code object (thread-local tier cache lookup, generation-checked).
fn resolved_native_table(key: *const CodeObject) -> Option<StdRc<NativeTable>> {
    JIT.with(|cell| cell.borrow_mut().native_table_for(key))
}

/// RFC 0069 WS1 — the resolved native *method* table for a compiled
/// code object (thread-local tier cache lookup, generation-checked).
fn resolved_method_native_table(key: *const CodeObject) -> Option<StdRc<NativeTable>> {
    JIT.with(|cell| cell.borrow_mut().method_native_table_for(key))
}

/// RFC 0073 WS1 — the current compile generation, for stamping an
/// activation's resolved native tables.
fn current_compile_gen() -> u64 {
    JIT.with(|cell| cell.borrow().compile_gen)
}

/// RFC 0067 WS1 — attempt a native-to-native call for one marshaled
/// `CallPy` site. Returns `Some(CallStatus as i64)` when the call
/// completed through the native path (including via a materialized
/// deopt), or `None` when the caller should use the interpreter path.
///
/// RFC 0069 WS1 — a method call passes its guarded receiver as `recv`:
/// it is seeded as pin 0 of the callee's pin table (the receiver slot
/// carries the pin index), and the marshaled scalars fill parameter
/// slots `1..`.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`] — `jf`/`ctx` are the live,
/// exclusive buffers of the current native activation and `argc`
/// entries of the marshal buffers are initialized. `nc` must have been
/// resolved from this thread's tier cache (its `CompiledFrame` is
/// backed by the thread's engine).
#[allow(clippy::too_many_lines)]
unsafe fn try_native_call(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    nc: &NativeCallee,
    argc: u32,
    expect_tag: u32,
    recv: Option<&Object>,
) -> Option<i64> {
    // Deep native call trees have no back edges, so this is their poll
    // point (RFC 0067 WS2): hand the GIL off inline; route pending
    // interpreter work — and active observers, which need the callee's
    // trace events fired — through the interpreter path.
    crate::gil::yield_checkpoint();
    if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
        return None;
    }
    // RFC 0073 WS5 — an under-arity call site splices the missing
    // *trailing* parameters from the callee's defaults right here, so
    // the native fast path admits the whole `min_args..=arg_count`
    // window the analyzer already compiles. The compiled tuple
    // (`func.defaults`) is immutable on the function object; a live
    // `f.__defaults__ = …` override lands in the slot store and
    // routes through the interpreter's generic binder, exactly like
    // the tier-1 kwnames hit guard.
    let offset = usize::from(recv.is_some());
    let argc_usize = argc as usize;
    let n_params = nc.code.arg_count as usize;
    let supplied = argc_usize + offset;
    if supplied > n_params {
        return None;
    }
    let first_default = n_params - nc.func.defaults.len().min(n_params);
    if supplied < n_params {
        if nc.func.slot("__defaults__").is_some() {
            return None;
        }
        if supplied < first_default {
            // Unbindable — the interpreter path raises the faithful
            // TypeError.
            return None;
        }
        // Every spliced default must fit the callee's compiled
        // parameter lane (checked before any buffer is taken). List
        // lanes stay interpreted: a mutable default whose *identity*
        // matters must go through the generic binder.
        for k in supplied..n_params {
            let d = &nc.func.defaults[k - first_default];
            match nc.cf.local_types.get(k).copied().flatten() {
                None | Some(JitType::Obj) => {}
                Some(ty @ (JitType::Int | JitType::Bool | JitType::Float)) => {
                    pack(d, ty)?;
                }
                Some(JitType::Str) if matches!(d, Object::Str(_)) => {}
                Some(JitType::Bytes) if matches!(d, Object::Bytes(_)) => {}
                Some(JitType::Dict) if matches!(d, Object::Dict(_)) => {}
                Some(_) => return None,
            }
        }
    }
    // The receiver slot must be the object-pin lane the eligibility
    // check admitted (defensive — `native_method_callable` verified
    // this at resolution).
    if recv.is_some() && nc.cf.local_types.first().copied().flatten() != Some(JitType::Obj) {
        return None;
    }
    // Argument lanes must match the callee's compiled parameter lanes
    // exactly (`bool` is not `int` here, for the same reason the entry
    // type-guard separates them). RFC 0071 WS1 — an `ObjPin` argument
    // must also resolve in the *caller's* pin table (or be the
    // nullable lane's `-1`), so the translation below can't miss.
    for j in 0..argc_usize {
        let lane = nc.cf.local_types.get(j + offset).copied().flatten()?;
        // SAFETY: per the function contract, `argc` marshaled entries
        // are live.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        if lane_tag(lane) != tag {
            return None;
        }
        if tag == SlotTag::ObjPin as u32
            && bits != u64::MAX
            && !matches!(ctx.pins.get(bits as usize), Some(Pin::Obj(_)))
        {
            return None;
        }
    }
    // The callee's burned-in resolutions must hold before entry. A
    // self-call (same snapshot, same namespaces) is covered by the
    // caller's own discipline — validated at entry, revalidated after
    // every dirty call, and only native code ran since.
    let same_ns =
        StdRc::ptr_eq(&nc.snap, &ctx.guard_snapshot) && Rc::ptr_eq(&nc.func.globals, &ctx.globals);
    if !same_ns
        && !guards_hold(
            interp,
            &nc.func.globals,
            &nc.func.builtins,
            &nc.snap,
            &nc.callees,
            &nc.math,
        )
    {
        return None;
    }
    // The same recursion tick the interpreter path charges, so
    // `RecursionError` fires at the same depth in both tiers. On
    // overflow the interpreter path raises it with full fidelity.
    let recursion_guard = match crate::recursion::enter() {
        crate::recursion::Enter::Ok(g) => g,
        crate::recursion::Enter::Overflow => return None,
    };
    NATIVE_CALL_STATS.with(|s| s.calls.set(s.calls.get() + 1));

    let n_locals = nc.cf.n_locals as usize;
    let mut locals_buf = take_u64(n_locals);
    let mut pins: PinTable = Vec::new();
    if let Some(r) = recv {
        // The receiver slot carries pin index 0 (`take_u64` zeroed it).
        pins.push(Pin::Obj(r.clone()));
    }
    for j in 0..argc_usize {
        // SAFETY: as above — `argc` marshaled entries are live.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        // RFC 0071 WS1 — a caller pin index means nothing to the
        // callee: re-pin the object in the callee's own table (the
        // nullable `-1` passes through unchanged). Validated above.
        locals_buf[j + offset] = if tag == SlotTag::ObjPin as u32 && bits != u64::MAX {
            match ctx.pins.get(bits as usize) {
                Some(Pin::Obj(o)) => {
                    let idx = pins.len() as u64;
                    pins.push(Pin::Obj(o.clone()));
                    idx
                }
                // Unreachable per the validation pass; the nullable
                // `None` is the safe stand-in.
                _ => u64::MAX,
            }
        } else {
            bits
        };
    }
    // RFC 0073 WS5 — splice the trailing defaults (lane-validated
    // above) into the unsupplied parameter slots.
    #[allow(clippy::needless_range_loop)]
    for k in supplied..n_params {
        let d = &nc.func.defaults[k - first_default];
        locals_buf[k] = match nc.cf.local_types.get(k).copied().flatten() {
            Some(ty @ (JitType::Int | JitType::Bool | JitType::Float)) => pack(d, ty).unwrap_or(0),
            Some(JitType::Obj) => match d {
                Object::None => u64::MAX,
                o => {
                    let idx = pins.len() as u64;
                    pins.push(Pin::Obj(o.clone()));
                    idx
                }
            },
            Some(JitType::Str | JitType::Bytes | JitType::Dict) => {
                let idx = pins.len() as u64;
                pins.push(Pin::Obj(d.clone()));
                idx
            }
            _ => 0,
        };
    }
    let entry_pin_count = pins.len();
    let cap = nc.cf.max_stack as usize + 1;
    let mut spill = take_u64(cap);
    let mut tags = take_u32(cap);
    let call_cap = (nc.cf.max_call_args as usize).max(1);
    let mut call_args = take_u64(call_cap);
    let mut call_tags = take_u32(call_cap);
    let callee_key = Rc::as_ptr(&nc.code).cast::<CodeObject>();
    // Self-recursion reuses this activation's own table; anything else
    // resolves (generation-cached) from the tier cache.
    let (native, method_native) = if callee_key == ctx.code_ptr {
        (ctx.native.clone(), ctx.method_native.clone())
    } else {
        (
            resolved_native_table(callee_key),
            resolved_method_native_table(callee_key),
        )
    };
    let mut nctx = CallCtx {
        interp: ctx.interp,
        callees: nc.callees.clone(),
        guard_snapshot: nc.snap.clone(),
        globals: nc.func.globals.clone(),
        builtins: nc.func.builtins.clone(),
        // Native callees are cell-free by `py_callee_ok`.
        cells: crate::object::empty_cells(),
        parked: None,
        raised: None,
        const_pins: Vec::new(),
        pins,
        obj_globals: nc.obj_globals.clone(),
        obj_global_pins: Vec::new(),
        attr_guards: nc.attr_guards.clone(),
        methods: nc.methods.clone(),
        math: nc.math.clone(),
        dirty: false,
        code_ptr: callee_key,
        native,
        method_native,
        table_gen: current_compile_gen(),
        // The native call lanes push no interpreter frame for the
        // callee — keep it observable to callee-side stack walkers.
        frameless_code: Some(nc.code.clone()),
    };
    let mut njf = JitFrame {
        locals: locals_buf.as_mut_ptr(),
        n_locals: nc.cf.n_locals,
        entry_pc: 0,
        ret_bits: 0,
        ret_tag: 0,
        deopt_pc: 0,
        stack_spill: spill.as_mut_ptr(),
        stack_tags: tags.as_mut_ptr(),
        stack_len: 0,
        stack_cap: cap as u32,
        ctx: std::ptr::from_mut(&mut nctx).cast::<u8>(),
        call_args: call_args.as_mut_ptr(),
        call_tags: call_tags.as_mut_ptr(),
    };
    // SAFETY: the buffers are sized per the compiled frame's analysis
    // (the same invariants `enter_compiled` documents); the engine
    // backing `nc.cf` lives in this thread's `JIT` state for the
    // process lifetime; `nctx` outlives the call. The stack-growth
    // discipline mirrors `run_until_yield_or_return`: grow in segments,
    // except on a greenlet's dedicated (large, non-growable) stack.
    let status = if crate::stdlib::greenlet_native::on_greenlet_stack() {
        unsafe { nc.cf.enter(&raw mut njf) }
    } else {
        stacker::maybe_grow(512 * 1024, 8 * 1024 * 1024, || unsafe {
            nc.cf.enter(&raw mut njf)
        })
    };

    /// How the nested call concluded, before result-lane translation.
    enum Done {
        Scalar(u64, u32),
        Obj(Object),
        Raised(RuntimeError),
    }
    let done = match status {
        // A pin-tagged return names an entry in the *callee's* pin
        // table (dropped below): resolve it to the real object now.
        JitStatus::Returned => match SlotTag::from_raw(njf.ret_tag) {
            SlotTag::ListPin | SlotTag::ObjPin => {
                Done::Obj(unpack_pins(njf.ret_bits, njf.ret_tag, &nctx.pins))
            }
            _ => Done::Scalar(njf.ret_bits, njf.ret_tag),
        },
        // `Yielded` is unreachable here — generator bodies never
        // become native callees (`py_callee_ok` / `native_callable`
        // exclude them) — but the deopt materialization is the safe
        // catch-all if that invariant ever slips.
        JitStatus::Deopt | JitStatus::Raised | JitStatus::Yielded => {
            NATIVE_CALL_STATS.with(|s| s.deopts.set(s.deopts.get() + 1));
            nctx.dirty = true;
            // The materialized continuation is a full interpreter
            // activation that charges its own recursion tick — release
            // this level's first so the logical frame is counted once.
            drop(recursion_guard);
            let pending = if matches!(status, JitStatus::Raised) {
                Some(nctx.raised.take().unwrap_or_else(|| {
                    RuntimeError::Internal("JIT Raised exit without a parked exception".to_owned())
                }))
            } else {
                None
            };
            match finish_deopted_callee(
                interp,
                nc,
                &mut nctx,
                &locals_buf,
                &spill,
                &tags,
                &njf,
                pending,
            ) {
                Ok(v) => Done::Obj(v),
                Err(e) => Done::Raised(e),
            }
        }
    };
    put_u64(locals_buf);
    put_u64(spill);
    put_u32(tags);
    put_u64(call_args);
    put_u32(call_tags);
    // RFC 0070 WS1 — reap the callee's runtime pins (after every
    // pin-based rebuild above); a reap cascade runs Python, so it
    // dirties the call like any interpreter-path work.
    let child_dirty = nctx.dirty | drain_runtime_pins(interp, &mut nctx.pins, entry_pin_count);
    ctx.dirty |= child_dirty;

    Some(match done {
        Done::Raised(e) => {
            ctx.raised = Some(e);
            CallStatus::Raised as i64
        }
        Done::Scalar(bits, tag) => {
            // The callee may have rebound a burned-in global or a
            // callee's `__code__` — but only if Python actually ran on
            // its behalf. A clean native tree can't, so the guard
            // lookups are skipped entirely.
            if child_dirty
                && !guards_hold(
                    interp,
                    &ctx.globals,
                    &ctx.builtins,
                    &ctx.guard_snapshot,
                    &ctx.callees,
                    &ctx.math,
                )
            {
                ctx.parked = Some(unpack(bits, tag));
                CallStatus::Boxed as i64
            } else if tag == expect_tag
                && matches!(
                    SlotTag::from_raw(tag),
                    SlotTag::Int | SlotTag::Float | SlotTag::Bool | SlotTag::None
                )
            {
                // The `None` lane is the method procedure shape: the
                // caller pushes nothing, so the ret slot is ignored.
                jf.ret_bits = bits;
                jf.ret_tag = tag;
                CallStatus::Ok as i64
            } else {
                ctx.parked = Some(unpack(bits, tag));
                CallStatus::Boxed as i64
            }
        }
        Done::Obj(v) => {
            // Revalidate only when Python actually ran on the callee's
            // behalf (a materialized deopt always dirties; RFC 0071 WS1
            // adds *clean* object-lane returns, which can't rebind).
            let guards_ok = !child_dirty
                || guards_hold(
                    interp,
                    &ctx.globals,
                    &ctx.builtins,
                    &ctx.guard_snapshot,
                    &ctx.callees,
                    &ctx.math,
                );
            if guards_ok
                && matches!(SlotTag::from_raw(expect_tag), SlotTag::None)
                && matches!(v, Object::None)
            {
                // The procedure shape: nothing to write back.
                jf.ret_bits = 0;
                jf.ret_tag = expect_tag;
                return Some(CallStatus::Ok as i64);
            }
            // RFC 0071 WS1 — an object-lane return pins into the
            // *caller's* table (`None` rides as the nullable `-1`).
            if guards_ok && expect_tag == SlotTag::ObjPin as u32 {
                return Some(match obj_ret_bits(&v, &mut ctx.pins) {
                    Some(bits) => {
                        jf.ret_bits = bits;
                        jf.ret_tag = expect_tag;
                        CallStatus::Ok as i64
                    }
                    None => {
                        ctx.parked = Some(v);
                        CallStatus::Boxed as i64
                    }
                });
            }
            let expect = match SlotTag::from_raw(expect_tag) {
                SlotTag::Int => JitType::Int,
                SlotTag::Float => JitType::Float,
                SlotTag::Bool => JitType::Bool,
                SlotTag::None | SlotTag::Boxed | SlotTag::ListPin | SlotTag::ObjPin => {
                    JitType::Unknown
                }
            };
            match pack(&v, expect) {
                Some(bits) if guards_ok => {
                    jf.ret_bits = bits;
                    jf.ret_tag = expect_tag;
                    CallStatus::Ok as i64
                }
                _ => {
                    ctx.parked = Some(v);
                    CallStatus::Boxed as i64
                }
            }
        }
    })
}

/// RFC 0071 WS2 — the native class-construction fast path: allocate
/// the plain instance directly from the guarded default pipeline, run
/// the compiled `__init__` natively with the instance seeded as pin 0
/// (the method shape), and deliver the *instance* — not `__init__`'s
/// `None` — as the call site's value on the object lane. Returns
/// `None` when the caller should use the interpreter path (allocation
/// injection window, or [`try_native_call`]'s own rejections).
///
/// # Safety
///
/// Same contract as [`try_native_call`].
unsafe fn try_native_ctor(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    nc: &NativeCallee,
    argc: u32,
    expect_tag: u32,
) -> Option<i64> {
    let cls = nc.ctor.as_ref()?;
    let (inst, ran_finalizers) = interp.alloc_plain_instance(cls)?;
    if ran_finalizers {
        // Threshold collection ran finalizers — arbitrary Python.
        ctx.dirty = true;
    }
    // `__init__` is a procedure: its `None` rides the procedure lane.
    // A `try_native_call` rejection discards the fresh (empty, never
    // `__init__`-ed) instance and re-allocates on the interpreter path.
    let status =
        unsafe { try_native_call(jf, ctx, interp, nc, argc, SlotTag::None as u32, Some(&inst)) }?;
    if status == CallStatus::Raised as i64 {
        // A raising `__init__` discards the instance (CPython's
        // `type_call` propagates before returning it).
        return Some(status);
    }
    if status == CallStatus::Ok as i64 {
        // `__init__` completed and returned `None` with guards intact;
        // the call site's value is the fresh instance.
        if expect_tag == SlotTag::ObjPin as u32 {
            if let Some(bits) = obj_ret_bits(&inst, &mut ctx.pins) {
                jf.ret_bits = bits;
                jf.ret_tag = expect_tag;
                return Some(CallStatus::Ok as i64);
            }
        }
        ctx.parked = Some(inst);
        return Some(CallStatus::Boxed as i64);
    }
    // `Boxed`: `__init__`'s completed value is parked — either a
    // non-`None` return (a TypeError, as in `Interpreter::instantiate`)
    // or a `None` that couldn't ride the lane because a dirty sub-call
    // invalidated the caller's guards (then the *instance* is the
    // deopt-after-call value).
    let init_ret = ctx.parked.take().unwrap_or(Object::None);
    if !matches!(init_ret, Object::None) {
        ctx.raised = Some(crate::error::type_error(format!(
            "__init__() should return None, not '{}'",
            init_ret.type_name()
        )));
        return Some(CallStatus::Raised as i64);
    }
    ctx.parked = Some(inst);
    Some(CallStatus::Boxed as i64)
}

/// RFC 0067 WS1 — a nested native callee took a side exit: build the
/// interpreter frame it would have had (locals written back per lane,
/// operand stack rebuilt by the standard machinery, parked sub-call
/// results pushed), positioned at the deopt state, and finish it in
/// the interpreter. The rare path — it pays interpreter cost, never
/// loses state.
#[allow(clippy::too_many_arguments)]
fn finish_deopted_callee(
    interp: &mut super::Interpreter,
    nc: &NativeCallee,
    nctx: &mut CallCtx,
    locals_buf: &[u64],
    spill: &[u64],
    tags: &[u32],
    njf: &JitFrame,
    raised: Option<RuntimeError>,
) -> Result<Object, RuntimeError> {
    let code = &nc.code;
    let n_real = code.varnames.len();
    let mut locals_v: Vec<Object> = Vec::with_capacity(n_real);
    for slot in 0..n_real {
        match nc.cf.local_types.get(slot).copied().flatten() {
            Some(ty) => locals_v.push(unpack_ty(
                locals_buf.get(slot).copied().unwrap_or(0),
                ty,
                &nctx.pins,
            )),
            None => locals_v.push(Object::Unbound),
        }
    }
    let entry = CompiledEntry {
        cf: nc.cf.clone(),
        guard_snapshot: nc.snap.clone(),
        callees: nc.callees.clone(),
        obj_globals: nc.obj_globals.clone(),
        attr_guards: nc.attr_guards.clone(),
        methods: nc.methods.clone(),
        math: nc.math.clone(),
        native: None,
        method_native: None,
        // Synthetic entry, only for the stack rebuild below; `0` is
        // never a real compile id, so nothing can park against it.
        compile_id: 0,
    };
    let mut frame = super::Frame {
        code: code.clone(),
        locals: Rc::new(GilRefCell::new(locals_v)),
        cells: crate::object::empty_cells(),
        stack: Vec::new(),
        globals: nc.func.globals.clone(),
        builtins: nc.func.builtins.clone(),
        builtins_obj: None,
        class_namespace: None,
        class_namespace_obj: None,
        exc_handlers: Vec::new(),
        saved_exc_info: Vec::new(),
        agen_yielded_value: true,
        pc: 0,
        py_frame: None,
        gen_owner: None,
        cleanup_lasti: None,
        pending_lasti: None,
        suppress_call_event: true,
        gen_first_resume: false,
        shell_cache: None,
        parked_native: None,
    };
    rebuild_stack(
        interp, &mut frame, &entry, locals_buf, spill, tags, njf, &nctx.pins,
    );
    if raised.is_some() {
        // As though the raising CALL just executed: pc points past it
        // (`handle_exception` uses `pc - 1` as the raise site).
        frame.pc = njf.deopt_pc + 1;
    } else {
        // A deopt-after-call carries the parked, already-computed
        // result on top of the rebuilt stack.
        if let Some(v) = nctx.parked.take() {
            frame.stack.push(v);
        }
        frame.pc = njf.deopt_pc;
    }
    interp.run_deopted_frame(&mut frame, raised)
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
/// RFC 0076 — run an interpreter-executed call on behalf of a native
/// activation with the activation kept *observable*. A frameless entry
/// (the native call lanes, the direct interpreter→native call) has no
/// interpreter `Frame` on the spine, so a callee that walks the stack
/// (`sys._getframe`, `traceback.walk_stack`) would skip this
/// activation entirely — asyncio's `Handle.__init__` then attributes
/// the handle's creation to the wrong frame. Push a shell carrying the
/// activation's code and the current call-site pc (the lowering stores
/// it into `deopt_pc` right before every call helper), run the call,
/// pop. Framed entries (`frameless_code == None`) pass through: their
/// shell is already on the spine.
///
/// The shell's Python-visible locals are empty — the real locals live
/// lane-packed in the `JitFrame` — which is sufficient for the
/// mid-chain walkers above (they read code identity and `f_lineno`,
/// never a non-executing frame's `f_locals`).
fn call_with_activation_shell<T>(
    interp: &mut super::Interpreter,
    ctx: &CallCtx,
    jf: &JitFrame,
    f: impl FnOnce(&mut super::Interpreter) -> T,
) -> T {
    let Some(code) = &ctx.frameless_code else {
        return f(interp);
    };
    let shell = Rc::new(crate::object::FrameShell {
        code: code.clone(),
        locals: Rc::new(GilRefCell::new(Vec::new())),
        cells: ctx.cells.clone(),
        globals: ctx.globals.clone(),
        builtins: ctx.builtins.clone(),
        builtins_obj: None,
        class_namespace: None,
        class_namespace_obj: None,
        is_gen: false,
        gen_owner: GilRefCell::new(None),
        lasti: std::sync::atomic::AtomicU32::new(jf.deopt_pc),
        has_materialized: std::sync::atomic::AtomicBool::new(false),
        materialized: GilRefCell::new(None),
    });
    interp.frame_stack.borrow_mut().push(shell);
    let out = f(interp);
    interp.frame_stack.borrow_mut().pop();
    out
}

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

    // RFC 0067 WS1 — the native-to-native fast path: a compiled,
    // shape-eligible callee is entered directly with the marshaled
    // scalars, skipping the interpreter frame entirely. (The table
    // `StdRc` is cloned so `ctx` can be borrowed mutably below.)
    let mut native_table = ctx.native.clone();
    let mut tried_refresh = false;
    loop {
        if let Some(nc) = native_table
            .as_deref()
            .and_then(|t| t.get(token as usize))
            .and_then(Option::as_ref)
        {
            // SAFETY: `jf`/`ctx` are this activation's live buffers (see
            // the function contract) and `nc` came from this thread's
            // tier cache via the activation's resolved table.
            // RFC 0071 WS2 — a class-constructor callee allocates the
            // instance and enters the compiled `__init__` instead.
            let attempted = if nc.ctor.is_some() {
                unsafe { try_native_ctor(jf, ctx, interp, nc, argc, expect_tag) }
            } else {
                unsafe { try_native_call(jf, ctx, interp, nc, argc, expect_tag, None) }
            };
            match attempted {
                Some(status) => return status,
                None => {
                    NATIVE_CALL_STATS.with(|s| s.fallbacks.set(s.fallbacks.get() + 1));
                    break;
                }
            }
        }
        // RFC 0073 WS1 — the table may predate this callee's compile
        // (an OSR entry mid-warmup): re-resolve once per generation
        // move and retry the native path before paying the
        // interpreter fallback.
        if tried_refresh || !ctx.refresh_tables() {
            break;
        }
        tried_refresh = true;
        native_table = ctx.native.clone();
    }

    // Interpreter path: arbitrary Python runs on behalf of this
    // activation, so burned-in resolutions must be revalidated after
    // the call (RFC 0067 WS1's dirtiness discipline).
    ctx.dirty = true;
    let (callee, _code) = ctx.callees[token as usize].clone();
    let mut args: Vec<Object> = Vec::with_capacity(argc as usize);
    for j in 0..argc as usize {
        // SAFETY: native code wrote `argc` entries, and the buffers are
        // `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        // RFC 0071 WS1 — pin-tagged arguments resolve against this
        // activation's pin table.
        args.push(unpack_pins(bits, tag, &ctx.pins));
    }

    let called = call_with_activation_shell(interp, ctx, jf, |i| {
        i.call(&callee, &args, &[], &ctx.globals)
    });
    match called {
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
                &ctx.math,
            );
            if still_valid {
                // RFC 0071 WS1 — an object-lane result pins into this
                // activation's table.
                if expect_tag == SlotTag::ObjPin as u32 {
                    if let Some(bits) = obj_ret_bits(&v, &mut ctx.pins) {
                        jf.ret_bits = bits;
                        jf.ret_tag = expect_tag;
                        return CallStatus::Ok as i64;
                    }
                    ctx.parked = Some(v);
                    return CallStatus::Boxed as i64;
                }
                let expect = match SlotTag::from_raw(expect_tag) {
                    SlotTag::Int => JitType::Int,
                    SlotTag::Float => JitType::Float,
                    SlotTag::Bool => JitType::Bool,
                    // Other pin-lane call results are rejected at
                    // emission; `Unknown` never packs, forcing the
                    // boxed path.
                    SlotTag::None | SlotTag::Boxed | SlotTag::ListPin | SlotTag::ObjPin => {
                        JitType::Unknown
                    }
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

/// Shared result protocol for interpreter-executed calls made on
/// behalf of native code (`wpjit_call_method`'s fallback and surprise
/// lanes): pack the result into the expected lane and continue
/// (`Ok`), park an unrepresentable result or invalidated guards
/// (`Boxed` — deopt *after* the call), or park the raised exception
/// (`Raised`).
fn finish_interp_call(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    res: Result<Object, RuntimeError>,
    expect_tag: u32,
) -> i64 {
    match res {
        Err(err) => {
            ctx.raised = Some(err);
            CallStatus::Raised as i64
        }
        Ok(v) => {
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            );
            if still_valid {
                match SlotTag::from_raw(expect_tag) {
                    // The procedure lane: nothing to write back, the
                    // compiled code pushes no result.
                    SlotTag::None => {
                        if matches!(v, Object::None) {
                            return CallStatus::Ok as i64;
                        }
                    }
                    SlotTag::Int | SlotTag::Float | SlotTag::Bool => {
                        let expect = match SlotTag::from_raw(expect_tag) {
                            SlotTag::Int => JitType::Int,
                            SlotTag::Float => JitType::Float,
                            _ => JitType::Bool,
                        };
                        if let Some(bits) = pack(&v, expect) {
                            jf.ret_bits = bits;
                            jf.ret_tag = expect_tag;
                            return CallStatus::Ok as i64;
                        }
                    }
                    // RFC 0071 WS1 — an object-lane result pins into
                    // this activation's table.
                    SlotTag::ObjPin => {
                        if let Some(bits) = obj_ret_bits(&v, &mut ctx.pins) {
                            jf.ret_bits = bits;
                            jf.ret_tag = expect_tag;
                            return CallStatus::Ok as i64;
                        }
                    }
                    SlotTag::Boxed | SlotTag::ListPin => {}
                }
            }
            ctx.parked = Some(v);
            CallStatus::Boxed as i64
        }
    }
}

/// The `wpjit_call_method` helper (RFC 0069 WS1): native code calls
/// this with a burned-in method token, the receiver's pin, and the
/// marshaled scalar arguments (receiver excluded). The guard is
/// re-validated *before* the call runs — receiver class identity +
/// attr-version (which pins the MRO hit), no instance-dict shadowing,
/// and the resolved function still wearing its compile-time
/// `__code__`. A mismatch takes the *surprise-receiver lane* (RFC
/// 0074): the attribute resolves generically through the interpreter
/// — raising the exact `AttributeError` a re-executed `LOAD_ATTR`
/// would — and the bound result is called generically, reporting
/// through the same protocol. (The old `CallStatus::Reject` deopt is
/// wrong here: the rebuild re-binds the open method span with a
/// fresh attribute load on the receiver, which on a receiver that
/// never matched the guard can *fail*, leaving `None` where the
/// interpreter expects the callable.) A validated call runs through
/// the interpreter and reports like [`wpjit_call_py`], with one more
/// lane: [`SlotTag::None`] as `expect_tag` accepts exactly the
/// `None` result (the procedure shape) and parks anything else.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_call_method(
    frame: *mut JitFrame,
    token: u32,
    recv_pin: i64,
    argc: u32,
    expect_tag: u32,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` that entered native code is dormant
    // while the helper runs; this is the only live path to it.
    let interp = unsafe { &mut *ctx.interp };
    NATIVE_CALL_STATS.with(|s| s.method_calls.set(s.method_calls.get() + 1));
    let guard_miss = || {
        NATIVE_CALL_STATS.with(|s| s.method_guard_misses.set(s.method_guard_misses.get() + 1));
        CallStatus::Reject as i64
    };

    // The method table is snapshotted per activation (an `StdRc`), so
    // cloning the handle ends the `ctx` borrow before the native path
    // below needs `ctx` mutably.
    let methods = ctx.methods.clone();
    let Some(entry) = methods.get(token as usize) else {
        return guard_miss();
    };
    let recv = match ctx.pins.get(recv_pin as usize) {
        Some(Pin::Obj(o)) => o.clone(),
        _ => return guard_miss(),
    };
    let guard_ok = match &recv {
        Object::Instance(inst) => {
            let cls_ok = {
                let cls = inst.class.borrow();
                crate::specialize::rc_id(&cls) == entry.type_id
                    && cls.attr_version.get() == entry.ver
            };
            cls_ok
                && inst.dict.borrow().get(&StrKey(&entry.name)).is_none()
                && Rc::ptr_eq(&entry.func.code.borrow(), &entry.code)
        }
        _ => false,
    };
    if !guard_ok {
        // RFC 0074 — surprise-receiver lane: the burned resolution
        // doesn't apply (different class, shadowed name, swapped
        // `__code__`, or a non-instance receiver). Resolve and call
        // generically instead of deopting: a missing attribute raises
        // here exactly as the interpreter's `LOAD_ATTR` would.
        NATIVE_CALL_STATS.with(|s| s.method_guard_misses.set(s.method_guard_misses.get() + 1));
        ctx.dirty = true;
        let bound = match interp.load_attr_public(&recv, &entry.name) {
            Err(err) => {
                ctx.raised = Some(err);
                return CallStatus::Raised as i64;
            }
            Ok(b) => b,
        };
        let mut args: Vec<Object> = Vec::with_capacity(argc as usize);
        for j in 0..argc as usize {
            // SAFETY: native code wrote `argc` entries, and the buffers
            // are `max_call_args` wide.
            let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
            args.push(unpack_pins(bits, tag, &ctx.pins));
        }
        let res = call_with_activation_shell(interp, ctx, jf, |i| {
            i.call(&bound, &args, &[], &ctx.globals)
        });
        return finish_interp_call(jf, ctx, interp, res, expect_tag);
    }

    // RFC 0069 WS1 — the native fast path: the guarded method's own
    // body is compiled and shape-eligible, so enter it directly with
    // the receiver seeded as its pin 0. The table is parallel to
    // `ctx.methods` (same compile artifacts), and the guard above
    // already pinned func/`__code__` identity.
    let mut method_native = ctx.method_native.clone();
    let mut tried_refresh = false;
    loop {
        if let Some(nc) = method_native
            .as_deref()
            .and_then(|t| t.get(token as usize))
            .and_then(Option::as_ref)
        {
            if Rc::ptr_eq(&nc.func, &entry.func) && Rc::ptr_eq(&nc.code, &entry.code) {
                // SAFETY: `jf`/`ctx` are this activation's live buffers
                // (see the function contract) and `nc` came from this
                // thread's tier cache via the activation's resolved table.
                match unsafe { try_native_call(jf, ctx, interp, nc, argc, expect_tag, Some(&recv)) }
                {
                    Some(status) => return status,
                    None => {
                        NATIVE_CALL_STATS.with(|s| s.fallbacks.set(s.fallbacks.get() + 1));
                        break;
                    }
                }
            }
        }
        // RFC 0073 WS1 — the table may predate this method's compile
        // (an OSR entry mid-warmup): re-resolve once per generation
        // move and retry the native path before paying the
        // interpreter fallback.
        if tried_refresh || !ctx.refresh_tables() {
            break;
        }
        tried_refresh = true;
        method_native = ctx.method_native.clone();
    }

    // Interpreter path: arbitrary Python runs on behalf of this
    // activation, so burned-in resolutions must be revalidated after
    // the call (RFC 0067 WS1's dirtiness discipline).
    NATIVE_CALL_STATS.with(|s| {
        s.method_call_fallbacks
            .set(s.method_call_fallbacks.get() + 1);
    });
    ctx.dirty = true;
    let callee = Object::Function(entry.func.clone());
    let mut args: Vec<Object> = Vec::with_capacity(argc as usize + 1);
    args.push(recv.clone());
    for j in 0..argc as usize {
        // SAFETY: native code wrote `argc` entries, and the buffers are
        // `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        // RFC 0071 WS1 — pin-tagged arguments resolve against this
        // activation's pin table.
        args.push(unpack_pins(bits, tag, &ctx.pins));
    }

    let res = call_with_activation_shell(interp, ctx, jf, |i| {
        i.call(&callee, &args, &[], &ctx.globals)
    });
    finish_interp_call(jf, ctx, interp, res, expect_tag)
}

/// RFC 0073 WS3 — the per-method resolved `str` builtin bodies,
/// memoized process-wide (builtin type surfaces are immutable, so one
/// resolution is good for the process lifetime). Indexed by the
/// [`weavepy_jit::StrMethod`] discriminant.
fn str_method_table() -> &'static [Rc<crate::object::BuiltinFn>] {
    static TABLE: std::sync::OnceLock<Vec<Rc<crate::object::BuiltinFn>>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let probe = Object::from_static("");
        weavepy_jit::StrMethod::ALL
            .iter()
            .map(|m| {
                match crate::builtins::lookup_method(&probe, m.name()) {
                    Some(Object::Builtin(b)) => b,
                    // Unreachable: every `StrMethod` name is in
                    // `lookup_method`'s `str` table. A panic here is a
                    // build-time table mismatch, caught by any test
                    // exercising the lane.
                    _ => unreachable!("str method {} missing from lookup_method", m.name()),
                }
            })
            .collect()
    })
}

/// The `wpjit_str_method` helper (RFC 0073 WS3): native code calls
/// this with a burned-in [`weavepy_jit::StrMethod`] discriminant, the
/// exact-`str` receiver's pin, and the marshaled lane-typed arguments
/// (receiver excluded). No guard revalidation is needed — exact `str`
/// is immutable and its method table can't be shadowed — so the
/// dispatch is a direct invocation of the same builtin body tier-1's
/// `CallNativeMethod` IC calls (identical validation, arity wording,
/// and raise behavior). Reporting mirrors [`wpjit_call_method`]:
/// `Ok` with the result packed on the expected lane (fresh `str`/list
/// results pin), `Raised` with the exception parked, `Boxed` for a
/// lane surprise (e.g. a `WStr`-producing result — parked, deopt
/// *after* the call), `Reject` to re-execute the `CALL` generically
/// (pin miss, pin-cap pressure, or a `join` argument the direct body
/// can't take without the interpreter's iterator protocol).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_str_method(
    frame: *mut JitFrame,
    method: u32,
    recv_pin: i64,
    argc: u32,
    expect_tag: u32,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(m) = weavepy_jit::StrMethod::from_raw(method) else {
        return CallStatus::Reject as i64;
    };
    let recv = match ctx.pins.get(recv_pin as usize) {
        Some(Pin::Obj(o @ Object::Str(_))) => o.clone(),
        _ => return CallStatus::Reject as i64,
    };
    let mut args: Vec<Object> = Vec::with_capacity(argc as usize + 1);
    args.push(recv);
    for j in 0..argc as usize {
        // SAFETY: native code wrote `argc` entries, and the buffers are
        // `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        args.push(unpack_pins(bits, tag, &ctx.pins));
    }
    // `str.join` iterates its argument: the direct body handles
    // list/tuple natively, but a generator or instance argument needs
    // the interpreter's iterator protocol (the dispatch chain's `join`
    // arm) — reject so the interpreter re-executes the call.
    if m == weavepy_jit::StrMethod::Join
        && !matches!(args.get(1), Some(Object::List(_) | Object::Tuple(_)))
    {
        return CallStatus::Reject as i64;
    }
    let bf = &str_method_table()[method as usize];
    // Positional spellings only this wave (`sep=`/`maxsplit=` stay
    // interpreted): kwargs-aware bodies get an empty kwargs slice.
    let result = match bf.call_kw.as_ref() {
        Some(ckw) => ckw(&args, &[]),
        None => (bf.call)(&args),
    };
    // Pure native code ran: no Python, no guard invalidation, `ctx`
    // stays clean.
    match result {
        Err(err) => {
            ctx.raised = Some(err);
            CallStatus::Raised as i64
        }
        Ok(v) => {
            match SlotTag::from_raw(expect_tag) {
                SlotTag::Int | SlotTag::Bool => {
                    let expect = if SlotTag::from_raw(expect_tag) == SlotTag::Int {
                        JitType::Int
                    } else {
                        JitType::Bool
                    };
                    if let Some(bits) = pack(&v, expect) {
                        jf.ret_bits = bits;
                        jf.ret_tag = expect_tag;
                        return CallStatus::Ok as i64;
                    }
                }
                // A fresh exact-`str` result pins (the `Str` lane).
                SlotTag::ObjPin => {
                    if matches!(v, Object::Str(_)) && ctx.pins.len() < RUNTIME_PIN_CAP {
                        jf.ret_bits = ctx.pins.len() as u64;
                        jf.ret_tag = expect_tag;
                        ctx.pins.push(Pin::Obj(v));
                        return CallStatus::Ok as i64;
                    }
                }
                // `split`/`rsplit` — a fresh list of exact strings,
                // pinned on the object-element lane (`ForList`/
                // `ListGet` hand elements out as `Obj` pins).
                SlotTag::ListPin => {
                    if let Object::List(l) = &v {
                        if ctx.pins.len() < RUNTIME_PIN_CAP {
                            jf.ret_bits = ctx.pins.len() as u64;
                            jf.ret_tag = expect_tag;
                            ctx.pins.push(Pin::List(l.clone(), JitType::Obj));
                            return CallStatus::Ok as i64;
                        }
                    }
                }
                SlotTag::None | SlotTag::Float | SlotTag::Boxed => {}
            }
            // Lane surprise (`WStr` result, huge `int`, pin-cap
            // pressure): park the exact result and deopt after the
            // call — the interpreter resumes with it on the stack.
            ctx.parked = Some(v);
            CallStatus::Boxed as i64
        }
    }
}

/// The `sin` intrinsic helper (RFC 0069 WS2) — the same `f64::sin`
/// the interpreter's `math.sin` computes; the compiled guard already
/// excluded the infinite inputs whose `NaN` result the interpreter
/// turns into `ValueError`.
extern "C" fn wpjit_math_sin(x: f64) -> f64 {
    x.sin()
}

/// The `cos` intrinsic helper (RFC 0069 WS2); see [`wpjit_math_sin`].
extern "C" fn wpjit_math_cos(x: f64) -> f64 {
    x.cos()
}

/// Python-semantics `float` floor division (RFC 0069 WS2): CPython's
/// `float_divmod` quotient, sign discipline included. The compiled
/// guard deopts the zero-divisor case *before* the call (the
/// interpreter re-executes and raises the exact `ZeroDivisionError`),
/// so the error arm is unreachable-defensive.
extern "C" fn wpjit_float_floordiv(a: f64, b: f64) -> f64 {
    crate::py_float_divmod(a, b, "float floor division").map_or(f64::NAN, |(div, _)| div)
}

/// Python-semantics `float` modulo (RFC 0069 WS2): the remainder takes
/// the divisor's sign. Zero divisors deopt before the call, as with
/// [`wpjit_float_floordiv`].
extern "C" fn wpjit_float_mod(a: f64, b: f64) -> f64 {
    crate::py_float_divmod(a, b, "float modulo").map_or(f64::NAN, |(_, m)| m)
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
    // Scoped so the list borrow (through `ctx.pins`) ends before an
    // object element appends a fresh pin (RFC 0071 WS4).
    let outcome: Result<u64, Object> = {
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
            (Object::Int(v), JitType::Int) => Ok(*v as u64),
            (Object::Float(f), JitType::Float) => Ok(f.to_bits()),
            // RFC 0071 WS4 — the object element lane: `None` rides as
            // the nullable `-1`; an instance pins below.
            (Object::None, JitType::Obj) => Ok(u64::MAX),
            // RFC 0073 WS3 — exact-`str` elements pin on the object
            // lane (indexing into a `split` result).
            (v @ (Object::Instance(_) | Object::Str(_)), JitType::Obj) => Err(v.clone()),
            _ => return 1,
        }
    };
    match outcome {
        Ok(bits) => {
            jf.ret_bits = bits;
            0
        }
        Err(obj) => {
            if ctx.pins.len() >= RUNTIME_PIN_CAP {
                return 1;
            }
            jf.ret_bits = ctx.pins.len() as u64;
            ctx.pins.push(Pin::Obj(obj));
            0
        }
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
    // RFC 0071 WS4 — the object lane's staged bits are a pin index
    // (`-1` for `None`), resolved before the list borrow below.
    let staged_obj = match ctx.pins.get(pin as usize) {
        Some(Pin::List(_, JitType::Obj)) => {
            if jf.ret_bits == u64::MAX {
                Some(Object::None)
            } else {
                match ctx.pins.get(jf.ret_bits as usize) {
                    Some(Pin::Obj(o)) => Some(o.clone()),
                    _ => return 1,
                }
            }
        }
        _ => None,
    };
    let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let v = match elem {
        JitType::Int => Object::Int(jf.ret_bits as i64),
        JitType::Float => Object::Float(f64::from_bits(jf.ret_bits)),
        JitType::Obj => match staged_obj {
            Some(o) => o,
            None => return 1,
        },
        _ => return 1,
    };
    let mut items = list.borrow_mut();
    let len = items.len() as i64;
    let i = if idx < 0 { idx + len } else { idx };
    if i < 0 || i >= len {
        return 1;
    }
    let dst = &mut items[i as usize];
    // A displaced heap value that could carry a finalizer must drop on
    // the interpreter's store path (prompt reap, parked finalizers) —
    // deopt before the store. RFC 0071 WS4 — an object-lane list still
    // holds a strong reference to the displaced instance through the
    // pin table only if it was loaded before; conservatively deopt for
    // any displaced instance so its drop runs interpreted.
    if !matches!(
        dst,
        Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
    ) {
        return 1;
    }
    *dst = v;
    0
}

/// The `wpjit_cell_get` helper (RFC 0076 WS6): read closure cell `idx`
/// of the activation's cell array, re-validating the site's burned
/// `lane` against the live value — cells are shared mutable state, so
/// an aliased rebind through another closure (or a `del`) can retype
/// or unbind the cell between accesses. Returns `0` (Ok) with the
/// value's bits in [`JitFrame::ret_bits`], or `1` to deopt (the
/// interpreter re-executes the `LOAD_DEREF`, raising the exact
/// `NameError`/`UnboundLocalError` for the unbound case). Never runs
/// Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_cell_get(frame: *mut JitFrame, idx: i64, lane: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(cell) = ctx.cells.get(idx as usize) else {
        return 1;
    };
    let outcome: Result<u64, Object> = match (&*cell.borrow(), JitType::from_cell_lane_code(lane)) {
        (Object::Int(v), Some(JitType::Int)) => Ok(*v as u64),
        (Object::Float(f), Some(JitType::Float)) => Ok(f.to_bits()),
        (Object::Bool(b), Some(JitType::Bool)) => Ok(u64::from(*b)),
        // The nullable object lane re-reads the payload per access:
        // `None` rides as `-1`, anything else (except an unbound
        // cell) pins fresh below — no burn-in.
        (Object::None, Some(JitType::Obj)) => Ok(u64::MAX),
        (Object::Unbound, _) => return 1,
        (v, Some(JitType::Obj)) => Err(v.clone()),
        _ => return 1,
    };
    match outcome {
        Ok(bits) => {
            jf.ret_bits = bits;
            0
        }
        Err(obj) => match pin_any(obj, &mut ctx.pins) {
            Some(bits) => {
                jf.ret_bits = bits;
                0
            }
            None => 1,
        },
    }
}

/// The `wpjit_cell_set` helper (RFC 0076 WS6): write closure cell
/// `idx` with the value pre-staged in [`JitFrame::ret_bits`],
/// interpreted per the site's burned `lane`. Deopts (`1`) when the
/// displaced value is a heap object — replacing it here would drop it
/// inside the helper, and the drop-site machinery (prompt reap,
/// parked finalizers) belongs to the interpreter's store path. Never
/// runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_cell_set(frame: *mut JitFrame, idx: i64, lane: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(cell) = ctx.cells.get(idx as usize) else {
        return 1;
    };
    let v = match JitType::from_cell_lane_code(lane) {
        Some(JitType::Int) => Object::Int(jf.ret_bits as i64),
        Some(JitType::Float) => Object::Float(f64::from_bits(jf.ret_bits)),
        Some(JitType::Bool) => Object::Bool(jf.ret_bits != 0),
        // The object lane stages a pin index (`-1` for `None`).
        Some(JitType::Obj) => {
            if jf.ret_bits == u64::MAX {
                Object::None
            } else {
                match ctx.pins.get(jf.ret_bits as usize) {
                    Some(Pin::Obj(o)) => o.clone(),
                    // A `ListPin` value is a legitimate object store.
                    Some(p @ Pin::List(..)) => p.to_object(),
                    None => return 1,
                }
            }
        }
        _ => return 1,
    };
    let mut slot = cell.borrow_mut();
    // A displaced heap value must drop on the interpreter's store path
    // (prompt reap, parked finalizers) — deopt before the store. An
    // `Unbound` cell stores fine: there is nothing to drop.
    if !matches!(
        &*slot,
        Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None | Object::Unbound
    ) {
        return 1;
    }
    *slot = v;
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
    // RFC 0071 WS4 — an object-lane append stages a pin index.
    let staged_obj = match ctx.pins.get(pin as usize) {
        Some(Pin::List(_, JitType::Obj)) => {
            if jf.ret_bits == u64::MAX {
                Some(Object::None)
            } else {
                match ctx.pins.get(jf.ret_bits as usize) {
                    Some(Pin::Obj(o)) => Some(o.clone()),
                    _ => return 1,
                }
            }
        }
        _ => None,
    };
    let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let v = match elem {
        JitType::Int => Object::Int(jf.ret_bits as i64),
        JitType::Float => Object::Float(f64::from_bits(jf.ret_bits)),
        JitType::Obj => match staged_obj {
            Some(o) => o,
            None => return 1,
        },
        _ => return 1,
    };
    list.borrow_mut().push(v);
    0
}

/// The `wpjit_str_eq` helper (RFC 0071 WS6): equality of two pinned
/// `str` values. Identical pins and pointer-equal payloads (interned
/// or shared `Rc`s) answer before the content compare. Returns `0`
/// (unequal), `1` (equal), other (pin miss — deopt). Never runs
/// Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_str_eq(frame: *mut JitFrame, a: i64, b: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::Obj(Object::Str(sa))) = ctx.pins.get(a as usize) else {
        return 2;
    };
    let Some(Pin::Obj(Object::Str(sb))) = ctx.pins.get(b as usize) else {
        return 2;
    };
    if a == b || Rc::ptr_eq(sa, sb) {
        return 1;
    }
    i64::from(sa == sb)
}

/// The `wpjit_str_len` helper (RFC 0071 WS6): `len` of a pinned `str`
/// — the *character* count, matching `str.__len__`. Negative return
/// deopts (pin miss).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_str_len(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::Obj(Object::Str(s))) = ctx.pins.get(pin as usize) else {
        return -1;
    };
    s.chars().count() as i64
}

/// The `wpjit_bytes_len` helper (RFC 0071 WS6): `len` of a pinned
/// `bytes`. Negative return deopts (pin miss).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_bytes_len(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::Obj(Object::Bytes(b))) = ctx.pins.get(pin as usize) else {
        return -1;
    };
    b.len() as i64
}

/// The `wpjit_bytes_get` helper (RFC 0071 WS6): `bytes[i]` on a pinned
/// `bytes` (negative index normalized; out of range deopts and the
/// interpreter re-executes the subscript to raise the exact
/// `IndexError`). The byte lands in [`JitFrame::ret_bits`].
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_bytes_get(frame: *mut JitFrame, pin: i64, idx: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::Obj(Object::Bytes(b))) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let len = b.len() as i64;
    let i = if idx < 0 { idx + len } else { idx };
    if i < 0 || i >= len {
        return 1;
    }
    jf.ret_bits = u64::from(b[i as usize]);
    0
}

/// RFC 0073 WS2 — resolve a dict-helper call's pinned dict and its key
/// operand (`key_tag` selects the decoding: `Int` bits, or a `str`
/// pin). `None` = pin-table surprise (deopt).
fn dict_pin_and_key(
    ctx: &CallCtx,
    pin: i64,
    key_bits: i64,
    key_tag: i64,
) -> Option<(Rc<GilRefCell<DictData>>, Object)> {
    let Some(Pin::Obj(Object::Dict(d))) = ctx.pins.get(pin as usize) else {
        return None;
    };
    let d = d.clone();
    let key = if key_tag == weavepy_jit::DICT_KEY_STR {
        match ctx.pins.get(key_bits as usize) {
            Some(Pin::Obj(o @ Object::Str(_))) => o.clone(),
            _ => return None,
        }
    } else {
        Object::Int(key_bits)
    };
    Some((d, key))
}

/// RFC 0073 WS2 — the Python-free dict probe shared by the get and
/// contains helpers: the native lookup phase of
/// [`crate::builtins::dict_lookup`], *without* its reentrant retry —
/// a stored key that would need a Python `__eq__` (`deferred`, and not
/// natively found) reports `Err(())` so the caller deopts and the
/// interpreter runs the comparison with full semantics.
fn dict_probe_native(d: &Rc<GilRefCell<DictData>>, key: &Object) -> Result<Option<Object>, ()> {
    let (found, deferred) = crate::object::with_key_eq_deferred(|| {
        crate::object::key_cmp_scope(|| d.borrow().get(&DictKey(key.clone())).cloned())
    });
    match found {
        Ok(Some(v)) => Ok(Some(v)),
        Ok(None) if deferred => Err(()),
        Ok(None) => Ok(None),
        Err(_) => Err(()),
    }
}

/// The `wpjit_dict_get` helper (RFC 0073 WS2): `d[k]` on a pinned
/// exact dict. Returns `0` with the value's bits in
/// [`JitFrame::ret_bits`] (per `val_tag`; an object value pins), `1`
/// to deopt (pin/lane surprise, a comparison that would need Python,
/// pin-cap pressure), or `2` with the exact `KeyError(key)` parked in
/// the activation's `raised` slot — a missing key is control flow, not
/// a deopt. Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_dict_get(
    frame: *mut JitFrame,
    pin: i64,
    key_bits: i64,
    key_tag: i64,
    val_tag: i64,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some((d, key)) = dict_pin_and_key(ctx, pin, key_bits, key_tag) else {
        return 1;
    };
    let found = match dict_probe_native(&d, &key) {
        Ok(f) => f,
        Err(()) => return 1,
    };
    let Some(v) = found else {
        ctx.raised = Some(crate::error::key_error_object(key));
        return 2;
    };
    match (val_tag, &v) {
        (weavepy_jit::DICT_VAL_INT, Object::Int(i)) => {
            jf.ret_bits = *i as u64;
            0
        }
        (weavepy_jit::DICT_VAL_FLOAT, Object::Float(f)) => {
            jf.ret_bits = f.to_bits();
            0
        }
        (weavepy_jit::DICT_VAL_OBJ, Object::None) => {
            jf.ret_bits = u64::MAX;
            0
        }
        (weavepy_jit::DICT_VAL_OBJ, Object::Instance(_)) => {
            if ctx.pins.len() >= RUNTIME_PIN_CAP {
                return 1;
            }
            jf.ret_bits = ctx.pins.len() as u64;
            ctx.pins.push(Pin::Obj(v));
            0
        }
        _ => 1,
    }
}

/// The `wpjit_dict_set` helper (RFC 0073 WS2): `d[k] = v` on a pinned
/// exact dict, with the value pre-staged in [`JitFrame::ret_bits`]
/// (per `val_tag`). The store goes through the interpreter's own
/// [`crate::builtins::dict_insert`] chokepoint, so PEP 509 / watcher
/// discipline is identical to the tier-1 `StoreSubscrDict` cache. A
/// displaced value that would run the prompt-reap cascade deopts
/// *before* the store (the `wpjit_attr_set` discipline — the generic
/// path performs the store and the reap); active C-API dict watchers
/// and any surprise deopt too. The pre-store probe guarantees the
/// insert stays Python-free.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_dict_set(
    frame: *mut JitFrame,
    pin: i64,
    key_bits: i64,
    key_tag: i64,
    val_tag: i64,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    if crate::capi_watchers::dicts_active() {
        return 1;
    }
    let v = match val_tag {
        weavepy_jit::DICT_VAL_INT => Object::Int(jf.ret_bits as i64),
        weavepy_jit::DICT_VAL_FLOAT => Object::Float(f64::from_bits(jf.ret_bits)),
        weavepy_jit::DICT_VAL_OBJ => {
            if jf.ret_bits == u64::MAX {
                Object::None
            } else {
                match ctx.pins.get(jf.ret_bits as usize) {
                    Some(Pin::Obj(o)) => o.clone(),
                    _ => return 1,
                }
            }
        }
        _ => return 1,
    };
    let Some((d, key)) = dict_pin_and_key(ctx, pin, key_bits, key_tag) else {
        return 1;
    };
    // Python-free pre-probe: displaced-value discipline + deferral
    // detection (a deferral means the insert below could run Python).
    let old = match dict_probe_native(&d, &key) {
        Ok(f) => f,
        Err(()) => return 1,
    };
    if let Some(old) = &old {
        if !matches!(
            old,
            Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
        ) && super::Interpreter::local_needs_prompt_reap(old)
            && super::Interpreter::looks_reapable_temporary(old)
        {
            return 1;
        }
    }
    // The displaced value (probed non-reapable above) drops by plain
    // refcount, exactly like the no-op arm of the interpreter's
    // prompt-reap check.
    match crate::builtins::dict_insert(&d, key, v) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

/// The `wpjit_dict_contains` helper (RFC 0073 WS2): `k in d` on a
/// pinned exact dict. Returns `0` with the `bool` in
/// [`JitFrame::ret_bits`] (negation is native), or `1` to deopt (pin
/// surprise, or a membership answer that would need a Python
/// `__eq__`). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_dict_contains(
    frame: *mut JitFrame,
    pin: i64,
    key_bits: i64,
    key_tag: i64,
    _val_tag: i64,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some((d, key)) = dict_pin_and_key(ctx, pin, key_bits, key_tag) else {
        return 1;
    };
    match dict_probe_native(&d, &key) {
        Ok(found) => {
            jf.ret_bits = u64::from(found.is_some());
            0
        }
        Err(()) => 1,
    }
}

/// The `wpjit_const_str` helper (RFC 0073 WS2): materialize the
/// activation's code-object `str` constant `idx` as an exact-`str`
/// pin, memoized per `(activation, idx)` — a loop re-executing the
/// `LOAD_CONST` reuses one pin, so the pin table stays bounded.
/// Returns the pin index or a negative value to deopt (cap pressure,
/// or a defensive constant-shape miss). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]. `ctx.code_ptr` stays alive for
/// the whole activation (the entering frame / native-callee entry
/// holds the `Rc`).
unsafe extern "C" fn wpjit_const_str(frame: *mut JitFrame, idx: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let idx = idx as u32;
    if let Some(&(_, pin)) = ctx.const_pins.iter().find(|&&(i, _)| i == idx) {
        return pin as i64;
    }
    if ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -1;
    }
    // SAFETY: per the function contract, the activation keeps its code
    // object alive.
    let code = unsafe { &*ctx.code_ptr };
    let Some(weavepy_compiler::Constant::Str(s)) = code.constants.get(idx as usize) else {
        return -1;
    };
    let obj = Object::from_str(s.clone());
    let pin = ctx.pins.len() as u64;
    ctx.pins.push(Pin::Obj(obj));
    ctx.const_pins.push((idx, pin));
    pin as i64
}

/// The `wpjit_dict_iter_new` helper (RFC 0073 WS2): materialize the
/// pinned exact dict's *real* `DictKeys` iterator — the same object
/// (and the same creation-time length snapshot for the mutation
/// guard) the interpreter's `GET_ITER` builds — and answer its fresh
/// pin. The loop then steps through `wpjit_iter_next`'s checked
/// iterator step, so a structural mutation raises the exact CPython
/// `RuntimeError`, and a mid-loop deopt re-inserts this very iterator
/// on the rebuilt stack. Returns a negative value to deopt (pin
/// surprise, cap pressure). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_dict_iter_new(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::Obj(o @ Object::Dict(_))) = ctx.pins.get(pin as usize) else {
        return -1;
    };
    if ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -1;
    }
    let Ok(it) = o.make_iter() else {
        return -1;
    };
    let idx = ctx.pins.len() as i64;
    ctx.pins
        .push(Pin::Obj(Object::Iter(Rc::new(crate::sync::RefCell::new(
            it,
        )))));
    idx
}

/// The `wpjit_dict_len` helper (RFC 0073 WS2): `len(d)` on a pinned
/// exact dict. Returns the length, or a negative value to deopt (pin
/// miss). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_dict_len(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(Object::Dict(d))) => d.borrow().len() as i64,
        _ => -1,
    }
}

/// The `wpjit_list_next` helper (RFC 0071 WS4): one step of a
/// [`weavepy_jit::TTerm`]`::ForList` loop. Re-checks the index against
/// the *live* length (mutation during iteration is defined behavior)
/// and re-validates the element lane per step. Returns `0` with the
/// element's lane bits in [`JitFrame::ret_bits`] (an instance element
/// pins; `None` rides as `-1`), `1` on exhaustion, `2` to deopt at the
/// header (element-shape surprise or pin-cap pressure). Never runs
/// Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_list_next(frame: *mut JitFrame, pin: i64, idx: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // Scoped so the list borrow ends before an object element pins.
    let outcome: Result<u64, Object> = {
        let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
            return 2;
        };
        let items = list.borrow();
        if idx < 0 {
            return 2;
        }
        let Some(v) = items.get(idx as usize) else {
            return 1;
        };
        match (v, elem) {
            (Object::Int(v), JitType::Int) => Ok(*v as u64),
            (Object::Float(f), JitType::Float) => Ok(f.to_bits()),
            (Object::Bool(b), JitType::Bool) => Ok(u64::from(*b)),
            (Object::None, JitType::Obj) => Ok(u64::MAX),
            // RFC 0073 WS3 — exact-`str` elements pin on the object
            // lane (a `split` result's `ForList` consumer).
            (v @ (Object::Instance(_) | Object::Str(_)), JitType::Obj) => Err(v.clone()),
            _ => return 2,
        }
    };
    match outcome {
        Ok(bits) => {
            jf.ret_bits = bits;
            0
        }
        Err(obj) => {
            if ctx.pins.len() >= RUNTIME_PIN_CAP {
                return 2;
            }
            jf.ret_bits = ctx.pins.len() as u64;
            ctx.pins.push(Pin::Obj(obj));
            0
        }
    }
}

/// The `wpjit_get_iter` helper (RFC 0071 WS4): admit a pinned object
/// as the iterator of an opaque `for` loop. Only *identity iterables*
/// (`iter(x) is x` — generators and builtin iterators) qualify, so the
/// erased `GET_ITER` is a no-op and the pin doubles as the iterator.
/// Anything else returns non-zero (deopt: the interpreter executes the
/// `GET_ITER` — and the loop — generically, including `__iter__`
/// dispatch on instances). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_get_iter(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(Object::Generator(_) | Object::Iter(_))) => 0,
        _ => 1,
    }
}

/// The `wpjit_iter_next` helper (RFC 0071 WS4): one step of a
/// [`weavepy_jit::TTerm`]`::ForIter` loop over a pinned identity
/// iterable. **Runs Python code** for a generator source (the resume
/// executes the generator body — possibly natively, through
/// `try_enter_resume`), so burned-in resolutions are revalidated after
/// a dirty step; builtin iterators step without running Python and
/// skip the revalidation. Statuses per [`weavepy_jit::IterNextHelper`]:
/// `0` element in the lane, `1` exhausted, `2` deopt at the header
/// (nothing consumed), `3` element consumed but outside the lane (raw
/// object pinned, resume at the fused store), `4` raised.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_iter_next(frame: *mut JitFrame, pin: i64, elem_tag: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` that entered native code is
    // dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // The loop's poll point (the header's countdown poll covers the
    // native back edge; this covers the Python the step may run):
    // pending interpreter work and active observers route the loop
    // through the interpreter.
    crate::gil::yield_checkpoint();
    if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
        return 2;
    }
    let it = match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(o @ (Object::Generator(_) | Object::Iter(_)))) => o.clone(),
        _ => return 2,
    };
    // A builtin-iterator step is pure native code; everything else
    // (generator resume) runs arbitrary Python on behalf of this
    // activation.
    let runs_python = !matches!(it, Object::Iter(_));
    if runs_python {
        ctx.dirty = true;
    }
    match interp.iter_next(&it, &ctx.globals) {
        Err(err) => {
            ctx.raised = Some(err);
            4
        }
        Ok(None) => 1,
        Ok(Some(v)) => {
            // The step may have rebound a burned-in global (generator
            // bodies are arbitrary Python); the *next* burned
            // operation would then be wrong — surrender the consumed
            // element through the store-pc deopt.
            let still_valid = !runs_python
                || guards_hold(
                    interp,
                    &ctx.globals,
                    &ctx.builtins,
                    &ctx.guard_snapshot,
                    &ctx.callees,
                    &ctx.math,
                );
            if still_valid {
                // RFC 0073 WS2 — the exact-`str` element lane (dict-keys
                // loops): pin str elements; anything else surrenders
                // through the store-pc deopt below.
                let packed = if elem_tag == weavepy_jit::ITER_ELEM_STR {
                    match &v {
                        Object::Str(_) if ctx.pins.len() < RUNTIME_PIN_CAP => {
                            ctx.pins.push(Pin::Obj(v.clone()));
                            Some((ctx.pins.len() - 1) as u64)
                        }
                        _ => None,
                    }
                } else {
                    match SlotTag::from_raw(elem_tag as u32) {
                        SlotTag::Int => pack(&v, JitType::Int),
                        SlotTag::Float => pack(&v, JitType::Float),
                        SlotTag::Bool => pack(&v, JitType::Bool),
                        SlotTag::ObjPin => obj_ret_bits(&v, &mut ctx.pins),
                        _ => None,
                    }
                };
                if let Some(bits) = packed {
                    jf.ret_bits = bits;
                    return 0;
                }
            }
            // Consumed but unrepresentable in the compiled lane (or
            // the guards fell): pin the raw element (`None` rides the
            // nullable `-1`) and resume interpreted at the fused
            // store, which consumes it exactly once.
            jf.ret_bits = if matches!(v, Object::None) {
                u64::MAX
            } else {
                ctx.pins.push(Pin::Obj(v));
                (ctx.pins.len() - 1) as u64
            };
            3
        }
    }
}

/// Box one marshaled element by its [`SlotTag`] against the pin
/// table (RFC 0073 WS1 — the mixed-lane staging shared by
/// `wpjit_build_list` and `wpjit_build_tuple`). `None` on a tag the
/// literal lanes never produce (a compiler invariant break — the
/// caller deopts defensively).
fn boxed_element(ctx: &CallCtx, bits: u64, tag: u32) -> Option<Object> {
    Some(match SlotTag::from_raw(tag) {
        SlotTag::Int => Object::Int(bits as i64),
        SlotTag::Float => Object::Float(f64::from_bits(bits)),
        SlotTag::Bool => Object::Bool(bits != 0),
        // The object lane: `-1` is the nullable `None`; any pin
        // (instance, str/bytes, nested list) resolves to its real
        // object.
        SlotTag::ObjPin if bits == u64::MAX => Object::None,
        SlotTag::ObjPin | SlotTag::ListPin => ctx.pins.get(bits as usize)?.to_object(),
        _ => return None,
    })
}

/// The `wpjit_build_list` helper (RFC 0071 WS4): build a fresh list
/// from `n` elements staged in the marshal buffer (uniform lane per
/// `elem_tag`; `none_fill` writes `n` `None`s from an empty buffer),
/// pin it, and answer the pin index — negative deopts (cap pressure,
/// or a defensive shape miss). RFC 0073 WS1 — a negative `elem_tag`
/// reads per-element tags from the marshal tag buffer (a mixed-lane
/// literal) and pins the result on the object-element lane. Never
/// runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`] — `n` marshal entries (and,
/// for a negative `elem_tag`, their tags) are initialized unless
/// `none_fill`.
unsafe extern "C" fn wpjit_build_list(
    frame: *mut JitFrame,
    n: i64,
    elem_tag: i64,
    none_fill: i64,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    if ctx.pins.len() >= RUNTIME_PIN_CAP || n < 0 {
        return -1;
    }
    let n = n as usize;
    let elem = if elem_tag < 0 {
        JitType::Obj
    } else {
        match SlotTag::from_raw(elem_tag as u32) {
            SlotTag::Int => JitType::Int,
            SlotTag::Float => JitType::Float,
            SlotTag::ObjPin => JitType::Obj,
            _ => return -1,
        }
    };
    let items: Vec<Object> = if none_fill != 0 {
        vec![Object::None; n]
    } else {
        let mut out = Vec::with_capacity(n);
        for j in 0..n {
            // SAFETY: per the function contract, `n` marshaled
            // entries are live.
            let bits = unsafe { *jf.call_args.add(j) };
            let obj = if elem_tag < 0 {
                // SAFETY: mixed staging writes a tag per entry.
                let tag = unsafe { *jf.call_tags.add(j) };
                match boxed_element(ctx, bits, tag) {
                    Some(o) => o,
                    None => return -1,
                }
            } else {
                match elem {
                    JitType::Int => Object::Int(bits as i64),
                    JitType::Float => Object::Float(f64::from_bits(bits)),
                    // The object lane: `-1` is the nullable `None`;
                    // any pin resolves to its real object.
                    JitType::Obj if bits == u64::MAX => Object::None,
                    JitType::Obj => match ctx.pins.get(bits as usize) {
                        Some(p) => p.to_object(),
                        None => return -1,
                    },
                    _ => return -1,
                }
            };
            out.push(obj);
        }
        out
    };
    let list = Rc::new(crate::sync::RefCell::new(items));
    // RFC 0073 WS1 — the interpreter tracks *every* list it builds
    // (`gc.is_tracked([])` is True and any list can close a cycle by
    // later mutation); a natively built list is no different, and a
    // comprehension accumulator in particular always escapes.
    crate::gc_trace::track(Object::List(list.clone()));
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::List(list, elem));
    idx
}

/// The `wpjit_build_tuple` helper (RFC 0073 WS1): build a fresh tuple
/// from `n` per-element-tagged marshal entries, pin it on the object
/// lane, and answer the pin index — negative deopts (cap pressure or
/// a defensive shape miss). Fresh tuples are *not* GC-tracked,
/// matching the interpreter's `BUILD_TUPLE` (immutable; refcount
/// suffices unless a cycle-closing container is born inside, which
/// the element lanes cannot express). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`] — `n` marshal entries and their
/// tags are initialized.
unsafe extern "C" fn wpjit_build_tuple(frame: *mut JitFrame, n: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    if ctx.pins.len() >= RUNTIME_PIN_CAP || n < 0 {
        return -1;
    }
    let n = n as usize;
    let mut items = Vec::with_capacity(n);
    for j in 0..n {
        // SAFETY: per the function contract, `n` marshaled entries
        // and tags are live.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        match boxed_element(ctx, bits, tag) {
            Some(o) => items.push(o),
            None => return -1,
        }
    }
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::Obj(Object::new_tuple(items)));
    idx
}

/// Resolve a pin to its exact-`str` payload (RFC 0073 WS3). `None`
/// on any surprise — the callers deopt.
fn pin_str(ctx: &CallCtx, pin: i64) -> Option<Rc<str>> {
    match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(Object::Str(s))) => Some(s.clone()),
        _ => None,
    }
}

/// The `wpjit_str_concat` helper (RFC 0073 WS3): guarded exact-`str`
/// `+`. Allocates the joined `Rc<str>` and pins it — the same
/// allocation the interpreter's `BinOpAddStr` fast path performs,
/// with fewer dispatches. Negative deopts (pin surprise, cap
/// pressure). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_str_concat(frame: *mut JitFrame, a: i64, b: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let (Some(sa), Some(sb)) = (pin_str(ctx, a), pin_str(ctx, b)) else {
        return -1;
    };
    if ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -1;
    }
    let mut joined = String::with_capacity(sa.len() + sb.len());
    joined.push_str(&sa);
    joined.push_str(&sb);
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::Obj(Object::from_str(joined)));
    idx
}

/// The `wpjit_str_get` helper (RFC 0073 WS3): `s[i]` on a pinned
/// exact `str` — the tier-1 `SubscrStrInt` discipline: O(1) byte
/// indexing on an ASCII payload only, single-codepoint result pinned.
/// Negative deopts (non-ASCII receiver, out-of-range index — the
/// interpreter's re-execution raises the exact `IndexError` — pin
/// surprise, cap pressure). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_str_get(frame: *mut JitFrame, pin: i64, idx: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(s) = pin_str(ctx, pin) else {
        return -1;
    };
    if !s.is_ascii() || ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -1;
    }
    let len = s.len() as i64;
    let i = if idx < 0 { idx + len } else { idx };
    if i < 0 || i >= len {
        return -1;
    }
    let i = i as usize;
    let ch = Object::from_str(&s[i..=i]);
    let out = ctx.pins.len() as i64;
    ctx.pins.push(Pin::Obj(ch));
    out
}

/// The `wpjit_build_string` helper (RFC 0073 WS3): `BUILD_STRING n` —
/// concatenate `n` `str` pins staged in order in the marshal buffer
/// and pin the joined string. Negative deopts (pin surprise, cap
/// pressure). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`] — `n` marshal entries are
/// initialized (all `str` pins; the analyzer enforced the lanes).
unsafe extern "C" fn wpjit_build_string(frame: *mut JitFrame, n: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    if ctx.pins.len() >= RUNTIME_PIN_CAP || n < 0 {
        return -1;
    }
    let mut parts = Vec::with_capacity(n as usize);
    for j in 0..n as usize {
        // SAFETY: per the function contract, `n` marshaled entries are
        // live.
        let bits = unsafe { *jf.call_args.add(j) };
        match pin_str(ctx, bits as i64) {
            Some(s) => parts.push(s),
            None => return -1,
        }
    }
    let total: usize = parts.iter().map(|s| s.len()).sum();
    let mut joined = String::with_capacity(total);
    for s in &parts {
        joined.push_str(s);
    }
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::Obj(Object::from_str(joined)));
    idx
}

/// The `wpjit_build_map` helper (RFC 0073 WS2): build a fresh dict
/// from `n` key/value pairs staged interleaved (`k1, v1, …`) in the
/// marshal buffer with per-slot tags, pin it, and answer the pin
/// index — negative deopts (cap pressure, or a key outside the exact
/// `str`/`int` lanes). The fresh dict is GC-tracked, exactly like the
/// interpreter's `BUILD_MAP` (any dict can close a cycle by later
/// mutation). Duplicate keys keep the last value (CPython literal
/// semantics — a plain replace on a fresh unwatched dict). Never runs
/// Python code: exact `str`/`int` keys never defer to a Python
/// `__eq__`.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`] — `2n` marshal entries and
/// their tags are initialized.
unsafe extern "C" fn wpjit_build_map(frame: *mut JitFrame, n: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    if ctx.pins.len() >= RUNTIME_PIN_CAP || n < 0 {
        return -1;
    }
    let n = n as usize;
    let mut d = DictData::default();
    for p in 0..n {
        // SAFETY: per the function contract, `2n` marshaled entries
        // and tags are live.
        let (kbits, ktag) = unsafe { (*jf.call_args.add(2 * p), *jf.call_tags.add(2 * p)) };
        let (vbits, vtag) = unsafe { (*jf.call_args.add(2 * p + 1), *jf.call_tags.add(2 * p + 1)) };
        let Some(k) = boxed_element(ctx, kbits, ktag) else {
            return -1;
        };
        if !matches!(k, Object::Str(_) | Object::Int(_)) {
            return -1;
        }
        let Some(v) = boxed_element(ctx, vbits, vtag) else {
            return -1;
        };
        d.insert(DictKey(k), v);
    }
    let obj = Object::Dict(Rc::new(GilRefCell::new(d)));
    // The interpreter tracks every dict it builds; a natively built
    // dict is no different.
    crate::gc_trace::track(obj.clone());
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::Obj(obj));
    idx
}

/// The `wpjit_list_repeat` helper (RFC 0071 WS4): `list * int` on a
/// pinned list — element handles cloned (CPython's aliasing), the
/// fresh list pinned on the same element lane. Negative deopts (cap
/// pressure or an absurd size, which the interpreter turns into the
/// exact `MemoryError`/`OverflowError`). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_list_repeat(frame: *mut JitFrame, pin: i64, count: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    if ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -1;
    }
    let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
        return -1;
    };
    let elem = *elem;
    let items = list.borrow();
    let reps = usize::try_from(count).unwrap_or(0);
    let Some(total) = items.len().checked_mul(reps) else {
        return -1;
    };
    // A repeat the interpreter would refuse (or that would exhaust
    // memory) deopts instead of allocating here.
    if total > (isize::MAX as usize) / size_of::<Object>() {
        return -1;
    }
    let mut out = Vec::with_capacity(total);
    for _ in 0..reps {
        out.extend(items.iter().cloned());
    }
    drop(items);
    let fresh = Rc::new(crate::sync::RefCell::new(out));
    // See `wpjit_build_list`: every built list is GC-tracked.
    crate::gc_trace::track(Object::List(fresh.clone()));
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::List(fresh, elem));
    idx
}

/// The `wpjit_list_slice` helper (RFC 0071 WS4): `xs[a:b]` (unit
/// step) on a pinned list. Bounds clamp CPython-style (negative
/// bounds add `len`, then clamp to `[0, len]`); `i64::MIN` marks an
/// absent bound. The fresh list pins on the source's element lane.
/// Negative deopts (cap pressure). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_list_slice(
    frame: *mut JitFrame,
    pin: i64,
    start: i64,
    stop: i64,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    if ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -1;
    }
    let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
        return -1;
    };
    let elem = *elem;
    let items = list.borrow();
    let len = items.len() as i64;
    let clamp = |b: i64, absent: i64| {
        if b == i64::MIN {
            absent
        } else if b < 0 {
            (b + len).clamp(0, len)
        } else {
            b.min(len)
        }
    };
    let a = clamp(start, 0);
    let b = clamp(stop, len);
    let out: Vec<Object> = if a < b {
        items[a as usize..b as usize].to_vec()
    } else {
        Vec::new()
    };
    drop(items);
    let fresh = Rc::new(crate::sync::RefCell::new(out));
    // See `wpjit_build_list`: every built list is GC-tracked.
    crate::gc_trace::track(Object::List(fresh.clone()));
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::List(fresh, elem));
    idx
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
    // Scoped so the receiver borrow of `ctx.pins` ends before an
    // object-lane result appends a fresh pin (RFC 0070 WS1).
    let outcome: Result<u64, Object> = {
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
        // RFC 0070 WS1 — the nullable object lane: `None` is the
        // machine value `-1`; an instance value gets a fresh runtime
        // pin. Any other value drifted from the compiled lane and
        // deopts.
        let classify = |v: &Object| -> Option<Result<u64, Object>> {
            match (g.lane, v) {
                (JitType::Obj, Object::None) => Some(Ok(u64::MAX)),
                (JitType::Obj, Object::Instance(_)) => Some(Err(v.clone())),
                (JitType::Obj, _) => None,
                // RFC 0071 WS6 — `str`/`bytes` read lanes pin the
                // value; a drifted type deopts like any lane miss.
                (JitType::Str, Object::Str(_)) | (JitType::Bytes, Object::Bytes(_)) => {
                    Some(Err(v.clone()))
                }
                (JitType::Str | JitType::Bytes, _) => None,
                _ => pack(v, g.lane).map(Ok),
            }
        };
        match g.storage {
            AttrStorage::Slot => {
                // RFC 0070 WS3 — a `__slots__` member: read the slot
                // side table by name (an unset slot deopts; the
                // interpreter raises the faithful AttributeError).
                let Some(v) = inst.slot_get(&g.name) else {
                    return 1;
                };
                match classify(&v) {
                    Some(o) => o,
                    None => return 1,
                }
            }
            AttrStorage::Indexed(key_idx) => {
                let dict = inst.dict.borrow();
                match dict.get_index(key_idx as usize) {
                    Some((k, v)) if key_is(k, &g.name) => match classify(v) {
                        Some(o) => o,
                        None => return 1,
                    },
                    _ => return 1,
                }
            }
            // A new-key fingerprint is a store-only shape.
            AttrStorage::NewKey => return 1,
        }
    };
    match outcome {
        Ok(bits) => {
            jf.ret_bits = bits;
            0
        }
        Err(obj) => {
            // Runtime pins are append-only and capped: a table at the
            // cap deopts (the activation exits; a fresh entry starts
            // with a fresh table), trading a re-entry for boundedness.
            if ctx.pins.len() >= RUNTIME_PIN_CAP {
                return 1;
            }
            jf.ret_bits = ctx.pins.len() as u64;
            ctx.pins.push(Pin::Obj(obj));
            0
        }
    }
}

/// The `wpjit_attr_set` helper (RFC 0065 WS5 / RFC 0070 WS1):
/// overwrite one attribute of a pinned instance (value pre-staged in
/// [`JitFrame::ret_bits`], interpreted per the site's lane — for the
/// object lane the bits are a pin index, or `-1` for `None`) under the
/// same guards as [`wpjit_attr_get`], plus one more: a *displaced*
/// heap value that looks like a dying temporary must run the
/// interpreter's prompt-reap cascade (`__del__`, weakref finalizers),
/// so that case deopts *before* the store and the generic path
/// re-executes it. Any other displaced value drops by plain refcount
/// exactly as `maybe_prompt_reap_replaced` would.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_attr_set(frame: *mut JitFrame, pin: i64, site: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(g) = ctx.attr_guards.get(site as usize) else {
        return 1;
    };
    let v = match g.lane {
        JitType::Int => Object::Int(jf.ret_bits as i64),
        JitType::Float => Object::Float(f64::from_bits(jf.ret_bits)),
        JitType::Bool => Object::Bool(jf.ret_bits != 0),
        // RFC 0070 WS1 — the object lane: resolve the staged pin
        // (or the `-1` `None`) back into the real object.
        JitType::Obj => {
            if jf.ret_bits == u64::MAX {
                Object::None
            } else {
                match ctx.pins.get(jf.ret_bits as usize) {
                    Some(Pin::Obj(o)) => o.clone(),
                    _ => return 1,
                }
            }
        }
        _ => return 1,
    };
    let Some(Pin::Obj(Object::Instance(inst))) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let guard_ok = {
        let cls = inst.class.borrow();
        crate::specialize::rc_id(&cls) == g.type_id && cls.attr_version.get() == g.ver
    };
    if !guard_ok {
        return 1;
    }
    match g.storage {
        // RFC 0070 WS3 — a `__slots__` member: write the slot side
        // table by name. Tier-1's `StoreAttrSlot` (like the generic
        // `member_set`) neither gc-tracks nor prompt-reaps the
        // displaced slot value, so the in-place overwrite is exactly
        // faithful.
        AttrStorage::Slot => {
            inst.slot_set(&g.name, v);
            0
        }
        AttrStorage::Indexed(key_idx) => {
            let mut dict = inst.dict.borrow_mut();
            let Some((k, dst)) = dict.get_index_mut(key_idx as usize) else {
                return 1;
            };
            if !key_is(k, &g.name) {
                return 1;
            }
            // RFC 0070 WS1 — the displaced value: if the interpreter's
            // store would run the prompt-reap cascade on it (a
            // finalizable temporary losing its last binding), deopt
            // *before* storing so the generic path performs the store
            // and the reap; otherwise the overwrite drops it by plain
            // refcount, exactly like the no-op arm of
            // `maybe_prompt_reap_replaced`.
            if !matches!(
                dst,
                Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
            ) && super::Interpreter::local_needs_prompt_reap(dst)
                && super::Interpreter::looks_reapable_temporary(dst)
            {
                return 1;
            }
            let old = std::mem::replace(dst, v);
            drop(dict);
            drop(old);
            0
        }
        // RFC 0071 WS2 — the constructor-pattern store: a single-probe
        // insert-or-replace, exactly the tier-1 `StoreAttrNewKey`
        // execution. Watched instance dicts deopt so the generic path
        // fires the exact watcher events.
        AttrStorage::NewKey => {
            if crate::capi_watchers::dicts_active() {
                return 1;
            }
            let mut dict = inst.dict.borrow_mut();
            let old = if let Some(dst) = dict.get_mut(&StrKey(&g.name)) {
                // The displaced-value discipline of the indexed arm.
                if !matches!(
                    dst,
                    Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
                ) && super::Interpreter::local_needs_prompt_reap(dst)
                    && super::Interpreter::looks_reapable_temporary(dst)
                {
                    return 1;
                }
                Some(std::mem::replace(dst, v))
            } else {
                // Same interning contract as the slow path
                // (`generic_setattr_instance`) and the tier-1 cache.
                dict.insert(
                    crate::object::DictKey(crate::stdlib::sys::intern_name(&g.name)),
                    v,
                );
                None
            };
            drop(dict);
            drop(old);
            0
        }
    }
}

/// RFC 0074 WS1 — the `wpjit_global_obj` helper: pin the snapshotted
/// object behind obj-global table index `token`, memoized per
/// `(activation, token)` like `wpjit_const_str`. The identity guard
/// (validated at entry and after every dirty call) makes the table
/// entry exact, so the helper never re-resolves the name. `-2` deopts
/// (cap pressure, or a defensive table miss). Never runs Python code.
///
/// A `None` snapshot answers `-1` — the object lane's nullable
/// encoding — **not** a pin: a pinned `None` reads as non-null to the
/// native `IsNone` fence, so `if _global is None:` compiled to the
/// wrong branch the moment the function tiered up (torch's
/// `_cupti_monitor.push_user_annotation`, RFC 0076 WS5). The emitter
/// guards on `< -1` accordingly.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_global_obj(frame: *mut JitFrame, token: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let token = token as u32;
    if let Some(&(_, pin)) = ctx.obj_global_pins.iter().find(|&&(t, _)| t == token) {
        return pin as i64;
    }
    let Some(obj) = ctx.obj_globals.get(token as usize).cloned() else {
        return -2;
    };
    if matches!(obj, Object::None) {
        return -1;
    }
    if ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -2;
    }
    let pin = ctx.pins.len() as u64;
    ctx.pins.push(Pin::Obj(obj));
    ctx.obj_global_pins.push((token, pin));
    pin as i64
}

/// Pin an arbitrary object-lane value (RFC 0074 WS2): `None` rides
/// the nullable `-1`; anything else gets a fresh runtime pin. `None`
/// (the Option) only on cap pressure.
fn pin_any(v: Object, pins: &mut PinTable) -> Option<u64> {
    if matches!(v, Object::None) {
        return Some(u64::MAX);
    }
    if pins.len() >= RUNTIME_PIN_CAP {
        return None;
    }
    pins.push(Pin::Obj(v));
    Some((pins.len() - 1) as u64)
}

/// RFC 0076 WS7 — the opaque-call lane's per-kind fast path: a callee
/// that is a compiled Python function, a fully-bound method over one,
/// or a class whose burned constructor plan carries a compiled
/// `__init__` enters natively (the `wpjit_call_py` native-to-native
/// machinery), skipping the interpreter core. The callee's own return
/// lane crosses the call, then re-stages as the dyn site's object pin
/// (the site types every result `Obj`). `None` = no native shape, or
/// the attempt declined (lane mismatch, guards, recursion) — the
/// caller pays the interpreter path, exactly as before.
///
/// # Safety
///
/// Same contract as [`wpjit_call_dyn`] — `jf`/`ctx` are the live,
/// exclusive buffers of the current native activation and `argc`
/// marshal entries are initialized.
unsafe fn try_dyn_native(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    callee: &Object,
    argc: u32,
) -> Option<i64> {
    let (nc, recv) = match callee {
        Object::Function(pf) => {
            let fcode = pf.code.borrow().clone();
            let nc = JIT.with(|c| c.borrow().resolve_native_func(pf, &fcode, false))?;
            (nc, None)
        }
        // A deferred special-method dispatch (`redispatch_descriptor`)
        // re-resolves `__get__` at call time — interpreter territory.
        Object::BoundMethod(bm) if !bm.redispatch_descriptor => {
            let Object::Function(pf) = &bm.function else {
                return None;
            };
            let fcode = pf.code.borrow().clone();
            let nc = JIT.with(|c| c.borrow().resolve_native_func(pf, &fcode, true))?;
            (nc, Some(bm.receiver.clone()))
        }
        Object::Type(t) => {
            // Mirror `resolve_native_callee`'s constructor arm: the
            // memoised instance plan must be current and carry a
            // plain-function `__init__`.
            let init = {
                let cached = t.instance_plan.borrow();
                let (ver, plan) = cached.as_ref()?.clone();
                if ver != t.attr_version.get() {
                    return None;
                }
                match plan.init_fn.as_ref() {
                    Some(Object::Function(f)) => f.clone(),
                    _ => return None,
                }
            };
            let fcode = init.code.borrow().clone();
            let nc = JIT.with(|c| c.borrow().resolve_native_callee(callee, &fcode))?;
            (nc, None)
        }
        _ => return None,
    };
    if nc.ctor.is_some() {
        // The constructor form allocates the instance and enters the
        // compiled `__init__`; the site's value is the instance pin.
        return unsafe { try_native_ctor(jf, ctx, interp, &nc, argc, SlotTag::ObjPin as u32) };
    }
    // Enter with the callee's *own* return lane (scalars cross the
    // call unboxed), then re-stage the result on the dyn site's
    // object pin. An `Obj`-lane return already rides the pin lane.
    let expect = if nc.cf.ret_none {
        SlotTag::None as u32
    } else {
        let tag = lane_tag(nc.cf.ret_lane?);
        if tag == u32::MAX {
            return None;
        }
        tag
    };
    let status = unsafe { try_native_call(jf, ctx, interp, &nc, argc, expect, recv.as_ref()) }?;
    if status == CallStatus::Ok as i64 && jf.ret_tag != SlotTag::ObjPin as u32 {
        let v = unpack(jf.ret_bits, jf.ret_tag);
        match pin_any(v.clone(), &mut ctx.pins) {
            Some(bits) => {
                jf.ret_bits = bits;
                jf.ret_tag = SlotTag::ObjPin as u32;
            }
            None => {
                // Cap pressure: the call completed — park and deopt
                // after it, never re-executing (the `Boxed` contract).
                ctx.parked = Some(v);
                return Some(CallStatus::Boxed as i64);
            }
        }
    }
    Some(status)
}

/// The `wpjit_call_dyn` helper (RFC 0074 WS2): call an arbitrary
/// pinned callee through the interpreter core with the `argc + kwc`
/// tag-staged arguments (keyword names from the interned constant
/// tuple `names` when `kwc > 0`). Arbitrary Python runs — the
/// activation goes dirty and burned-in resolutions are revalidated
/// after the call. Statuses per [`weavepy_jit::CallDynHelper`]:
/// `Ok` (pinned result in `ret_bits`, native execution continues),
/// `Raised`, `Boxed` (completed; parked result, deopt after the
/// call), `Reject` (defensive pin miss before any Python ran).
/// RFC 0076 WS7 — a compiled Python callee short-circuits through
/// [`try_dyn_native`] before any of that.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`] — `argc + kwc` marshal entries
/// and tags are initialized.
unsafe extern "C" fn wpjit_call_dyn(
    frame: *mut JitFrame,
    callee_pin: i64,
    argc: u32,
    kwc: u32,
    names: u32,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` that entered native code is
    // dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // The callee rode a pinned lane; `-1` is the nullable `None`
    // (calling `None` raises) — both misses re-execute generically.
    let callee = match ctx.pins.get(callee_pin as usize) {
        Some(p) => p.to_object(),
        None => return CallStatus::Reject as i64,
    };
    // RFC 0076 WS7 — the per-kind fast path: a compiled Python callee
    // (a function, a fully-bound method over one, a class whose
    // burned constructor plan carries a compiled `__init__`) enters
    // natively through the `wpjit_call_py` machinery; everything else
    // pays the interpreter core below. Keyword sites stay generic —
    // the kwnames binder is the interpreter's.
    if kwc == 0 {
        // SAFETY: per the function contract — same live buffers.
        if let Some(status) = unsafe { try_dyn_native(jf, ctx, interp, &callee, argc) } {
            return status;
        }
    }
    let n = (argc + kwc) as usize;
    let mut args: Vec<Object> = Vec::with_capacity(n);
    for j in 0..n {
        // SAFETY: native code wrote `argc + kwc` entries, and the
        // buffers are `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        args.push(unpack_pins(bits, tag, &ctx.pins));
    }
    // The keyword tail pairs with the interned names tuple (the plan
    // scan admitted only a non-empty all-`str` tuple constant).
    let mut kwargs: Vec<(String, Object)> = Vec::new();
    if kwc > 0 {
        // SAFETY: per the function contract, the activation keeps its
        // code object alive.
        let code = unsafe { &*ctx.code_ptr };
        let Some(weavepy_compiler::Constant::Tuple(items)) = code.constants.get(names as usize)
        else {
            return CallStatus::Reject as i64;
        };
        if items.len() != kwc as usize {
            return CallStatus::Reject as i64;
        }
        for (c, v) in items.iter().zip(args.split_off(argc as usize)) {
            let weavepy_compiler::Constant::Str(s) = c else {
                return CallStatus::Reject as i64;
            };
            kwargs.push((s.clone(), v));
        }
    }
    // Arbitrary Python runs on behalf of this activation (RFC 0067
    // WS1's dirtiness discipline).
    note_generic_dyn_call(ctx);
    ctx.dirty = true;
    let called = call_with_activation_shell(interp, ctx, jf, |i| {
        i.call_object_with_globals(&callee, &args, &kwargs, &ctx.globals)
    });
    match called {
        Err(err) => {
            ctx.raised = Some(err);
            CallStatus::Raised as i64
        }
        Ok(v) => {
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            );
            if still_valid {
                if let Some(bits) = pin_any(v.clone(), &mut ctx.pins) {
                    jf.ret_bits = bits;
                    jf.ret_tag = SlotTag::ObjPin as u32;
                    return CallStatus::Ok as i64;
                }
            }
            ctx.parked = Some(v);
            CallStatus::Boxed as i64
        }
    }
}

/// RFC 0076 WS7 follow-up — charge one generic interpreter round-trip
/// (a `wpjit_call_dyn` leg `try_dyn_native` refused) to the calling
/// activation's code entry. The retirement judgment itself lives in
/// [`note_native_exit`], where the per-entry denominator is bumped.
fn note_generic_dyn_call(ctx: &CallCtx) {
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        st.stats.dyn_generic_calls += 1;
        if let Some(ce) = st.cache.get_mut(&ctx.code_ptr) {
            ce.generic_dyn_calls = ce.generic_dyn_calls.saturating_add(1);
        }
    });
}

/// The attribute name behind `names` index `idx` of the activation's
/// code object (RFC 0074 WS2/WS4).
///
/// # Safety
///
/// `ctx.code_ptr` stays alive for the whole activation (the entering
/// frame / native-callee entry holds the `Rc`).
unsafe fn ctx_name(ctx: &CallCtx, idx: i64) -> Option<&str> {
    // SAFETY: per the function contract.
    let code = unsafe { &*ctx.code_ptr };
    code.names.get(idx as usize).map(String::as_str)
}

/// The `wpjit_dyn_attr_get` helper (RFC 0074 WS2/WS4): the
/// interpreter's exact attribute load on a pinned receiver (bound
/// methods materialize, descriptors and `__getattr__` run — arbitrary
/// Python; the dirtiness discipline applies). Statuses: `0` ok (fresh
/// pin in `ret_bits`), `1` raised, `2` completed but guards fell / cap
/// pressure (result parked; deopt at the *next* pc — never
/// re-executed), `3` rejected before any Python ran (deopt here,
/// re-execute generically).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_dyn_attr_get(frame: *mut JitFrame, pin: i64, name: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    let recv = match ctx.pins.get(pin as usize) {
        Some(p) => p.to_object(),
        // `-1` (the nullable `None`) loads attributes of `None` —
        // legitimate (`None.__class__`) but cold; re-execute.
        None => return 3,
    };
    // SAFETY: the activation keeps its code object alive.
    let Some(attr) = (unsafe { ctx_name(ctx, name) }) else {
        return 3;
    };
    let attr = attr.to_owned();
    ctx.dirty = true;
    match interp.load_attr_public(&recv, &attr) {
        Err(err) => {
            ctx.raised = Some(err);
            1
        }
        Ok(v) => {
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            );
            if still_valid {
                if let Some(bits) = pin_any(v.clone(), &mut ctx.pins) {
                    jf.ret_bits = bits;
                    return 0;
                }
            }
            ctx.parked = Some(v);
            2
        }
    }
}

/// The `wpjit_dyn_attr_set` helper (RFC 0074 WS4): the interpreter's
/// exact attribute store on a pinned receiver (`__setattr__` dispatch
/// included — arbitrary Python; the dirtiness discipline applies).
/// The value is staged tag-typed in `call_args[0]` / `call_tags[0]`.
/// Statuses as [`wpjit_dyn_attr_get`], with no result on ok (`Boxed`
/// means the store *completed* with invalidated guards — deopt at the
/// next pc, never re-executed).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`] — one marshal entry and tag are
/// initialized.
unsafe extern "C" fn wpjit_dyn_attr_set(frame: *mut JitFrame, pin: i64, name: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    let recv = match ctx.pins.get(pin as usize) {
        Some(p) => p.to_object(),
        None => return 3,
    };
    // SAFETY: the activation keeps its code object alive.
    let Some(attr) = (unsafe { ctx_name(ctx, name) }) else {
        return 3;
    };
    let attr = attr.to_owned();
    // SAFETY: native code staged the value in slot 0.
    let (bits, tag) = unsafe { (*jf.call_args, *jf.call_tags) };
    let value = unpack_pins(bits, tag, &ctx.pins);
    ctx.dirty = true;
    match interp.store_attr_public(&recv, &attr, value) {
        Err(err) => {
            ctx.raised = Some(err);
            1
        }
        Ok(()) => {
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            );
            if still_valid {
                0
            } else {
                2
            }
        }
    }
}

/// The `wpjit_truth` helper (RFC 0076 WS8): the interpreter's exact
/// truthiness on a pinned value ([`weavepy_jit::TOp`]'s `Truth`). The
/// pure kinds — the nullable `None` (`-1`, falsy without a lookup),
/// scalars, container emptiness, instances carrying neither `__bool__`
/// nor `__len__` — answer without dirty marking or guard
/// revalidation, since no Python runs. A dunder-bearing instance, a
/// foreign value (its `nb_bool` — a multi-element numpy array raises
/// "truth value ... is ambiguous"), or a mapping proxy dispatches the
/// full `obj_truthy` protocol: arbitrary Python may run, the
/// dirtiness discipline applies. Statuses as [`wpjit_dyn_attr_get`]
/// (`0` ok with the bool's bits in `ret_bits`; `2` parks the computed
/// bool and deopts at the *next* pc — the dunder never re-runs).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_truth(frame: *mut JitFrame, pin: i64, _reserved: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    if pin < 0 {
        jf.ret_bits = 0;
        return 0;
    }
    let v = match ctx.pins.get(pin as usize) {
        Some(p) => p.to_object(),
        None => return 3,
    };
    let pure = match &v {
        Object::Foreign(_) | Object::MappingProxyObj(_) => false,
        Object::Instance(_) => {
            // `NotImplemented` in a boolean context warns (arbitrary
            // Python through the warnings machinery).
            !v.is_same(&crate::vm_singletons::not_implemented())
                && crate::instance_method(&v, "__bool__").is_none()
                && crate::instance_method(&v, "__len__").is_none()
        }
        _ => true,
    };
    if pure {
        jf.ret_bits = u64::from(v.is_truthy());
        return 0;
    }
    ctx.dirty = true;
    match interp.obj_truthy(&v, &ctx.globals) {
        Err(err) => {
            ctx.raised = Some(err);
            1
        }
        Ok(b) => {
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            );
            if still_valid {
                jf.ret_bits = u64::from(b);
                0
            } else {
                // The dunder already ran — park the answer and deopt
                // after this pc, never re-executing it.
                ctx.parked = Some(Object::Bool(b));
                2
            }
        }
    }
}

/// The `wpjit_contains_dyn` helper (RFC 0076 WS8): the interpreter's
/// exact `in` protocol on a pinned container (`__contains__`, native
/// container tests, the iteration fallback — arbitrary Python may
/// run; the dirtiness discipline applies). The item is staged
/// tag-typed in `call_args[0]` / `call_tags[0]`; `negate` answers the
/// `not in` form. Statuses as [`wpjit_truth`] (`0` ok with the
/// already-negated bool's bits in `ret_bits`; `2` parks the computed
/// bool and deopts at the *next* pc — the protocol never re-runs).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`] — one marshal entry and tag are
/// initialized.
unsafe extern "C" fn wpjit_contains_dyn(frame: *mut JitFrame, pin: i64, negate: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // `x in None` raises — cold; re-execute generically for the exact
    // TypeError.
    let container = match ctx.pins.get(pin as usize) {
        Some(p) => p.to_object(),
        None => return 3,
    };
    // SAFETY: native code staged the item in slot 0.
    let (bits, tag) = unsafe { (*jf.call_args, *jf.call_tags) };
    let item = unpack_pins(bits, tag, &ctx.pins);
    ctx.dirty = true;
    match interp.py_contains(&container, &item) {
        Err(err) => {
            ctx.raised = Some(err);
            1
        }
        Ok(found) => {
            let b = found != (negate != 0);
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            );
            if still_valid {
                jf.ret_bits = u64::from(b);
                0
            } else {
                // The protocol already ran — park the answer and
                // deopt after this pc, never re-executing it.
                ctx.parked = Some(Object::Bool(b));
                2
            }
        }
    }
}

/// The `wpjit_build_set` helper (RFC 0076 WS8): build a fresh set from
/// `n` per-element-tagged entries staged in the marshal buffer, pin
/// it, and answer the pin index — negative deopts (cap pressure, or
/// an element whose hashing/de-duplication could run Python: only
/// scalars, `None`, and exact `str`/`bytes` admit; the interpreter
/// then re-executes the `BUILD_SET` generically, raising exactly for
/// the unhashable case). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`] — `n` marshal entries and tags
/// are initialized.
unsafe extern "C" fn wpjit_build_set(frame: *mut JitFrame, n: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    if ctx.pins.len() >= RUNTIME_PIN_CAP || n < 0 {
        return -1;
    }
    let n = n as usize;
    let mut items = Vec::with_capacity(n);
    for j in 0..n {
        // SAFETY: per the function contract, `n` marshaled entries
        // and tags are live.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        let obj = match boxed_element(ctx, bits, tag) {
            Some(o) => o,
            None => return -1,
        };
        // Hashing and de-duplication must never run Python here.
        if !matches!(
            obj,
            Object::Int(_)
                | Object::Float(_)
                | Object::Bool(_)
                | Object::None
                | Object::Str(_)
                | Object::Bytes(_)
        ) {
            return -1;
        }
        items.push(obj);
    }
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::Obj(Object::new_set_from(items)));
    idx
}

/// The `wpjit_iter_new` helper (RFC 0074 WS3): materialize `iter(x)`
/// for a pinned iterable and answer the fresh iterator's pin — the
/// materializing arm of `IterCapture`. Receivers whose `iter()` would
/// dispatch Python (`__iter__` on instances, metaclass `__iter__`,
/// object-backed mapping proxies) deopt *before* anything runs, so
/// the interpreter executes the `GET_ITER` — and its side effects —
/// exactly once, generically. Everything else builds the iterator
/// through the interpreter core without running Python. Negative
/// deopts (Python-dispatching receiver, non-iterable — the
/// re-execution raises exactly — cap pressure).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_iter_new(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    if ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -1;
    }
    let recv = match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(o)) => o.clone(),
        Some(Pin::List(l, _)) => Object::List(l.clone()),
        None => return -1,
    };
    // `make_iter` dispatches Python for exactly these receiver
    // shapes; a fresh compile would double `__iter__`'s side effects
    // on a post-hoc deopt, so they never enter the helper.
    if matches!(
        recv,
        Object::Instance(_) | Object::Type(_) | Object::MappingProxyObj(_)
    ) {
        return -1;
    }
    match interp.make_iter(&recv, &ctx.globals) {
        Ok(it) => {
            let idx = ctx.pins.len() as i64;
            ctx.pins.push(Pin::Obj(it));
            idx
        }
        Err(_) => -1,
    }
}

/// Pack one yielded element into a compiled loop-variable lane
/// (RFC 0074 WS3 — the [`weavepy_jit::ITER_ELEM_STR`]-aware sibling
/// of `wpjit_iter_next`'s packing). `None` = outside the lane.
fn pack_iter_elem(v: &Object, tag: i64, pins: &mut PinTable) -> Option<u64> {
    if tag == weavepy_jit::ITER_ELEM_STR {
        return match v {
            Object::Str(_) if pins.len() < RUNTIME_PIN_CAP => {
                pins.push(Pin::Obj(v.clone()));
                Some((pins.len() - 1) as u64)
            }
            _ => None,
        };
    }
    match SlotTag::from_raw(tag as u32) {
        SlotTag::Int => pack(v, JitType::Int),
        SlotTag::Float => pack(v, JitType::Float),
        SlotTag::Bool => pack(v, JitType::Bool),
        SlotTag::ObjPin => obj_ret_bits(v, pins),
        _ => None,
    }
}

/// The `wpjit_iter_next_pair` helper (RFC 0074 WS3): one step of a
/// [`weavepy_jit::TTerm`]`::ForIterPair` loop — advance the pinned
/// iterator through the interpreter core (**runs Python** for
/// generator sources) and unpack the yielded 2-tuple into the
/// compiled element lanes. Statuses per
/// [`weavepy_jit::IterNextPairHelper`]: `0` unpacked (`ret_bits` /
/// `call_args[0]`), `1` exhausted, `2` deopt at the header (nothing
/// consumed), `3` consumed but not a 2-tuple in the lanes (raw
/// element pinned; resume at the erased `UNPACK_SEQUENCE`), `4`
/// raised.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_iter_next_pair(
    frame: *mut JitFrame,
    pin: i64,
    tag1: i64,
    tag2: i64,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // The loop's poll point — see `wpjit_iter_next`.
    crate::gil::yield_checkpoint();
    if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
        return 2;
    }
    let it = match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(o @ (Object::Generator(_) | Object::Iter(_) | Object::LazyIter(_)))) => {
            o.clone()
        }
        _ => return 2,
    };
    // A builtin-iterator step is pure native code; generator resumes
    // (and lazy iterators, which may drive Python) run arbitrary
    // Python on behalf of this activation.
    let runs_python = !matches!(it, Object::Iter(_));
    if runs_python {
        ctx.dirty = true;
    }
    match interp.iter_next(&it, &ctx.globals) {
        Err(err) => {
            ctx.raised = Some(err);
            4
        }
        Ok(None) => 1,
        Ok(Some(v)) => {
            let still_valid = !runs_python
                || guards_hold(
                    interp,
                    &ctx.globals,
                    &ctx.builtins,
                    &ctx.guard_snapshot,
                    &ctx.callees,
                    &ctx.math,
                );
            if still_valid {
                // Exactly a 2-tuple unpacks in the lanes (the erased
                // `UNPACK_SEQUENCE 2` admits lists and general
                // iterables too — those surrender through the
                // store-pc deopt and unpack generically).
                if let Object::Tuple(items) = &v {
                    if items.len() == 2 {
                        let packed1 = pack_iter_elem(&items[0], tag1, &mut ctx.pins);
                        let packed2 =
                            packed1.and_then(|_| pack_iter_elem(&items[1], tag2, &mut ctx.pins));
                        if let (Some(b1), Some(b2)) = (packed1, packed2) {
                            jf.ret_bits = b1;
                            // SAFETY: the marshal buffer is at least
                            // one slot wide (`max_call_args.max(1)`).
                            unsafe {
                                *jf.call_args = b2;
                            }
                            return 0;
                        }
                    }
                }
            }
            // Consumed but not unpacked (or the guards fell): pin the
            // raw element and resume interpreted at the erased
            // `UNPACK_SEQUENCE`, which consumes it exactly once.
            jf.ret_bits = if matches!(v, Object::None) {
                u64::MAX
            } else {
                ctx.pins.push(Pin::Obj(v));
                (ctx.pins.len() - 1) as u64
            };
            3
        }
    }
}

/// The `wpjit_str_mod` helper (RFC 0074 WS5): `str % x` through the
/// interpreter's `%`-formatting core (`__str__`/`__repr__` of the
/// staged operand may run — the dirtiness discipline applies when it
/// can). Statuses per [`weavepy_jit::StrModHelper`]: `0` ok (fresh
/// exact-`str` pin in `ret_bits`), `1` raised, `2` completed but the
/// result surprised / guards fell / cap pressure (parked; deopt at
/// the next pc — formatting side effects never re-run), `3` rejected
/// before running.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_str_mod(
    frame: *mut JitFrame,
    lhs_pin: i64,
    rhs_bits: i64,
    rhs_tag: i64,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    let lhs = match ctx.pins.get(lhs_pin as usize) {
        Some(Pin::Obj(o @ Object::Str(_))) => o.clone(),
        _ => return 3,
    };
    let rhs = unpack_pins(rhs_bits as u64, rhs_tag as u32, &ctx.pins);
    // Scalar and exact-`str`/tuple-of-scalar operands format without
    // running Python; anything else may dispatch `__str__`/`__repr__`.
    let may_run_python = !matches!(
        rhs,
        Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::Str(_) | Object::None
    );
    if may_run_python {
        ctx.dirty = true;
    }
    match interp.percent_mod_left_slot(&lhs, &rhs, &ctx.globals) {
        Err(err) => {
            ctx.raised = Some(err);
            1
        }
        Ok(v) => {
            let still_valid = !may_run_python
                || guards_hold(
                    interp,
                    &ctx.globals,
                    &ctx.builtins,
                    &ctx.guard_snapshot,
                    &ctx.callees,
                    &ctx.math,
                );
            if still_valid {
                if matches!(v, Object::Str(_)) && ctx.pins.len() < RUNTIME_PIN_CAP {
                    jf.ret_bits = ctx.pins.len() as u64;
                    ctx.pins.push(Pin::Obj(v));
                    return 0;
                }
            }
            ctx.parked = Some(v);
            2
        }
    }
}

/// The `wpjit_str_slice` helper (RFC 0074 WS5): `s[a:b]` (unit step)
/// on a pinned exact `str` — the tier-1 `SubscrStrInt` discipline
/// extended to slices: O(1) byte slicing on an ASCII payload only
/// (code points and bytes coincide), CPython clamping, `i64::MIN` =
/// absent bound. Negative deopts (non-ASCII receiver — the generic
/// path slices by code point — pin surprise, cap pressure). Never
/// runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_str_slice(frame: *mut JitFrame, pin: i64, start: i64, stop: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(s) = pin_str(ctx, pin) else {
        return -1;
    };
    if !s.is_ascii() || ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -1;
    }
    let len = s.len() as i64;
    let clamp = |b: i64, absent: i64| {
        if b == i64::MIN {
            absent
        } else if b < 0 {
            (b + len).clamp(0, len)
        } else {
            b.min(len)
        }
    };
    let a = clamp(start, 0);
    let b = clamp(stop, len);
    let out = if a < b {
        &s[a as usize..b as usize]
    } else {
        ""
    };
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::Obj(Object::from_str(out)));
    idx
}

/// RFC 0069 WS3b — a frameless interpreter→native call. When the
/// tier-1 exact-arity call fast path targets a function whose code is
/// already tier-2 compiled and native-enterable, enter the compiled
/// body directly from the argument objects — no interpreter `Frame`,
/// no locals vector, no `run_frame` dispatch. Two shapes qualify,
/// mirroring the native-to-native call lanes (RFC 0067 WS1 / 0069
/// WS1):
///
/// - **plain**: every parameter is a managed scalar lane
///   ([`native_callable`]);
/// - **method**: the receiver in slot 0 rides as pin 0 of the pin
///   table and the remaining parameters are scalars
///   ([`native_method_callable`]). Any instance receiver is safe: the
///   body's attribute helpers re-validate their guard fingerprints
///   per access and deopt on mismatch.
///
/// Returns `None` when the interpreter path must run instead (not
/// compiled, lane mismatch, observers active, guard failure, recursion
/// limit, …). On a native side exit the continuation materializes an
/// interpreter frame exactly like a deopted native-to-native callee
/// ([`finish_deopted_callee`]), so semantics match the framed path.
pub(crate) fn try_call_native_direct(
    interp: &mut super::Interpreter,
    f: &Rc<PyFunction>,
    code: &Rc<CodeObject>,
    args: &[Object],
) -> Option<Result<Object, RuntimeError>> {
    // One relaxed load gates every never-compilable callee.
    if code.jit_hint.is_not_jitable() {
        return None;
    }
    // Pending interpreter work and active observers (which need the
    // callee's trace events fired) route through the framed path.
    if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
        return None;
    }
    if args.len() != code.arg_count as usize {
        return None;
    }
    let key = Rc::as_ptr(code).cast::<CodeObject>();
    let entry = JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.enabled {
            return None;
        }
        st.direct_entry_for(key)
    })?;
    let cf = &entry.art.cf;
    let method_shape = entry.method_shape;
    if method_shape && !matches!(args[0], Object::Instance(_)) {
        return None;
    }
    // Argument lanes must match the compiled parameter lanes exactly
    // (the framed path's entry type-guard, applied to the call
    // arguments directly). RFC 0071 WS1 — object-lane parameters
    // accept an instance (pinned below) or the nullable `None`.
    let offset = usize::from(method_shape);
    for (j, a) in args.iter().enumerate().skip(offset) {
        let ty = cf.local_types.get(j).copied().flatten()?;
        if ty == JitType::Obj {
            // RFC 0076 WS8 — the object lane admits any bound value
            // (see `entry_local_ok`).
            if matches!(a, Object::Unbound) {
                return None;
            }
        } else {
            pack(a, ty)?;
        }
    }
    // Deep call chains have no back edges below this point — poll
    // *before* guard validation (the handoff can run Python that
    // rebinds a guarded global).
    crate::gil::yield_checkpoint();
    // The callee's burned-in resolutions must hold before entry.
    if !guards_hold(
        interp,
        &f.globals,
        &f.builtins,
        &entry.art.snap,
        &entry.art.callees,
        &entry.art.math,
    ) {
        JIT.with(|cell| cell.borrow_mut().stats.entry_guard_failures += 1);
        return None;
    }
    // The same recursion tick the framed path would charge (on
    // overflow the interpreter path raises with full fidelity).
    let recursion_guard = match crate::recursion::enter() {
        crate::recursion::Enter::Ok(g) => g,
        crate::recursion::Enter::Overflow => return None,
    };

    // One pooled buffer per element width: locals + stack spill +
    // call-arg marshal share a single allocation (the take/put round
    // trips are per-call costs).
    let n = cf.n_locals as usize;
    let cap = cf.max_stack as usize + 1;
    let call_cap = (cf.max_call_args as usize).max(1);
    let mut u64_buf = take_u64(n + cap + call_cap);
    let (locals_buf, rest) = u64_buf.split_at_mut(n);
    let (spill, call_args) = rest.split_at_mut(cap);
    let mut u32_buf = take_u32(cap + call_cap);
    let (tags, call_tags) = u32_buf.split_at_mut(cap);
    let mut pins: PinTable = Vec::new();
    if method_shape {
        // The receiver slot carries pin index 0 (`take_u64` zeroed it).
        pins.push(Pin::Obj(args[0].clone()));
    }
    for (j, a) in args.iter().enumerate().skip(offset) {
        let ty = cf.local_types[j].expect("checked above");
        // RFC 0071 WS1 — object-lane arguments pin into the fresh
        // activation's table (`None` rides as `-1`).
        locals_buf[j] = if ty == JitType::Obj {
            match a {
                Object::None => u64::MAX,
                _ => {
                    let idx = pins.len() as u64;
                    pins.push(Pin::Obj(a.clone()));
                    idx
                }
            }
        } else {
            pack(a, ty).expect("checked above")
        };
    }
    let entry_pin_count = pins.len();
    let mut ctx = CallCtx {
        interp: std::ptr::from_mut(interp),
        callees: entry.art.callees.clone(),
        guard_snapshot: entry.art.snap.clone(),
        globals: f.globals.clone(),
        builtins: f.builtins.clone(),
        // Direct-callable code is cell-free (`native_callable`).
        cells: crate::object::empty_cells(),
        parked: None,
        raised: None,
        const_pins: Vec::new(),
        pins,
        obj_globals: entry.art.obj_globals.clone(),
        obj_global_pins: Vec::new(),
        attr_guards: entry.art.attr_guards.clone(),
        methods: entry.art.methods.clone(),
        math: entry.art.math.clone(),
        dirty: false,
        code_ptr: key,
        native: entry.native.clone(),
        method_native: entry.method_native.clone(),
        table_gen: current_compile_gen(),
        // The frameless direct entry pushes no interpreter frame —
        // keep the activation observable to callee-side stack walkers.
        frameless_code: Some(code.clone()),
    };
    let mut jf = JitFrame {
        locals: locals_buf.as_mut_ptr(),
        n_locals: cf.n_locals,
        entry_pc: 0,
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
    // SAFETY: the buffers are sized per the compiled frame's analysis
    // (the invariants `enter_compiled` documents); the engine backing
    // `cf` lives in this thread's `JIT` state for the process
    // lifetime; `ctx` outlives the call. Stack growth mirrors
    // `try_native_call`.
    let status = if crate::stdlib::greenlet_native::on_greenlet_stack() {
        unsafe { cf.enter(&raw mut jf) }
    } else {
        stacker::maybe_grow(512 * 1024, 8 * 1024 * 1024, || unsafe {
            cf.enter(&raw mut jf)
        })
    };

    NATIVE_CALL_STATS.with(|s| s.direct_calls.set(s.direct_calls.get() + 1));

    let out = match status {
        JitStatus::Returned => Ok(unpack_pins(jf.ret_bits, jf.ret_tag, &ctx.pins)),
        // `Yielded` is unreachable — generator bodies never register
        // as direct-callable (`native_callable` excludes them) — but
        // the deopt materialization is the safe catch-all.
        JitStatus::Deopt | JitStatus::Raised | JitStatus::Yielded => {
            // Deopt accounting + budget, exactly like the framed entry
            // path (off the happy path, so the state borrow is fine).
            JIT.with(|cell| {
                let mut st = cell.borrow_mut();
                if matches!(status, JitStatus::Deopt) {
                    st.stats.deopts += 1;
                    if let Some(ce) = st.cache.get_mut(&key) {
                        ce.deopts += 1;
                        if ce.deopts >= DEOPT_BUDGET {
                            ce.tier = Tier::NotJitable;
                            code.jit_hint.mark_not_jitable();
                        }
                    }
                }
            });
            // The materialized continuation is a full interpreter
            // activation that charges its own recursion tick — release
            // this level's first so the logical frame is counted once.
            drop(recursion_guard);
            let pending = if matches!(status, JitStatus::Raised) {
                Some(ctx.raised.take().unwrap_or_else(|| {
                    RuntimeError::Internal("JIT Raised exit without a parked exception".to_owned())
                }))
            } else {
                None
            };
            let nc = NativeCallee {
                cf: entry.art.cf.clone(),
                snap: entry.art.snap.clone(),
                callees: entry.art.callees.clone(),
                obj_globals: entry.art.obj_globals.clone(),
                attr_guards: entry.art.attr_guards.clone(),
                methods: entry.art.methods.clone(),
                math: entry.art.math.clone(),
                func: f.clone(),
                code: code.clone(),
                ctor: None,
            };
            finish_deopted_callee(interp, &nc, &mut ctx, locals_buf, spill, tags, &jf, pending)
        }
    };
    // RFC 0070 WS1 — reap the activation's runtime pins (after the
    // pin-based unpacks above).
    drain_runtime_pins(interp, &mut ctx.pins, entry_pin_count);
    put_u64(u64_buf);
    put_u32(u32_buf);
    Some(out)
}

/// Offer a fresh frame (pc 0, empty stack) to the JIT. See [`JitEntry`].
pub(crate) fn try_enter(interp: &mut super::Interpreter, frame: &mut super::Frame) -> JitEntry {
    // RFC 0067 — code the JIT already rejected skips tier-up on one
    // relaxed load; with the JIT on by default this is the per-call
    // tax on every never-compilable function (kwargs/defaults/
    // generator shapes), so it must stay off the map lookup.
    if frame.code.jit_hint.is_not_jitable() {
        return JitEntry::Skip;
    }
    // RFC 0070 WS2 — generator bodies compile, but a fresh pc-0
    // activation is the *bootstrap*: the interpreter must execute
    // `RETURN_GENERATOR` to create the generator object before any
    // body code runs. Native entry happens only at OSR pcs (loop
    // back edges inside resumed activations), so generator code
    // heats through `note_backedge` alone.
    if frame.code.is_generator {
        return JitEntry::Skip;
    }
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
        let mut probe_dict = |slot: u32| probe_dict_lane(frame_ref, slot);
        let mut probe_attr = |slot: u32, path: &[String], name: &str, store: bool| {
            probe_attr_lane(frame_ref, slot, path, name, store)
        };
        let mut attr_guard = |site: &AttrSiteMeta| attr_site_guard(interp_ref, frame_ref, site);
        let mut probe_method = |slot: u32, path: &[String], name: &str| {
            probe_method_entry(interp_ref, frame_ref, slot, path, name)
        };
        let mut math_attr =
            |name: &str, attr: &str| math_attr_object(interp_ref, frame_ref, name, attr);
        let mut probe_param = |slot: u32| probe_param_lane(frame_ref, slot);
        let mut probe_class = |cls: &Rc<TypeObject>| probe_class_ctor(interp_ref, cls);
        let mut probe_ctor_fld =
            |cls: &str, attr: &str| probe_ctor_field(interp_ref, frame_ref, cls, attr);
        let mut probe_cell = |idx: u32| probe_cell_lane(frame_ref, idx);
        let mut probe_obj = |slot: u32| probe_obj_live(frame_ref, slot);
        st.get_compiled(
            &frame.code,
            frame.pc as u32,
            &mut VmProbes {
                resolve_obj: &mut resolve,
                ret_lane_of: &mut ret_of,
                list: &mut probe,
                dict: &mut probe_dict,
                attr: &mut probe_attr,
                attr_guard_of: &mut attr_guard,
                method: &mut probe_method,
                math_attr: &mut math_attr,
                param: &mut probe_param,
                class_ctor: &mut probe_class,
                ctor_field: &mut probe_ctor_fld,
                cell: &mut probe_cell,
                obj_live: &mut probe_obj,
            },
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
        &entry.math,
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

    enter_compiled(interp, frame, &entry, 0, &[], None)
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
        let mut probe_dict = |slot: u32| probe_dict_lane(frame_ref, slot);
        let mut probe_attr = |slot: u32, path: &[String], name: &str, store: bool| {
            probe_attr_lane(frame_ref, slot, path, name, store)
        };
        let mut attr_guard = |site: &AttrSiteMeta| attr_site_guard(interp_ref, frame_ref, site);
        let mut probe_method = |slot: u32, path: &[String], name: &str| {
            probe_method_entry(interp_ref, frame_ref, slot, path, name)
        };
        let mut math_attr =
            |name: &str, attr: &str| math_attr_object(interp_ref, frame_ref, name, attr);
        let mut probe_param = |slot: u32| probe_param_lane(frame_ref, slot);
        let mut probe_class = |cls: &Rc<TypeObject>| probe_class_ctor(interp_ref, cls);
        let mut probe_ctor_fld =
            |cls: &str, attr: &str| probe_ctor_field(interp_ref, frame_ref, cls, attr);
        let mut probe_cell = |idx: u32| probe_cell_lane(frame_ref, idx);
        let mut probe_obj = |slot: u32| probe_obj_live(frame_ref, slot);
        st.get_compiled(
            &frame.code,
            frame.pc as u32,
            &mut VmProbes {
                resolve_obj: &mut resolve,
                ret_lane_of: &mut ret_of,
                list: &mut probe,
                dict: &mut probe_dict,
                attr: &mut probe_attr,
                attr_guard_of: &mut attr_guard,
                method: &mut probe_method,
                math_attr: &mut math_attr,
                param: &mut probe_param,
                class_ctor: &mut probe_class,
                ctor_field: &mut probe_ctor_fld,
                cell: &mut probe_cell,
                obj_live: &mut probe_obj,
            },
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
    let Some(osr) = cf.osr_entries.iter().find(|e| e.pc == pc) else {
        return fail(&frame.code);
    };
    if !guards_hold(
        interp,
        &frame.globals,
        &frame.builtins,
        &entry.guard_snapshot,
        &entry.callees,
        &entry.math,
    ) {
        return fail(&frame.code);
    }
    // Every managed *real* local must hold its lane right now — unlike a
    // fresh entry there is no definite-assignment argument that the
    // native code writes before it reads. RFC 0073 WS1 — except an
    // *unbound* object-lane local whose slot the per-entry analysis
    // proved is written before any native read from this entry: it
    // seeds as a pinned `Unbound` (a deopt writes back exactly the
    // unbound state).
    {
        let locals = frame.locals.borrow();
        let n_real = frame.code.varnames.len();
        for slot in 0..n_real {
            if let Some(ty) = cf.local_types.get(slot).copied().flatten() {
                let ok = locals.get(slot).is_some_and(|o| {
                    entry_local_ok(o, ty)
                        || (ty == JitType::Obj
                            && matches!(o, Object::Unbound)
                            && !osr.unassigned_reads.contains(&(slot as u32)))
                });
                if !ok {
                    drop(locals);
                    return fail(&frame.code);
                }
            }
        }
    }
    // The interpreter stack at the loop header holds exactly the live
    // iterators of the enclosing rewritten loops (range, list, and —
    // RFC 0071 WS4 — opaque), outermost first (ascending `live_from`).
    // Decompose them into the synthetic slots the compiled loops run
    // on; the headers re-check their bounds on entry.
    let Some(synth) = decompose_live_loops(cf, pc, &frame.stack) else {
        return fail(&frame.code);
    };
    // The iterators are consumed by the decomposition: native code owns
    // the loops from here (a deopt reconstructs fresh iterators).
    frame.stack.clear();
    JIT.with(|cell| cell.borrow_mut().stats.osr_entries += 1);
    enter_compiled(interp, frame, &entry, pc, &synth, None)
}

/// Decompose the live rewritten-loop iterators sitting on the
/// interpreter stack (`stack[..n]`, outermost first by ascending
/// `live_from`) into the synthetic-slot seeds the compiled loops run
/// on. `None` when the stack shape or any iterator's shape doesn't
/// match what the compile assumed (the caller falls back to the
/// interpreter). Shared by the OSR and generator-resume entries; a
/// resume's trailing sent value must be excluded by the caller
/// (`stack.len()` here must equal the live-loop count).
fn decompose_live_loops(
    cf: &weavepy_jit::CompiledFrame,
    pc: u32,
    stack: &[Object],
) -> Option<Vec<(u32, SynthSeed)>> {
    enum LiveLoop<'a> {
        Range(&'a weavepy_jit::RangeLoopMeta),
        List(&'a weavepy_jit::ListLoopMeta),
        Iter(&'a weavepy_jit::IterLoopMeta),
    }
    let mut live: Vec<(u32, LiveLoop<'_>)> = cf
        .range_loops
        .iter()
        .filter(|l| l.live_from <= pc && pc < l.live_to)
        .map(|l| (l.live_from, LiveLoop::Range(l)))
        .chain(
            cf.list_loops
                .iter()
                .filter(|l| l.live_from <= pc && pc < l.live_to)
                .map(|l| (l.live_from, LiveLoop::List(l))),
        )
        .chain(
            cf.iter_loops
                .iter()
                .filter(|l| l.live_from <= pc && pc < l.live_to)
                .map(|l| (l.live_from, LiveLoop::Iter(l))),
        )
        .collect();
    live.sort_unstable_by_key(|(from, _)| *from);
    if stack.len() != live.len() {
        return None;
    }
    let mut synth: Vec<(u32, SynthSeed)> = Vec::with_capacity(live.len() * 2);
    for (idx, (_, lp)) in live.iter().enumerate() {
        // RFC 0071 WS4 — an opaque loop's stack entry is the identity
        // iterable itself (never decomposed): pin it whole into the
        // iterator slot. Exactly the shapes `wpjit_get_iter` admits.
        if let LiveLoop::Iter(lp) = lp {
            let o = &stack[idx];
            if !matches!(o, Object::Generator(_) | Object::Iter(_)) {
                return None;
            }
            synth.push((lp.iter_slot, SynthSeed::PinObj(o.clone())));
            continue;
        }
        let Object::Iter(it) = &stack[idx] else {
            return None;
        };
        match lp {
            LiveLoop::Range(lp) => {
                let decomposed = match &*it.borrow() {
                    PyIterator::Range {
                        current,
                        stop,
                        step: 1,
                    } => Some((*current as u64, *stop as u64)),
                    _ => None,
                };
                let (cur, stop) = decomposed?;
                synth.push((lp.cur_slot, SynthSeed::Bits(cur)));
                synth.push((lp.stop_slot, SynthSeed::Bits(stop)));
            }
            // RFC 0071 WS4 — a live *list* iterator decomposes into
            // (pinned list, index). Only a plain-list source (no
            // subclass keepalive) whose current shape matches the
            // compiled element lane is admitted; the step helper
            // re-validates each element anyway.
            LiveLoop::List(lp) => {
                let lane = match cf.local_types.get(lp.seq_slot as usize).copied().flatten() {
                    Some(t) if t.is_list() => t,
                    _ => return None,
                };
                let decomposed = match &*it.borrow() {
                    PyIterator::List {
                        items,
                        index,
                        owner: None,
                    } if entry_local_ok(&Object::List(items.clone()), lane) => {
                        Some((items.clone(), *index as u64))
                    }
                    _ => None,
                };
                let (items, index) = decomposed?;
                let elem = lane.elem_lane().unwrap_or(JitType::Unknown);
                synth.push((lp.seq_slot, SynthSeed::PinList(items, elem)));
                synth.push((lp.idx_slot, SynthSeed::Bits(index)));
            }
            LiveLoop::Iter(_) => unreachable!("handled above"),
        }
    }
    Some(synth)
}

/// Attempt a native *resume* entry for a suspended generator
/// (RFC 0071 WS5). `frame.pc` must be a yield continuation the
/// compiled frame registered as a resume entry, and `frame.stack`
/// must hold exactly the live rewritten-loop iterators with the sent
/// value on top (pushed by the resume machinery). On `Skip` the frame
/// is untouched and the interpreter resumes normally.
pub(crate) fn try_enter_resume(
    interp: &mut super::Interpreter,
    frame: &mut super::Frame,
) -> JitEntry {
    // RFC 0073 WS4 — a parked native activation resumes on its own
    // buffers, skipping the marshal-in below entirely. Any refusal
    // inside materializes first, so a `Skip` never leaves the
    // interpreter a stale frame.
    if frame.parked_native.is_some() {
        return resume_parked(interp, frame);
    }
    if frame.code.jit_hint.is_not_jitable() {
        return JitEntry::Skip;
    }
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
        let mut probe_dict = |slot: u32| probe_dict_lane(frame_ref, slot);
        let mut probe_attr = |slot: u32, path: &[String], name: &str, store: bool| {
            probe_attr_lane(frame_ref, slot, path, name, store)
        };
        let mut attr_guard = |site: &AttrSiteMeta| attr_site_guard(interp_ref, frame_ref, site);
        let mut probe_method = |slot: u32, path: &[String], name: &str| {
            probe_method_entry(interp_ref, frame_ref, slot, path, name)
        };
        let mut math_attr =
            |name: &str, attr: &str| math_attr_object(interp_ref, frame_ref, name, attr);
        let mut probe_param = |slot: u32| probe_param_lane(frame_ref, slot);
        let mut probe_class = |cls: &Rc<TypeObject>| probe_class_ctor(interp_ref, cls);
        let mut probe_ctor_fld =
            |cls: &str, attr: &str| probe_ctor_field(interp_ref, frame_ref, cls, attr);
        let mut probe_cell = |idx: u32| probe_cell_lane(frame_ref, idx);
        let mut probe_obj = |slot: u32| probe_obj_live(frame_ref, slot);
        st.get_compiled(
            &frame.code,
            frame.pc as u32,
            &mut VmProbes {
                resolve_obj: &mut resolve,
                ret_lane_of: &mut ret_of,
                list: &mut probe,
                dict: &mut probe_dict,
                attr: &mut probe_attr,
                attr_guard_of: &mut attr_guard,
                method: &mut probe_method,
                math_attr: &mut math_attr,
                param: &mut probe_param,
                class_ctor: &mut probe_class,
                ctor_field: &mut probe_ctor_fld,
                cell: &mut probe_cell,
                obj_live: &mut probe_obj,
            },
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
    // A pc that isn't a registered resume entry is the *expected* case
    // for e.g. the first resume (which enters at the body start, not a
    // yield continuation): plain skip, never charged to the OSR-failure
    // budget — that backoff must stay available for the real loop
    // entries this generator still takes.
    if !cf.resume_entries.iter().any(|e| e.pc == pc) {
        return JitEntry::Skip;
    }
    if !guards_hold(
        interp,
        &frame.globals,
        &frame.builtins,
        &entry.guard_snapshot,
        &entry.callees,
        &entry.math,
    ) {
        return fail(&frame.code);
    }
    // Every managed real local must hold its lane right now (same
    // contract as an OSR entry — the prologue loads them all).
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
    // Stack shape: the live rewritten-loop iterators (outermost first),
    // then the sent value the resume machinery pushed on top. The sent
    // value must fit the object lane (`None` or an instance) — anything
    // else resumes interpreted (the compiled continuation types it Obj).
    let Some(sent) = frame.stack.last() else {
        return fail(&frame.code);
    };
    if !matches!(sent, Object::None | Object::Instance(_)) {
        return fail(&frame.code);
    }
    let Some(synth) = decompose_live_loops(cf, pc, &frame.stack[..frame.stack.len() - 1]) else {
        return fail(&frame.code);
    };
    let sent = frame.stack.pop().expect("sent value verified above");
    frame.stack.clear();
    JIT.with(|cell| cell.borrow_mut().stats.gen_resumes += 1);
    enter_compiled(interp, frame, &entry, pc, &synth, Some(sent))
}

/// RFC 0071 WS4 — how an OSR entry seeds one synthetic slot: raw lane
/// bits (range bounds, list indices), or a list to pin into the entry
/// pin table (the slot then carries the fresh pin index).
enum SynthSeed {
    Bits(u64),
    PinList(Rc<GilRefCell<Vec<Object>>>, JitType),
    /// RFC 0071 WS4 — an opaque loop's live iterator, pinned whole.
    PinObj(Object),
}

/// Marshal locals, enter the compiled frame at `entry_pc`, and translate
/// the native exit back into interpreter state. Guards must already
/// hold, `frame.stack` must be empty, and `synth_init` seeds synthetic
/// slots for an OSR entry. RFC 0071 WS5 — `resume_sent` carries a
/// generator resume's sent value into the dispatch preamble through
/// `ret_bits` (object-lane packed: a pin index, `None` as `-1`).
fn enter_compiled(
    interp: &mut super::Interpreter,
    frame: &mut super::Frame,
    entry: &CompiledEntry,
    entry_pc: u32,
    synth_init: &[(u32, SynthSeed)],
    resume_sent: Option<Object>,
) -> JitEntry {
    let cf = &entry.cf;
    let n = cf.n_locals as usize;
    let mut locals_buf = take_u64(n);
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
                    match locals.get(slot) {
                        // RFC 0070 WS1 — the nullable lane: `None`
                        // packs as `-1` (never a valid pin index).
                        Some(Object::None) => *dst = u64::MAX,
                        // RFC 0071 WS4 — identity iterables pin like
                        // instances (the opaque-loop capture reads
                        // them; other helpers deopt on them).
                        // RFC 0073 WS1 — anything else (including
                        // `Unbound`, admitted by the OSR entry's
                        // definite-assignment check) pins too: every
                        // access helper re-validates and deopts on a
                        // non-instance, and a deopt before the slot's
                        // first write must restore the exact prior
                        // state — never a dangling `0` bit pattern
                        // aliasing pin 0.
                        Some(o) => {
                            *dst = pins.len() as u64;
                            pins.push(Pin::Obj(o.clone()));
                        }
                        None => {
                            *dst = pins.len() as u64;
                            pins.push(Pin::Obj(Object::Unbound));
                        }
                    }
                    continue;
                }
                // RFC 0071 WS6 — `str`/`bytes` read lanes pin the
                // exact-typed payload (never nullable). RFC 0073 WS2 —
                // the exact-`dict` lane pins the same way.
                if matches!(ty, JitType::Str | JitType::Bytes | JitType::Dict) {
                    match (ty, locals.get(slot)) {
                        (JitType::Str, Some(o @ Object::Str(_)))
                        | (JitType::Bytes, Some(o @ Object::Bytes(_)))
                        | (JitType::Dict, Some(o @ Object::Dict(_))) => {
                            *dst = pins.len() as u64;
                            pins.push(Pin::Obj(o.clone()));
                        }
                        _ => {}
                    }
                    continue;
                }
                *dst = locals.get(slot).and_then(|o| pack(o, ty)).unwrap_or(0);
            }
        }
    }
    for (slot, seed) in synth_init {
        locals_buf[*slot as usize] = match seed {
            SynthSeed::Bits(bits) => *bits,
            // RFC 0071 WS4 — a decomposed list iterator's source list
            // pins here so the slot carries a valid pin index.
            SynthSeed::PinList(l, elem) => {
                let idx = pins.len() as u64;
                pins.push(Pin::List(l.clone(), *elem));
                idx
            }
            // RFC 0071 WS4 — an opaque loop's identity iterable pins
            // whole; the iterator slot carries the pin index.
            SynthSeed::PinObj(o) => {
                let idx = pins.len() as u64;
                pins.push(Pin::Obj(o.clone()));
                idx
            }
        };
    }
    // RFC 0071 WS5 — pack the sent value for the resume dispatch: it
    // rides `ret_bits` into the continuation block's boundary value.
    let resume_bits = resume_sent.map(|sent| match sent {
        Object::None => u64::MAX,
        obj => {
            let idx = pins.len() as u64;
            pins.push(Pin::Obj(obj));
            idx
        }
    });
    let entry_pin_count = pins.len();
    let cap = cf.max_stack as usize + 1;
    let mut spill = take_u64(cap);
    let mut tags = take_u32(cap);
    let call_cap = (cf.max_call_args as usize).max(1);
    let mut call_args = take_u64(call_cap);
    let mut call_tags = take_u32(call_cap);
    let mut ctx = CallCtx {
        interp: std::ptr::from_mut(interp),
        callees: entry.callees.clone(),
        guard_snapshot: entry.guard_snapshot.clone(),
        globals: frame.globals.clone(),
        builtins: frame.builtins.clone(),
        cells: frame.cells.clone(),
        parked: None,
        raised: None,
        const_pins: Vec::new(),
        pins,
        obj_globals: entry.obj_globals.clone(),
        obj_global_pins: Vec::new(),
        attr_guards: entry.attr_guards.clone(),
        methods: entry.methods.clone(),
        math: entry.math.clone(),
        dirty: false,
        code_ptr: Rc::as_ptr(&frame.code).cast::<CodeObject>(),
        native: entry.native.clone(),
        method_native: entry.method_native.clone(),
        table_gen: current_compile_gen(),
        // Framed entry: this activation's `Frame` shell is on the
        // spine already.
        frameless_code: None,
    };
    let mut jf = JitFrame {
        locals: locals_buf.as_mut_ptr(),
        n_locals: cf.n_locals,
        entry_pc,
        ret_bits: resume_bits.unwrap_or(0),
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

    note_native_exit(frame, &jf, status);

    // RFC 0073 WS4 — a healthy yield whose continuation is a
    // registered resume entry parks the *whole* activation on the
    // frame: no locals writeback, no stack rebuild, no pin drain —
    // the buffers and pins move into the box and the next resume
    // re-enters natively on them.
    if matches!(status, JitStatus::Yielded) {
        if let Some(plan) = park_plan(frame, entry, &jf) {
            let yielded = unpack_pins(spill[0], tags[0], &ctx.pins);
            frame.pc = jf.deopt_pc + 1;
            frame.parked_native = Some(Box::new(NativeActivation {
                compile_id: entry.compile_id,
                yield_pc: jf.deopt_pc,
                locals_buf,
                spill,
                tags,
                call_args,
                call_tags,
                pins: ctx.pins,
                const_pins: ctx.const_pins,
                obj_global_pins: ctx.obj_global_pins,
                entry_pin_count,
                dirty: ctx.dirty,
                local_types: cf.local_types.clone(),
                plan,
            }));
            JIT.with(|cell| cell.borrow_mut().stats.gen_parks += 1);
            return JitEntry::Yielded(yielded);
        }
    }

    let out = match status {
        JitStatus::Returned => JitEntry::Ran(unpack_pins(jf.ret_bits, jf.ret_tag, &ctx.pins)),
        // RFC 0070 WS2 — a `Yielded` exit that could not park takes
        // the deopt writeback verbatim: the frame parks *at* the
        // `YIELD_VALUE` pc with the yielded value on top of the
        // rebuilt stack, and the interpreter's own execution of the
        // yield performs the suspension (park, `gi_frame`
        // consistency, exception-state swap-out).
        JitStatus::Deopt | JitStatus::Raised | JitStatus::Yielded => native_exit_writeback(
            interp,
            frame,
            entry,
            &locals_buf,
            &spill,
            &tags,
            &jf,
            &mut ctx,
            status,
        ),
    };
    // RFC 0070 WS1 — runtime pins mirror popped temporaries: reap the
    // ones dying with the activation (after every pin-based rebuild
    // above, so nothing is unpacked from a drained table).
    drain_runtime_pins(interp, &mut ctx.pins, entry_pin_count);
    put_u64(locals_buf);
    put_u64(spill);
    put_u32(tags);
    put_u64(call_args);
    put_u32(call_tags);
    out
}

/// Post-exit accounting shared by [`enter_compiled`] and
/// [`resume_parked`]: entry/yield/deopt counters and the deopt-backoff
/// budget that retires chronically side-exiting code.
fn note_native_exit(frame: &super::Frame, jf: &JitFrame, status: JitStatus) {
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        st.stats.native_entries += 1;
        // RFC 0076 WS7 follow-up — the generic-call backoff. Framed
        // entries are the denominator; `wpjit_call_dyn`'s generic legs
        // (charged by `note_generic_dyn_call`) the numerator. A
        // compiled frame averaging `GENERIC_CALL_RETIRE_RATIO`+
        // interpreter round-trips per activation is a thin native
        // driver around interpreter calls — each paying activation-
        // shell setup plus a full `guards_hold` re-validation the
        // interpreter wouldn't — so it is retired like the deopt
        // budget retires chronic side-exiters.
        {
            let key = Rc::as_ptr(&frame.code).cast::<CodeObject>();
            if let Some(ce) = st.cache.get_mut(&key) {
                ce.native_entries = ce.native_entries.saturating_add(1);
                if ce.native_entries >= GENERIC_RETIRE_MIN_ENTRIES
                    && ce.generic_dyn_calls / ce.native_entries >= GENERIC_CALL_RETIRE_RATIO
                    && !matches!(ce.tier, Tier::NotJitable)
                {
                    ce.tier = Tier::NotJitable;
                    frame.code.jit_hint.mark_not_jitable();
                    st.stats.generic_retires += 1;
                }
            }
        }
        // RFC 0070 WS2 — a yield is the *healthy* exit of a generator
        // activation: counted for visibility, never charged to the
        // deopt-backoff budget.
        if matches!(status, JitStatus::Yielded) {
            st.stats.yields += 1;
        }
        if matches!(status, JitStatus::Deopt) {
            st.stats.deopts += 1;
            if std::env::var_os("WEAVEPY_JIT_TRACE").is_some() {
                eprintln!("jit deopt {:?} pc {}", frame.code.name, jf.deopt_pc);
            }
            // Deopt backoff: a compiled frame whose activations keep
            // side-exiting is a net loss (marshal-in + native entry +
            // frame materialization per call, all to end up in the
            // interpreter anyway). Past the budget, retire the code
            // exactly as an analyzer rejection would — the `jit_hint`
            // fast-out then gates every later activation and back
            // edge, and `Tier::NotJitable` stops recompilation.
            let key = Rc::as_ptr(&frame.code).cast::<CodeObject>();
            if let Some(ce) = st.cache.get_mut(&key) {
                ce.deopts += 1;
                if ce.deopts >= DEOPT_BUDGET {
                    ce.tier = Tier::NotJitable;
                    frame.code.jit_hint.mark_not_jitable();
                }
            }
        }
    });
}

/// The deopt-style writeback for a native side exit (shared by
/// [`enter_compiled`] and [`resume_parked`]): write back managed
/// locals (synthetic range slots have no interpreter home — they feed
/// the iterator rebuild), rebuild the operand stack from the spill,
/// and position `frame.pc` at the deopt point.
#[allow(clippy::too_many_arguments)]
fn native_exit_writeback(
    interp: &mut super::Interpreter,
    frame: &mut super::Frame,
    entry: &CompiledEntry,
    locals_buf: &[u64],
    spill: &[u64],
    tags: &[u32],
    jf: &JitFrame,
    ctx: &mut CallCtx,
    status: JitStatus,
) -> JitEntry {
    let cf = &entry.cf;
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
    rebuild_stack(interp, frame, entry, locals_buf, spill, tags, jf, &ctx.pins);
    if matches!(status, JitStatus::Raised) {
        // As though the CALL instruction just executed and
        // raised: pc points past it (`handle_exception` uses
        // `pc - 1` as the raise site).
        frame.pc = jf.deopt_pc + 1;
        let err = ctx.raised.take().unwrap_or_else(|| {
            RuntimeError::Internal("JIT Raised exit without a parked exception".to_owned())
        });
        JitEntry::Raised(err)
    } else {
        // A deopt-after-call carries the parked, already-
        // computed result: it goes on top of the rebuilt stack
        // and the interpreter resumes after the call.
        if let Some(v) = ctx.parked.take() {
            frame.stack.push(v);
        }
        frame.pc = jf.deopt_pc;
        JitEntry::Deopt
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
    // Erased objects to re-insert, by ascending interpreter depth.
    // RFC 0073 WS1 — live loop iterators join the depth-keyed insert
    // walk (they used to be pushed at the stack bottom outright,
    // which was equivalent while every loop header required an empty
    // boundary stack; a comprehension loop's iterator sits above its
    // accumulator and the surrounding expression stack, at the
    // `interp_depth` the analyzer recorded). The parked saved-target
    // `Unbound` of each live comprehension rides the same walk.
    let mut inserts: Vec<(u32, Object)> = Vec::new();
    for lp in &cf.range_loops {
        if lp.live_from <= jf.deopt_pc && jf.deopt_pc < lp.live_to {
            let current = locals_buf[lp.cur_slot as usize] as i64;
            let stop = locals_buf[lp.stop_slot as usize] as i64;
            inserts.push((
                lp.interp_depth,
                Object::Iter(Rc::new(crate::sync::RefCell::new(PyIterator::Range {
                    current,
                    stop,
                    step: 1,
                }))),
            ));
        }
    }
    for lp in &cf.list_loops {
        if lp.live_from <= jf.deopt_pc && jf.deopt_pc < lp.live_to {
            let items = match pins.get(locals_buf[lp.seq_slot as usize] as usize) {
                Some(Pin::List(l, _)) => l.clone(),
                // Unreachable by construction (the seq slot holds a
                // valid pin throughout the live span); an empty list
                // keeps the rebuild total.
                _ => Rc::new(crate::sync::RefCell::new(Vec::new())),
            };
            let index = locals_buf[lp.idx_slot as usize] as usize;
            inserts.push((
                lp.interp_depth,
                Object::Iter(Rc::new(crate::sync::RefCell::new(PyIterator::List {
                    items,
                    index,
                    owner: None,
                }))),
            ));
        }
    }
    // RFC 0071 WS4 — an opaque loop's iterator was never decomposed:
    // the pinned identity iterable itself goes back on the stack.
    for lp in &cf.iter_loops {
        if lp.live_from <= jf.deopt_pc && jf.deopt_pc < lp.live_to {
            let it = pins
                .get(locals_buf[lp.iter_slot as usize] as usize)
                .map_or(Object::None, Pin::to_object);
            inserts.push((lp.interp_depth, it));
        }
    }
    // RFC 0073 WS1 — the parked prior value of a live comprehension
    // target, proven `Unbound` at admission: the interpreter's own
    // epilogue (or exception handler) consumes it.
    for s in &cf.comp_saved {
        if s.live_from <= jf.deopt_pc && jf.deopt_pc < s.live_to {
            inserts.push((s.interp_depth, Object::Unbound));
        }
    }
    // Callee spans open at the deopt pc. Every span family records
    // `live_to` = pc *after* the consuming CALL, so a deopt landing
    // exactly on that CALL (an inner call's parked-result exit resumes
    // there) still sees the span open and rebuilds the pending callee.
    // RFC 0068 — every erased callee load is immediately followed by a
    // PUSH_NULL in the self-or-null calling convention, so each open
    // span reinserts the callee *and* the `Unbound` marker above it.
    for s in cf
        .callee_spans
        .iter()
        .filter(|s| s.live_from < jf.deopt_pc && jf.deopt_pc < s.live_to)
    {
        inserts.push((s.interp_depth, entry.callees[s.token as usize].0.clone()));
        inserts.push((s.interp_depth + 1, Object::Unbound));
    }
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
            inserts.push((s.interp_depth + 1, Object::Unbound));
        }
    }
    // RFC 0069 WS2 — open math-intrinsic spans: the interpreter holds
    // the bound intrinsic function (from the per-guard snapshot) and
    // the self-or-null marker above it.
    for s in cf
        .math_spans
        .iter()
        .filter(|s| s.live_from < jf.deopt_pc && jf.deopt_pc < s.live_to)
    {
        let f = entry
            .math
            .get(s.token as usize)
            .map_or(Object::None, |(_, _, f)| f.clone());
        inserts.push((s.interp_depth, f));
        inserts.push((s.interp_depth + 1, Object::Unbound));
    }
    // RFC 0074 WS2 — open opaque-call null spans: only the `Unbound`
    // self-or-null marker is interpreter-side (the loaded callee
    // itself is an ordinary spilled native value below it).
    for s in cf
        .null_spans
        .iter()
        .filter(|s| s.live_from < jf.deopt_pc && jf.deopt_pc < s.live_to)
    {
        inserts.push((s.interp_depth, Object::Unbound));
    }
    inserts.sort_unstable_by_key(|(depth, _)| *depth);
    // Open method spans: the spilled entry at `native_index` must
    // rebuild as the bound method, not the bare pin — via a fresh
    // `append` load for the RFC 0065 list shape (`token: None`), or
    // the burned-in site's method name for an RFC 0069 WS1 site.
    let mut bound_recv: Vec<(u32, &str)> = cf
        .method_spans
        .iter()
        .filter(|s| s.live_from < jf.deopt_pc && jf.deopt_pc < s.live_to)
        .map(|s| {
            let name = s
                .token
                .and_then(|t| cf.method_sites.get(t as usize))
                .map_or("append", |site| site.name.as_str());
            (s.native_index, name)
        })
        .collect();
    // RFC 0073 WS3 — open native `str`-method spans rebuild the same
    // way; the burned site's static name resolves the bound method on
    // the pinned `str` receiver.
    bound_recv.extend(
        cf.str_method_spans
            .iter()
            .filter(|s| s.live_from < jf.deopt_pc && jf.deopt_pc < s.live_to)
            .map(|s| {
                let name = s
                    .token
                    .and_then(|t| cf.str_method_sites.get(t as usize))
                    .map_or("upper", |m| m.name());
                (s.native_index, name)
            }),
    );
    if std::env::var_os("WEAVEPY_JIT_TRACE").is_some() {
        eprintln!(
            "jit rebuild {:?} deopt_pc {} stack_len {} inserts {:?} null_spans {:?} callee_spans {:?}",
            frame.code.name,
            jf.deopt_pc,
            jf.stack_len,
            inserts
                .iter()
                .map(|(d, o)| (*d, o.type_name()))
                .collect::<Vec<_>>(),
            cf.null_spans
                .iter()
                .map(|s| (s.live_from, s.live_to, s.interp_depth))
                .collect::<Vec<_>>(),
            cf.callee_spans
                .iter()
                .map(|s| (s.live_from, s.live_to, s.interp_depth))
                .collect::<Vec<_>>(),
        );
    }
    let mut next = 0usize;
    for i in 0..jf.stack_len as usize {
        while next < inserts.len() && inserts[next].0 as usize == frame.stack.len() {
            frame.stack.push(inserts[next].1.clone());
            next += 1;
        }
        let mut v = unpack_pins(spill[i], tags[i], pins);
        let rebound = bound_recv.iter().find(|(ni, _)| *ni == i as u32);
        if let Some((_, name)) = rebound {
            // The receiver of an open method span: what the
            // interpreter holds here is the *bound method*. The load
            // cannot fail (`list` always has `append`; a burned-in
            // site's guard held when the span opened; `str`'s method
            // table is immutable); `None` is an unreachable defensive
            // fallback.
            v = interp.load_attr_public(&v, name).unwrap_or(Object::None);
        }
        frame.stack.push(v);
        if rebound.is_some() {
            // RFC 0068 — LOAD_ATTR in method form leaves the
            // self-or-null `Unbound` marker above the bound method.
            frame.stack.push(Object::Unbound);
        }
    }
    while next < inserts.len() {
        frame.stack.push(inserts[next].1.clone());
        next += 1;
    }
}

// ---------- RFC 0073 WS4 — persistent native generator activations ----------

/// One stack slot of a parked activation's interp-free
/// materialization plan (bottom→top, *excluding* the yielded value,
/// which is delivered at park time). Everything here rebuilds from
/// the box's own buffers — no compiled-frame metadata, no
/// interpreter — so materialization works on any thread, even after
/// the compilation that produced the box is gone.
enum PlanSlot {
    /// A pre-cloned object: an erased callee / `len` / math intrinsic
    /// insert (guard-stable for the box's whole life), its
    /// self-or-null `Unbound` marker, or a live comprehension's parked
    /// saved-target `Unbound`.
    Obj(Object),
    /// A rewritten `range` loop's live iterator, rebuilt from the
    /// synthetic locals slots (step is always 1 in the admitted shape,
    /// as in [`rebuild_stack`]).
    RangeIter { cur_slot: u32, stop_slot: u32 },
    /// A decomposed list loop's live iterator: the pinned source list
    /// plus the synthetic index slot.
    ListIter { seq_slot: u32, idx_slot: u32 },
    /// An opaque loop's identity iterable, pinned whole.
    OpaqueIter { iter_slot: u32 },
}

/// A suspended generator's live *native* activation (RFC 0073 WS4).
///
/// Parked on [`super::Frame::parked_native`] at a `Yielded` exit
/// instead of the wave-8 writeback: the marshal buffers, the pin
/// table, and enough `Send`-safe metadata to (a) resume natively with
/// zero re-marshaling when the same compilation is still cached on
/// the resuming thread, or (b) materialize back into interpreter
/// state without an interpreter or the (thread-local, `!Send`)
/// compiled artifacts. Lives inside the generator's
/// `Box<dyn Any + Send + Sync>` frame, hence no `StdRc` anywhere.
pub(crate) struct NativeActivation {
    /// Identity of the compilation that laid out the buffers
    /// ([`Artifacts::compile_id`], process-unique).
    compile_id: u64,
    /// The `YIELD_VALUE` pc this activation parked at (trace only —
    /// `frame.pc` carries the continuation).
    yield_pc: u32,
    locals_buf: Vec<u64>,
    spill: Vec<u64>,
    tags: Vec<u32>,
    call_args: Vec<u64>,
    call_tags: Vec<u32>,
    /// The activation's pin table, kept whole across the suspension —
    /// spill/locals slots reference it by index, and it is what keeps
    /// the pinned objects alive (and visible to the cycle GC through
    /// [`Self::visit_objects`]).
    pins: PinTable,
    const_pins: Vec<(u32, u64)>,
    /// RFC 0074 WS1 — the memoized obj-global pins, parked alongside
    /// `const_pins` so a resumed activation reuses them.
    obj_global_pins: Vec<(u32, u64)>,
    /// Pin-table size at the *first* native entry; the eventual
    /// `Returned`/deopt exit drains runtime pins from here.
    entry_pin_count: usize,
    dirty: bool,
    /// Per-slot lanes for the locals writeback at materialization
    /// (cloned once from the compiled frame at first park).
    local_types: Vec<Option<JitType>>,
    plan: Vec<PlanSlot>,
}

// The box rides inside `GeneratorState`'s `Box<dyn Any + Send + Sync>`.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<NativeActivation>();
};

impl NativeActivation {
    /// Every object this parked activation keeps alive, for the cycle
    /// collector's traverse and the prompt-reap harvest. Scalar lanes
    /// hold no references; all edges live in the pin table.
    pub(crate) fn visit_objects(&self, visit: &mut dyn FnMut(&Object)) {
        for p in &self.pins {
            let o = p.to_object();
            visit(&o);
        }
    }

    /// The pinned objects, cloned (for [`super::frame_reapables`]).
    pub(crate) fn pinned_objects(&self) -> impl Iterator<Item = Object> + '_ {
        self.pins.iter().map(Pin::to_object)
    }
}

/// Decide whether a `Yielded` exit can park (RFC 0073 WS4) and, if
/// so, build the interp-free materialization plan. `None` sends the
/// exit down the wave-8 writeback instead. The conditions guarantee
/// materialization never needs an interpreter or compiled metadata:
///
/// * a plain generator body (never coroutines / async generators);
/// * no Python-visible `PyFrame` exists — one would read the shared
///   (stale while parked) locals storage behind our back, and
///   `gen_py_frame` materializes before ever creating one;
/// * no observers (they want the interpreter's own `YIELD_VALUE`);
/// * the continuation is a registered resume entry, whose admission
///   contract fixes the native spill to exactly the yielded value;
/// * no open method spans (their rebuild needs `load_attr_public`);
/// * the remaining interpreter stack is exactly the live-loop /
///   erased-object inserts at contiguous depths.
fn park_plan(frame: &super::Frame, entry: &CompiledEntry, jf: &JitFrame) -> Option<Vec<PlanSlot>> {
    let cf = &entry.cf;
    let code = &frame.code;
    if !code.is_generator || code.is_coroutine || code.is_async_generator {
        return None;
    }
    if frame.py_frame.is_some()
        || frame
            .shell_cache
            .as_ref()
            .is_some_and(|s| s.materialized.borrow().is_some())
    {
        return None;
    }
    if crate::trace::any_observers_active() {
        return None;
    }
    let pc = jf.deopt_pc;
    if jf.stack_len != 1 || !cf.resume_entries.iter().any(|e| e.pc == pc + 1) {
        return None;
    }
    if cf
        .method_spans
        .iter()
        .chain(cf.str_method_spans.iter())
        .any(|s| s.live_from < pc && pc < s.live_to)
    {
        return None;
    }
    let mut inserts: Vec<(u32, PlanSlot)> = Vec::new();
    for lp in &cf.range_loops {
        if lp.live_from <= pc && pc < lp.live_to {
            inserts.push((
                lp.interp_depth,
                PlanSlot::RangeIter {
                    cur_slot: lp.cur_slot,
                    stop_slot: lp.stop_slot,
                },
            ));
        }
    }
    for lp in &cf.list_loops {
        if lp.live_from <= pc && pc < lp.live_to {
            inserts.push((
                lp.interp_depth,
                PlanSlot::ListIter {
                    seq_slot: lp.seq_slot,
                    idx_slot: lp.idx_slot,
                },
            ));
        }
    }
    for lp in &cf.iter_loops {
        if lp.live_from <= pc && pc < lp.live_to {
            inserts.push((
                lp.interp_depth,
                PlanSlot::OpaqueIter {
                    iter_slot: lp.iter_slot,
                },
            ));
        }
    }
    for s in &cf.comp_saved {
        if s.live_from <= pc && pc < s.live_to {
            inserts.push((s.interp_depth, PlanSlot::Obj(Object::Unbound)));
        }
    }
    for s in cf
        .callee_spans
        .iter()
        .filter(|s| s.live_from < pc && pc < s.live_to)
    {
        inserts.push((
            s.interp_depth,
            PlanSlot::Obj(entry.callees[s.token as usize].0.clone()),
        ));
        inserts.push((s.interp_depth + 1, PlanSlot::Obj(Object::Unbound)));
    }
    if !cf.len_spans.is_empty() {
        let len_obj = entry
            .guard_snapshot
            .iter()
            .find(|(name, _)| name == "len")
            .map(|(_, o)| o.clone());
        for s in cf
            .len_spans
            .iter()
            .filter(|s| s.live_from < pc && pc < s.live_to)
        {
            inserts.push((
                s.interp_depth,
                PlanSlot::Obj(len_obj.clone().unwrap_or(Object::None)),
            ));
            inserts.push((s.interp_depth + 1, PlanSlot::Obj(Object::Unbound)));
        }
    }
    for s in cf
        .math_spans
        .iter()
        .filter(|s| s.live_from < pc && pc < s.live_to)
    {
        let f = entry
            .math
            .get(s.token as usize)
            .map_or(Object::None, |(_, _, f)| f.clone());
        inserts.push((s.interp_depth, PlanSlot::Obj(f)));
        inserts.push((s.interp_depth + 1, PlanSlot::Obj(Object::Unbound)));
    }
    // RFC 0074 WS2 — an open opaque-call null span parks its
    // interpreter-only `Unbound` marker at its recorded depth.
    for s in cf
        .null_spans
        .iter()
        .filter(|s| s.live_from < pc && pc < s.live_to)
    {
        inserts.push((s.interp_depth, PlanSlot::Obj(Object::Unbound)));
    }
    inserts.sort_by_key(|(depth, _)| *depth);
    // With the single spill (the yielded value) delivered at park, the
    // suspended interpreter stack is exactly the inserts — which must
    // therefore occupy contiguous depths from 0, all below the value.
    if inserts
        .iter()
        .enumerate()
        .any(|(i, (depth, _))| *depth as usize != i)
    {
        return None;
    }
    Some(inserts.into_iter().map(|(_, slot)| slot).collect())
}

/// Write a parked native activation back into interpreter state
/// (RFC 0073 WS4): locals from the marshal buffer, the operand stack
/// from the park-time plan (spliced *below* anything pushed since —
/// a resume's sent value). Afterwards the frame is indistinguishable
/// from an interpreted suspension. No-op without a parked box; never
/// needs an interpreter (park refused any shape whose rebuild would).
pub(crate) fn materialize_parked(frame: &mut super::Frame) {
    let Some(act) = frame.parked_native.take() else {
        return;
    };
    if std::env::var_os("WEAVEPY_JIT_TRACE").is_some() {
        eprintln!(
            "jit gen materialize {:?} yield pc {}",
            frame.code.name, act.yield_pc
        );
    }
    {
        let mut locals = frame.locals.borrow_mut();
        for (slot, &bits) in act.locals_buf.iter().enumerate() {
            if let Some(ty) = act.local_types.get(slot).copied().flatten() {
                if let Some(dst) = locals.get_mut(slot) {
                    *dst = unpack_ty(bits, ty, &act.pins);
                }
            }
        }
    }
    let rebuilt: Vec<Object> = act
        .plan
        .iter()
        .map(|slot| match slot {
            PlanSlot::Obj(o) => o.clone(),
            PlanSlot::RangeIter {
                cur_slot,
                stop_slot,
            } => Object::Iter(Rc::new(crate::sync::RefCell::new(PyIterator::Range {
                current: act.locals_buf[*cur_slot as usize] as i64,
                stop: act.locals_buf[*stop_slot as usize] as i64,
                step: 1,
            }))),
            PlanSlot::ListIter { seq_slot, idx_slot } => {
                let items = match act.pins.get(act.locals_buf[*seq_slot as usize] as usize) {
                    Some(Pin::List(l, _)) => l.clone(),
                    // Unreachable by construction (the seq slot holds a
                    // valid pin throughout the live span).
                    _ => Rc::new(crate::sync::RefCell::new(Vec::new())),
                };
                Object::Iter(Rc::new(crate::sync::RefCell::new(PyIterator::List {
                    items,
                    index: act.locals_buf[*idx_slot as usize] as usize,
                    owner: None,
                })))
            }
            PlanSlot::OpaqueIter { iter_slot } => act
                .pins
                .get(act.locals_buf[*iter_slot as usize] as usize)
                .map_or(Object::None, Pin::to_object),
        })
        .collect();
    frame.stack.splice(0..0, rebuilt);
    // Buffers return to the pools; the pins drop with the box (plain
    // refcount decrements — the prompt-reap cascade needs an
    // interpreter, which materialization sites don't all have).
    let NativeActivation {
        locals_buf,
        spill,
        tags,
        call_args,
        call_tags,
        ..
    } = *act;
    put_u64(locals_buf);
    put_u64(spill);
    put_u32(tags);
    put_u64(call_args);
    put_u32(call_tags);
    JIT.with(|cell| cell.borrow_mut().stats.gen_materialized += 1);
}

/// Resume a parked native activation (RFC 0073 WS4): revalidate the
/// entry guards, seed the sent value straight into the boxed pin
/// table, and re-enter the compiled continuation on the boxed buffers
/// — no locals re-marshal, no live-loop decomposition. Every refusal
/// materializes first, so the interpreter never observes the stale
/// frame; `frame.stack` holds exactly the sent value on entry.
fn resume_parked(interp: &mut super::Interpreter, frame: &mut super::Frame) -> JitEntry {
    let mut act = frame
        .parked_native
        .take()
        .expect("resume_parked called with a parked activation");
    let refuse = |frame: &mut super::Frame, act: Box<NativeActivation>| {
        frame.parked_native = Some(act);
        materialize_parked(frame);
        JitEntry::Skip
    };
    if frame.code.jit_hint.is_not_jitable() {
        return refuse(frame, act);
    }
    let entry = JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.enabled {
            return None;
        }
        st.parked_entry(Rc::as_ptr(&frame.code).cast::<CodeObject>(), act.compile_id)
    });
    let Some(entry) = entry else {
        return refuse(frame, act);
    };
    let cf = entry.cf.clone();
    let pc = frame.pc;
    if !cf.resume_entries.iter().any(|e| e.pc == pc) {
        return refuse(frame, act);
    }
    if !guards_hold(
        interp,
        &frame.globals,
        &frame.builtins,
        &entry.guard_snapshot,
        &entry.callees,
        &entry.math,
    ) {
        return refuse(frame, act);
    }
    // The compiled continuation types the sent value on the object
    // lane — same admission as `try_enter_resume`.
    let Some(sent) = frame.stack.last() else {
        return refuse(frame, act);
    };
    if !matches!(sent, Object::None | Object::Instance(_)) {
        return refuse(frame, act);
    }
    // A table already at the cap would deopt on the first runtime pin;
    // materialize instead and let a fresh entry rebuild a small one.
    if act.pins.len() >= RUNTIME_PIN_CAP {
        return refuse(frame, act);
    }
    let sent = frame.stack.pop().expect("sent value verified above");
    debug_assert!(frame.stack.is_empty());
    let mut pins = std::mem::take(&mut act.pins);
    let resume_bits = match sent {
        Object::None => u64::MAX,
        obj => {
            let idx = pins.len() as u64;
            pins.push(Pin::Obj(obj));
            idx
        }
    };
    let mut ctx = CallCtx {
        interp: std::ptr::from_mut(interp),
        callees: entry.callees.clone(),
        guard_snapshot: entry.guard_snapshot.clone(),
        globals: frame.globals.clone(),
        builtins: frame.builtins.clone(),
        cells: frame.cells.clone(),
        parked: None,
        raised: None,
        const_pins: std::mem::take(&mut act.const_pins),
        pins,
        obj_globals: entry.obj_globals.clone(),
        obj_global_pins: std::mem::take(&mut act.obj_global_pins),
        attr_guards: entry.attr_guards.clone(),
        methods: entry.methods.clone(),
        math: entry.math.clone(),
        dirty: act.dirty,
        code_ptr: Rc::as_ptr(&frame.code).cast::<CodeObject>(),
        native: entry.native.clone(),
        method_native: entry.method_native.clone(),
        table_gen: current_compile_gen(),
        // Framed entry (generator resume): the resumed `Frame`'s shell
        // is on the spine already.
        frameless_code: None,
    };
    let mut jf = JitFrame {
        locals: act.locals_buf.as_mut_ptr(),
        n_locals: cf.n_locals,
        entry_pc: pc,
        ret_bits: resume_bits,
        ret_tag: 0,
        deopt_pc: 0,
        stack_spill: act.spill.as_mut_ptr(),
        stack_tags: act.tags.as_mut_ptr(),
        stack_len: 0,
        stack_cap: act.spill.len() as u32,
        ctx: std::ptr::from_mut(&mut ctx).cast::<u8>(),
        call_args: act.call_args.as_mut_ptr(),
        call_tags: act.call_tags.as_mut_ptr(),
    };
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        st.stats.gen_resumes += 1;
        st.stats.gen_parked_resumes += 1;
    });
    // SAFETY: the buffers were sized by this exact compilation
    // (`compile_id` matched above) and live in the box across the
    // call; the engine backing `cf` lives in this thread's `JIT`
    // thread-local for the process lifetime; `ctx` outlives the call.
    let status = unsafe { cf.enter(&raw mut jf) };
    note_native_exit(frame, &jf, status);
    if matches!(status, JitStatus::Yielded) {
        if let Some(plan) = park_plan(frame, &entry, &jf) {
            // Re-park in place: same box, same buffers, zero moves.
            let yielded = unpack_pins(act.spill[0], act.tags[0], &ctx.pins);
            act.pins = ctx.pins;
            act.const_pins = ctx.const_pins;
            act.obj_global_pins = ctx.obj_global_pins;
            act.dirty = ctx.dirty;
            act.yield_pc = jf.deopt_pc;
            act.plan = plan;
            frame.pc = jf.deopt_pc + 1;
            frame.parked_native = Some(act);
            JIT.with(|cell| cell.borrow_mut().stats.gen_parks += 1);
            return JitEntry::Yielded(yielded);
        }
    }
    let out = match status {
        JitStatus::Returned => {
            // Final locals writeback so the frame teardown reaps the
            // real last values, not the park-time snapshot.
            let mut locals = frame.locals.borrow_mut();
            for (slot, &bits) in act.locals_buf.iter().enumerate() {
                if let Some(ty) = act.local_types.get(slot).copied().flatten() {
                    if let Some(dst) = locals.get_mut(slot) {
                        *dst = unpack_ty(bits, ty, &ctx.pins);
                    }
                }
            }
            drop(locals);
            JitEntry::Ran(unpack_pins(jf.ret_bits, jf.ret_tag, &ctx.pins))
        }
        JitStatus::Deopt | JitStatus::Raised | JitStatus::Yielded => native_exit_writeback(
            interp,
            frame,
            &entry,
            &act.locals_buf,
            &act.spill,
            &act.tags,
            &jf,
            &mut ctx,
            status,
        ),
    };
    drain_runtime_pins(interp, &mut ctx.pins, act.entry_pin_count);
    let NativeActivation {
        locals_buf,
        spill,
        tags,
        call_args,
        call_tags,
        ..
    } = *act;
    put_u64(locals_buf);
    put_u64(spill);
    put_u32(tags);
    put_u64(call_args);
    put_u32(call_tags);
    out
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

/// Test hook: `Yielded` exit count for the current thread (RFC 0070
/// WS2).
#[cfg(test)]
pub(crate) fn yield_stats_for_test() -> u64 {
    JIT.with(|cell| cell.borrow().stats.yields)
}

/// Test hook: native generator *resume* entry count for the current
/// thread (RFC 0071 WS5).
#[cfg(test)]
pub(crate) fn gen_resume_stats_for_test() -> u64 {
    JIT.with(|cell| cell.borrow().stats.gen_resumes)
}

/// Test hook: `(parks, parked_resumes, materialized)` for the current
/// thread's persistent generator activations (RFC 0073 WS4).
#[cfg(test)]
pub(crate) fn gen_park_stats_for_test() -> (u64, u64, u64) {
    JIT.with(|cell| {
        let s = &cell.borrow().stats;
        (s.gen_parks, s.gen_parked_resumes, s.gen_materialized)
    })
}

/// Test hook: `(native_calls, fallbacks, deopts)` for the current
/// thread's native-to-native call fast path (RFC 0067 WS1).
#[cfg(test)]
pub(crate) fn native_call_stats_for_test() -> (u64, u64, u64) {
    NATIVE_CALL_STATS.with(|s| (s.calls.get(), s.fallbacks.get(), s.deopts.get()))
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
        let (ncalls, nfallbacks, ndeopts, mcalls, mfallbacks, mmisses, direct) = NATIVE_CALL_STATS
            .with(|n| {
                (
                    n.calls.get(),
                    n.fallbacks.get(),
                    n.deopts.get(),
                    n.method_calls.get(),
                    n.method_call_fallbacks.get(),
                    n.method_guard_misses.get(),
                    n.direct_calls.get(),
                )
            });
        Some(format!(
            "\n## Tier-2 JIT stats\n\n\
             - frames seen: **{}**\n\
             - frames compiled: **{}**\n\
             - frames not JITable: **{}**\n\
             - native entries: **{}**\n\
             - OSR entries: **{}**\n\
             - direct native calls: **{}**\n\
             - yields: **{}**\n\
             - generator resumes: **{}**\n\
             - generator parks: **{}**\n\
             - parked resumes: **{}**\n\
             - parked materializations: **{}**\n\
             - deopts: **{}**\n\
             - entry-guard failures: **{}**\n\
             - native-to-native calls: **{}**\n\
             - native-call fallbacks: **{}**\n\
             - native-call deopts: **{}**\n\
             - method calls: **{}**\n\
             - method-call fallbacks: **{}**\n\
             - method guard misses: **{}**\n\
             - generic dyn calls: **{}**\n\
             - generic-call retirements: **{}**\n",
            s.frames_seen,
            s.frames_compiled,
            s.frames_notjitable,
            s.native_entries,
            s.osr_entries,
            direct,
            s.yields,
            s.gen_resumes,
            s.gen_parks,
            s.gen_parked_resumes,
            s.gen_materialized,
            s.deopts,
            s.entry_guard_failures,
            ncalls,
            nfallbacks,
            ndeopts,
            mcalls,
            mfallbacks,
            mmisses,
            s.dyn_generic_calls,
            s.generic_retires,
        ))
    })
}
