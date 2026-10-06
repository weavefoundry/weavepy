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

use crate::shared_value::SharedStr;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
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
    /// A `__slots__` member with a name-checked ordered index. A different
    /// population order retains the named lookup or insertion path.
    Slot(u32),
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
    name: SharedStr,
    /// Python hash of the name, shared by every constructor-store probe.
    name_hash: i64,
    /// The class value can't acquire descriptor hooks independently of
    /// the receiver's class version. Only native scalar updates use this.
    stable_descriptor: bool,
    /// The index the name holds in its class's shared names, when the
    /// site's index is that one (`u32::MAX` otherwise): compiled code
    /// reads and writes a split-layout instance's field there in line
    /// (see [`obj_layout`]). For a `__slots__` member the class lays out,
    /// its position in the layout, marked [`weavepy_jit::SLOT_FIELD`].
    split_idx: u32,
    /// The value lane the site was compiled with.
    lane: JitType,
    /// The class's `attr_version` at compile time. Tokens are globally unique
    /// across class lifetimes and mutations, so this guard needn't own the
    /// original class. Each live receiver owns the class checked at access.
    ver: u64,
    /// How the access reaches its storage.
    storage: AttrStorage,
    /// The class's slot layout's word (see
    /// [`crate::shared_value::SharedSlice::word`]) for a member slot
    /// [`Self::split_idx`] marks, `0` otherwise: an instance of the class
    /// at [`Self::ver`] whose slots are laid out over it keeps the member
    /// at that position (the class owns its layout, so the version names
    /// it).
    slot_layout: usize,
    /// Advisory index into the current activation's pin table. Guards are
    /// shared by nested and suspended activations, so every hit must validate
    /// the actual object's identity in the calling activation's table.
    last_result_pin: Cell<usize>,
}

/// RFC 0069 WS1 — one burned-in method-site resolution: the class-
/// resolved plain Python function the `(slot, name)` probe found, plus
/// the guard fingerprint (a globally unique class-resolution token) and
/// its `__code__` at compile time. `wpjit_call_method` revalidates all of
/// it per call and rejects a mismatch, so class mutation,
/// instance-dict shadowing, and `__code__` rebinding introduced after
/// compilation stay exact.
struct MethodEntry {
    /// What the class resolves the name to.
    callee: MethodCallee,
    /// The class forbids an instance `__dict__` (its `__slots__` cover
    /// every base), so no instance attribute can shadow the method: the
    /// guard skips that probe.
    no_dict: bool,
    /// The method name (for the shadow check and the span rebuild).
    name: String,
    /// `hash(name)`, so the per-call shadow probe hashes nothing.
    name_hash: i64,
    /// Positional arity, `self` included.
    arg_count: u32,
    /// Arity minus trailing defaults, `self` included.
    min_args: u32,
    /// The burned-in result typing.
    ret: MethodRet,
    /// A globally unique resolution token. The live receiver owns the class
    /// checked at access; this cached method must not keep that class alive.
    ver: u64,
    /// What compiled code needs to run this method's callback-free field
    /// update (`self.n += k; return self.n`) in line, armed by
    /// `wpjit_call_method` once the helper has run it (see [`arm_update`]).
    update: InlineUpdate,
    /// The scalar fields the method reads off `self` where the receiver's
    /// class at the entry's version keeps them in split values or laid-out
    /// member slots (see [`inline_fields_of`]): what a call site needs to
    /// run the body in line (see [`inline_method_body`]).
    fields: Vec<InlineField>,
    /// The same for the object arguments of each call site the method was
    /// resolved for (see [`method_call_sites`]).
    sites: Vec<SiteArgs>,
    /// Some compiled site checks this entry's guard in line (it calls the
    /// method's body directly or runs it in line), so the helper keeps the
    /// guard's shadow bound armed (see [`arm_method_guard`]).
    guarded_in_line: Cell<bool>,
}

/// One scalar field a method reads off a receiver (see
/// [`MethodEntry::fields`]).
struct InlineField {
    name: String,
    /// The value's lane when the method was resolved.
    lane: JitType,
    /// Where the receiver's class keeps it (its names and slot layout
    /// never move).
    at: weavepy_jit::FieldAt,
}

/// What one call site of a method passes it, as the live values in its
/// frame showed when the site was compiled: for each argument, its class's
/// version and the fields the method reads off it, when it's an instance
/// whose class keeps some of them in place.
struct SiteArgs {
    /// The site's call instruction.
    pc: u32,
    args: Vec<Option<(u64, Vec<InlineField>)>>,
}

/// [`MethodEntry::update`]: compiled code reads these in place (see
/// [`obj_layout`]) and calls the helper unless the receiver's class still
/// has the entry's version, its values are split over the class's names
/// and are too few to shadow the method's name, the function still wears
/// the entry's code, and no observer or exotic key could tell.
#[repr(C)]
struct InlineUpdate {
    /// The split index of the field the update adds to (`u32::MAX` while
    /// unarmed).
    idx: Cell<u32>,
    /// A receiver holding more split values than this may shadow the
    /// method's name.
    shadow: Cell<u32>,
    /// `1` when the increment is the call's argument; `0` when it's
    /// [`Self::inc`].
    from_arg: Cell<u32>,
    /// The literal increment.
    inc: Cell<i64>,
    /// Where the function keeps its code pointer, and the entry's code
    /// pointer it must equal.
    code_at: Cell<usize>,
    code: Cell<usize>,
}

impl InlineUpdate {
    fn unarmed() -> Self {
        Self {
            idx: Cell::new(u32::MAX),
            shadow: Cell::new(0),
            from_arg: Cell::new(0),
            inc: Cell::new(0),
            code_at: Cell::new(0),
            code: Cell::new(0),
        }
    }
}

/// The callee a burned-in method site resolved to.
enum MethodCallee {
    /// A plain Python function and its `__code__` at compile time
    /// (rebindable, so the guard compares it per call).
    Py {
        func: Rc<PyFunction>,
        code: Rc<CodeObject>,
    },
    /// A native accelerator's method: a builtin that binds its instance,
    /// registered as a leaf and opted into this lane
    /// (`leaf_builtins::register_jit_method`), such as `deque.append`.
    /// Its body runs no Python code, so the helper calls it directly
    /// with no interpreter frame; `fast` is its pure fast half when the
    /// full body may reach Python (an argument's `__index__`).
    Native {
        builtin: Rc<crate::object::BuiltinFn>,
        fast: Option<crate::leaf_builtins::Fast>,
        /// The builtin's direct operation
        /// (`collections_native::fast_op`), `0` for none.
        op: u8,
    },
}

/// One slot per method token (parallel to `cf.method_sites`).
type MethodTable = Vec<MethodEntry>;

/// The widest positional arity a native method site admits (receiver
/// included). The builtin validates its own arity, so this only bounds
/// the marshal buffer.
const NATIVE_METHOD_MAX_ARGS: u32 = 8;

/// One burned-in math intrinsic. The ordinary global snapshot guards the
/// module's identity; this guard reads its current attribute by a checked
/// dictionary index. Neither a value replacement nor a moved key is hidden.
struct MathGuard {
    dict: Rc<GilRefCell<DictData>>,
    attr: SharedStr,
    hash: i64,
    key_idx: std::cell::Cell<usize>,
    expected: Object,
}

type MathTable = Vec<MathGuard>;

impl MathGuard {
    fn snapshot(module: &Object, attr: &str, expected: Object) -> Option<Self> {
        let Object::Module(module) = module else {
            return None;
        };
        let dict = module.dict.borrow();
        let (key_idx, key, value) = dict.get_full(&StrKey(attr))?;
        if !value.is_same(&expected) {
            return None;
        }
        let Object::Str(name) = &key.0 else {
            return None;
        };
        Some(Self {
            dict: module.dict.clone(),
            attr: name.clone(),
            hash: SharedStr::hash_cached(name),
            key_idx: std::cell::Cell::new(key_idx),
            expected,
        })
    }

    #[inline]
    fn holds(&self) -> bool {
        // SAFETY: tier 2 runs under the GIL, and this check invokes no Python
        // callbacks. The guard's owner keeps the dictionary alive.
        let Some(dict) = (unsafe { self.dict.peek() }) else {
            return false;
        };
        if let Some((key, value)) = dict.get_index(self.key_idx.get()) {
            if key_is(key, &self.attr) {
                return value.is_same(&self.expected);
            }
        }
        // Deleting an earlier key moves indices without changing the math
        // function. Refresh the hint with a callback-free name probe.
        let probe = crate::object::LeafNameProbe::new(&self.attr, self.hash);
        let found = dict.get_full(&probe);
        if probe.saw_exotic() {
            return false;
        }
        match found {
            Some((index, _, value)) if value.is_same(&self.expected) => {
                self.key_idx.set(index);
                true
            }
            _ => false,
        }
    }
}

/// The burned-in globals a compilation guards on (`name` → the object
/// it resolved to at compile time), with the namespaces' mutation
/// stamps from the last successful validation: while the entering
/// frame's globals and builtins dicts are the same objects in the same
/// state, every name still resolves identically and the per-name
/// probes are skipped (a resume or entry then costs two stamp reads).
struct GuardSnapshot {
    entries: Vec<(String, Object)>,
    /// `(globals id, globals stamp, builtins id, builtins stamp, global
    /// value epoch)` of the last full validation that held; all-zero
    /// until one has.
    last_ok: std::cell::Cell<(usize, u64, usize, u64, u64)>,
    /// Callees whose defaults the compiled code may have burned in (a
    /// direct leaf call that leaves parameters out): they must keep them.
    defaults: Vec<Rc<PyFunction>>,
    /// Per callee-table entry, the class version at which a burned-in
    /// constructor's full probe last held (`u64::MAX` for none): the
    /// construction plan is a function of it, so an unchanged class only
    /// needs its `__init__` code and metaclass rechecked.
    ctor_vers: Vec<Cell<u64>>,
    /// Live activations of this compile whose frame Python code has
    /// inspected (see [`sync_native_locals`]): while any is, the guards
    /// fail, so each such activation leaves native code right after the
    /// call that inspected it (and no new one enters).
    introspected: Cell<u32>,
}

impl GuardSnapshot {
    fn new(entries: Vec<(String, Object)>, defaults: Vec<Rc<PyFunction>>, callees: usize) -> Self {
        Self {
            entries,
            last_ok: std::cell::Cell::new((0, 0, 0, 0, 0)),
            defaults,
            ctor_vers: (0..callees).map(|_| Cell::new(u64::MAX)).collect(),
            introspected: Cell::new(0),
        }
    }
}

/// Whether `f.__defaults__` no longer is the tuple `f` was defined with.
#[inline]
fn defaults_overridden(f: &PyFunction) -> bool {
    f.defaults_maybe_overridden() && f.slot("__defaults__").is_some()
}

impl std::ops::Deref for GuardSnapshot {
    type Target = [(String, Object)];
    fn deref(&self) -> &[(String, Object)] {
        &self.entries
    }
}

/// A compiled frame plus the globals it burned in: `snapshot[i]` is the
/// object `guards[i].name` resolved to at compile time. Every entry
/// re-resolves each name against the entering frame's namespaces and
/// requires identity (`is_same`) with the snapshot (RFC 0058 WS4).
struct CompiledEntry {
    cf: StdRc<CompiledFrame>,
    guard_snapshot: StdRc<GuardSnapshot>,
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
    // One immutable bundle per compilation, shared without rebuilding its
    // seven component handles on every dynamic call.
    art: StdRc<Artifacts>,
    func: Rc<PyFunction>,
    code: Rc<CodeObject>,
    /// RFC 0071 WS2 — `Some(cls)` when this callee is a *class
    /// constructor*: `cf`/`func`/`code` describe the compiled
    /// `__init__` (method shape, the fresh instance as pin 0), and the
    /// call site's value is the allocated instance, not `__init__`'s
    /// `None`.
    ctor: Option<Rc<TypeObject>>,
}

impl std::ops::Deref for NativeCallee {
    type Target = Artifacts;

    fn deref(&self) -> &Self::Target {
        &self.art
    }
}

/// One slot per callee-table token (parallel to [`CalleeTable`]).
type NativeTable = Vec<Option<NativeCallee>>;

/// RFC 0069 WS3b — everything a frameless interpreter→native call
/// needs, resolved once per compile generation and handed out as a
/// single `Rc` clone per call (the per-call lookup cost is what makes
/// or breaks a ~100ns call).
struct DirectEntry {
    art: StdRc<Artifacts>,
    native: Option<StdRc<NativeTable>>,
    method_native: Option<StdRc<NativeTable>>,
    /// `true` = receiver-in-slot-0 shape ([`native_method_callable`]);
    /// `false` = all-scalar parameters ([`native_callable`]).
    method_shape: bool,
}

/// Everything one successful compile produced (RFC 0069 — the pieces
/// outgrew a tuple): the native frame plus the guard snapshots and
/// resolution tables its entries validate against.
struct Artifacts {
    /// The compiled code object. A compiled entry pins its code (the
    /// native callers that hold these artifacts read it); an entry that
    /// never compiled holds only a weak handle (see [`CacheEntry::code`]).
    code: Rc<CodeObject>,
    cf: StdRc<CompiledFrame>,
    /// A plan for a callback-free bound update, owned by this compilation.
    /// Other code adds no allocation and keeps ordinary native dispatch.
    scalar_update: Option<Box<ScalarFieldUpdatePlan>>,
    snap: StdRc<GuardSnapshot>,
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
    /// Native-to-native entries of this compilation, and the interpreter
    /// round-trips they made (see [`note_callee_exit`]).
    callee_entries: Cell<u32>,
    callee_roundtrips: Cell<u32>,
}

/// RFC 0073 WS4 — source of [`Artifacts::compile_id`].
static NEXT_COMPILE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Per-`CodeObject` compilation state.
enum Tier {
    Cold,
    NotJitable,
    Compiled(StdRc<Artifacts>),
}

struct CacheEntry {
    counter: u32,
    tier: Tier,
    /// A short first activation doesn't have enough remaining work to pay
    /// for compilation. A fresh call clears this advisory deferral.
    defer_osr: bool,
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
    /// A compiled frame took a cold exit (see `CompiledFrame::cold_exits`):
    /// the code after it was unsupported *with the values live when it was
    /// compiled* (typically a later loop's receiver, still unbound when an
    /// earlier loop triggered the compile). The next OSR request at a loop
    /// header the frame can't enter recompiles from there, with that
    /// loop's live values, at most [`COLD_RECOMPILE_BUDGET`] times.
    recompile_at_osr: bool,
    /// Recompiles taken for [`Self::recompile_at_osr`].
    cold_recompiles: u8,
    /// Back edges run while cold (counted a consultation's stride at a
    /// time) and the activations they ran in: the activations counted
    /// here, plus the lean ones the code's `jit_hint` counts (credited
    /// here before its count restarts, and debited when it moves ahead,
    /// so `calls` wraps). Their ratio decides
    /// whether the interpreter's calls should enter the compiled code
    /// (see [`LOOP_CALL_ITERATIONS`]).
    backedges: u32,
    calls: u32,
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
    /// Keeps the code object's allocation reserved, so its address can't
    /// be reused while this entry is keyed by it, without keeping the code
    /// itself alive: every code object that runs gets an entry, and most
    /// never compile (a module body runs once), so a strong handle kept
    /// each one's instructions, constants, and side tables for the rest
    /// of the process. A compiled entry's [`Artifacts::code`] pins it. A
    /// dead code's entry is dropped by the next sweep (see
    /// [`cache_entry`] and [`gc_sweep`]).
    code: crate::sync::Weak<CodeObject>,
}

impl CacheEntry {
    fn new(code: &Rc<CodeObject>) -> CacheEntry {
        CacheEntry {
            counter: 0,
            tier: Tier::Cold,
            defer_osr: false,
            osr_failures: 0,
            backedges: 0,
            calls: 0,
            deopts: 0,
            native_entries: 0,
            generic_dyn_calls: 0,
            probe_misses: Vec::new(),
            recompile_at_osr: false,
            cold_recompiles: 0,
            native: None,
            method_native: None,
            direct: None,
            code: Rc::downgrade(code),
        }
    }

    /// Whether nothing but this thread's tier cache keeps the entry's code
    /// alive: it is dead, or only the entry's own compiled artifacts hold it.
    fn code_unowned(&self) -> bool {
        match &self.tier {
            Tier::Compiled(a) => Rc::strong_count(&a.code) == 1,
            _ => self.code.strong_count() == 0,
        }
    }
}

/// The fewest tier-cache entries that trigger a sweep of dead ones.
const CACHE_SWEEP_MIN: usize = 1024;

/// The tier-cache entry for `code` in `cache`, made on its first use. A
/// cache that has grown to `sweep_at` entries first drops those of code
/// objects that have died (see [`JitState::sweep_at`]).
fn cache_entry<'a>(
    cache: &'a mut CodeMap<CacheEntry>,
    sweep_at: &mut usize,
    code: &Rc<CodeObject>,
) -> &'a mut CacheEntry {
    if cache.len() >= *sweep_at {
        cache.retain(|_, e| e.code.strong_count() != 0);
        *sweep_at = (cache.len() * 2).max(CACHE_SWEEP_MIN);
    }
    cache
        .entry(Rc::as_ptr(code).cast::<CodeObject>())
        .or_insert_with(|| CacheEntry::new(code))
}

/// Give up on OSR for a code object after this many failed validations.
const OSR_FAILURE_BUDGET: u32 = 64;

/// The fewest loop iterations a call, as a compile measures them (back
/// edges per activation while the code was cold), for the interpreter's
/// calls to enter the compiled code at pc 0 instead of running it lean.
/// The few iterations of `raytrace`'s `first_hit` gain more natively than
/// the general call costs (the benchmark retires 3% fewer instructions),
/// while tomllib's `skip_chars`, about one iteration a call, would cost
/// `tomllib_loads` 23% more. A lean call that runs long still enters
/// native code from its back edge.
const LOOP_CALL_ITERATIONS: u32 = 2;

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

/// [`GENERIC_CALL_RETIRE_RATIO`] for native-to-native entries. Such a
/// callee is usually loop-free, so native code saves it a few dozen
/// nanoseconds per activation while one interpreter call from it costs
/// several hundred more than the interpreter's inline call (measured on
/// deltablue's `execute` / `input` / `output` methods: 4x slower compiled).
pub(crate) const CALLEE_ROUNDTRIP_RETIRE_RATIO: u32 = 1;

/// Retire a compiled code object — and deopt the running activation —
/// once one activation has made this many interpreter round-trips
/// through the call helpers. Each such call pays activation-shell
/// setup, a generic call, and a full `guards_hold` re-validation; the
/// interpreter's own call path is several times cheaper. A native
/// activation that keeps coming back here is a loop around calls the
/// JIT cannot run natively, so the interpreter finishes it (and runs
/// every later activation).
/// Zero disables the backoff (its `!= 0` guards then short-circuit, and
/// clippy reads the `>=` behind them as vacuous — hence the allows at
/// the three sites). Kept as a constant rather than deleted: the
/// measurement that zeroed it is recorded above, and re-arming it is a
/// one-line change.
pub(crate) const INTERP_CALL_RETIRE_BUDGET: u32 = 0;

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
    /// Fully reconstructed exits requested to release native temporary pins.
    /// These do not spend the speculative-deopt budget.
    pub pin_pressure_exits: u64,
    /// Expected hand-offs at cold exits (see `CompiledFrame::cold_exits`).
    pub cold_exits: u64,
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
    /// Bounded scalar calls entered without a pin table or call context.
    scalar_leaf_calls: std::cell::Cell<u64>,
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

// Code identities are aligned allocation addresses. Fold their upper hash
// bits into the lower bits, as the GC index does, to spread home buckets.
#[derive(Default)]
struct CodeIdentityHasher(crate::fasthash::FxHasher);

impl Hasher for CodeIdentityHasher {
    #[inline]
    fn finish(&self) -> u64 {
        let hash = self.0.finish();
        hash ^ (hash >> 32)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.0.write(bytes);
    }

    #[inline]
    fn write_usize(&mut self, word: usize) {
        self.0.write_usize(word);
    }
}

type CodeMap<V> = HashMap<*const CodeObject, V, BuildHasherDefault<CodeIdentityHasher>>;

struct JitState {
    enabled: bool,
    threshold: u32,
    range_budget: bool,
    engine: Option<JitEngine>,
    cache: CodeMap<CacheEntry>,
    /// The cache size at which [`cache_entry`] next sweeps out the
    /// entries of dead code objects (twice the live count the last sweep
    /// left, so sweeping stays amortized constant time per entry).
    sweep_at: usize,
    stats: JitStats,
    /// RFC 0067 WS1 — bumped on every successful compile; stale
    /// native-callee tables (stamped with an older generation) are
    /// re-resolved so a newly compiled callee graduates from the
    /// interpreter call path to the native one.
    compile_gen: u64,
}

/// Process-wide "could any thread's JIT be on?" gate: `0` not yet
/// derived, `1` possibly on, `2` off for the whole run. Whether the JIT
/// runs is settled by the environment and the free-threading mode, both
/// fixed before the first Python call, so every per-thread [`JitState`]
/// agrees. The tier-up hooks sit on every Python call and loop back
/// edge; with the JIT off they answer from this one relaxed load instead
/// of a thread-local borrow.
static JIT_PROCESS_GATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Warm the code generator on a background thread (see
/// [`prewarm_codegen`]), once per process, when the first code object is
/// halfway to its compile threshold. Not at start-up: paging the code
/// generator in costs a program that never compiles anything about 2 MB
/// of resident memory (a third of an empty script's over CPython's).
fn spawn_codegen_prewarm() {
    static SPAWNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SPAWNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("weavepy-jit-warm".to_owned())
        .spawn(prewarm_codegen);
}

/// Compile, on a throwaway engine, a small counted loop of the shape hot
/// code takes, and discard it. A process's first compile otherwise pays
/// the code generator's cold start (its code paged in and its tables
/// built: about 0.7 ms on the development host, most of the first
/// compile) on the thread that needs the compiled code. Touches no
/// interpreter state.
fn prewarm_codegen() {
    const SOURCE: &str =
        "def f(n):\n    t = 0\n    for i in range(n):\n        t = t + i * 2\n    return t\n";
    let _ = std::panic::catch_unwind(|| {
        let Ok(module) = weavepy_parser::parse_module(SOURCE) else {
            return;
        };
        let Ok(code) = weavepy_compiler::compile_module(&module) else {
            return;
        };
        let Some(f) = code.constants.iter().find_map(|c| match c {
            weavepy_compiler::Constant::Code(f) => Some(f.clone()),
            _ => None,
        }) else {
            return;
        };
        let Some(mut engine) = JitEngine::new() else {
            return;
        };
        let _ = engine.compile(&f, &mut |name| {
            if name == "range" {
                weavepy_jit::ResolvedGlobal::RangeBuiltin
            } else {
                weavepy_jit::ResolvedGlobal::Opaque
            }
        });
    });
}

/// The `WEAVEPY_JIT` / free-threading verdict shared by every thread.
fn jit_enabled_by_config() -> bool {
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
    enabled && !crate::gil::free_threading_requested()
}

/// Whether interpreter startup (the `site` import and everything it
/// pulls in) is over. Startup frame/backedge counters use the existing
/// sixteen-times threshold; naturally hot lean callees can still compile
/// through `warm_compile`, as they did before explicit startup deferral.
static STARTUP_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompilationPhase {
    Normal,
    Startup,
    Import,
}

thread_local! {
    /// Startup keeps its own admission policy across nested imports.
    static COMPILATION_PHASE: std::cell::Cell<CompilationPhase> =
        const { std::cell::Cell::new(CompilationPhase::Normal) };

    /// The lean entry count at which the interpreter warms a tier-2
    /// compile, clamped to the tier-2 threshold.
    ///
    /// A loop-free callee never gets a frame entry to count, so this is
    /// the only thing that compiles it — and a native caller can only
    /// take a direct lane into a callee that *is* compiled. Waiting for
    /// the lean constant when tier-2 would have compiled after far fewer
    /// entries left those callees interpreted and every native call site
    /// generic.
    ///
    /// Thread-local, like the threshold it is clamped to: a process-wide
    /// cell would let one thread's threshold decide another's warm
    /// point, which in the test binary means whichever test ran last.
    static LEAN_WARM_AT: std::cell::Cell<u32> =
        const { std::cell::Cell::new(LEAN_WARM_COMPILE_THRESHOLD_CAP) };
}

/// The cap the clamp above starts from.
pub(crate) const LEAN_WARM_COMPILE_THRESHOLD_CAP: u32 = 24;

#[inline]
/// Credit `n` activations a frameless path ran for `code` to its tier-2
/// warm-up counter (the framed entries that count otherwise never happen
/// for them). Returns whether the next framed or lean entry should
/// compile it: a compile is due, or no entry exists yet to count in.
pub(crate) fn note_frameless_calls(code: &CodeObject, n: u32) -> bool {
    JIT.with(|cell| {
        let Ok(mut st) = cell.try_borrow_mut() else {
            return false;
        };
        if !st.enabled {
            return false;
        }
        let threshold = st.threshold;
        match st.cache.get_mut(&std::ptr::from_ref(code)) {
            None => true,
            Some(entry) if matches!(entry.tier, Tier::Cold) => {
                entry.counter = entry.counter.saturating_add(n);
                let next = entry.counter.saturating_add(1);
                let due = next >= threshold && compile_allowed(next, threshold);
                if !due {
                    // The caller restarts the lean count.
                    entry.calls = entry.calls.wrapping_add(n);
                }
                due
            }
            Some(_) => false,
        }
    })
}

pub(crate) fn lean_warm_at() -> u32 {
    LEAN_WARM_AT
        .try_with(std::cell::Cell::get)
        .unwrap_or(LEAN_WARM_COMPILE_THRESHOLD_CAP)
}

/// Clamp [`LEAN_WARM_AT`] to `threshold` (called wherever the tier-2
/// threshold is established).
fn set_lean_warm_from_threshold(threshold: u32) {
    let v = LEAN_WARM_COMPILE_THRESHOLD_CAP.min(threshold.max(1));
    let _ = LEAN_WARM_AT.try_with(|c| c.set(v));
}

/// A nested compilation scope. The marker keeps restoration on the thread
/// that established it, including when unwinding from module execution.
pub(crate) struct CompilationGuard {
    previous: CompilationPhase,
    _thread: std::marker::PhantomData<*mut ()>,
}

pub(crate) fn startup_compilation_scope() -> CompilationGuard {
    CompilationGuard {
        previous: COMPILATION_PHASE.with(|phase| phase.replace(CompilationPhase::Startup)),
        _thread: std::marker::PhantomData,
    }
}

pub(crate) fn budget_import_compilation() -> CompilationGuard {
    CompilationGuard {
        previous: COMPILATION_PHASE.with(|phase| {
            let previous = phase.get();
            if previous != CompilationPhase::Startup {
                phase.set(CompilationPhase::Import);
            }
            previous
        }),
        _thread: std::marker::PhantomData,
    }
}

impl Drop for CompilationGuard {
    fn drop(&mut self) {
        COMPILATION_PHASE.with(|phase| phase.set(self.previous));
    }
}

fn compilation_phase() -> CompilationPhase {
    COMPILATION_PHASE.with(std::cell::Cell::get)
}

#[cfg(test)]
fn import_compilation_budget() -> bool {
    compilation_phase() == CompilationPhase::Import
}

/// Mark interpreter start-up finished (see [`STARTUP_DONE`]).
pub(crate) fn note_startup_finished() {
    STARTUP_DONE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a code object whose counter reached `counter` against
/// `threshold` may compile now. Fresh imports and embedders that never
/// report startup completion require sustained work beyond the normal
/// threshold. Startup scopes keep this escape hatch and exclude nested
/// import budgets rather than prohibiting compilation altogether.
#[inline]
fn compile_allowed(counter: u32, threshold: u32) -> bool {
    let phase = compilation_phase();
    (phase == CompilationPhase::Normal && STARTUP_DONE.load(std::sync::atomic::Ordering::Relaxed))
        || counter >= threshold.saturating_mul(IMPORT_THRESHOLD_FACTOR)
}

/// How many times the normal threshold's work code run during start-up
/// or an import needs before it compiles: import-time code (a regex
/// compiler, a table builder) mostly runs once per process, and an
/// attempt costs milliseconds.
const IMPORT_THRESHOLD_FACTOR: u32 = 64;

/// [`compile_allowed`] for the frame compiler (see `frame_jit`), which
/// keeps tier 2's start-up and import budgets.
pub(crate) fn frame_compile_allowed(counter: u32, threshold: u32) -> bool {
    compile_allowed(counter, threshold)
}

/// True when no thread's JIT can be enabled (see [`JIT_PROCESS_GATE`]).
#[inline]
pub(crate) fn jit_off_for_process() -> bool {
    match JIT_PROCESS_GATE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => jit_process_gate_init(),
    }
}

#[cold]
fn jit_process_gate_init() -> bool {
    let on = jit_enabled_by_config();
    JIT_PROCESS_GATE.store(if on { 1 } else { 2 }, std::sync::atomic::Ordering::Relaxed);
    !on
}

impl JitState {
    fn new() -> JitState {
        let enabled = jit_enabled_by_config();
        let explicit_threshold = std::env::var("WEAVEPY_JIT_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|n| *n > 0);
        let threshold = explicit_threshold.unwrap_or(50);
        set_lean_warm_from_threshold(threshold);
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
        weavepy_jit::register_list_from_range_helper(wpjit_list_from_range);
        // RFC 0071 WS6 — the string/bytes read helpers.
        weavepy_jit::register_str_helpers(
            wpjit_str_eq,
            wpjit_str_len,
            wpjit_bytes_len,
            wpjit_bytes_get,
        );
        weavepy_jit::register_attr_helpers(wpjit_attr_get, wpjit_attr_set);
        weavepy_jit::register_attr_get_chain_helper(wpjit_attr_get_chain);
        weavepy_jit::register_cached_attr_chain_helper(wpjit_cached_attr_chain);
        // RFC 0073 WS2 — the dict-lane helpers.
        weavepy_jit::register_dict_helpers(
            wpjit_dict_get,
            wpjit_dict_set,
            wpjit_dict_contains,
            wpjit_dict_len,
        );
        weavepy_jit::register_dict_del_helper(wpjit_dict_del);
        weavepy_jit::register_build_map_helper(wpjit_build_map);
        weavepy_jit::register_const_str_helper(wpjit_const_str);
        weavepy_jit::register_tuple_read_helpers(wpjit_const_tuple, wpjit_tuple_len);
        weavepy_jit::register_unbox_int_helper(wpjit_unbox_int);
        weavepy_jit::register_unbox_float_helper(wpjit_unbox_float);
        weavepy_jit::register_is_obj_helper(wpjit_is_obj);
        weavepy_jit::register_dict_iter_helper(wpjit_dict_iter_new);
        // RFC 0073 WS3 — the string write lanes.
        weavepy_jit::register_str_write_helpers(
            wpjit_str_concat,
            wpjit_str_get,
            wpjit_build_string,
        );
        // RFC 0067 WS2 — the eval-breaker poll for native loop headers.
        weavepy_jit::register_poll_helper(wpjit_poll);
        weavepy_jit::register_self_call_helpers(wpjit_self_enter, wpjit_self_exit, wpjit_self_slow);
        weavepy_jit::register_method_enter_helper(wpjit_method_enter);
        // RFC 0069 WS1 — the guarded method-call lane.
        weavepy_jit::register_call_method_helper(wpjit_call_method);
        weavepy_jit::register_call_native_method_helper(wpjit_call_native_method);
        // Native container subscripts (`deque.__getitem__`) and native
        // method-site load guards.
        weavepy_jit::register_obj_getitem_helper(wpjit_obj_getitem);
        weavepy_jit::register_guard_method_helper(wpjit_guard_method);
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
        weavepy_jit::register_float_pow_helper(wpjit_float_pow);
        // RFC 0074 — the frame-coverage lanes: obj globals, the
        // opaque-call lane, dynamic attributes, generic/pair
        // iteration, str %-format and slice.
        weavepy_jit::register_global_obj_helper(wpjit_global_obj);
        weavepy_jit::register_call_dyn_helper(wpjit_call_dyn);
        weavepy_jit::register_call_dyn_int_helper(wpjit_call_dyn_int);
        weavepy_jit::register_dyn_attr_helpers(wpjit_dyn_attr_get, wpjit_dyn_attr_set);
        // RFC 0076 WS8 — object-lane truthiness, generic membership,
        // and set literals.
        weavepy_jit::register_truth_helper(wpjit_truth);
        weavepy_jit::register_contains_dyn_helper(wpjit_contains_dyn);
        weavepy_jit::register_dyn_op_helpers(wpjit_dyn_binop, wpjit_dyn_compare);
        weavepy_jit::register_dyn_item_helpers(
            wpjit_dyn_getitem,
            wpjit_dyn_setitem,
            wpjit_dyn_unary,
        );
        weavepy_jit::register_build_set_helper(wpjit_build_set);
        weavepy_jit::register_iter_new_helper(wpjit_iter_new);
        weavepy_jit::register_iter_next_pair_helper(wpjit_iter_next_pair);
        weavepy_jit::register_str_format_helpers(wpjit_str_mod, wpjit_str_slice);
        JitState {
            enabled,
            threshold,
            range_budget: explicit_threshold.is_none(),
            engine: None,
            cache: CodeMap::default(),
            sweep_at: CACHE_SWEEP_MIN,
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
        // Whether the code's calls run enough iterations of its loops for
        // a native entry to pay for the general call.
        let long_calls;
        {
            let entry = cache_entry(&mut self.cache, &mut self.sweep_at, code);
            if entry_pc == 0 {
                // Backedge heat survives: repeated calls can amortize the
                // compile even when an individual activation is short.
                entry.defer_osr = false;
            }
            // A cold exit handed the activation to the interpreter, which
            // now asks to enter a loop the frame can't: compile again from
            // here (see `CacheEntry::recompile_at_osr`).
            // A loop carved out for its code alone stays interpreted; one
            // carved out for want of live values recompiles from inside.
            let recompile = entry_pc != 0
                && matches!(&entry.tier, Tier::Compiled(a)
                    if !a.cf.osr_entries.iter().any(|e| e.pc == entry_pc)
                        && ((entry.recompile_at_osr && !a.cf.stayed_heads.contains(&entry_pc))
                            || (a.cf.env_heads.contains(&entry_pc)
                                && entry.cold_recompiles < COLD_RECOMPILE_BUDGET)));
            if recompile {
                entry.recompile_at_osr = false;
                entry.cold_recompiles = entry.cold_recompiles.saturating_add(1);
                entry.osr_failures = 0;
                entry.tier = Tier::Cold;
                entry.counter = entry.counter.max(self.threshold);
            }
            match &entry.tier {
                Tier::Cold if recompile => {}
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
                    if entry_pc == 0 {
                        entry.calls = entry.calls.wrapping_add(1);
                    }
                    if entry.counter == self.threshold / 2 && self.engine.is_none() {
                        spawn_codegen_prewarm();
                    }
                    if entry.counter < self.threshold
                        || !compile_allowed(entry.counter, self.threshold)
                    {
                        return None;
                    }
                    // Hot code keeps its inline-cache tables from here on,
                    // whatever the verdict (the compiled form's interpreter
                    // round trips and every later deopt use them).
                    super::mark_site_tables_warm(code);
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
            let calls = entry
                .calls
                .wrapping_add(code.jit_hint.lean_entries())
                .max(1);
            long_calls = entry.backedges / calls >= LOOP_CALL_ITERATIONS;
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
            stack_iter,
            pairs,
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
            // A `RefCell` because the direct-method lookup below reads the
            // table while `probe_method` (a sibling `&mut` closure) grows it.
            let methods: std::cell::RefCell<MethodTable> = std::cell::RefCell::new(Vec::new());
            let mut method_tokens: HashMap<(u32, Vec<String>, String), u32> = HashMap::new();
            let mut probe_method =
                |slot: u32, path: &[String], name: &str| -> Option<MethodResolution> {
                    if let Some(&token) = method_tokens.get(&(slot, path.to_vec(), name.to_owned()))
                    {
                        let methods = methods.borrow();
                        let e = &methods[token as usize];
                        return Some(MethodResolution {
                            token,
                            arg_count: e.arg_count,
                            min_args: e.min_args,
                            ret: e.ret,
                            native: matches!(e.callee, MethodCallee::Native { .. }),
                            elem: matches!(e.callee, MethodCallee::Native { op, .. }
                                if crate::stdlib::collections_native::op_returns_element(op)),
                        });
                    }
                    let e = method(slot, path, name)?;
                    let mut methods = methods.borrow_mut();
                    let token = methods.len() as u32;
                    method_tokens.insert((slot, path.to_vec(), name.to_owned()), token);
                    let res = MethodResolution {
                        token,
                        arg_count: e.arg_count,
                        min_args: e.min_args,
                        ret: e.ret,
                        native: matches!(e.callee, MethodCallee::Native { .. }),
                        elem: matches!(e.callee, MethodCallee::Native { op, .. }
                                if crate::stdlib::collections_native::op_returns_element(op)),
                    };
                    methods.push(e);
                    Some(res)
                };
            // RFC 0069 WS2 — the math probe reports eligibility only; the
            // guard snapshot below re-resolves each burned pair.
            let mut probe_math = |name: &str, attr_name: &str| math_attr(name, attr_name).is_some();
            // RFC 0069 WS3 — parameter-lane seeding, active on retry only.
            // The same live-local grading types an OSR-only region's
            // inputs on every attempt (`Probes::local`).
            let param_cell = std::cell::RefCell::new(&mut **param);
            let mut probe_param = |slot: u32| {
                if seed_params {
                    (param_cell.borrow_mut())(slot)
                } else {
                    None
                }
            };
            let mut probe_local = |slot: u32| (param_cell.borrow_mut())(slot);
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
                local: &mut probe_local,
                stack_iter: &mut **stack_iter,
                pairs: &mut **pairs,
                entry_pc: Some(entry_pc),
                carve_env: seed_params,
                paths: &mut path_arena,
            };
            let t0 = std::env::var_os("WEAVEPY_JIT_TRACE")
                .is_some()
                .then(std::time::Instant::now);
            // A callee already compiled as a guard-free scalar leaf is
            // entered directly by native code (its identity is guarded
            // with the callee table like any burned-in callee).
            // A site that leaves trailing parameters out binds the
            // function's scalar defaults there (guarded through
            // `GuardSnapshot::defaults`).
            let mut direct = |token: u32| {
                let callees = callees.borrow();
                let (Object::Function(f), fcode) = callees.get(token as usize)? else {
                    return None;
                };
                let k = Rc::as_ptr(fcode).cast::<CodeObject>();
                let leaf = match &cache_ref.get(&k)?.tier {
                    Tier::Compiled(a) => a.cf.direct_leaf(fcode.arg_count)?,
                    _ => return None,
                };
                if f.defaults.is_empty() || defaults_overridden(f) {
                    return Some(leaf);
                }
                let n = leaf.params().len();
                let first = n.saturating_sub(f.defaults.len());
                let defaults = (0..n)
                    .map(|i| {
                        let d = f.defaults.get((i + f.defaults.len()).checked_sub(n)?)?;
                        let lane = leaf.params()[i];
                        // `bool` is not `int` here (the lanes are exact).
                        let exact = matches!(
                            (lane, d),
                            (JitType::Int, Object::Int(_))
                                | (JitType::Float, Object::Float(_))
                                | (JitType::Bool, Object::Bool(_))
                        );
                        (i >= first && exact).then(|| pack(d, lane)).flatten()
                    })
                    .collect();
                Some(leaf.with_defaults(defaults))
            };
            // A method site whose resolved function is compiled as a scalar
            // body that never reads `self` enters it directly once the
            // site's guard holds (see `wpjit_method_enter`).
            let mut direct_method = |token: u32| {
                let methods = methods.borrow();
                let entry = methods.get(token as usize)?;
                let MethodCallee::Py { code: mcode, .. } = &entry.callee else {
                    return None;
                };
                let k = Rc::as_ptr(mcode).cast::<CodeObject>();
                let leaf = match &cache_ref.get(&k)?.tier {
                    Tier::Compiled(a) => a.cf.direct_method_leaf(mcode.arg_count)?,
                    _ => return None,
                };
                entry.guarded_in_line.set(true);
                Some(leaf)
            };
            // A method site passing scalar arguments runs a body that only
            // computes over them and `self`'s scalar fields in line (see
            // `inline_method_body`).
            let mut inline_method = |token: u32, lanes: &[JitType], pc: u32| {
                let methods = methods.borrow();
                let entry = methods.get(token as usize)?;
                let body = inline_method_body(entry, lanes, pc)?;
                entry.guarded_in_line.set(true);
                Some(body)
            };
            // An attribute site whose receiver keeps the attribute in a
            // laid-out member slot reads and writes it there in line.
            let mut slot_member =
                |site: &AttrSiteMeta| attr_guard_of(site).is_some_and(|g| g.slot_layout != 0);
            ensure_obj_layout();
            let r = engine.compile_frame_direct(
                code,
                &mut classify,
                &mut jit_probes,
                &mut direct,
                &mut direct_method,
                &mut inline_method,
                &mut slot_member,
            );
            if let Some(t0) = t0 {
                eprintln!("jit compile-time {:?} {:?}", code.name, t0.elapsed());
            }
            let obj_names = obj_names.into_inner().into_iter().map(|(n, _)| n).collect();
            (r, callees.into_inner(), methods.into_inner(), obj_names)
        };
        let (res, callees, methods, obj_names) = {
            let first = run(false);
            if matches!(
                first.0,
                Err(weavepy_jit::JitVerdict::TypeUnknown | weavepy_jit::JitVerdict::ProbeMiss(_))
            ) {
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
                    let guard = math_attr(&g.name, &g.attr).and_then(|expected| {
                        // Every math module is also an identity-guarded global.
                        // Reuse that snapshot instead of resolving it per call.
                        let (_, module) = snap.iter().find(|(name, _)| name == &g.name)?;
                        MathGuard::snapshot(module, &g.attr, expected)
                    });
                    match guard {
                        Some(guard) => math_tbl.push(guard),
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
                    if crate::hot_gates::env_flags::jit_trace() {
                        eprintln!(
                            "jit compile {:?} (entry pc {entry_pc}, scalar leaf {}, generic/total ops {:?}, kinds {:?})",
                            code.name,
                            cf.is_scalar_leaf(),
                            cf.op_mix(),
                            cf.op_kinds()
                        );
                    }
                    // The interpreter's lean paths hand a self-recursive
                    // compiled body's calls to the native entry from now
                    // on: it reaches its native self through the direct
                    // lanes. Any other loop-free body is cheaper to
                    // interpret than to frame (and an OO body's calls
                    // would bounce back through the interpreter).
                    //
                    // So are the calls of a loop that runs a few
                    // iterations a call or more (see
                    // [`LOOP_CALL_ITERATIONS`]): a native entry at pc 0
                    // pays for the general call. A loop that runs fewer (a
                    // scanner's `while s[i] in chars`) stays interpreted,
                    // entering its native form from the back edge when a
                    // call runs long.
                    let recursive = entry_pc == 0
                        && callees
                            .iter()
                            .any(|(_, c)| std::ptr::eq(Rc::as_ptr(c), std::ptr::from_ref(&**code)));
                    let hot_loop = long_calls
                        && cf.interp_entry
                        && !(code.is_generator || code.is_coroutine || code.is_async_generator);
                    if recursive || hot_loop {
                        code.jit_hint.mark_compiled();
                    }
                    let scalar_update =
                        scalar_field_update_plan(code, &cf, &attr_guards).map(Box::new);
                    // Every callee whose defaults a direct leaf call could
                    // have bound (see `direct` above).
                    let burned_defaults = callees
                        .iter()
                        .filter_map(|(f, _)| match f {
                            Object::Function(f)
                                if !f.defaults.is_empty() && !defaults_overridden(f) =>
                            {
                                Some(f.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    let artifacts = StdRc::new(Artifacts {
                        code: code.clone(),
                        cf: StdRc::new(cf),
                        scalar_update,
                        snap: StdRc::new(GuardSnapshot::new(snap, burned_defaults, callees.len())),
                        callees: StdRc::new(callees),
                        obj_globals: StdRc::new(obj_globals),
                        attr_guards: StdRc::new(attr_guards),
                        methods: StdRc::new(methods),
                        math: StdRc::new(math_tbl),
                        compile_id: NEXT_COMPILE_ID
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                        callee_entries: Cell::new(0),
                        callee_roundtrips: Cell::new(0),
                    });
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
                if crate::hot_gates::env_flags::jit_trace() {
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
            cf: art.cf.clone(),
            guard_snapshot: art.snap.clone(),
            callees: art.callees.clone(),
            obj_globals: art.obj_globals.clone(),
            attr_guards: art.attr_guards.clone(),
            methods: art.methods.clone(),
            math: art.math.clone(),
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
            .map(|m| match &m.callee {
                MethodCallee::Py { func, code } => self.resolve_native_func(func, code, true),
                MethodCallee::Native { .. } => None,
            })
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
            (a.clone(), a.code.clone())
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
            Object::Type(t) if t.metaclass_is_type() => {
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
            art: a.clone(),
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
        let entry = cache_entry(&mut self.cache, &mut self.sweep_at, code);
        match entry.tier {
            Tier::Cold => {
                // One consultation stands for a stride of back edges.
                let stride = u32::from(weavepy_compiler::JitHint::BACKEDGE_STRIDE);
                entry.backedges = entry.backedges.saturating_add(stride);
                if entry.defer_osr {
                    return false;
                }
                entry.counter = entry.counter.saturating_add(stride);
                entry.counter >= self.threshold
            }
            Tier::Compiled(..) => {
                if entry.osr_failures < OSR_FAILURE_BUDGET {
                    true
                } else {
                    code.jit_hint.set_backedge_quiet();
                    false
                }
            }
            Tier::NotJitable => {
                code.jit_hint.mark_not_jitable();
                false
            }
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
    /// The lane of the live loop iterator at an interpreter-stack index
    /// of the requesting activation (see [`probe_stack_iter_lane`]).
    stack_iter: &'a mut dyn FnMut(u32) -> Option<JitType>,
    /// The pair lanes of a local list or tuple of 2-tuples (see
    /// [`probe_pair_lanes`]).
    pairs: &'a mut dyn FnMut(u32) -> Option<(JitType, JitType)>,
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
            OpCode::LoadFast | OpCode::LoadConst | OpCode::LoadSmallInt => {
                let src = match ins[i].op {
                    // CPython 3.14 LOAD_SMALL_INT (`self.x = 0`).
                    OpCode::LoadSmallInt => CtorFieldSrc::Lane(JitType::Int),
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
    let (icode, arg_count, min_args) = probe_class_ctor_shape(interp, cls)?;
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

/// [`probe_class_ctor`] without the field plan (the guard predicate only
/// needs the constructor's `__init__` code): `(init code, arg_count,
/// min_args)`.
fn probe_class_ctor_shape(
    interp: &super::Interpreter,
    cls: &Rc<TypeObject>,
) -> Option<(Rc<CodeObject>, u32, u32)> {
    let bt = crate::builtin_types::builtin_types();
    // `type` subclasses (metaclasses) construct *classes* through the
    // three-argument form, never plain instances.
    if cls.flags.is_builtin || cls.is_subclass_of(&bt.type_) || !cls.metaclass_is_type() {
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
    Some((icode, arg_count, min_args))
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
    // A pinned list crosses as its pin too (re-pinned on the callee side
    // with the same element lane).
    scalar_lane_ty(t) || t == JitType::Obj || t.is_list()
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
    /// identity. A weak handle pins the allocation address against reuse
    /// without keeping its code payload alive. A strong handle here and in
    /// the tier cache would keep each cache from becoming the sole owner.
    /// Both are *predictions* — the call helpers re-check the actual
    /// result at runtime — so staleness (e.g. the callee's own globals
    /// changing what its analysis would say) costs a deopt, never
    /// correctness.
    static RET_LANE_CACHE: RefCell<
        CodeMap<(Option<JitType>, bool, crate::sync::Weak<CodeObject>)>,
    > = RefCell::new(CodeMap::default());
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
    let mut obj_global = |name: &str| resolve(name).map(|o| grade_obj_global(&o));
    let (lane, ret_none) = match weavepy_jit::analyze_for_ret(fcode, &mut classify, &mut obj_global)
    {
        Ok(tf) => (tf.ret_lane, tf.ret_none),
        Err(e) => {
            if crate::hot_gates::env_flags::jit_trace() {
                eprintln!("jit ret-lane {:?}: {:?}", fcode.name, e);
            }
            (None, weavepy_jit::returns_none_syntactically(fcode))
        }
    };
    RET_LANE_CACHE.with(|c| {
        c.borrow_mut()
            .insert(key, (lane, ret_none, Rc::downgrade(fcode)))
    });
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
/// `return self.x * self.y`-shaped bodies typable. `args`, when given,
/// are a call site's live arguments: they stand in for the other
/// parameters the same way (`return self.x * o.x`), and seed their lanes.
/// Uncached — the shared ret cache has no receiver in its key, and a
/// method body is analyzed at most twice per `(slot, name)` site per
/// compile.
fn method_ret_info(
    interp: &super::Interpreter,
    f: &Rc<PyFunction>,
    fcode: &Rc<CodeObject>,
    recv: &Object,
    args: &[Option<Object>],
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
    let live = |slot: u32| -> Option<&Object> {
        match slot {
            0 => Some(recv),
            _ => args.get(slot as usize - 1)?.as_ref(),
        }
    };
    let mut attr = |slot: u32, path: &[String], name: &str, store: bool| -> Option<JitType> {
        // RFC 0071 WS3 — chains walk from the caller's live receiver.
        let mut cur = live(slot)?.clone();
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
    // seeding stays off (RFC 0069 WS3) unless a call site's arguments
    // stand in for them.
    let mut param = |slot: u32| match live(slot) {
        Some(v) if slot != 0 => scalar_lane(v).or(match v {
            Object::Instance(_) => Some(JitType::Obj),
            _ => None,
        }),
        _ => None,
    };
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
        local: &mut |_| None,
        stack_iter: &mut |_| None,
        pairs: &mut |_| None,
        entry_pc: None,
        carve_env: false,
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
// `repr(C, u8)`: compiled code reads an object pin's discriminant and
// value in place (see `obj_layout`).
#[repr(C, u8)]
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

/// Whether a pin holds an object that something else keeps alive too (or
/// one that owns nothing): releasing it would free nothing and finalize
/// nothing, so holding it costs only the table's slot.
fn pin_is_shared(p: &Pin) -> bool {
    match p {
        Pin::List(l, _) => Rc::strong_count(l) > 1,
        Pin::Obj(o) => match o {
            Object::None | Object::Int(_) | Object::Float(_) | Object::Bool(_) => true,
            Object::Instance(i) => Rc::strong_count(i) > 1,
            Object::List(l) => Rc::strong_count(l) > 1,
            _ => false,
        },
    }
}

/// At the soft pin limit, decide whether the activation may keep going:
/// the limit bounds how many objects a long activation keeps alive past
/// their last other reference (delaying their release and finalizers), so
/// pins whose objects are still referenced elsewhere (a loop over a list's
/// elements, results stored as they're made) don't count. When few of the
/// temporaries are last references, the limit rises by another soft
/// limit's worth (up to half the hard cap); otherwise the poll leaves.
/// Each pin is counted once, when the limit is first reached past it.
fn relax_pin_limit(ctx: &mut CallCtx) -> bool {
    let from = ctx.pins_counted.max(ctx.entry_pin_count);
    if let Some(fresh) = ctx.pins.get(from..) {
        ctx.last_ref_pins += fresh.iter().filter(|p| !pin_is_shared(p)).count();
    }
    ctx.pins_counted = ctx.pins.len();
    let temporaries = ctx.pins.len().saturating_sub(ctx.entry_pin_count);
    // Half the hard cap stays free: a poll interval that pins heavily
    // must still reach the next poll before the helpers refuse to pin.
    if ctx.last_ref_pins < RUNTIME_PIN_SOFT_LIMIT / 2
        && ctx.pins.len() + RUNTIME_PIN_SOFT_LIMIT <= RUNTIME_PIN_CAP / 2
    {
        ctx.pin_limit = temporaries + RUNTIME_PIN_SOFT_LIMIT;
        return true;
    }
    false
}

/// RFC 0070 WS1 — hard cap on an activation's pin table. Object-lane
/// attribute loads append pins for changing results (a list traversal
/// appends one per node), so an unbounded loop needs a bound: at the cap the
/// access deopts, the activation exits, and the re-entry (usually OSR
/// at the loop header) starts over with a fresh table.
const RUNTIME_PIN_CAP: usize = 1 << 16;

/// Request reconstruction at the next loop poll after this many new pins.
/// The hard cap still bounds allocations between polls. This counts handles,
/// not bytes, and a polling stride can overshoot the soft limit. A pin can
/// hold a whole structure (each `copy.deepcopy` result of a loop), so the
/// limit is kept low: at 4096 the deepcopy benchmark kept about 17 MB of
/// dead copies alive, and a pressure exit every few hundred pins costs
/// nothing measurable.
const RUNTIME_PIN_SOFT_LIMIT: usize = 1 << 9;

/// Retire every pin after the activation's result or complete interpreter
/// state has been reconstructed. A pin that held an object's last
/// reference frees it; a dying object only queues work for the next safe
/// point, so no Python runs here. Returns whether Python could have run
/// (it can't), for the callers that revalidate their compiled resolutions.
/// A suspended native activation must keep its entire table instead.
fn drain_activation_pins(_interp: &mut super::Interpreter, pins: &mut PinTable) -> bool {
    pins.clear();
    false
}

/// Retire an activation's pins after its complete interpreter frame has
/// been rebuilt (see [`drain_activation_pins`]).
fn defer_activation_pins(pins: &mut PinTable) {
    pins.clear();
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
        SlotTag::Boxed | SlotTag::ListPin | SlotTag::ObjPin | SlotTag::Default => Object::None,
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
        | JitType::ListListFloat
        | JitType::ListListInt
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
    match (l.borrow().first(), elem) {
        // A float-list element: its own first element is the proxy.
        (Some(Object::List(inner)), JitType::ListFloat) => {
            matches!(inner.borrow().first(), None | Some(Object::Float(_)))
        }
        (Some(Object::List(inner)), JitType::ListInt) => {
            matches!(inner.borrow().first(), None | Some(Object::Int(_)))
        }
        (first, elem) => matches!(
            (first, elem),
            (None, _)
                | (Some(Object::Int(_)), JitType::Int)
                | (Some(Object::Float(_)), JitType::Float)
                | (
                    Some(Object::Instance(_) | Object::Str(_) | Object::None | Object::Tuple(_)),
                    JitType::Obj
                )
        ),
    }
}

/// The compile-time shape probe (RFC 0061 WS5): report the element lane
/// of local `slot` when it currently holds a homogeneous non-empty
/// `int` or `float` list; `Some(Unknown)` for an *empty* list
/// (definitely a list, but with no lane evidence — RFC 0065 WS5 lets
/// `append`'s value lane pin it); `None` otherwise.
fn probe_pair_lanes(frame: &super::Frame, slot: u32) -> Option<(JitType, JitType)> {
    /// Enough pairs to see a mixed component without scanning a long
    /// container on every compile.
    const SAMPLE: usize = 16;
    let locals = frame.locals.borrow();
    let grade = |items: &[Object]| -> Option<(JitType, JitType)> {
        let lane = |v: &Object| match v {
            Object::Int(_) => JitType::Int,
            Object::Float(_) => JitType::Float,
            _ => JitType::Obj,
        };
        let mut out: Option<(JitType, JitType)> = None;
        for it in items.iter().take(SAMPLE) {
            let Object::Tuple(t) = it else {
                return None;
            };
            let [a, b] = &t[..] else {
                return None;
            };
            let (a, b) = (lane(a), lane(b));
            out = Some(match out {
                None => (a, b),
                Some((x, y)) => (
                    if x == a { x } else { JitType::Obj },
                    if y == b { y } else { JitType::Obj },
                ),
            });
        }
        out
    };
    match locals.get(slot as usize) {
        Some(Object::List(l)) => grade(&l.borrow()),
        Some(Object::Tuple(t)) => grade(t),
        _ => None,
    }
}

fn probe_list_lane(frame: &super::Frame, slot: u32) -> Option<JitType> {
    let locals = frame.locals.borrow();
    let Some(Object::List(l)) = locals.get(slot as usize) else {
        return None;
    };
    let items = l.borrow();
    if items.is_empty() {
        return Some(JitType::Unknown);
    }
    list_elem_lane(&items)
}

/// The uniform element lane of a non-empty list's `items` (see
/// [`probe_list_lane`]), `None` for a mixed or unsupported list.
fn list_elem_lane(items: &[Object]) -> Option<JitType> {
    let mut lane: Option<JitType> = None;
    for it in items {
        let t = match it {
            Object::Int(_) => JitType::Int,
            Object::Float(_) => JitType::Float,
            // RFC 0071 WS4 — instances (and `None`) ride the object
            // element lane; the access helpers re-validate per element.
            // Tuples too (a list of pairs a `for a, b in` loop unpacks).
            Object::Instance(_) | Object::None | Object::Tuple(_) => JitType::Obj,
            // A non-empty all-`float` or all-`int` list element: the
            // nested lanes.
            Object::List(inner) => {
                let inner = inner.borrow();
                if !inner.is_empty() && inner.iter().all(|x| matches!(x, Object::Float(_))) {
                    JitType::ListFloat
                } else if !inner.is_empty() && inner.iter().all(|x| matches!(x, Object::Int(_))) {
                    JitType::ListInt
                } else {
                    return None;
                }
            }
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

/// The lane of the live loop iterator at index `depth` of the requesting
/// activation's operand stack: `Int` for a unit-step `range` iterator,
/// the list lane of a plain list iterator's items, `Obj` for any other
/// iterator, `None` for anything else.
fn probe_stack_iter_lane(frame: &super::Frame, depth: u32) -> Option<JitType> {
    match frame.stack.get(depth as usize)? {
        Object::Iter(it) => Some(match &*it.borrow() {
            PyIterator::Range { step: 1, .. } => JitType::Int,
            PyIterator::List {
                items, owner: None, ..
            } => list_elem_lane(&items.borrow())
                .and_then(JitType::list_of)
                .unwrap_or(JitType::Obj),
            _ => JitType::Obj,
        }),
        Object::Generator(_) => Some(JitType::Obj),
        _ => None,
    }
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
        // A homogeneous list rides its list lane (re-read and
        // re-validated per access, like the object lane).
        Object::List(l) => Some(
            list_elem_lane(&l.borrow())
                .and_then(JitType::list_of)
                .unwrap_or(JitType::Obj),
        ),
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
    let (lane, _, storage) = attr_fingerprint_obj(obj, name, false)?;
    if lane != JitType::Obj {
        return None;
    }
    let Object::Instance(inst) = obj else {
        return None;
    };
    let v = match storage {
        AttrStorage::Slot(_) => inst.slot_get(name)?,
        // Either layout, without materializing a split one.
        AttrStorage::Indexed(key_idx) => inst.attr_index_map(key_idx as usize, |_, v| v.clone())?,
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
    // Reuse the code object's materialized name. Slot creation can then
    // clone one shared key instead of allocating a string per instance.
    // A synthetic site absent from co_names owns one key in its guard.
    let shared_name = || {
        frame
            .code
            .names
            .iter()
            .position(|name| name == &site.name)
            .and_then(|idx| super::code_name_obj(&frame.code, idx as u32))
            .and_then(|name| match name {
                Object::Str(name) => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| match crate::stdlib::sys::intern_name(&site.name) {
                Object::Str(name) => name,
                _ => unreachable!("intern_name returns a string"),
            })
    };
    // RFC 0073 WS1 — a constructor-resolved site has no live receiver
    // to fingerprint: burn the indexed guard from the class's
    // post-construction canonical shape instead. It is exactly the
    // fingerprint the site would learn from a live instance one call
    // later, and the runtime helpers re-validate `(ver,
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
            name: shared_name(),
            name_hash: crate::object::py_str_hash(&site.name),
            stable_descriptor: false,
            lane: site.lane,
            ver: cls.attr_version.get(),
            storage: AttrStorage::Indexed(*field_idx),
            split_idx: split_index(&cls, AttrStorage::Indexed(*field_idx), &site.name),
            slot_layout: 0,
            last_result_pin: Cell::new(usize::MAX),
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
        let IC::StoreAttrNewKey { ver } =
            crate::specialize::attempt_specialize_store_attr(&recv, &site.name)
        else {
            return None;
        };
        let Object::Instance(inst) = &recv else {
            return None;
        };
        return Some(AttrGuard {
            name: shared_name(),
            name_hash: crate::object::py_str_hash(&site.name),
            stable_descriptor: false,
            lane: site.lane,
            ver,
            storage: AttrStorage::Indexed(field_idx),
            split_idx: split_index(&inst.cls(), AttrStorage::Indexed(field_idx), &site.name),
            slot_layout: 0,
            last_result_pin: Cell::new(usize::MAX),
        });
    }
    let recv = walk_attr_path(frame, site.slot, &site.path)?;
    let (lane, ver, storage) = attr_fingerprint_obj(&recv, &site.name, site.store)?;
    // RFC 0071 WS2 — a new-key site has no current value, so its lane
    // came from the stored value; the storage modes must agree.
    if site.new_key {
        if storage != AttrStorage::NewKey {
            return None;
        }
    } else if storage == AttrStorage::NewKey || lane != site.lane {
        return None;
    }
    let stable_descriptor = site.store
        && scalar_field_update_shape(&frame.code)
        && match storage {
            AttrStorage::Indexed(_) => scalar_update_class_value_stable(&recv, &site.name, ver),
            AttrStorage::Slot(_) => scalar_update_slot_stable(&recv, &site.name, ver),
            AttrStorage::NewKey => false,
        };
    let (split_idx, slot_layout) = match (&recv, storage) {
        // A member slot the class lays out (its position is its class's,
        // whatever the instance's storage).
        (Object::Instance(inst), AttrStorage::Slot(_)) => inst
            .laid_out_position(&site.name)
            .filter(|&(i, _)| i & weavepy_jit::SLOT_FIELD == 0)
            .map_or((u32::MAX, 0), |(i, l)| (i | weavepy_jit::SLOT_FIELD, l)),
        (Object::Instance(inst), _) => (split_index(&inst.cls(), storage, &site.name), 0),
        _ => (u32::MAX, 0),
    };
    Some(AttrGuard {
        name: shared_name(),
        name_hash: crate::object::py_str_hash(&site.name),
        stable_descriptor,
        lane: site.lane,
        ver,
        storage,
        split_idx,
        slot_layout,
        last_result_pin: Cell::new(usize::MAX),
    })
}

/// [`AttrGuard::split_idx`] for an indexed site of `cls`: the index, when
/// the class's shared names hold `name` there (they never move).
///
/// A new-key site's is the name's position among its class's shared names
/// (compiled code appends the field there, in line, while a fresh instance
/// sets its fields in the class's order).
fn split_index(cls: &TypeObject, storage: AttrStorage, name: &str) -> u32 {
    let i = match storage {
        AttrStorage::Indexed(i) => i,
        AttrStorage::NewKey if cls.native_kind.get() == 0 => {
            let Some(keys) = cls.shared_keys.get() else {
                return u32::MAX;
            };
            match u32::try_from(keys.names_before(name, crate::object::py_str_hash(name))) {
                Ok(i) => i,
                Err(_) => return u32::MAX,
            }
        }
        _ => return u32::MAX,
    };
    match cls.shared_keys.get().and_then(|keys| keys.get(i as usize)) {
        Some(DictKey(Object::Str(s))) if &**s == name => i,
        _ => u32::MAX,
    }
}

/// Measure where pinned instances keep what compiled code's in-line field
/// reads and writes touch (see [`weavepy_jit::ObjLayout`]), check every
/// offset against `obj` (a live instance), and publish the layout. Once:
/// later calls return straight away.
#[inline]
pub(crate) fn ensure_obj_layout() {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    publish_obj_layout();
}

#[cold]
#[inline(never)]
fn publish_obj_layout() {
    // A private instance of `object` to measure against (never seen by
    // Python code; nothing tracks it).
    let cls = crate::builtin_types::builtin_types().object_.clone();
    let inst = Rc::new(crate::types::PyInstance::new(cls));
    let obj = Object::Instance(inst.clone());
    if let Some(layout) = obj_layout(&obj, &inst) {
        let _ = PUBLISHED_LAYOUT.set(layout);
        weavepy_jit::set_obj_layout(layout);
    }
}

static PUBLISHED_LAYOUT: std::sync::OnceLock<weavepy_jit::ObjLayout> = std::sync::OnceLock::new();

/// The object layout [`ensure_obj_layout`] published, for the VM's other
/// native code (the leaf plans').
pub(crate) fn published_obj_layout() -> Option<&'static weavepy_jit::ObjLayout> {
    PUBLISHED_LAYOUT.get()
}

/// The layout [`ensure_obj_layout`] publishes, `None` when any offset
/// doesn't read back what the safe accessors say about `obj`.
fn obj_layout(obj: &Object, inst: &Rc<crate::types::PyInstance>) -> Option<weavepy_jit::ObjLayout> {
    use crate::inst_dict::{InstDict, SplitValues};
    use crate::sync::GilCell;
    use crate::types::PyInstance;
    if crate::gil::free_threading_enabled()
        || std::env::var_os("WEAVEPY_JIT_NO_INLINE_ATTRS").is_some()
    {
        return None;
    }
    let word = |p: *const u8, off: usize| -> usize {
        // SAFETY: every read below is inside a live object the caller
        // holds, at an offset measured from its own type.
        unsafe { p.add(off).cast::<usize>().read_unaligned() }
    };
    let tag = |v: &Object| -> u8 {
        // SAFETY: `Object` is `repr(u8)`: its first byte is the tag.
        unsafe { *std::ptr::from_ref(v).cast::<u8>() }
    };
    let obj_p = std::ptr::from_ref(obj).cast::<u8>();
    // The payload word is the `Arc` allocation; the instance follows its
    // two counts.
    let arc = word(obj_p, 8);
    let data = Rc::as_ptr(inst) as usize;
    let arc_data = data.checked_sub(arc)?;
    let i32_of = |v: usize| i32::try_from(v).ok();
    let inst_class =
        arc_data + std::mem::offset_of!(PyInstance, class) + GilCell::<Rc<TypeObject>>::DATA_OFFSET;
    let dict = arc_data + std::mem::offset_of!(PyInstance, dict);
    let inst_dict_lazy = dict + InstDict::LAZY_OFFSET;
    let inst_split_borrow = dict + InstDict::SPLIT_OFFSET + GilCell::<SplitValues>::BORROW_OFFSET;
    let inst_split_block = dict
        + InstDict::SPLIT_OFFSET
        + GilCell::<SplitValues>::DATA_OFFSET
        + SplitValues::BLOCK_OFFSET;
    let arc_p = arc as *const u8;
    // The class pointer is its `Arc` allocation too.
    let cls = inst.cls();
    let cls_arc = word(arc_p, inst_class);
    if cls_arc + arc_data != Rc::as_ptr(&cls) as usize {
        return None;
    }
    // A class value's payload is the same allocation pointer.
    let cls_obj = Object::Type(cls.clone());
    if word(std::ptr::from_ref(&cls_obj).cast::<u8>(), 8) != cls_arc {
        return None;
    }
    // The native body's word: a `Cell<usize>` alone in its wrapper,
    // checked on a sample.
    let inst_c_body = arc_data + std::mem::offset_of!(PyInstance, c_body);
    {
        let sample = crate::types::CBody::default();
        sample.set(0x5eed_c0de);
        if std::mem::size_of::<crate::types::CBody>() != 8
            || word(std::ptr::from_ref(&sample).cast::<u8>(), 0) != 0x5eed_c0de
            || word(arc as *const u8, inst_c_body) != inst.c_body.get()
        {
            return None;
        }
    }
    let type_attr_version = arc_data + std::mem::offset_of!(TypeObject, attr_version);
    let type_shared_keys = arc_data
        + std::mem::offset_of!(TypeObject, shared_keys)
        + crate::sync::LazyArc::<crate::inst_dict::SharedKeys>::POINTER_OFFSET;
    let cls_p = cls_arc as *const u8;
    // SAFETY: as `word`, a `u64` field.
    let ver = unsafe { cls_p.add(type_attr_version).cast::<u64>().read_unaligned() };
    let keys = cls
        .shared_keys
        .get()
        .map_or(0, |k| std::ptr::from_ref(k) as usize);
    let published = inst.dict.published().is_some();
    if ver != cls.attr_version.get()
        || word(cls_p, type_shared_keys) != keys
        || (word(arc_p, inst_dict_lazy) != 0) != published
    {
        return None;
    }
    // SAFETY: as `word`, the `i32` borrow counter.
    let borrow = unsafe { arc_p.add(inst_split_borrow).cast::<i32>().read_unaligned() };
    if borrow != 0 {
        return None;
    }
    // A pin: `repr(C, u8)`.
    let sample = Pin::Obj(obj.clone());
    let sample_p = std::ptr::from_ref(&sample).cast::<u8>();
    let pin_obj = match &sample {
        Pin::Obj(o) => std::ptr::from_ref(o) as usize - sample_p as usize,
        Pin::List(..) => return None,
    };
    // SAFETY: the discriminant byte of a `repr(C, u8)` enum.
    let pin_obj_tag = unsafe { *sample_p };
    if word(sample_p, pin_obj + 8) != arc {
        return None;
    }
    drop(sample);
    // A vector's buffer pointer, length and capacity, measured on one
    // whose three words all differ.
    let mut v: Vec<Pin> = Vec::with_capacity(4);
    v.push(Pin::Obj(Object::None));
    let vp = std::ptr::from_ref(&v).cast::<u8>();
    let find = |want: usize| (0..3).map(|k| k * 8).find(|&off| word(vp, off) == want);
    let (vec_ptr, vec_len, vec_cap) = (find(v.as_ptr() as usize)?, find(1)?, find(v.capacity())?);
    drop(v);
    // A list pin: the list's `Arc` allocation and its element lane.
    let list = Rc::new(GilRefCell::new(vec![Object::Int(1)]));
    let list_arc = {
        // SAFETY: `Rc` is one pointer to its allocation.
        unsafe { std::mem::transmute_copy::<Rc<GilRefCell<Vec<Object>>>, usize>(&list) }
    };
    let sample = Pin::List(list.clone(), JitType::Float);
    let sample_p = std::ptr::from_ref(&sample).cast::<u8>();
    // SAFETY: the discriminant byte of a `repr(C, u8)` enum.
    let pin_list_tag = unsafe { *sample_p };
    let (pin_list, pin_list_elem) = match &sample {
        Pin::List(l, e) => (
            std::ptr::from_ref(l) as usize - sample_p as usize,
            std::ptr::from_ref(e) as usize - sample_p as usize,
        ),
        Pin::Obj(_) => return None,
    };
    // SAFETY: the element lane's byte.
    let lane_byte = unsafe { *sample_p.add(pin_list_elem) };
    if word(sample_p, pin_list) != list_arc
        || lane_byte != JitType::Float as u8
        || pin_list_tag == pin_obj_tag
        || std::mem::size_of::<JitType>() != 1
    {
        return None;
    }
    drop(sample);
    let list_data = list_arc_data(&list, list_arc)?;
    let list_borrow = list_data + GilCell::<Vec<Object>>::BORROW_OFFSET;
    let list_vec = list_data + GilCell::<Vec<Object>>::DATA_OFFSET;
    let list_p = list_arc as *const u8;
    // SAFETY: as `word`, the `i32` borrow counter.
    let list_free = unsafe { list_p.add(list_borrow).cast::<i32>().read_unaligned() } == 0;
    let items = list.borrow();
    if !list_free
        || word(list_p, list_vec + vec_ptr) != items.as_ptr() as usize
        || word(list_p, list_vec + vec_len) != items.len()
        || word(list_p, list_vec + vec_cap) != items.capacity()
    {
        return None;
    }
    drop(items);
    let guards: StdRc<Vec<AttrGuard>> = StdRc::new(Vec::with_capacity(2));
    let rc_box = {
        // SAFETY: `std::rc::Rc` is one pointer to its allocation.
        unsafe { std::mem::transmute_copy::<StdRc<Vec<AttrGuard>>, usize>(&guards) }
    };
    let guards_vec = std::ptr::from_ref::<Vec<AttrGuard>>(&guards) as usize - rc_box;
    let guards_buf = guards_vec + vec_ptr;
    if word(rc_box as *const u8, guards_buf) != guards.as_ptr() as usize {
        return None;
    }
    // The method entries hang off the same shape of shared vector.
    let methods: StdRc<MethodTable> = StdRc::new(Vec::with_capacity(3));
    let methods_box = {
        // SAFETY: as for the guards above.
        unsafe { std::mem::transmute_copy::<StdRc<MethodTable>, usize>(&methods) }
    };
    let methods_vec = std::ptr::from_ref::<MethodTable>(&methods) as usize - methods_box;
    if word(methods_box as *const u8, methods_vec + vec_ptr) != methods.as_ptr() as usize
        || word(methods_box as *const u8, methods_vec + vec_len) != 0
    {
        return None;
    }
    let update = std::mem::offset_of!(MethodEntry, update);
    let ctx_pins = std::mem::offset_of!(CallCtx, pins);
    // The member slots' cell: its borrow counter, and where the laid-out
    // form keeps its layout and values (see `laid_out_slots_layout`).
    let inst_slots = arc_data + std::mem::offset_of!(PyInstance, slots);
    let slots_borrow = inst_slots + GilCell::<crate::types::SlotStorage>::BORROW_OFFSET;
    let slots_at = inst_slots + GilCell::<crate::types::SlotStorage>::DATA_OFFSET;
    let slots = laid_out_slots_layout().and_then(|(tag, fixed, layout, values)| {
        Some((
            i32_of(slots_at + tag)?,
            fixed,
            i32_of(slots_at + layout)?,
            i32_of(slots_at + values)?,
        ))
    });
    Some(weavepy_jit::ObjLayout {
        slots_ok: slots.is_some(),
        tag_unbound: tag(&Object::Unbound),
        inst_slots_borrow: i32_of(slots_borrow)?,
        inst_slots_tag: slots.map_or(0, |s| s.0),
        slots_laid_out: slots.map_or(0, |s| s.1),
        inst_slots_layout: slots.map_or(0, |s| s.2),
        inst_slots_values: slots.map_or(0, |s| s.3),
        ctx_pins_ptr: i32_of(ctx_pins + vec_ptr)?,
        ctx_pins_len: i32_of(ctx_pins + vec_len)?,
        pin_size: i32_of(std::mem::size_of::<Pin>())?,
        pin_tag: 0,
        pin_obj_tag,
        pin_obj: i32_of(pin_obj)?,
        ctx_guards: i32_of(std::mem::offset_of!(CallCtx, attr_guards))?,
        guards_buf: i32_of(guards_buf)?,
        guard_size: i32_of(std::mem::size_of::<AttrGuard>())?,
        guard_ver: i32_of(std::mem::offset_of!(AttrGuard, ver))?,
        guard_split_idx: i32_of(std::mem::offset_of!(AttrGuard, split_idx))?,
        guard_slot_layout: i32_of(std::mem::offset_of!(AttrGuard, slot_layout))?,
        tag_instance: tag(obj),
        tag_int: tag(&Object::Int(0)),
        tag_float: tag(&Object::Float(0.0)),
        tag_bool: tag(&Object::Bool(false)),
        tag_none: tag(&Object::None),
        tag_type: tag(&cls_obj),
        inst_class: i32_of(inst_class)?,
        inst_dict_lazy: i32_of(inst_dict_lazy)?,
        inst_split_borrow: i32_of(inst_split_borrow)?,
        inst_split_block: i32_of(inst_split_block)?,
        type_attr_version: i32_of(type_attr_version)?,
        type_shared_keys: i32_of(type_shared_keys)?,
        split_keys: i32_of(SplitValues::KEYS_OFFSET)?,
        split_len: i32_of(SplitValues::LEN_OFFSET)?,
        split_cap: i32_of(SplitValues::CAP_OFFSET)?,
        inst_c_body: i32_of(inst_c_body)?,
        split_values: i32_of(SplitValues::VALUES_OFFSET)?,
        cells_unguarded: crate::sync::cells_unguarded_flag() as usize,
        pin_list_tag,
        pin_list: i32_of(pin_list)?,
        pin_list_elem: i32_of(pin_list_elem)?,
        list_borrow: i32_of(list_borrow)?,
        list_ptr: i32_of(list_vec + vec_ptr)?,
        list_len: i32_of(list_vec + vec_len)?,
        list_cap: i32_of(list_vec + vec_cap)?,
        ctx_methods: i32_of(std::mem::offset_of!(CallCtx, methods))?,
        methods_buf: i32_of(methods_vec + vec_ptr)?,
        methods_len: i32_of(methods_vec + vec_len)?,
        method_size: i32_of(std::mem::size_of::<MethodEntry>())?,
        method_ver: i32_of(std::mem::offset_of!(MethodEntry, ver))?,
        method_upd_idx: i32_of(update + std::mem::offset_of!(InlineUpdate, idx))?,
        method_upd_shadow: i32_of(update + std::mem::offset_of!(InlineUpdate, shadow))?,
        method_upd_from_arg: i32_of(update + std::mem::offset_of!(InlineUpdate, from_arg))?,
        method_upd_inc: i32_of(update + std::mem::offset_of!(InlineUpdate, inc))?,
        method_upd_code_at: i32_of(update + std::mem::offset_of!(InlineUpdate, code_at))?,
        method_upd_code: i32_of(update + std::mem::offset_of!(InlineUpdate, code))?,
        observers: crate::trace::observer_count_flag() as usize,
        dict_watchers: crate::capi_watchers::dicts_active_flag() as usize,
        exotic_keys: crate::object::exotic_str_keys_flag() as usize,
        hot_gates: crate::hot_gates::hot_ptr() as usize,
        recursion_limit: crate::recursion::recursion_limit_ptr() as usize,
        ctx_depth_cell: i32_of(std::mem::offset_of!(CallCtx, depth_cell))?,
    })
}

/// Where slot storage keeps a laid-out instance's member slots (see
/// [`crate::types::TypeObject::fresh_slots`]), measured on samples of
/// every form it takes: the offset of the byte that tells the laid-out
/// form apart and its value there, then the offsets of the layout's
/// pointer and the values' pointer. `None` when the forms don't measure
/// as expected.
///
/// The storage is an enum whose laid-out form fills three of its four
/// words, so the fourth holds what tells the forms apart: in the
/// single-slot form, an object's tag byte (the slot's name's or value's),
/// and so in the laid-out form a value no object's tag takes. Each sample
/// checks that reading.
#[cfg(target_pointer_width = "64")]
fn laid_out_slots_layout() -> Option<(usize, u8, usize, usize)> {
    use crate::types::SlotStorage;
    const WORDS: usize = 4;
    if std::mem::size_of::<SlotStorage>() != WORDS * 8
        || std::mem::size_of::<crate::shared_value::SharedSlice<DictKey>>() != 8
    {
        return None;
    }
    let words = |s: &SlotStorage| -> [usize; WORDS] {
        let p = std::ptr::from_ref(s).cast::<u8>();
        // SAFETY: four words inside a live value.
        std::array::from_fn(|k| unsafe { p.add(k * 8).cast::<usize>().read_unaligned() })
    };
    let byte = |s: &SlotStorage, at: usize| -> u8 {
        // SAFETY: a byte inside a live value.
        unsafe { *std::ptr::from_ref(s).cast::<u8>().add(at) }
    };
    let tag = |v: &Object| -> u8 {
        // SAFETY: `Object` is `repr(u8)`: its first byte is the tag.
        unsafe { *std::ptr::from_ref(v).cast::<u8>() }
    };
    let name = |s: &str| DictKey(crate::stdlib::sys::intern_name(s));
    let layout = crate::shared_value::SharedSlice::from(vec![name("a"), name("b"), name("c")]);
    let layout_addr = crate::shared_value::SharedSlice::word(&layout);
    let fixed = SlotStorage::from_layout(
        layout.clone(),
        vec![Object::Int(1), Object::Unbound, Object::Float(2.0)],
    );
    let values = fixed.values_for_layout(&layout)?.as_ptr() as usize;
    let w = words(&fixed);
    let find = |want: usize| -> Option<usize> {
        let mut hits = (0..WORDS).filter(|&k| w[k] == want);
        let k = hits.next()?;
        hits.next().is_none().then_some(k * 8)
    };
    let (layout_at, values_at, len_at) = (find(layout_addr)?, find(values)?, find(3)?);
    let free = (0..WORDS)
        .map(|k| k * 8)
        .find(|o| ![layout_at, values_at, len_at].contains(o))?;
    let laid_out = byte(&fixed, free);
    // The single-slot form: the discriminating byte is the tag of its name
    // or of its value, whatever the value.
    let mut name_tag = None;
    let mut value_tag = None;
    for value in [
        Object::Int(5),
        Object::Float(0.5),
        Object::None,
        Object::Bool(true),
        Object::Str(crate::shared_value::SharedStr::from("v")),
    ] {
        let mut single = SlotStorage::default();
        single.insert("a", value.clone());
        let b = byte(&single, free);
        let key_tag = tag(&Object::Str(crate::shared_value::SharedStr::from("a")));
        if b == key_tag && value_tag.is_none() {
            name_tag = Some(b);
        } else if b == tag(&value) && name_tag.is_none() {
            value_tag = Some(b);
        } else {
            return None;
        }
        if b == laid_out {
            return None;
        }
    }
    // Every other form reads differently there.
    let others = [
        SlotStorage::default(),
        SlotStorage::from_entries(vec![
            (name("a"), Object::Int(1)),
            (name("b"), Object::Int(2)),
        ]),
        SlotStorage::from_entries(
            (0..20)
                .map(|i| (name(&format!("s{i}")), Object::Int(i)))
                .collect(),
        ),
    ];
    if others
        .iter()
        .any(|s| byte(s, free) == laid_out || s.values_for_layout(&layout).is_some())
    {
        return None;
    }
    // A second laid-out sample reads the same.
    let unset = SlotStorage::from_layout(layout.clone(), vec![Object::Unbound; 3]);
    let w2 = words(&unset);
    if byte(&unset, free) != laid_out
        || w2[layout_at / 8] != layout_addr
        || w2[values_at / 8] != unset.values_for_layout(&layout)?.as_ptr() as usize
    {
        return None;
    }
    Some((free, laid_out, layout_at, values_at))
}

#[cfg(not(target_pointer_width = "64"))]
fn laid_out_slots_layout() -> Option<(usize, u8, usize, usize)> {
    None
}

/// Where a list's cell sits in its `Arc` allocation at `arc`.
fn list_arc_data(list: &Rc<GilRefCell<Vec<Object>>>, arc: usize) -> Option<usize> {
    (Rc::as_ptr(list) as usize).checked_sub(arc)
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
    // Read without materializing a split layout: a probe must not change
    // how the instance stores its attributes.
    if inst.attr_get_str(name).is_some() {
        return None;
    }
    let f = match cls.lookup(name) {
        // The analyzer probes `__getitem__` for native subscripts only
        // (`TOp::ObjGetItem`); a Python-level one must not take a token
        // that no site would use.
        Some(Object::Function(_)) if name == "__getitem__" => return None,
        Some(Object::Function(f)) => f,
        // A native accelerator's leaf method (`deque.append`): any
        // positional arity (the body validates it and raises exactly),
        // and an object-lane result (`None` rides the nullable `-1`, so a
        // procedure's result costs no pin).
        Some(Object::Builtin(b)) if b.binds_instance => {
            let op = crate::leaf_builtins::jit_method_op(&b)?;
            let fast = match interp.leaf_call_kind(&b)? {
                super::LeafKind::Opaque => None,
                super::LeafKind::Fast(fast) => Some(fast),
                _ => return None,
            };
            return Some(MethodEntry {
                callee: MethodCallee::Native {
                    builtin: b,
                    fast,
                    op,
                },
                no_dict: cls.forbids_dict,
                name: name.to_owned(),
                name_hash: crate::object::py_str_hash(name),
                arg_count: NATIVE_METHOD_MAX_ARGS,
                min_args: 1,
                ret: MethodRet::Scalar(JitType::Obj),
                ver: cls.attr_version.get(),
                update: InlineUpdate::unarmed(),
                fields: Vec::new(),
                sites: Vec::new(),
                guarded_in_line: Cell::new(false),
            });
        }
        _ => return None,
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
    // The method's call sites in this frame, with the live values each
    // passes: a body reading fields of an object argument (`o.x`) types
    // from them when it types from nothing else.
    let calls = method_call_sites(frame, slot, path, name, fcode.arg_count);
    let (mut lane, mut ret_none) = method_ret_info(interp, &f, &fcode, &recv, &[]);
    if lane.is_none() && !ret_none {
        if let Some((_, args)) = calls.first() {
            (lane, ret_none) = method_ret_info(interp, &f, &fcode, &recv, args);
        }
    }
    let ret = if ret_none {
        MethodRet::None
    } else {
        // RFC 0071 WS1 — object-lane returns cross the boundary as a
        // fresh caller pin, so they are admissible alongside scalars.
        MethodRet::Scalar(lane.filter(|t| marshalable_lane_ty(*t))?)
    };
    let ver = cls.attr_version.get();
    let arg_count = fcode.arg_count;
    let fields = inline_fields_of(&recv, &fcode, ver);
    let sites = calls
        .iter()
        .map(|(pc, args)| SiteArgs {
            pc: *pc,
            args: args
                .iter()
                .map(|a| {
                    let Some(Object::Instance(inst)) = a else {
                        return None;
                    };
                    let aver = inst.cls().attr_version.get();
                    let fields = inline_fields_of(a.as_ref()?, &fcode, aver);
                    (!fields.is_empty()).then_some((aver, fields))
                })
                .collect(),
        })
        .collect();
    let entry = MethodEntry {
        callee: MethodCallee::Py {
            func: f,
            code: fcode,
        },
        no_dict: cls.forbids_dict,
        name: name.to_owned(),
        name_hash: crate::object::py_str_hash(name),
        arg_count,
        min_args,
        ret,
        ver,
        update: InlineUpdate::unarmed(),
        fields,
        sites,
        guarded_in_line: Cell::new(false),
    };
    arm_method_guard(&entry, &cls);
    Some(entry)
}

/// The calls in `frame`'s code of method `name` on the local in `slot`
/// walked through `path` that pass `arg_count - 1` arguments, each a local
/// read through attribute loads: per call, its instruction and the live
/// value of each argument (`None` for an unbound local or a walk that
/// fails). At most a handful.
fn method_call_sites(
    frame: &super::Frame,
    slot: u32,
    path: &[String],
    name: &str,
    arg_count: u32,
) -> Vec<(u32, Vec<Option<Object>>)> {
    use weavepy_compiler::OpCode;
    const MAX_SITES: usize = 8;
    let code = &frame.code;
    let ins = &code.instructions;
    let named = |i: usize, op: OpCode, want: &str| {
        ins.get(i).is_some_and(|x| {
            x.op == op && code.names.get(x.arg as usize).is_some_and(|n| n == want)
        })
    };
    let argc = arg_count.saturating_sub(1) as usize;
    let mut out = Vec::new();
    for p in 0..ins.len() {
        if out.len() >= MAX_SITES {
            break;
        }
        if !named(p, OpCode::LoadMethodAttr, name) {
            continue;
        }
        // The receiver: the local, then the path's loads.
        let Some(start) = p.checked_sub(path.len() + 1) else {
            continue;
        };
        if ins[start].op != OpCode::LoadFast
            || ins[start].arg != slot
            || !path
                .iter()
                .enumerate()
                .all(|(k, seg)| named(start + 1 + k, OpCode::LoadAttr, seg))
        {
            continue;
        }
        // The arguments: each a local and its attribute loads, then the
        // call.
        let mut q = p + 1;
        let mut args = Vec::with_capacity(argc);
        while args.len() < argc {
            let Some(load) = ins.get(q).filter(|x| x.op == OpCode::LoadFast) else {
                break;
            };
            q += 1;
            let mut names = Vec::new();
            while let Some(x) = ins.get(q).filter(|x| x.op == OpCode::LoadAttr) {
                let Some(n) = code.names.get(x.arg as usize) else {
                    break;
                };
                names.push(n.clone());
                q += 1;
            }
            args.push(
                walk_attr_path(frame, load.arg, &names).filter(|v| !matches!(v, Object::Unbound)),
            );
        }
        let called = ins
            .get(q)
            .is_some_and(|x| x.op == OpCode::Call && x.arg as usize == argc);
        if args.len() == argc && called {
            out.push((q as u32, args));
        }
    }
    out
}

/// The scalar fields `code` (a method) reads off a receiver whose
/// attribute loads on `recv` (an instance of the class at version `ver`)
/// are plain split-value reads or laid-out member slot reads, as the
/// attribute sites' guards classify them: no class attribute or
/// descriptor intervenes, and the class's names (or slot layout) hold the
/// field at its index. At most a handful, in first-read order.
fn inline_fields_of(recv: &Object, code: &CodeObject, ver: u64) -> Vec<InlineField> {
    const MAX_FIELDS: usize = 8;
    let Object::Instance(inst) = recv else {
        return Vec::new();
    };
    let mut out: Vec<InlineField> = Vec::new();
    for ins in code.instructions.iter() {
        if ins.op != weavepy_compiler::OpCode::LoadAttr {
            continue;
        }
        let Some(name) = code.names.get(ins.arg as usize) else {
            continue;
        };
        if out.len() >= MAX_FIELDS || out.iter().any(|f| f.name == *name) {
            continue;
        }
        let Some((lane, fver, storage)) = attr_fingerprint_obj(recv, name, false) else {
            continue;
        };
        if fver != ver || !matches!(lane, JitType::Int | JitType::Float | JitType::Bool) {
            continue;
        }
        let at = match storage {
            AttrStorage::Indexed(i) if split_index(&inst.cls(), storage, name) == i => {
                weavepy_jit::FieldAt::Split(i)
            }
            AttrStorage::Slot(_) => match inst.laid_out_position(name) {
                Some((idx, layout)) => weavepy_jit::FieldAt::Slot { idx, layout },
                None => continue,
            },
            _ => continue,
        };
        out.push(InlineField {
            name: name.clone(),
            lane,
            at,
        });
    }
    out
}

/// The body of `entry`'s method as the call site at `pc` passing arguments
/// of `lanes` runs it in line (see [`weavepy_jit::InlineMethod`]): the
/// method analyzed with those parameter lanes and the fields of `self` and
/// of its object arguments (see [`MethodEntry::fields`] and
/// [`MethodEntry::sites`]) and nothing else of the world (no globals,
/// calls or other attributes), when the analysis takes the in-line shape
/// and reads only those fields.
fn inline_method_body(
    entry: &MethodEntry,
    lanes: &[JitType],
    pc: u32,
) -> Option<weavepy_jit::InlineMethod> {
    let MethodCallee::Py { code, .. } = &entry.callee else {
        return None;
    };
    if code.arg_count as usize != lanes.len() + 1 {
        return None;
    }
    let site = entry.sites.iter().find(|s| s.pc == pc);
    // The fields of the receiver in parameter `slot`, and the version its
    // class must have (`0` for `self`, which the site's guard checks).
    let receiver = |slot: u32| -> Option<(&[InlineField], u64)> {
        if slot == 0 {
            return Some((&entry.fields, 0));
        }
        if lanes.get(slot as usize - 1) != Some(&JitType::Obj) {
            return None;
        }
        let (ver, fields) = site?.args.get(slot as usize - 1)?.as_ref()?;
        Some((fields, *ver))
    };
    let mut classify = |_: &str| ResolvedGlobal::Opaque;
    let mut attr = |slot: u32, path: &[String], name: &str, store: bool| -> Option<JitType> {
        if !path.is_empty() || store {
            return None;
        }
        let (fields, _) = receiver(slot)?;
        fields.iter().find(|f| f.name == name).map(|f| f.lane)
    };
    let mut param = |slot: u32| lanes.get((slot as usize).checked_sub(1)?).copied();
    let mut path_arena = weavepy_jit::PathArena::default();
    let mut probes = Probes {
        list: &mut |_| None,
        dict: &mut |_| None,
        attr: &mut attr,
        method: &mut |_, _, _| None,
        math: &mut |_, _| false,
        ctor_field: &mut |_, _| None,
        param: &mut param,
        kw_slot: &mut |_, _| None,
        obj_global: &mut |_| None,
        cell: &mut |_| None,
        obj: &mut |_| false,
        local: &mut |_| None,
        stack_iter: &mut |_| None,
        pairs: &mut |_| None,
        entry_pc: None,
        carve_env: false,
        paths: &mut path_arena,
    };
    let tf = weavepy_jit::analyze_frame(code, &mut classify, &mut probes).ok()?;
    let body = weavepy_jit::InlineMethod::of(&tf, code.arg_count)?;
    let at = body
        .field_names()
        .iter()
        .zip(body.field_lanes())
        .zip(body.field_receivers())
        .map(|((name, &lane), &slot)| {
            receiver(slot)
                .and_then(|(fields, _)| fields.iter().find(|f| f.name == *name && f.lane == lane))
                .map_or(weavepy_jit::FieldAt::Unknown, |f| f.at)
        })
        .collect();
    let vers = (0..code.arg_count)
        .map(|slot| receiver(slot).map_or(0, |(_, ver)| ver))
        .collect();
    body.with_fields(at, vers)
}

/// Arm what compiled code checks of `entry`'s guard in line before a
/// direct method call (see [`InlineUpdate`]): the function's code pair
/// and the shadow bound, from `cls` (the class the entry resolved against,
/// at its version). A bound computed now holds for good: the class's names
/// only grow, and an instance holding no more values than precede the
/// method's name there has no attribute of that name. Without names yet
/// the bound stays as it is, and an instance with values takes the helper,
/// which arms it once they exist.
fn arm_method_guard(entry: &MethodEntry, cls: &TypeObject) {
    let MethodCallee::Py { func, code } = &entry.callee else {
        return;
    };
    let u = &entry.update;
    if u.code_at.get() == 0 {
        u.code_at.set(func.code.as_ptr() as usize);
        // SAFETY: `Rc` is one pointer, compared and never dereferenced.
        u.code
            .set(unsafe { std::mem::transmute_copy::<Rc<CodeObject>, usize>(code) });
    }
    if let Some(keys) = cls.shared_keys.get() {
        if (u.shadow.get() as usize) < keys.len() {
            let shadow = keys.names_before(&entry.name, entry.name_hash);
            u.shadow.set(u32::try_from(shadow).unwrap_or(0));
        }
    }
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
) -> Option<(JitType, u64, AttrStorage)> {
    use weavepy_compiler::InlineCache as IC;
    let Object::Instance(inst) = obj else {
        return None;
    };
    // RFC 0070 WS3 / RFC 0071 WS2 — the tier-1 predicate classifies
    // the storage: an indexed instance-dict hit, a `__slots__` member
    // (read and written through the slot side table by name), or the
    // new-key insert shape (stores only).
    let (ver, storage) = if store {
        match crate::specialize::attempt_specialize_store_attr(obj, name) {
            IC::StoreAttrInstance { key_idx, ver } => (ver, AttrStorage::Indexed(key_idx)),
            IC::StoreAttrSlot { key_idx, ver } => (ver, AttrStorage::Slot(key_idx)),
            IC::StoreAttrNewKey { ver } => (ver, AttrStorage::NewKey),
            _ => return None,
        }
    } else {
        match crate::specialize::attempt_specialize_load_attr(obj, name) {
            IC::LoadAttrInstance { key_idx, ver } => (ver, AttrStorage::Indexed(key_idx)),
            IC::LoadAttrSlot { key_idx, ver } => (ver, AttrStorage::Slot(key_idx)),
            _ => return None,
        }
    };
    // The current value pins the lane. An unset slot has no lane
    // evidence (and a load would raise), so it stays uncompiled.
    // RFC 0071 WS2 — a new-key store has no current value by
    // definition: the `Unknown` lane tells the analyzer to type the
    // site from the stored value instead.
    let current = match storage {
        AttrStorage::NewKey => return Some((JitType::Unknown, ver, storage)),
        AttrStorage::Slot(_) => inst.slot_get(name)?,
        AttrStorage::Indexed(key_idx) => inst.attr_index_map(key_idx as usize, |_, v| v.clone())?,
    };
    let v = &current;
    // RFC 0070 WS1 — instance- or `None`-valued attributes take the
    // nullable object lane (loads pin the value at runtime; stores
    // resolve the staged pin); RFC 0071 WS6 — exact `str`/`bytes`
    // values take the read lanes; anything else must be a scalar.
    let lane = match v {
        // A `list`/`dict` attribute pins exactly like an instance one —
        // the value is pinned at the load and the guard re-validates the
        // lane per access — and an object-holding attribute is very
        // common (`self.constraints`, `self.cache`). Without this the
        // site stayed fully dynamic.
        Object::Instance(_) | Object::None | Object::List(_) | Object::Dict(_) => JitType::Obj,
        Object::Str(_) => JitType::Str,
        Object::Bytes(_) => JitType::Bytes,
        _ => scalar_lane(v)?,
    };
    Some((lane, ver, storage))
}

/// Drop tier-cache entries whose code object is dead or kept alive only
/// by the entry's own compiled artifacts, then discard return-lane
/// entries whose weak code is dead. A compiled entry pins its code, which
/// would otherwise make every compiled code object immortal (observable
/// through `weakref` on `__code__`). Called from `gc.collect()`. Eviction
/// is always safe: a live code object re-enters the cache through the
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
                .filter(|(_, e)| e.code_unowned())
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
                .filter(|(_, (_, _, code))| code.strong_count() == 0)
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
/// The leaf burst's back-edge hook: whether the next back edge's
/// consultation of the tier-2 state is due. The burst then hands the
/// edge to the full `JUMP_BACKWARD` handler, whose [`note_backedge`]
/// ticks, consults, and attempts the OSR entry.
#[inline]
pub(crate) fn backedge_due(code: &Rc<CodeObject>) -> bool {
    !code.jit_hint.is_not_jitable() && !jit_off_for_process() && code.jit_hint.backedge_pending()
}

pub(crate) fn note_backedge(code: &Rc<CodeObject>) -> bool {
    // RFC 0067 — same fast-out as `try_enter`: rejected code pays one
    // relaxed load per back edge, not a thread-local + map lookup. The
    // state itself is consulted only every `BACKEDGE_STRIDE` back edges.
    if code.jit_hint.is_not_jitable() || jit_off_for_process() || !code.jit_hint.backedge_tick() {
        return false;
    }
    JIT.with(|cell| cell.borrow_mut().note_backedge(code))
}

/// Compile `frame`'s code if it is hot, without entering it: the lean
/// interpreter path runs a loop-free body itself (a framed native entry
/// would cost more than the body), but a native caller can still take
/// the direct call lanes into the compiled form, and those resolve only
/// once the code has been compiled. Called on the lean path's threshold.
pub(crate) fn warm_compile(interp: &mut super::Interpreter, frame: &mut super::Frame) {
    let phase = compilation_phase();
    if frame.code.jit_hint.is_not_jitable() || jit_off_for_process() {
        return;
    }
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.enabled {
            return;
        }
        let threshold = st.threshold;
        // Embedders that never report start-up finished still compile,
        // after sustained work. Both budgets here must match
        // [`compile_allowed`]'s: a compile it refused would leave the lean
        // checkpoint passed and the code interpreted for good.
        let warm = if phase == CompilationPhase::Normal
            && STARTUP_DONE.load(std::sync::atomic::Ordering::Relaxed)
        {
            threshold
        } else {
            threshold.saturating_mul(IMPORT_THRESHOLD_FACTOR)
        };
        let st = &mut *st;
        let entry = cache_entry(&mut st.cache, &mut st.sweep_at, &frame.code);
        if matches!(entry.tier, Tier::Cold) {
            if phase != CompilationPhase::Normal {
                // A loop-free body gains nothing from native code until
                // compiled callers exist to take its direct lanes, while
                // compiling one during start-up or an import costs time and
                // memory the program may never recover (`ABCMeta.register`
                // while `_collections_abc` loads). The lean path has no
                // ordinary frame-entry counter: account for the interval
                // just completed and count another, so only sustained work
                // compiles (the checkpoint stays reachable afterwards, even
                // when pure-leaf calls skip frames).
                let interval = lean_warm_at();
                entry.counter = entry.counter.saturating_add(interval);
                if entry.counter < interval.saturating_mul(IMPORT_THRESHOLD_FACTOR) {
                    let hint = &frame.code.jit_hint;
                    entry.calls = entry.calls.wrapping_add(hint.lean_entries());
                    hint.defer_lean_compile();
                    return;
                }
            }
            // Preserve the earlier lean warm point relative to frame/loop
            // hotness.
            entry.counter = entry.counter.max(warm);
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
        let mut probe_iter = |depth: u32| probe_stack_iter_lane(frame_ref, depth);
        let mut probe_pairs = |slot: u32| probe_pair_lanes(frame_ref, slot);
        let _ = st.get_compiled(
            &frame.code,
            0,
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
                stack_iter: &mut probe_iter,
                pairs: &mut probe_pairs,
            },
        );
    });
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
///
/// A non-empty list with a uniform element lane rides its pinned-list
/// lane, as a list local does (see [`probe_list_lane`]): the identity
/// guard keeps the list object, and every element access re-validates
/// the element lane, so later changes to the list's contents deopt.
fn grade_obj_global(obj: &Object) -> JitType {
    match obj {
        Object::Str(_) => JitType::Str,
        Object::List(l) => {
            let items = l.borrow();
            if items.is_empty() {
                return JitType::Obj;
            }
            list_elem_lane(&items)
                .and_then(JitType::list_of)
                .unwrap_or(JitType::Obj)
        }
        _ => JitType::Obj,
    }
}

/// The guard snapshot's value for the erased builtin global named
/// `code.names[name_idx]` (a `len` or `math` intrinsic span's callee).
fn erased_global(entry: &CompiledEntry, code: &CodeObject, name_idx: u32) -> Object {
    let Some(name) = code.names.get(name_idx as usize) else {
        return Object::None;
    };
    entry
        .guard_snapshot
        .iter()
        .find(|(n, _)| n == name)
        .map_or(Object::None, |(_, o)| o.clone())
}

fn classify_global(obj: Option<&Object>) -> ResolvedGlobal {
    // A `math` intrinsic bound to a global (`from math import sqrt`).
    if let Some(builtin @ Object::Builtin(b)) = obj {
        if let Some(func) = weavepy_jit::MathFunc::from_attr(b.name) {
            if math_builtin_ok(builtin, b.name) {
                return ResolvedGlobal::MathGlobal(func);
            }
        }
    }
    match obj {
        Some(Object::Builtin(b)) if b.name == "range" => ResolvedGlobal::RangeBuiltin,
        Some(Object::Type(t)) if Rc::ptr_eq(t, &crate::builtin_types::builtin_types().range_) => {
            ResolvedGlobal::RangeBuiltin
        }
        // RFC 0065 WS5 — `len` on a pinned list lowers to `ListLen`.
        // Builtins reject attribute mutation, so identity (the entry
        // guard) implies unmodified call semantics.
        Some(Object::Builtin(b)) if b.name == "len" => ResolvedGlobal::LenBuiltin,
        // The canonical `list` type (builtins may hold the function
        // flavour): `list(range(...))` builds natively.
        Some(Object::Builtin(b)) if b.name == "list" => ResolvedGlobal::ListBuiltin,
        Some(Object::Type(t)) if Rc::ptr_eq(t, &crate::builtin_types::builtin_types().list_) => {
            ResolvedGlobal::ListBuiltin
        }
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
    /// The running compilation's frame layout, owned by whoever entered
    /// this activation. A direct self call shares its caller's context,
    /// so its deopt rebuild reads the layout here rather than through
    /// the tier cache, which may retire the code mid-recursion.
    cf: *const CompiledFrame,
    guard_snapshot: StdRc<GuardSnapshot>,
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
    /// Memoized string and tuple constant pins: (constant index, pin bits).
    /// A loop reuses one pin per constant, keeping the pin table bounded.
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
    /// Entry roots do not count toward the temporary-pin soft limit.
    entry_pin_count: usize,
    /// How many temporary pins this activation may hold before its next
    /// poll leaves (see [`relax_pin_limit`]): the soft limit, raised while
    /// most of them aren't the last reference to their object.
    pin_limit: usize,
    /// The temporary pins [`relax_pin_limit`] has counted so far, and how
    /// many of those held their object's last reference when it did.
    pins_counted: usize,
    last_ref_pins: usize,
    /// Set only when a helper is committed to a reconstruction exit.
    /// Generated code exits before another native operation can run.
    pin_pressure_exit: bool,
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
    /// Interpreter round-trips this activation has made through the call
    /// helpers (see [`INTERP_CALL_RETIRE_BUDGET`]).
    interp_calls: u32,
    /// Generic calls of an *interpreted* callee since the last poll (the
    /// density the poll judges: an attribute round-trip or a native
    /// callee is charged separately).
    dyn_py_calls: u32,
    /// Generic calls of native callees this activation has made (see
    /// [`charge_native_roundtrip`]).
    native_calls: u32,
    /// Native loop polls this activation made (one per
    /// `JIT_POLL_STRIDE` loop-header iterations): the native work that
    /// earns generic native-callee calls their keep (see
    /// [`charge_native_roundtrip`]).
    polls: u32,
    /// The context of this activation's most recent native callee, kept
    /// for reuse by its next call of the same callee (see
    /// `try_native_call`). Owns its handles; taken out while in use.
    child: Option<Box<CallCtx>>,
    /// The executing thread's recursion-depth cell (see
    /// `crate::recursion::depth_cell`), resolved once per entry into
    /// native code and inherited by nested native calls.
    depth_cell: *const std::cell::Cell<usize>,
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
    /// The last dynamic call's resolved native callee, keyed by the
    /// function and code identities and the method form: a site calling
    /// the same function again (a stored bound method, a function held in
    /// a local) re-enters without re-resolving or rebuilding the handle.
    /// Taken out for the call's duration and put back after.
    dyn_callee: Option<(usize, usize, bool, NativeCallee)>,
    /// Pin indices recently handed out for instances and strings, by
    /// object address (see [`pin_reusing`]): a loop over the same few
    /// objects reuses their pins instead of growing the table. Advisory;
    /// every hit is checked against the pin it names.
    pin_memo: [u32; PIN_MEMO],
    /// Python code inspected this activation's frame (its `f_locals`):
    /// the frame's locals were synced from the native buffers then, and
    /// the activation leaves native code after the call that did it,
    /// keeping whatever the inspection wrote (see [`sync_native_locals`]).
    introspected: Cell<bool>,
    /// Per closure cell, the pin of the list a list-lane read of it pinned
    /// last (`u32::MAX` for none): reread while the cell holds the same
    /// list, it costs no new pin.
    cell_list_pins: Vec<u32>,
    /// A frameless activation's inspected locals: the storage of the
    /// activation shell the inspection found (a framed activation's are
    /// its frame's own).
    inspected_locals: std::cell::RefCell<Option<Rc<GilRefCell<Vec<Object>>>>>,
}

/// A framed native activation running on this thread, registered so
/// frame introspection can find its live locals (see
/// [`sync_native_locals`]).
struct NativeFrameRec {
    /// The interpreter frame's (or activation shell's) locals storage,
    /// the identity a frame object's locals mirror shares.
    locals: *const GilRefCell<Vec<Object>>,
    ctx: *const CallCtx,
    jf: *const JitFrame,
}

thread_local! {
    /// The framed native activations on this thread's stack, innermost
    /// last.
    static NATIVE_FRAMES: std::cell::RefCell<Vec<NativeFrameRec>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Register the native activation `jf` (whose context is `jf.ctx`) as
/// running the frame whose locals storage is `locals`, for the duration
/// of `run`.
fn with_native_frame<T>(
    locals: &Rc<GilRefCell<Vec<Object>>>,
    jf: *const JitFrame,
    run: impl FnOnce() -> T,
) -> T {
    // SAFETY: `jf` is the live activation's frame; its context pointer
    // was set from the activation's `CallCtx`.
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { (*jf).ctx.cast::<CallCtx>() };
    NATIVE_FRAMES.with(|recs| {
        recs.borrow_mut().push(NativeFrameRec {
            locals: Rc::as_ptr(locals),
            ctx,
            jf,
        });
    });
    let out = run();
    NATIVE_FRAMES.with(|recs| {
        recs.borrow_mut().pop();
    });
    out
}

/// The end of an activation's native run: an inspected activation stops
/// failing its compile's guards (see [`GuardSnapshot::introspected`]).
fn end_introspection(ctx: &CallCtx) {
    if ctx.introspected.get() {
        let n = ctx.guard_snapshot.introspected.get();
        ctx.guard_snapshot.introspected.set(n.saturating_sub(1));
    }
}

/// Make the frame whose locals storage is `locals` current for Python
/// code inspecting it (a `FrameLocalsProxy`, PEP 667): when native code
/// is running that frame, its locals live in the native buffers (written
/// back before every call out of native code), so copy them into the
/// frame. The activation is then marked: it leaves native code right
/// after the call it is suspended in, and its exit keeps the frame's
/// locals, including anything the inspection wrote.
pub(crate) fn sync_native_locals(locals: &Rc<GilRefCell<Vec<Object>>>) {
    let p = Rc::as_ptr(locals);
    NATIVE_FRAMES.with(|recs| {
        let recs = recs.borrow();
        let Some(rec) = recs.iter().rev().find(|r| r.locals == p) else {
            return;
        };
        // SAFETY: a registered activation is live, suspended in a helper
        // call below this one; its context and frame outlive the record.
        let ctx = unsafe { &*rec.ctx };
        let jf = unsafe { &*rec.jf };
        // SAFETY: the compiled frame outlives its activations.
        let Some(cf) = (unsafe { ctx.cf.as_ref() }) else {
            return;
        };
        let Ok(mut out) = locals.try_borrow_mut() else {
            return;
        };
        // A frameless activation's shell starts out empty.
        if let Some(code) = &ctx.frameless_code {
            if out.len() < code.varnames.len() {
                out.resize(code.varnames.len(), Object::Unbound);
            }
            *ctx.inspected_locals.borrow_mut() = Some(locals.clone());
        }
        let n = out
            .len()
            .min(cf.local_types.len())
            .min(jf.n_locals as usize);
        for (slot, dst) in out.iter_mut().enumerate().take(n) {
            if let Some(ty) = cf.local_types[slot] {
                // SAFETY: the buffer is `n_locals` wide.
                let bits = unsafe { *jf.locals.add(slot) };
                *dst = unpack_ty(bits, ty, &ctx.pins);
            }
        }
        if !ctx.introspected.get() {
            ctx.introspected.set(true);
            let n = ctx.guard_snapshot.introspected.get();
            ctx.guard_snapshot.introspected.set(n + 1);
        }
    });
}

/// The size of [`CallCtx::pin_memo`].
const PIN_MEMO: usize = 16;

/// The identity [`pin_reusing`] keys an object by (`0` for objects whose
/// pins aren't reused).
#[inline]
fn pin_identity(v: &Object) -> usize {
    match v {
        Object::Instance(i) => Rc::as_ptr(i) as usize,
        Object::Str(s) => SharedStr::as_ptr(s).cast::<u8>() as usize,
        _ => 0,
    }
}

/// The [`CallCtx::pin_memo`] entry for an object identity.
#[inline]
fn pin_memo_slot(id: usize) -> usize {
    (id >> 4 ^ id >> 9) % PIN_MEMO
}

/// The pin that already holds this very object, when `memo` remembers
/// one. A pin only owns its object, so two sites that pin the same object
/// can share one.
#[inline]
fn pin_memo_hit(v: &Object, pins: &PinTable, memo: &[u32; PIN_MEMO]) -> Option<u64> {
    let id = pin_identity(v);
    if id == 0 {
        return None;
    }
    let hint = memo[pin_memo_slot(id)] as usize;
    match pins.get(hint) {
        Some(Pin::Obj(p)) if pin_identity(p) == id => Some(hint as u64),
        _ => None,
    }
}

/// The pin that already holds this very list (a row of a nested list
/// read again), when the memo remembers one.
#[inline]
fn list_pin_memo_hit(
    l: &Rc<crate::sync::RefCell<Vec<Object>>>,
    pins: &PinTable,
    memo: &[u32; PIN_MEMO],
) -> Option<u64> {
    let id = Rc::as_ptr(l) as usize;
    let hint = memo[pin_memo_slot(id)] as usize;
    match pins.get(hint) {
        Some(Pin::List(p, _)) if Rc::ptr_eq(p, l) => Some(hint as u64),
        _ => None,
    }
}

/// Remember that pin `ix` will hold `p`'s object.
#[inline]
fn pin_memo_note(p: &Pin, ix: usize, memo: &mut [u32; PIN_MEMO]) {
    match p {
        Pin::Obj(v) => {
            let id = pin_identity(v);
            if id != 0 {
                memo[pin_memo_slot(id)] = ix as u32;
            }
        }
        Pin::List(l, _) => memo[pin_memo_slot(Rc::as_ptr(l) as usize)] = ix as u32,
    }
}

/// Pin `v` in `pins`, reusing the pin that already holds this very object
/// (see [`pin_memo_hit`]); `None` at the pin cap.
#[inline]
fn pin_reusing(v: &Object, pins: &mut PinTable, memo: &mut [u32; PIN_MEMO]) -> Option<u64> {
    if let Some(bits) = pin_memo_hit(v, pins, memo) {
        return Some(bits);
    }
    if pins.len() >= RUNTIME_PIN_CAP {
        return None;
    }
    let p = Pin::Obj(v.clone());
    pin_memo_note(&p, pins.len(), memo);
    pins.push(p);
    Some((pins.len() - 1) as u64)
}

impl CallCtx {
    fn temporary_pin_limit_reached(&self) -> bool {
        self.pins.len().saturating_sub(self.entry_pin_count) >= self.pin_limit
    }

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
/// In line up to the namespaces' stamps, which settle the common case.
#[inline]
fn guards_hold(
    interp: &super::Interpreter,
    globals: &Rc<GilRefCell<DictData>>,
    builtins: &Rc<GilRefCell<DictData>>,
    guard_snapshot: &GuardSnapshot,
    callees: &CalleeTable,
    math: &MathTable,
) -> bool {
    if guard_snapshot.introspected.get() != 0 {
        return false;
    }
    // SAFETY (stamp reads): GIL-serialized raw reads of the dicts'
    // mutation stamps — no dict borrow is held while native code is
    // entered or resumed (the reentrant dict paths exist so user code
    // never runs under one).
    let key = (
        Rc::as_ptr(globals) as usize,
        unsafe { (*globals.as_ptr()).mutation_stamp() },
        Rc::as_ptr(builtins) as usize,
        unsafe { (*builtins.as_ptr()).mutation_stamp() },
        // A rebinding in place leaves the stamps alone (see `STORE_GLOBAL`).
        crate::object::global_value_epoch(),
    );
    if (guard_snapshot.last_ok.get() != key || interp.globals_missing_any.get())
        && !global_guards_hold(interp, globals, builtins, guard_snapshot, key)
    {
        return false;
    }
    (callees.is_empty() || callee_guards_hold(interp, callees, &guard_snapshot.ctor_vers))
        && math.iter().all(MathGuard::holds)
        && !guard_snapshot
            .defaults
            .iter()
            .any(|f| defaults_overridden(f))
}

/// [`guards_hold`]'s globals, resolved name by name (and remembered as
/// holding at `key` when they do).
#[inline(never)]
fn global_guards_hold(
    interp: &super::Interpreter,
    globals: &Rc<GilRefCell<DictData>>,
    builtins: &Rc<GilRefCell<DictData>>,
    guard_snapshot: &GuardSnapshot,
    key: (usize, u64, usize, u64, u64),
) -> bool {
    for (name, expected) in guard_snapshot.entries.iter() {
        let ok = resolve_plain_dicts(interp, globals, builtins, name)
            .is_some_and(|cur| cur.is_same(expected));
        if !ok {
            return false;
        }
    }
    guard_snapshot.last_ok.set(key);
    true
}

/// [`guards_hold`]'s burned-in callees.
#[inline(never)]
fn callee_guards_hold(
    interp: &super::Interpreter,
    callees: &CalleeTable,
    ctor_vers: &[Cell<u64>],
) -> bool {
    for (i, (f, code_snap)) in callees.iter().enumerate() {
        match f {
            Object::Function(pf) => {
                // SAFETY: GIL-serialized raw read of the code cell (see
                // `PyFunction::code`); only the pointer is compared.
                if !std::ptr::eq(
                    unsafe { Rc::as_ptr(&*pf.code.as_ptr()) },
                    Rc::as_ptr(code_snap),
                ) {
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
                let ver = t.attr_version.get();
                let memo = ctor_vers.get(i);
                if memo.is_some_and(|m| m.get() == ver) && ctor_still_inits_with(t, ver, code_snap)
                {
                    continue;
                }
                let ok = matches!(
                    probe_class_ctor_shape(interp, t),
                    Some((init_code, ..)) if Rc::ptr_eq(&init_code, code_snap)
                );
                if !ok {
                    return false;
                }
                if let Some(m) = memo {
                    m.set(ver);
                }
            }
            _ => return false,
        }
    }
    true
}

/// Whether class `t`, unchanged since its constructor's probe held at
/// version `ver`, still constructs through type's own metaclass with the
/// plain `__init__` wearing `code`: the rest of the probe is a function of
/// the version (the memoised plan says so for the plan itself).
fn ctor_still_inits_with(t: &TypeObject, ver: u64, code: &Rc<CodeObject>) -> bool {
    if !t.metaclass_is_type() {
        return false;
    }
    let Ok(plan) = t.instance_plan.try_borrow() else {
        return false;
    };
    match plan.as_ref() {
        Some((v, plan)) if *v == ver => match plan.init_fn.as_ref() {
            // SAFETY: GIL-serialized raw read of the code cell; only the
            // pointer is compared.
            Some(Object::Function(f)) => {
                std::ptr::eq(unsafe { Rc::as_ptr(&*f.code.as_ptr()) }, Rc::as_ptr(code))
            }
            _ => false,
        },
        _ => false,
    }
}

/// RFC 0067 WS1 — pooled exchange buffers for nested native entries
/// (and the top-level `enter_compiled`), so a call-recursive program
/// doesn't `malloc` five vectors per call.
#[derive(Default)]
struct JitBufs {
    u64s: Vec<Vec<u64>>,
    u32s: Vec<Vec<u32>>,
    u64_capacity_bytes: usize,
    u32_capacity_bytes: usize,
}

const JIT_BUF_POOL_CAP: usize = 64;
// Bound retained element capacity as well as entry count. This excludes
// allocator rounding and the pool's own Vec headers. Active buffers can grow
// without this limit; oversized owners are simply released after their call.
const JIT_BUF_POOL_CAPACITY_BYTES: usize = 16 * 1024;

thread_local! {
    static JIT_BUFS: RefCell<JitBufs> = RefCell::new(JitBufs::default());
}

/// A pooled `u64` buffer of exactly `n` zeroed entries.
fn take_u64(n: usize) -> Vec<u64> {
    let mut v = JIT_BUFS
        .with(|p| {
            let mut p = p.borrow_mut();
            let v = p.u64s.pop()?;
            p.u64_capacity_bytes -= v.capacity() * std::mem::size_of::<u64>();
            Some(v)
        })
        .unwrap_or_default();
    v.clear();
    v.resize(n, 0);
    v
}

/// A pooled `u32` buffer of exactly `n` zeroed entries.
fn take_u32(n: usize) -> Vec<u32> {
    let mut v = JIT_BUFS
        .with(|p| {
            let mut p = p.borrow_mut();
            let v = p.u32s.pop()?;
            p.u32_capacity_bytes -= v.capacity() * std::mem::size_of::<u32>();
            Some(v)
        })
        .unwrap_or_default();
    v.clear();
    v.resize(n, 0);
    v
}

fn put_u64(v: Vec<u64>) {
    JIT_BUFS.with(|p| {
        let mut p = p.borrow_mut();
        let bytes = v.capacity() * std::mem::size_of::<u64>();
        if p.u64s.len() < JIT_BUF_POOL_CAP
            && bytes <= JIT_BUF_POOL_CAPACITY_BYTES - p.u64_capacity_bytes
        {
            p.u64_capacity_bytes += bytes;
            p.u64s.push(v);
        }
    });
}

fn put_u32(v: Vec<u32>) {
    JIT_BUFS.with(|p| {
        let mut p = p.borrow_mut();
        let bytes = v.capacity() * std::mem::size_of::<u32>();
        if p.u32s.len() < JIT_BUF_POOL_CAP
            && bytes <= JIT_BUF_POOL_CAPACITY_BYTES - p.u32_capacity_bytes
        {
            p.u32_capacity_bytes += bytes;
            p.u32s.push(v);
        }
    });
}

thread_local! {
    /// The pin table of the last framed activation to finish, drained and
    /// kept for the next one: a long loop that leaves at the pin limit and
    /// re-enters would otherwise regrow its table from empty each time.
    static SPARE_PINS: Cell<PinTable> = const { Cell::new(Vec::new()) };
}

/// The most pins [`SPARE_PINS`] keeps capacity for: as many as an
/// activation reaches before a pressure exit (see [`relax_pin_limit`]),
/// with room for its entry pins (under a megabyte).
const SPARE_PINS_CAP: usize = RUNTIME_PIN_CAP / 2 + 2 * RUNTIME_PIN_SOFT_LIMIT;

/// An empty pin table, with the spare's capacity when there is one.
fn take_pins() -> PinTable {
    SPARE_PINS.with(Cell::take)
}

/// Keep `pins`' allocation for the next activation (see [`SPARE_PINS`]).
fn put_pins(mut pins: PinTable) {
    if pins.capacity() == 0 || pins.capacity() > SPARE_PINS_CAP {
        return;
    }
    pins.clear();
    SPARE_PINS.with(|spare| {
        let old = spare.take();
        spare.set(if old.capacity() >= pins.capacity() {
            old
        } else {
            pins
        });
    });
}

#[cfg(test)]
mod scratch_pool_tests {
    use super::*;

    fn assert_accounting() {
        JIT_BUFS.with(|pool| {
            let pool = pool.borrow();
            assert!(pool.u64s.len() <= JIT_BUF_POOL_CAP);
            assert!(pool.u32s.len() <= JIT_BUF_POOL_CAP);
            assert!(pool.u64_capacity_bytes <= JIT_BUF_POOL_CAPACITY_BYTES);
            assert!(pool.u32_capacity_bytes <= JIT_BUF_POOL_CAPACITY_BYTES);
            assert_eq!(
                pool.u64_capacity_bytes,
                pool.u64s.iter().map(|v| v.capacity() * 8).sum::<usize>()
            );
            assert_eq!(
                pool.u32_capacity_bytes,
                pool.u32s.iter().map(|v| v.capacity() * 4).sum::<usize>()
            );
        });
    }

    #[test]
    fn scratch_pool_enforces_count_and_capacity_limits() {
        std::thread::spawn(|| {
            for _ in 0..JIT_BUF_POOL_CAP + 3 {
                put_u64(vec![17]);
                put_u32(vec![19]);
            }
            assert_accounting();
            JIT_BUFS.with(|pool| {
                let mut pool = pool.borrow_mut();
                assert_eq!(pool.u64s.len(), JIT_BUF_POOL_CAP);
                assert_eq!(pool.u32s.len(), JIT_BUF_POOL_CAP);
                *pool = JitBufs::default();
            });
            put_u64(Vec::with_capacity(JIT_BUF_POOL_CAPACITY_BYTES / 8 + 1));
            put_u32(Vec::with_capacity(JIT_BUF_POOL_CAPACITY_BYTES / 4 + 1));
            JIT_BUFS.with(|pool| {
                let pool = pool.borrow();
                assert!(pool.u64s.is_empty());
                assert!(pool.u32s.is_empty());
            });
            assert_accounting();
            for _ in 0..3 {
                put_u64(Vec::with_capacity(JIT_BUF_POOL_CAPACITY_BYTES / 16));
                put_u32(Vec::with_capacity(JIT_BUF_POOL_CAPACITY_BYTES / 8));
            }
            assert_accounting();
            JIT_BUFS.with(|pool| {
                let pool = pool.borrow();
                assert_eq!(pool.u64s.len(), 2);
                assert_eq!(pool.u32s.len(), 2);
            });
        })
        .join()
        .unwrap();
    }

    #[test]
    fn scratch_pool_releases_budget_before_reused_buffers_grow() {
        std::thread::spawn(|| {
            for _ in 0..8 {
                for n in [1, 128, 4096, 2, 8192, 0] {
                    let mut words = take_u64(n);
                    assert_eq!(words.len(), n);
                    assert!(words.iter().all(|v| *v == 0));
                    assert_accounting();
                    words.fill(17);
                    put_u64(words);
                    let mut tags = take_u32(n);
                    assert_eq!(tags.len(), n);
                    assert!(tags.iter().all(|v| *v == 0));
                    assert_accounting();
                    tags.fill(19);
                    put_u32(tags);
                    assert_accounting();
                }
            }
        })
        .join()
        .unwrap();
    }
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
    {
        // SAFETY: as below.
        let jf = unsafe { &*frame };
        if !jf.ctx.is_null() {
            #[allow(clippy::cast_ptr_alignment)]
            let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
            ctx.polls = ctx.polls.saturating_add(1);
            // A poll interval dense with generic calls — of a native
            // callee, or of an interpreted one the callee tables could
            // not enter natively: the loop is a native driver around
            // interpreter calls, each paying an activation shell, a
            // generic call and a full `guards_hold` revalidation. The
            // interpreter's own inline call path serves such a loop
            // several times better (measured on OO call loops: a
            // one-call-per-iteration loop ran 3x slower compiled).
            let dense = (ctx.native_calls > NATIVE_CALLS_PER_POLL
                || ctx.dyn_py_calls > INTERP_CALLS_PER_POLL)
                && !ctx.code_ptr.is_null();
            ctx.native_calls = 0;
            ctx.dyn_py_calls = 0;
            if dense {
                retire_native_driver(ctx);
                return 1;
            }
        }
    }
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
        if ctx.temporary_pin_limit_reached() && !relax_pin_limit(ctx) {
            // The generated poll immediately spills live locals and the
            // boundary stack, then exits. No pin is freed while native
            // registers can still refer to its index.
            ctx.pin_pressure_exit = true;
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
        JitType::ListInt
        | JitType::ListFloat
        | JitType::ListObj
        | JitType::ListListFloat
        | JitType::ListListInt => SlotTag::ListPin as u32,
        _ => u32::MAX,
    }
}

/// RFC 0071 WS1 — pack a call result for an `ObjPin` return lane: the
/// `None` singleton is the nullable lane's `-1`; an instance pins into
/// the caller's table (capped), reusing the pin that already holds it
/// (a method returning `self`). Anything else can't ride the lane.
fn obj_ret_bits(v: &Object, pins: &mut PinTable, memo: &mut [u32; PIN_MEMO]) -> Option<u64> {
    match v {
        Object::None => Some(u64::MAX),
        Object::Instance(_) => pin_reusing(v, pins, memo),
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

/// Bump a native-call diagnostic counter. The counters only feed the
/// VM stats report (and unit tests), so they stay off the call path
/// otherwise: each is a thread-local access.
#[inline(always)]
fn native_stat(f: impl FnOnce(&NativeCallStats)) {
    if cfg!(test) || crate::specialize::stats_enabled() {
        NATIVE_CALL_STATS.with(f);
    }
}

const SCALAR_LEAF_SLOTS: usize = 32;

/// Enter a certified scalar leaf with bounded stack storage. The caller
/// has validated argument/default lanes, global and math guards, and charged
/// the recursion tick.
///
/// # Safety
///
/// `cf` is a live scalar leaf with fewer than `SCALAR_LEAF_SLOTS` stack
/// entries and at most that many locals, compiled from `func`'s `code`.
/// `argc` entries of `jf.call_args`
/// are initialized, and all missing trailing arguments have scalar defaults.
#[inline(never)]
unsafe fn enter_scalar_leaf(
    cf: &CompiledFrame,
    func: &PyFunction,
    code: &CodeObject,
    jf: &JitFrame,
    argc: usize,
) -> Option<(u64, u32)> {
    // Only the locals need initializing: the operand spill and its tags
    // are written by native code before anything reads them.
    let mut locals = [0u64; SCALAR_LEAF_SLOTS];
    for (j, slot) in locals.iter_mut().enumerate().take(argc) {
        // SAFETY: the caller validated the marshaled argument prefix.
        *slot = unsafe { *jf.call_args.add(j) };
    }
    for (k, slot) in locals
        .iter_mut()
        .enumerate()
        .take(code.arg_count as usize)
        .skip(argc)
    {
        let default = &func.defaults[func.defaults.len() - (code.arg_count as usize - k)];
        *slot = pack(default, cf.local_types[k]?)?;
    }
    let mut spill = std::mem::MaybeUninit::<[u64; SCALAR_LEAF_SLOTS]>::uninit();
    let mut tags = std::mem::MaybeUninit::<[u32; SCALAR_LEAF_SLOTS]>::uninit();
    let mut frame = JitFrame {
        locals: locals.as_mut_ptr(),
        n_locals: cf.n_locals,
        entry_pc: 0,
        ret_bits: 0,
        ret_tag: 0,
        deopt_pc: 0,
        stack_spill: spill.as_mut_ptr().cast::<u64>(),
        stack_tags: tags.as_mut_ptr().cast::<u32>(),
        stack_len: 0,
        stack_cap: SCALAR_LEAF_SLOTS as u32,
        ctx: std::ptr::null_mut(),
        call_args: std::ptr::null_mut(),
        call_tags: std::ptr::null_mut(),
    };
    // SAFETY: the engine's scalar-leaf allowlist excludes every contextual
    // helper, pin, poll, and Python call. Its math helpers need only scalar
    // arguments. All buffers fit and the guarded native entry is live.
    let status = unsafe { cf.enter(&raw mut frame) };
    (status == JitStatus::Returned).then_some((frame.ret_bits, frame.ret_tag))
}

/// One already compiled method's exact update shape. The attribute guard
/// owns the name and the class/storage snapshot; no receiver is retained.
struct ScalarFieldUpdatePlan {
    store_token: usize,
    /// None reads the second bound parameter; Some is a literal increment.
    increment: Option<i64>,
}

/// A stable built-in class value can't acquire descriptor hooks on its own.
/// Mutable class values retain ordinary calls, even while they have no hooks.
fn scalar_update_class_value_stable(receiver: &Object, name: &str, ver: u64) -> bool {
    // An exotic class key can run Python equality during lookup, even if
    // default_getattribute has already cached the ordinary access method.
    if crate::object::exotic_str_keys_possible() {
        return false;
    }
    let Object::Instance(inst) = receiver else {
        return false;
    };
    let cls = inst.cls();
    cls.native_kind.get() == 0
        && super::Interpreter::default_getattribute(&cls)
        && matches!(
            cls.lookup(name),
            None | Some(
                Object::Function(_)
                    | Object::Int(_)
                    | Object::Long(_)
                    | Object::Float(_)
                    | Object::Bool(_)
                    | Object::None
                    | Object::Str(_)
                    | Object::WStr(_)
                    | Object::Bytes(_)
                    | Object::Complex(_)
            )
        )
        && cls.attr_version.get() == ver
}

/// [`scalar_update_class_value_stable`] for a `__slots__` member: the
/// class value is the member's own slot descriptor, which has no hooks a
/// class mutation wouldn't version.
fn scalar_update_slot_stable(receiver: &Object, name: &str, ver: u64) -> bool {
    if crate::object::exotic_str_keys_possible() {
        return false;
    }
    let Object::Instance(inst) = receiver else {
        return false;
    };
    let cls = inst.cls();
    cls.native_kind.get() == 0
        && super::Interpreter::default_getattribute(&cls)
        && matches!(cls.lookup(name), Some(Object::SlotDescriptor(sd)) if sd.name == name)
        && cls.attr_version.get() == ver
}

fn scalar_field_update_plan(
    code: &CodeObject,
    cf: &CompiledFrame,
    guards: &[AttrGuard],
) -> Option<ScalarFieldUpdatePlan> {
    use weavepy_compiler::{Constant, OpCode};
    if !scalar_field_update_shape(code) || cf.attr_sites.len() != guards.len() {
        return None;
    }
    let start = usize::from(code.instructions.first()?.op == OpCode::Resume);
    let name = code
        .names
        .get(code.instructions.get(start + 2)?.arg as usize)?;
    let amount = code.instructions.get(start + 3)?;
    let increment = match amount.op {
        OpCode::LoadFast => None,
        OpCode::LoadSmallInt => Some(i64::from(amount.arg)),
        OpCode::LoadConst => match code.constants.get(amount.arg as usize)? {
            Constant::Int(value) => Some(*value),
            _ => return None,
        },
        _ => return None,
    };
    let mut store_token = None;
    let mut read = false;
    let mut fingerprint = None;
    for (token, (site, guard)) in cf.attr_sites.iter().zip(guards).enumerate() {
        if site.slot != 0
            || !site.path.is_empty()
            || site.name != *name
            || site.ctor.is_some()
            || site.self_ctor.is_some()
            || site.new_key
        {
            return None;
        }
        let current = match guard.storage {
            AttrStorage::Indexed(index) => (guard.ver, index, false),
            AttrStorage::Slot(index) => (guard.ver, index, true),
            AttrStorage::NewKey => return None,
        };
        if fingerprint.is_some_and(|old| old != current) {
            return None;
        }
        fingerprint = Some(current);
        if site.store {
            if !guard.stable_descriptor || store_token.is_some() {
                return None;
            }
            store_token = Some(token);
        } else {
            read = true;
        }
    }
    if !read {
        return None;
    }
    Some(ScalarFieldUpdatePlan {
        store_token: store_token?,
        increment,
    })
}

/// Recognize a single in-place addition and return of the same field.
/// Mutation is a separate classification from the read-only leaf family.
#[cold]
#[inline(never)]
fn scalar_field_update_shape(code: &weavepy_compiler::CodeObject) -> bool {
    use weavepy_compiler::{BinOpKind, Constant, OpCode, BINARY_OP_INPLACE_FLAG};
    if code.is_generator
        || code.is_coroutine
        || code.is_async_generator
        || code.is_iterable_coroutine
        || code.is_class_body
        || !code.cellvars.is_empty()
        || !code.freevars.is_empty()
        || code.has_varargs
        || code.has_varkeywords
        || code.kwonly_count != 0
        || !(1..=2).contains(&code.arg_count)
        || code.varnames.len() != code.arg_count as usize
        || !code.exception_table.is_empty()
    {
        return false;
    }
    let body = code.instructions.as_slice();
    let body = if body.first().is_some_and(|i| i.op == OpCode::Resume) {
        &body[1..]
    } else {
        body
    };
    let [receiver, copy, read, amount, add, swap, store, returned, reread, ret] = body else {
        return false;
    };
    receiver.op == OpCode::LoadFast
        && receiver.arg == 0
        && copy.op == OpCode::CopyTop
        && copy.arg == 1
        && read.op == OpCode::LoadAttr
        && add.op == OpCode::BinaryOp
        && add.arg == (BinOpKind::Add as u32 | BINARY_OP_INPLACE_FLAG)
        && swap.op == OpCode::Swap
        && swap.arg == 2
        && store.op == OpCode::StoreAttr
        && store.arg == read.arg
        && returned.op == OpCode::LoadFast
        && returned.arg == 0
        && reread.op == OpCode::LoadAttr
        && reread.arg == read.arg
        && ret.op == OpCode::ReturnValue
        && match amount.op {
            OpCode::LoadFast => code.arg_count == 2 && amount.arg == 1,
            OpCode::LoadSmallInt => code.arg_count == 1,
            OpCode::LoadConst => {
                code.arg_count == 1
                    && matches!(
                        code.constants.get(amount.arg as usize),
                        Some(Constant::Int(_))
                    )
            }
            _ => false,
        }
}

/// Complete one callback-free update after the ordinary native-call preflight.
/// All guards and arithmetic precede the only store. The plan is tied to the
/// actual compiled artifact; the receiver, key, and values are checked live.
///
/// # Safety
///
/// The caller validated binding, defaults, argument lanes/pins, namespaces,
/// observers, and recursion. It retains the GIL and roots every borrowed value.
#[inline(never)]
unsafe fn native_scalar_field_update(
    jf: &JitFrame,
    ctx: &CallCtx,
    nc: &NativeCallee,
    receiver: &Object,
    argc: usize,
) -> Option<i64> {
    #[cfg(test)]
    crate::SCALAR_FIELD_UPDATE_ATTEMPTS.with(|hits| hits.set(hits.get() + 1));
    if crate::gil::free_threading_enabled()
        || crate::trace::any_observers_active()
        || crate::capi_watchers::dicts_active()
        || crate::object::exotic_str_keys_possible()
    {
        return None;
    }
    let plan = nc.scalar_update.as_deref()?;
    let guard = nc.attr_guards.get(plan.store_token)?;
    let (index, slot_storage) = match guard.storage {
        AttrStorage::Indexed(index) => (index, false),
        AttrStorage::Slot(index) => (index, true),
        AttrStorage::NewKey => return None,
    };
    let Object::Instance(inst) = receiver else {
        return None;
    };
    let cls = inst.cls_raw();
    if cls.attr_version.get() != guard.ver
        || cls.native_kind.get() != 0
        || !super::Interpreter::default_getattribute(cls)
    {
        return None;
    }
    let increment = if let Some(increment) = plan.increment {
        increment
    } else if argc == 0 {
        let Object::Int(value) = nc.func.defaults.last()? else {
            return None;
        };
        *value
    } else {
        // SAFETY: the ordinary native preflight checked this live argument.
        let (bits, tag) = unsafe { (*jf.call_args, *jf.call_tags) };
        if tag == SlotTag::Int as u32 {
            bits as i64
        } else if tag == SlotTag::ObjPin as u32 {
            let Some(Pin::Obj(Object::Int(value))) = ctx.pins.get(bits as usize) else {
                return None;
            };
            *value
        } else {
            return None;
        }
    };
    if slot_storage {
        // SAFETY: as for the dictionary below.
        let slots = unsafe { inst.slots.peek_mut() }?;
        let (key, old) = slots.get_index(index as usize)?;
        if !key_is(key, &guard.name) {
            return None;
        }
        let Object::Int(old) = old else {
            return None;
        };
        let value = old.checked_add(increment)?;
        let (_, slot) = slots.get_index_mut(index as usize)?;
        // An exact integer owns no destructor or GC edge.
        *slot = Object::Int(value);
        #[cfg(test)]
        crate::SCALAR_FIELD_UPDATE_NATIVE_CALLS.with(|hits| hits.set(hits.get() + 1));
        return Some(value);
    }
    // SAFETY: no callback, allocation, or Python execution can overlap this
    // exclusive view. A shared cell is rejected by peek_mut.
    let (key, slot) = unsafe { inst.attr_peek_index_mut(index as usize, true) }?;
    if !key_is(key, &guard.name) {
        return None;
    }
    let Object::Int(old) = slot else {
        return None;
    };
    let value = old.checked_add(increment)?;
    // Exact integers own no destructor or GC edge. Existing-key replacement
    // preserves key stamps. No failing operation follows the completed store.
    *slot = Object::Int(value);
    #[cfg(test)]
    crate::SCALAR_FIELD_UPDATE_NATIVE_CALLS.with(|hits| hits.set(hits.get() + 1));
    Some(value)
}

/// Arm `entry`'s in-line update (see [`InlineUpdate`]) after the helper
/// ran it on `receiver`: the method guard and [`native_scalar_field_update`]
/// have just held, so what compiled code still checks per call (the
/// receiver's class version and split names, the shadow bound, the code
/// identity, the observer and key gates) is all that can change. A split
/// field and the plain increments only; anything else stays unarmed.
fn arm_update(entry: &MethodEntry, nc: &NativeCallee, receiver: &Object, argc: u32) {
    let Some(plan) = nc.scalar_update.as_deref() else {
        return;
    };
    // (A native method's body is no field update.)
    let MethodCallee::Py { func, code } = &entry.callee else {
        return;
    };
    let Some(guard) = nc.attr_guards.get(plan.store_token) else {
        return;
    };
    let Object::Instance(inst) = receiver else {
        return;
    };
    let cls = inst.cls_raw();
    let Some(keys) = cls.shared_keys.get() else {
        return;
    };
    let (from_arg, inc) = match (plan.increment, argc) {
        (Some(inc), 0) => (0, inc),
        (None, 1) => (1, 0),
        _ => return,
    };
    if guard.split_idx == u32::MAX
        || guard.ver != entry.ver
        || cls.attr_version.get() != entry.ver
        || !matches!(guard.storage, AttrStorage::Indexed(i) if i == guard.split_idx)
    {
        return;
    }
    let shadow = keys.names_before(&entry.name, entry.name_hash);
    let u = &entry.update;
    u.shadow.set(u32::try_from(shadow).unwrap_or(0));
    u.from_arg.set(from_arg);
    u.inc.set(inc);
    u.code_at.set(func.code.as_ptr() as usize);
    // SAFETY: `Rc` is one pointer, compared and never dereferenced.
    u.code
        .set(unsafe { std::mem::transmute_copy::<Rc<CodeObject>, usize>(code) });
    u.idx.set(guard.split_idx);
    #[cfg(test)]
    crate::SCALAR_FIELD_UPDATE_ARMED.with(|armed| armed.set(armed.get() + 1));
}

/// Bind the slots a keyword call skipped (tagged [`SlotTag::Default`]
/// under [`weavepy_jit::CALL_GAPS`]) to the callee's current
/// `__defaults__` when those are scalars, so the call is a positional
/// prefix again (whose trailing window the lanes below bind). Returns
/// the argument count to call with and whether every skipped slot is
/// bound (`false` leaves the call to [`call_py_with_gaps`]).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]; the buffers are the compiled
/// frame's `max_call_args` wide.
unsafe fn bind_call_defaults(
    jf: &mut JitFrame,
    ctx: &CallCtx,
    token: u32,
    raw: u32,
) -> (u32, bool) {
    let gapped = raw & weavepy_jit::CALL_GAPS != 0;
    let argc = raw & !weavepy_jit::CALL_GAPS;
    let Some((Object::Function(f), code)) = ctx.callees.get(token as usize) else {
        return (argc, !gapped);
    };
    if !gapped {
        return (argc, true);
    }
    // A reassigned `__defaults__` lives in the function's slots: the
    // generic binder reads it.
    if f.defaults_maybe_overridden() {
        return (argc, false);
    }
    let npos = code.arg_count as usize;
    // SAFETY: the activation's compiled frame outlives its calls.
    let cap = unsafe { ctx.cf.as_ref() }.map_or(0, |cf| cf.max_call_args as usize);
    let first = npos.saturating_sub(f.defaults.len());
    let scalar = |j: usize| -> Option<(u64, SlotTag)> {
        let v = f.defaults.get(j.checked_sub(first)?)?;
        match v {
            Object::Int(i) => Some((*i as u64, SlotTag::Int)),
            Object::Float(x) => Some((x.to_bits(), SlotTag::Float)),
            Object::Bool(b) => Some((u64::from(*b), SlotTag::Bool)),
            Object::None => Some((u64::MAX, SlotTag::ObjPin)),
            _ => None,
        }
    };
    let write = |jf: &mut JitFrame, j: usize, (bits, tag): (u64, SlotTag)| {
        // SAFETY: `j` is below the buffers' width (checked by callers).
        unsafe {
            *jf.call_args.add(j) = bits;
            *jf.call_tags.add(j) = tag as u32;
        }
    };
    let mut bound = true;
    for j in 0..(argc as usize).min(cap).min(npos) {
        // SAFETY: native code wrote `argc` tags.
        if unsafe { *jf.call_tags.add(j) } == SlotTag::Default as u32 {
            match scalar(j) {
                Some(v) => write(jf, j, v),
                None => bound = false,
            }
        }
    }
    (argc, bound)
}

/// [`wpjit_call_py`] for a keyword call whose skipped defaulted slot
/// could not be bound natively (a non-scalar default, or none any
/// more): the generic call, with the prefix before the first skipped
/// slot positional and the rest by name, binds (or rejects) it exactly
/// as the interpreter would.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe fn call_py_with_gaps(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    token: u32,
    argc: u32,
    expect_tag: u32,
) -> i64 {
    ctx.dirty = true;
    let (callee, code) = ctx.callees[token as usize].clone();
    let mut args: Vec<Object> = Vec::new();
    let mut kwargs: Vec<(String, Object)> = Vec::new();
    for j in 0..argc as usize {
        // SAFETY: native code wrote `argc` entries.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        if tag == SlotTag::Default as u32 {
            continue;
        }
        let v = unpack_pins(bits, tag, &ctx.pins);
        if kwargs.is_empty() && args.len() == j {
            args.push(v);
        } else if let Some(name) = code.varnames.get(j) {
            kwargs.push((name.clone(), v));
        }
    }
    let res = call_with_activation_shell(interp, ctx, jf, |i| {
        i.call(&callee, &args, &kwargs, &ctx.globals)
    });
    finish_interp_call(jf, ctx, interp, res, expect_tag)
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
    interp.gil_countdown = interp.gil_countdown.wrapping_sub(1);
    if interp.gil_countdown == 0 {
        interp.gil_countdown = crate::gil::GIL_CHECK_INTERVAL;
        crate::gil::yield_checkpoint();
    }
    if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
        return None;
    }
    // A callee retired since this activation resolved its callee table
    // (the deopt backoff below, or another caller's) must not be entered
    // again: the table is only re-resolved when the compile generation
    // moves, and a chronic side-exiter would otherwise keep paying an
    // entry plus a full frame materialization per call.
    if nc.code.jit_hint.is_not_jitable() {
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
    if supplied < n_params && !native_defaults_fit(nc, supplied, n_params) {
        return None;
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
        // A list argument's pin must carry the callee's element lane.
        if tag == SlotTag::ListPin as u32
            && !matches!(ctx.pins.get(bits as usize), Some(Pin::List(_, e)) if Some(*e) == lane.elem_lane())
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
    let recursion_guard = match crate::recursion::enter_with(ctx.depth_cell) {
        crate::recursion::Enter::Ok(g) => g,
        crate::recursion::Enter::Overflow => return None,
    };
    native_stat(|s| s.calls.set(s.calls.get() + 1));

    if recv.is_none()
        && nc.cf.is_scalar_leaf()
        && nc.cf.n_locals as usize <= SCALAR_LEAF_SLOTS
        && (nc.cf.max_stack as usize) < SCALAR_LEAF_SLOTS
    {
        // SAFETY: the checks above establish the leaf's size, scalar
        // lane, default-binding, code-lifetime, and recursion invariants.
        let status = unsafe { scalar_leaf_call(jf, ctx, nc, argc_usize, expect_tag) };
        // Only pure numeric work has run when the leaf declines: the
        // ordinary call path can restart it to compute a bignum or raise
        // with a complete interpreter frame. This return releases the
        // recursion guard before that path charges its own tick.
        drop(recursion_guard);
        return status;
    }

    // A framed native call of a *tiny* callee (a getter, a comparison)
    // costs more than the interpreter's own inline activation of the
    // same body, so a loop dense with them belongs to tier-1: charge it
    // to the poll's density judgment (see `wpjit_poll`).
    // A body the interpreter evaluates without a frame at all (a pure
    // leaf: a getter, a comparison, a constant return) is one such
    // callee whatever its shape. A tiny *method* that stores — the
    // attribute fixtures' `tick` — is not, and its loop keeps its
    // native lane.
    let pure_leaf = match nc.code.jit_hint.pure_leaf() {
        Some(known) => known,
        None => crate::code_is_pure_leaf_pub(&nc.code),
    };
    if pure_leaf || (recv.is_none() && nc.code.instructions.len() <= TINY_CALLEE_OPS) {
        ctx.dyn_py_calls = ctx.dyn_py_calls.saturating_add(1);
    }
    // The framed native call below is itself expensive (buffers, pins, a
    // callee context, a recursion tick): a loop of them is call-shaped
    // too. Past the budget the interpreter path takes it (and retires
    // the code through `finish_interp_call`).
    #[allow(clippy::absurd_extreme_comparisons)] // budget currently 0 — see the constant
    if INTERP_CALL_RETIRE_BUDGET != 0
        && ctx.interp_calls.saturating_add(1) >= INTERP_CALL_RETIRE_BUDGET
    {
        drop(recursion_guard);
        return None;
    }
    ctx.interp_calls = ctx.interp_calls.saturating_add(1);
    if nc.scalar_update.is_some() {
        if let Some(receiver) = recv {
            // SAFETY: the ordinary native-call preflight above validated
            // binding, live lanes/pins, namespaces, observers, and recursion.
            if let Some(value) =
                unsafe { native_scalar_field_update(jf, ctx, nc, receiver, argc_usize) }
            {
                if expect_tag == SlotTag::Int as u32 {
                    jf.ret_bits = value as u64;
                    jf.ret_tag = SlotTag::Int as u32;
                    return Some(CallStatus::Ok as i64);
                }
                // The store is complete. Preserve its result on a lane
                // mismatch; never restart the body or repeat the mutation.
                #[cfg(test)]
                crate::SCALAR_FIELD_UPDATE_BOXED_RETURNS.with(|hits| hits.set(hits.get() + 1));
                ctx.parked = Some(Object::Int(value));
                return Some(CallStatus::Boxed as i64);
            }
        }
    }
    // Keep disjoint regions in one pooled owner per element width, as
    // direct native entry does. Owners stay live through every native or
    // materialized continuation; no pool borrow crosses the call.
    let n_locals = nc.cf.n_locals as usize;
    let cap = nc.cf.max_stack as usize + 1;
    let call_cap = (nc.cf.max_call_args as usize).max(1);
    // Small frames keep their buffers on this native frame (zeroed prefix
    // only); larger ones take pooled owners.
    const INLINE_WORDS: usize = 64;
    let words = n_locals + cap + call_cap;
    let halfs = cap + call_cap;
    let inline_bufs = words <= INLINE_WORDS;
    let mut inline_u64 = std::mem::MaybeUninit::<[u64; INLINE_WORDS]>::uninit();
    let mut inline_u32 = std::mem::MaybeUninit::<[u32; INLINE_WORDS]>::uninit();
    let mut u64_buf: Vec<u64> = if inline_bufs {
        Vec::new()
    } else {
        take_u64(words)
    };
    let mut u32_buf: Vec<u32> = if inline_bufs {
        Vec::new()
    } else {
        take_u32(halfs)
    };
    let (u64s, u32s): (&mut [u64], &mut [u32]) = if inline_bufs {
        // SAFETY: both prefixes fit the arrays and are zeroed before any
        // slice over them exists; the arrays outlive every use below.
        unsafe {
            // A fixed-size prefix zeroes with inline stores (no `memset`
            // call); larger frames finish the tail.
            const QUICK: usize = 16;
            let p = inline_u64.as_mut_ptr().cast::<u64>();
            p.cast::<[u64; QUICK]>().write([0; QUICK]);
            for k in QUICK..words {
                p.add(k).write(0);
            }
            let q = inline_u32.as_mut_ptr().cast::<u32>();
            q.cast::<[u32; QUICK]>().write([0; QUICK]);
            for k in QUICK..halfs {
                q.add(k).write(0);
            }
            (
                std::slice::from_raw_parts_mut(p, words),
                std::slice::from_raw_parts_mut(q, halfs),
            )
        }
    } else {
        (&mut u64_buf[..], &mut u32_buf[..])
    };
    let (locals_buf, rest) = u64s.split_at_mut(n_locals);
    let (spill, call_args) = rest.split_at_mut(cap);
    let (tags, call_tags) = u32s.split_at_mut(cap);
    let callee_key = Rc::as_ptr(&nc.code).cast::<CodeObject>();
    // The callee's context: this activation's cached child when it was
    // built for this very callee (same code, artifact tables and
    // namespaces — the child holds those handles, so pointer identity
    // is sound), else a fresh one. Reuse resets only the per-activation
    // state; the handles and resolved tables carry over (stale tables
    // re-resolve on demand, as for any long-lived activation).
    let reusable = ctx.child.as_deref().is_some_and(|c| {
        c.code_ptr == callee_key
            && StdRc::ptr_eq(&c.callees, &nc.callees)
            && StdRc::ptr_eq(&c.guard_snapshot, &nc.snap)
            && Rc::ptr_eq(&c.globals, &nc.func.globals)
            && Rc::ptr_eq(&c.builtins, &nc.func.builtins)
    });
    let mut child: Box<CallCtx> = if reusable {
        let mut c = ctx.child.take().expect("checked above");
        // (A previous call left `parked`/`raised` empty.)
        if !c.const_pins.is_empty() {
            c.const_pins.clear();
        }
        c.pin_pressure_exit = false;
        c.pin_limit = RUNTIME_PIN_SOFT_LIMIT;
        c.pins_counted = 0;
        c.last_ref_pins = 0;
        if !c.obj_global_pins.is_empty() {
            c.obj_global_pins.clear();
        }
        c.cf = StdRc::as_ptr(&nc.cf);
        c.dirty = false;
        c.interp_calls = 0;
        c.dyn_py_calls = 0;
        c.native_calls = 0;
        c.polls = 0;
        c
    } else {
        fresh_child(ctx, nc, callee_key)
    };
    // The callee's pins: the receiver, then the arguments' objects (the
    // reused child's drained table keeps its capacity).
    let pins = &mut child.pins;
    if !pins.is_empty() {
        pins.clear();
    }
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
        } else if tag == SlotTag::ListPin as u32 {
            match ctx.pins.get(bits as usize) {
                Some(Pin::List(l, e)) => {
                    let idx = pins.len() as u64;
                    pins.push(Pin::List(l.clone(), *e));
                    idx
                }
                // Unreachable per the validation pass.
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
        let d = &nc.func.defaults[nc.func.defaults.len() - (n_params - k)];
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
    child.entry_pin_count = child.pins.len();
    child.interp = ctx.interp;
    let nctx: &mut CallCtx = &mut child;
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
        ctx: std::ptr::from_mut::<CallCtx>(nctx).cast::<u8>(),
        call_args: call_args.as_mut_ptr(),
        call_tags: call_tags.as_mut_ptr(),
    };
    // SAFETY: the buffers are sized per the compiled frame's analysis
    // (the same invariants `enter_compiled` documents); the engine
    // backing `nc.cf` lives in this thread's `JIT` state for the
    // process lifetime; `nctx` outlives the call. The stack-growth
    // discipline mirrors `run_until_yield_or_return`: grow in segments
    // (probed every few nested levels — a native call level is far
    // smaller than the red zone divided by the probe interval), except
    // on a greenlet's dedicated (large, non-growable) stack.
    let status = if recursion_guard.depth() % 4 != 0
        || crate::stdlib::greenlet_native::on_greenlet_stack()
    {
        unsafe { nc.cf.enter(&raw mut njf) }
    } else {
        stacker::maybe_grow(512 * 1024, 8 * 1024 * 1024, || unsafe {
            nc.cf.enter(&raw mut njf)
        })
    };
    end_introspection(&child);

    // The common returns, from a callee that ran no Python (nothing needs
    // revalidating): a scalar in the expected lane, or `None` or an
    // instance on the object lane where the site takes `None` (the
    // procedure shape) or an object.
    if status == JitStatus::Returned && !child.dirty {
        let expect = SlotTag::from_raw(expect_tag);
        let ret = if njf.ret_tag == expect_tag
            && matches!(
                expect,
                SlotTag::Int | SlotTag::Float | SlotTag::Bool | SlotTag::None
            ) {
            Some(njf.ret_bits)
        } else if njf.ret_tag == SlotTag::ObjPin as u32 && njf.ret_bits == u64::MAX {
            // `None`.
            match expect {
                SlotTag::None => Some(0),
                SlotTag::ObjPin => Some(u64::MAX),
                _ => None,
            }
        } else if njf.ret_tag == SlotTag::ObjPin as u32 && expect == SlotTag::ObjPin {
            // An instance pins into this activation's table (a method
            // returning `self` finds its existing pin).
            match child.pins.get(njf.ret_bits as usize) {
                Some(Pin::Obj(v @ Object::Instance(_))) => {
                    pin_reusing(v, &mut ctx.pins, &mut ctx.pin_memo)
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(bits) = ret {
            note_callee_exit(&nc.art, &nc.code, &child);
            if !inline_bufs {
                put_u64(u64_buf);
                put_u32(u32_buf);
            }
            child.pins.clear();
            ctx.child = Some(child);
            jf.ret_bits = bits;
            jf.ret_tag = expect_tag;
            return Some(CallStatus::Ok as i64);
        }
    }
    // SAFETY: as above; the buffers are the ones the callee ran on.
    let status = unsafe {
        finish_native_call(
            jf,
            ctx,
            interp,
            nc,
            child,
            NativeExit {
                status,
                njf: &njf,
                locals: locals_buf,
                spill,
                tags,
                expect_tag,
            },
            recursion_guard,
        )
    };
    if !inline_bufs {
        put_u64(u64_buf);
        put_u32(u32_buf);
    }
    Some(status)
}

/// [`try_native_call`]'s under-arity check: every parameter from
/// `supplied` on must bind a default, and each must fit its compiled lane
/// (checked before any buffer is taken).
#[cold]
#[inline(never)]
fn native_defaults_fit(nc: &NativeCallee, supplied: usize, n_params: usize) -> bool {
    if nc.func.defaults_maybe_overridden() && nc.func.slot("__defaults__").is_some() {
        return false;
    }
    let first_default = n_params - nc.func.defaults.len().min(n_params);
    if supplied < first_default {
        // Unbindable — the interpreter path raises the faithful
        // TypeError.
        return false;
    }
    // List lanes stay interpreted: a mutable default whose *identity*
    // matters must go through the generic binder.
    (supplied..n_params).all(|k| {
        // Retained defaults can outnumber parameters after __code__
        // replacement. Match the parameter against the trailing suffix.
        let d = &nc.func.defaults[nc.func.defaults.len() - (n_params - k)];
        match nc.cf.local_types.get(k).copied().flatten() {
            None | Some(JitType::Obj) => true,
            Some(ty @ (JitType::Int | JitType::Bool | JitType::Float)) => pack(d, ty).is_some(),
            Some(JitType::Str) => matches!(d, Object::Str(_)),
            Some(JitType::Bytes) => matches!(d, Object::Bytes(_)),
            Some(JitType::Dict) => matches!(d, Object::Dict(_)),
            Some(_) => false,
        }
    })
}

/// [`try_native_call`] of a certified scalar leaf, on bounded stack
/// storage; `None` when the leaf declines (only pure numeric work ran).
///
/// # Safety
///
/// As [`enter_scalar_leaf`]; the caller holds the recursion tick.
#[inline(never)]
unsafe fn scalar_leaf_call(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    nc: &NativeCallee,
    argc: usize,
    expect_tag: u32,
) -> Option<i64> {
    // Grow before entering the separate function that owns the fixed
    // arrays, preserving the ordinary native call's stack discipline.
    native_stat(|s| s.scalar_leaf_calls.set(s.scalar_leaf_calls.get() + 1));
    // SAFETY: the caller's contract.
    let enter = || unsafe { enter_scalar_leaf(&nc.cf, &nc.func, &nc.code, jf, argc) };
    let result = if crate::stdlib::greenlet_native::on_greenlet_stack() {
        enter()
    } else {
        stacker::maybe_grow(512 * 1024, 8 * 1024 * 1024, enter)
    };
    match result {
        Some((bits, tag)) if tag == expect_tag => {
            jf.ret_bits = bits;
            jf.ret_tag = tag;
            Some(CallStatus::Ok as i64)
        }
        Some((bits, tag)) => {
            ctx.parked = Some(unpack(bits, tag));
            Some(CallStatus::Boxed as i64)
        }
        None => {
            native_stat(|s| s.deopts.set(s.deopts.get() + 1));
            None
        }
    }
}

/// A new context for a native callee this activation hasn't cached (see
/// [`try_native_call`]): the callee's handles and resolved tables, and
/// empty per-activation state.
#[cold]
#[inline(never)]
fn fresh_child(ctx: &CallCtx, nc: &NativeCallee, callee_key: *const CodeObject) -> Box<CallCtx> {
    // Self-recursion reuses this activation's own tables. An immutable
    // compilation with no call tokens needs no resolution, even when
    // another function compiles and advances the cache generation.
    // Nonempty tables retain the generation-checked cache lookup.
    let (native, method_native) = if callee_key == ctx.code_ptr {
        (ctx.native.clone(), ctx.method_native.clone())
    } else {
        (
            if nc.callees.is_empty() {
                None
            } else {
                resolved_native_table(callee_key)
            },
            if nc.methods.is_empty() {
                None
            } else {
                resolved_method_native_table(callee_key)
            },
        )
    };
    let table_gen = if callee_key == ctx.code_ptr {
        ctx.table_gen
    } else {
        current_compile_gen()
    };
    Box::new(CallCtx {
        interp: ctx.interp,
        callees: nc.callees.clone(),
        cf: StdRc::as_ptr(&nc.cf),
        guard_snapshot: nc.snap.clone(),
        globals: nc.func.globals.clone(),
        builtins: nc.func.builtins.clone(),
        // Native callees are cell-free by `py_callee_ok`.
        cells: crate::object::empty_cells(),
        parked: None,
        raised: None,
        const_pins: Vec::new(),
        pins: Vec::new(),
        entry_pin_count: 0,
        pin_pressure_exit: false,
        pin_limit: RUNTIME_PIN_SOFT_LIMIT,
        pins_counted: 0,
        last_ref_pins: 0,
        obj_globals: nc.obj_globals.clone(),
        obj_global_pins: Vec::new(),
        attr_guards: nc.attr_guards.clone(),
        methods: nc.methods.clone(),
        math: nc.math.clone(),
        dirty: false,
        interp_calls: 0,
        dyn_py_calls: 0,
        native_calls: 0,
        polls: 0,
        child: None,
        depth_cell: ctx.depth_cell,
        code_ptr: callee_key,
        native,
        method_native,
        table_gen,
        // The native call lanes push no interpreter frame for the
        // callee — keep it observable to callee-side stack walkers.
        frameless_code: Some(nc.code.clone()),
        dyn_callee: None,
        pin_memo: [u32::MAX; PIN_MEMO],
        introspected: Cell::new(false),
        inspected_locals: std::cell::RefCell::new(None),
        cell_list_pins: Vec::new(),
    })
}

/// How a native callee's activation ended, and the buffers it ran on.
struct NativeExit<'a> {
    status: JitStatus,
    njf: &'a JitFrame,
    locals: &'a [u64],
    spill: &'a [u64],
    tags: &'a [u32],
    expect_tag: u32,
}

/// [`try_native_call`] after the callee left native code any other way
/// than a clean scalar return: materialize a deopt or a raise, translate
/// an object result into this activation's lanes, revalidate this
/// activation's guards when Python ran, and put the callee's context back.
///
/// # Safety
///
/// Same contract as [`try_native_call`]; `child` is the context the callee
/// ran with and `exit` its frame and buffers.
#[inline(never)]
unsafe fn finish_native_call(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    nc: &NativeCallee,
    mut child: Box<CallCtx>,
    exit: NativeExit<'_>,
    recursion_guard: crate::recursion::Guard,
) -> i64 {
    let NativeExit {
        status,
        njf,
        expect_tag,
        ..
    } = exit;
    let nctx: &mut CallCtx = &mut child;

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
            if nctx.pin_pressure_exit {
                debug_assert_eq!(status, JitStatus::Deopt);
                JIT.with(|cell| cell.borrow_mut().stats.pin_pressure_exits += 1);
            } else {
                native_stat(|s| s.deopts.set(s.deopts.get() + 1));
                // The same deopt backoff the framed and frameless entries
                // charge: a callee that side-exits on most calls (a
                // burned-in attribute shape a second receiver class
                // misses) must retire, or every call keeps paying a
                // native entry plus a full frame materialization.
                if matches!(status, JitStatus::Deopt) {
                    let callee_key = Rc::as_ptr(&nc.code).cast::<CodeObject>();
                    JIT.with(|cell| {
                        let mut st = cell.borrow_mut();
                        if let Some(ce) = st.cache.get_mut(&callee_key) {
                            ce.deopts += 1;
                            if ce.deopts >= DEOPT_BUDGET {
                                ce.tier = Tier::NotJitable;
                                nc.code.jit_hint.mark_not_jitable();
                            }
                        }
                    });
                }
            }
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
                nctx,
                exit.locals,
                exit.spill,
                exit.tags,
                njf,
                pending,
            ) {
                Ok(v) => Done::Obj(v),
                Err(e) => Done::Raised(e),
            }
        }
    };
    note_callee_exit(&nc.art, &nc.code, nctx);
    // Reap the callee's pins (after every
    // pin-based rebuild above); a reap cascade runs Python, so it
    // dirties the call like any interpreter-path work.
    let child_dirty =
        nctx.dirty | (!nctx.pins.is_empty() && drain_activation_pins(interp, &mut nctx.pins));
    if nctx.parked.is_some() {
        nctx.parked = None;
    }
    if nctx.raised.is_some() {
        nctx.raised = None;
    }
    ctx.child = Some(child);
    ctx.dirty |= child_dirty;

    match done {
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
                return CallStatus::Ok as i64;
            }
            // RFC 0071 WS1 — an object-lane return pins into the
            // *caller's* table (`None` rides as the nullable `-1`).
            if guards_ok && expect_tag == SlotTag::ObjPin as u32 {
                return match obj_ret_bits(&v, &mut ctx.pins, &mut ctx.pin_memo) {
                    Some(bits) => {
                        jf.ret_bits = bits;
                        jf.ret_tag = expect_tag;
                        CallStatus::Ok as i64
                    }
                    None => {
                        ctx.parked = Some(v);
                        CallStatus::Boxed as i64
                    }
                };
            }
            let expect = match SlotTag::from_raw(expect_tag) {
                SlotTag::Int => JitType::Int,
                SlotTag::Float => JitType::Float,
                SlotTag::Bool => JitType::Bool,
                SlotTag::None
                | SlotTag::Boxed
                | SlotTag::ListPin
                | SlotTag::ObjPin
                | SlotTag::Default => JitType::Unknown,
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
    }
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
    // A native construction is a framed call plus allocation and
    // tracking: charged like any other heavy round-trip, and past the
    // budget left to the interpreter (see `charge_roundtrip`).
    #[allow(clippy::absurd_extreme_comparisons)] // budget currently 0 — see the constant
    if INTERP_CALL_RETIRE_BUDGET != 0
        && ctx.interp_calls.saturating_add(1) >= INTERP_CALL_RETIRE_BUDGET
    {
        return None;
    }
    ctx.interp_calls = ctx.interp_calls.saturating_add(1);
    // A constructor whose `__init__` is tiny is dominated by the call
    // and the allocation, which the interpreter's own `instantiate`
    // does with less ceremony: charge the poll's density judgment.
    if nc.code.instructions.len() <= TINY_CALLEE_OPS {
        ctx.dyn_py_calls = ctx.dyn_py_calls.saturating_add(1);
    }
    let (inst, ran_finalizers) = interp.alloc_plain_instance(cls)?;
    if ran_finalizers {
        // Threshold collection ran finalizers — arbitrary Python.
        ctx.dirty = true;
    }
    // Room for every field the class's instances set, so the compiled
    // `__init__` appends even its first one in line.
    if let (Object::Instance(i), Some(keys)) = (&inst, cls.shared_keys.get()) {
        let n = keys.len();
        if n > 0 && cls.native_kind.get() == 0 && i.dict.published().is_none() {
            // SAFETY: the instance is fresh and unshared; nothing else
            // borrows its values.
            if let Some(split) = unsafe { i.dict.split_cell().peek_mut() } {
                split.reserve_for(|| cls.shared_keys.share(), n);
            }
        }
    }
    // `__init__` is a procedure: its `None` rides the procedure lane.
    // A `try_native_call` rejection discards the fresh (empty, never
    // `__init__`-ed) instance and re-allocates on the interpreter path.
    // Python never saw it, so it dies without its finalizer.
    let Some(status) =
        (unsafe { try_native_call(jf, ctx, interp, nc, argc, SlotTag::None as u32, Some(&inst)) })
    else {
        if let Object::Instance(i) = &inst {
            i.finalize_ran.set(true);
        }
        return None;
    };
    if status == CallStatus::Raised as i64 {
        // A raising `__init__` discards the instance (CPython's
        // `type_call` propagates before returning it).
        return Some(status);
    }
    if status == CallStatus::Ok as i64 {
        // `__init__` completed and returned `None` with guards intact;
        // the call site's value is the fresh instance.
        if expect_tag == SlotTag::ObjPin as u32 {
            if let Some(bits) = obj_ret_bits(&inst, &mut ctx.pins, &mut ctx.pin_memo) {
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
        // Synthetic entry, only for the stack rebuild; `0` is never a
        // real compile id, so nothing can park against it.
        compile_id: 0,
    };
    finish_deopted(
        interp, &nc.code, &nc.func, &entry, nctx, locals_buf, spill, tags, njf, raised,
    )
}

/// [`finish_deopted_callee`] for an activation described by `entry`
/// (the callee's own tables), `code` and `func`.
#[allow(clippy::too_many_arguments)]
fn finish_deopted(
    interp: &mut super::Interpreter,
    code: &Rc<CodeObject>,
    func: &PyFunction,
    entry: &CompiledEntry,
    nctx: &mut CallCtx,
    locals_buf: &[u64],
    spill: &[u64],
    tags: &[u32],
    njf: &JitFrame,
    raised: Option<RuntimeError>,
) -> Result<Object, RuntimeError> {
    let n_real = code.varnames.len();
    let mut locals_v: Vec<Object> = Vec::with_capacity(n_real);
    // An inspected activation's shell holds its locals, plus whatever the
    // inspection wrote (see `sync_native_locals`).
    let inspected = nctx.inspected_locals.borrow_mut().take();
    if let Some(shell) = inspected.filter(|_| nctx.introspected.get()) {
        locals_v.extend(shell.borrow().iter().take(n_real).cloned());
        locals_v.resize(n_real, Object::Unbound);
    } else {
        for slot in 0..n_real {
            match entry.cf.local_types.get(slot).copied().flatten() {
                Some(ty) => locals_v.push(unpack_ty(
                    locals_buf.get(slot).copied().unwrap_or(0),
                    ty,
                    &nctx.pins,
                )),
                None => locals_v.push(Object::Unbound),
            }
        }
    }
    let mut frame = super::Frame {
        code: code.clone(),
        locals: Rc::new(GilRefCell::new(locals_v)),
        cells: crate::object::empty_cells(),
        stack: Vec::new(),
        globals: func.globals.clone(),
        builtins: func.builtins.clone(),
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
        sent_consumed: false,
        shell_cache: None,
        parked_native: None,
    };
    // A deopt-after-call carries the parked, already-computed result;
    // `rebuild_stack` places it at the exiting op's native depth.
    let parked = if raised.is_some() {
        None
    } else {
        nctx.parked.take()
    };
    rebuild_stack(
        interp, &mut frame, entry, locals_buf, spill, tags, njf, &nctx.pins, parked,
    );
    if raised.is_some() {
        // As though the raising CALL just executed: pc points past it
        // (`handle_exception` uses `pc - 1` as the raise site).
        frame.pc = njf.deopt_pc + 1;
    } else {
        frame.pc = njf.deopt_pc;
    }
    // The continuation owns every live pin-based value now. Keeping the
    // old table until it returns would retain obsolete native temporaries
    // throughout interpretation and any subsequent OSR activations.
    defer_activation_pins(&mut nctx.pins);
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
    let shell_locals = Rc::new(GilRefCell::new(Vec::new()));
    let shell = Rc::new(crate::object::FrameShell {
        code: crate::object::FrameSlot::new(code.clone()),
        locals: crate::object::FrameSlot::new(shell_locals.clone()),
        cells: crate::object::FrameSlot::new(ctx.cells.clone()),
        globals: crate::object::FrameSlot::new(ctx.globals.clone()),
        builtins: crate::object::FrameSlot::new(ctx.builtins.clone()),
        builtins_obj: None,
        class_namespace: None,
        class_namespace_obj: None,
        is_gen: false,
        gen_owner: GilRefCell::new(None),
        lasti: std::sync::atomic::AtomicU32::new(jf.deopt_pc),
        has_materialized: std::sync::atomic::AtomicBool::new(false),
        materialized: GilRefCell::new(None),
        tb_refs: std::sync::atomic::AtomicU32::new(0),
    });
    interp.frame_stack.borrow_mut().push(shell);
    // Inspection of the shell's frame finds this activation's locals.
    let out = with_native_frame(&shell_locals, jf, || f(interp));
    // The general pop: a traceback entry or a callee frame's `f_back` may
    // hold the shell lazily, and gets its frame object on the way out.
    interp.pop_frame_shell();
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

    // Defaulted parameters the site didn't pass: bound here, so every
    // lane below sees a full-arity call.
    // SAFETY: per the function contract.
    let (argc, bound) = unsafe { bind_call_defaults(jf, ctx, token, argc) };
    if !bound {
        // SAFETY: as above.
        return unsafe { call_py_with_gaps(jf, ctx, interp, token, argc, expect_tag) };
    }

    // A pure-leaf callee evaluates frameless (see `try_pure_leaf_call`),
    // unless its compiled scalar leaf runs it natively (cheaper still).
    let native_scalar = ctx
        .native
        .as_deref()
        .and_then(|t| t.get(token as usize))
        .and_then(Option::as_ref)
        .is_some_and(|nc| nc.ctor.is_none() && nc.cf.is_scalar_leaf());
    let maybe_pure = !native_scalar
        && ctx
            .callees
            .get(token as usize)
            .is_some_and(|(_, code)| code.jit_hint.pure_leaf() != Some(false));
    let callees = if maybe_pure {
        Some(StdRc::clone(&ctx.callees))
    } else {
        None
    };
    if let Some((Object::Function(f), code)) = callees.as_ref().and_then(|c| c.get(token as usize))
    {
        // SAFETY: GIL-serialized raw read of the function's code cell;
        // only compared.
        if std::ptr::eq(unsafe { Rc::as_ptr(&*f.code.as_ptr()) }, Rc::as_ptr(code)) {
            // SAFETY: `argc` marshaled entries are live (the function
            // contract).
            if let Some(status) =
                unsafe { try_pure_leaf_call(jf, ctx, interp, f, code, None, argc, expect_tag) }
            {
                return status;
            }
        }
    }

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
                    native_stat(|s| s.fallbacks.set(s.fallbacks.get() + 1));
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
            ctx.dyn_py_calls = ctx.dyn_py_calls.saturating_add(1);
            if charge_roundtrip(ctx) {
                ctx.parked = Some(v);
                return CallStatus::Boxed as i64;
            }
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
                    if let Some(bits) = obj_ret_bits(&v, &mut ctx.pins, &mut ctx.pin_memo) {
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
                    SlotTag::None
                    | SlotTag::Boxed
                    | SlotTag::ListPin
                    | SlotTag::ObjPin
                    | SlotTag::Default => JitType::Unknown,
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
            ctx.dyn_py_calls = ctx.dyn_py_calls.saturating_add(1);
            if charge_roundtrip(ctx) {
                // Call-shaped: hand the rest of this activation, and every
                // later one, to the interpreter (a `Boxed` exit spills the
                // result and resumes after the call).
                ctx.parked = Some(v);
                return CallStatus::Boxed as i64;
            }
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            );
            if still_valid {
                return deliver_call_result(jf, ctx, v, expect_tag);
            }
            ctx.parked = Some(v);
            CallStatus::Boxed as i64
        }
    }
}

/// Hand a completed call's result `v` to the compiled caller in its
/// `expect_tag` lane, or park it (`Boxed`: the caller deopts after the
/// call) when the lane cannot carry it.
fn deliver_call_result(jf: &mut JitFrame, ctx: &mut CallCtx, v: Object, expect_tag: u32) -> i64 {
    match SlotTag::from_raw(expect_tag) {
        // The procedure lane: nothing to write back, the compiled code
        // pushes no result.
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
        // RFC 0071 WS1 — an object-lane result pins into this
        // activation's table.
        SlotTag::ObjPin => {
            if let Some(bits) = obj_ret_bits(&v, &mut ctx.pins, &mut ctx.pin_memo) {
                jf.ret_bits = bits;
                jf.ret_tag = expect_tag;
                return CallStatus::Ok as i64;
            }
        }
        SlotTag::Boxed | SlotTag::ListPin | SlotTag::Default => {}
    }
    ctx.parked = Some(v);
    CallStatus::Boxed as i64
}

/// Evaluate a pure-leaf callee (see `code_is_pure_leaf`) frameless, as
/// the interpreter's core loop does: `recv` (a method's receiver) then
/// the `argc` marshaled arguments bind its parameters exactly. A native
/// activation would cost several times the body; `None` (nothing ran)
/// leaves the call to the ordinary paths.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]: `argc` marshal entries are live.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn try_pure_leaf_call(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &super::Interpreter,
    func: &crate::object::PyFunction,
    code: &CodeObject,
    recv: Option<*const Object>,
    argc: u32,
    expect_tag: u32,
) -> Option<i64> {
    // The common native callee is no pure leaf: one relaxed load decides.
    if code.jit_hint.pure_leaf() == Some(false)
        || code.arg_count != argc + u32::from(recv.is_some())
    {
        return None;
    }
    // SAFETY: the caller's contract.
    unsafe { pure_leaf_call(jf, ctx, interp, func, code, recv, argc, expect_tag) }
}

/// [`try_pure_leaf_call`]'s evaluation, out of line (its argument
/// buffers would otherwise widen every native call helper's frame).
///
/// # Safety
///
/// As [`try_pure_leaf_call`].
#[inline(never)]
#[allow(clippy::too_many_arguments)]
unsafe fn pure_leaf_call(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &super::Interpreter,
    func: &crate::object::PyFunction,
    code: &CodeObject,
    recv: Option<*const Object>,
    argc: u32,
    expect_tag: u32,
) -> Option<i64> {
    const MAX: usize = 8;
    let offset = usize::from(recv.is_some());
    let n = argc as usize + offset;
    if n > MAX
        || code.arg_count as usize != n
        || !code
            .jit_hint
            .pure_leaf()
            .unwrap_or_else(|| crate::code_is_pure_leaf_pub(code))
        || crate::hot_gates::load() != 0
        || crate::trace::any_observers_active()
    {
        return None;
    }
    // Only the marshaled arguments need owned values (a scalar lane
    // becomes its `Object`, a pin names the pinned one); the receiver is
    // borrowed where it lives.
    let mut owned = [const { std::mem::MaybeUninit::<Object>::uninit() }; MAX];
    /// Drops the first `.1` values at `.0` (the written arguments).
    struct Owned(*mut Object, usize);
    impl Drop for Owned {
        fn drop(&mut self) {
            for j in 0..self.1 {
                // SAFETY: the first `self.1` values were written below.
                unsafe { std::ptr::drop_in_place(self.0.add(j)) };
            }
        }
    }
    let mut written = Owned(owned.as_mut_ptr().cast::<Object>(), 0);
    let mut ptrs: [*const Object; MAX] = [std::ptr::null(); MAX];
    if let Some(r) = recv {
        ptrs[0] = r;
    }
    for j in 0..argc as usize {
        // SAFETY: the caller's contract — `argc` marshaled entries.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        // SAFETY: `j < argc <= MAX`; the slot is uninitialized until now.
        unsafe { written.0.add(j).write(unpack_pins(bits, tag, &ctx.pins)) };
        written.1 = j + 1;
        // SAFETY: as above.
        ptrs[offset + j] = unsafe { written.0.add(j) };
    }
    let v = interp.pure_leaf_eval::<false, false>(code, func, &ptrs[..n])?;
    // A call served without an interpreter frame, like a native one.
    native_stat(|s| s.calls.set(s.calls.get() + 1));
    Some(deliver_call_result(jf, ctx, v, expect_tag))
}

/// The direct self-call enter helper (see `weavepy_jit::SelfEnterHelper`):
/// the per-call work `try_native_call` does for a callee, reduced to what
/// a pin-free activation of the caller's own code needs — a GIL
/// checkpoint, the observer gate, and the recursion tick (checked
/// against the limit, and against the native stack's headroom every few
/// levels: the ordinary path grows the stack, this one cannot).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_self_enter(frame: *mut JitFrame) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` that entered native code is dormant
    // while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    interp.gil_countdown = interp.gil_countdown.wrapping_sub(1);
    if interp.gil_countdown == 0 {
        interp.gil_countdown = crate::gil::GIL_CHECK_INTERVAL;
        crate::gil::yield_checkpoint();
    }
    if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
        return 1;
    }
    // SAFETY: this thread's own depth cell (see `CallCtx::depth_cell`).
    let depth = unsafe { &*ctx.depth_cell };
    let n = depth.get() + 1;
    if n > crate::recursion::recursion_limit()
        || (n % 8 == 0 && stacker::remaining_stack().is_some_and(|r| r < 256 * 1024))
    {
        return 1;
    }
    depth.set(n);
    native_stat(|s| s.calls.set(s.calls.get() + 1));
    0
}

/// The direct method-call enter helper (see
/// `weavepy_jit::MethodEnterHelper`): [`wpjit_self_enter`]'s charge for a
/// call of the compiled body of method token `token`, after the GIL
/// checkpoint and the gates, once the receiver pin passes the guard
/// [`wpjit_call_method`] applies (the class version, no instance attribute
/// shadowing the name, the function still wearing the burned `__code__`).
/// Anything else declines with nothing charged, and the call takes
/// [`wpjit_call_method`], which resolves it exactly.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_method_enter(frame: *mut JitFrame, pin: i64, token: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` that entered native code is dormant
    // while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // The checkpoint first: another thread may rebind the method while
    // this one waits for the GIL.
    interp.gil_countdown = interp.gil_countdown.wrapping_sub(1);
    if interp.gil_countdown == 0 {
        interp.gil_countdown = crate::gil::GIL_CHECK_INTERVAL;
        crate::gil::yield_checkpoint();
    }
    if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
        return 1;
    }
    let Some(entry) = usize::try_from(token).ok().and_then(|t| ctx.methods.get(t)) else {
        return 1;
    };
    let MethodCallee::Py { func, code } = &entry.callee else {
        return 1;
    };
    let Some(Pin::Obj(Object::Instance(inst))) =
        usize::try_from(pin).ok().and_then(|p| ctx.pins.get(p))
    else {
        return 1;
    };
    // SAFETY: a read between two native ops; nothing here runs code (see
    // `GilCell::peek`).
    let same_code = match unsafe { func.code.peek() } {
        Some(c) => Rc::ptr_eq(c, code),
        None => Rc::ptr_eq(&func.code.borrow(), code),
    };
    if !same_code || !method_guard_ok(entry, inst) {
        return 1;
    }
    // Compiled code checks the next calls in line (see `arm_method_guard`).
    if !crate::gil::free_threading_enabled() {
        arm_method_guard(entry, inst.cls_raw());
    }
    // SAFETY: this thread's own depth cell (see `CallCtx::depth_cell`).
    let depth = unsafe { &*ctx.depth_cell };
    let n = depth.get() + 1;
    if n > crate::recursion::recursion_limit()
        || (n % 8 == 0 && stacker::remaining_stack().is_some_and(|r| r < 256 * 1024))
    {
        return 1;
    }
    depth.set(n);
    native_stat(|s| s.calls.set(s.calls.get() + 1));
    0
}

/// Release [`wpjit_self_enter`]'s recursion tick.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_self_exit(frame: *mut JitFrame) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &*frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &*jf.ctx.cast::<CallCtx>() };
    // SAFETY: as in `wpjit_self_enter`.
    let depth = unsafe { &*ctx.depth_cell };
    depth.set(depth.get().saturating_sub(1));
    0
}

/// Finish a direct self call whose callee did not return (see
/// `weavepy_jit::SelfSlowHelper`): exactly `try_native_call`'s deopt and
/// raise handling, the callee's frame and buffers being the ones on the
/// caller's native stack and its context the caller's own (a pin-free
/// activation leaves the shared table empty).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]; `callee` is the live callee frame.
unsafe extern "C" fn wpjit_self_slow(
    frame: *mut JitFrame,
    callee: *mut JitFrame,
    status: i64,
    token: i64,
    expect_tag: i64,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: as in `wpjit_self_enter`.
    let interp = unsafe { &mut *ctx.interp };
    // SAFETY: the caller's contract.
    let cjf = unsafe { &*callee };
    let raised = (status == JitStatus::Raised as i64).then(|| {
        ctx.raised.take().unwrap_or_else(|| {
            RuntimeError::Internal("JIT Raised exit without a parked exception".to_owned())
        })
    });
    let Some((Object::Function(pf), code)) = ctx.callees.get(token as usize).cloned() else {
        // Unreachable: the lowering only emits direct calls for tokens
        // naming this very function.
        ctx.raised = Some(raised.unwrap_or_else(|| {
            RuntimeError::Internal("direct self call without its callee".to_owned())
        }));
        return CallStatus::Raised as i64;
    };
    if status == JitStatus::Deopt as i64 {
        native_stat(|s| s.deopts.set(s.deopts.get() + 1));
        let key = Rc::as_ptr(&code).cast::<CodeObject>();
        JIT.with(|cell| {
            if let Some(ce) = cell.borrow_mut().cache.get_mut(&key) {
                ce.deopts += 1;
                if ce.deopts >= DEOPT_BUDGET {
                    ce.tier = Tier::NotJitable;
                    code.jit_hint.mark_not_jitable();
                }
            }
        });
    }
    // The callee runs the caller's own compilation, whose layout the
    // shared context holds (the tier cache may have just retired it).
    // SAFETY: `ctx.cf` came from a live `StdRc` its entry still owns.
    let cf = unsafe {
        StdRc::increment_strong_count(ctx.cf);
        StdRc::from_raw(ctx.cf)
    };
    let entry = CompiledEntry {
        cf,
        guard_snapshot: ctx.guard_snapshot.clone(),
        callees: ctx.callees.clone(),
        obj_globals: ctx.obj_globals.clone(),
        attr_guards: ctx.attr_guards.clone(),
        methods: ctx.methods.clone(),
        math: ctx.math.clone(),
        native: None,
        method_native: None,
        compile_id: 0,
    };
    ctx.dirty = true;
    // SAFETY: the callee frame's buffers are live on the caller's stack,
    // sized by its own compiled frame.
    let (locals, spill, tags) = unsafe {
        (
            std::slice::from_raw_parts(cjf.locals, cjf.n_locals as usize),
            std::slice::from_raw_parts(cjf.stack_spill, cjf.stack_cap as usize),
            std::slice::from_raw_parts(cjf.stack_tags, cjf.stack_cap as usize),
        )
    };
    match finish_deopted(
        interp, &code, &pf, &entry, ctx, locals, spill, tags, cjf, raised,
    ) {
        Err(e) => {
            ctx.raised = Some(e);
            CallStatus::Raised as i64
        }
        Ok(v) => {
            // Python ran for the continuation: the caller's burned-in
            // resolutions must still hold for it to continue natively.
            if !guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            ) {
                ctx.parked = Some(v);
                return CallStatus::Boxed as i64;
            }
            deliver_call_result(jf, ctx, v, expect_tag as u32)
        }
    }
}

/// The generic-call backoff for native-to-native and frameless direct
/// entries (the framed entries' twin lives in [`note_native_exit`]): a
/// compiled callee whose
/// activations average [`CALLEE_ROUNDTRIP_RETIRE_RATIO`] or more
/// interpreter calls is a thin native driver around them. Each such call pays pin
/// traffic, an activation shell and a generic call that the interpreter's
/// inline call path avoids, so the callee retires to tier-1.
#[inline(always)]
fn note_callee_exit(art: &Artifacts, code: &Rc<CodeObject>, child: &CallCtx) {
    let entries = art.callee_entries.get().saturating_add(1);
    art.callee_entries.set(entries);
    if child.dyn_py_calls != 0 {
        note_callee_roundtrips(art, code, child, entries);
    }
}

/// [`note_callee_exit`] for an activation that made interpreter
/// round-trips, out of line.
#[cold]
#[inline(never)]
fn note_callee_roundtrips(art: &Artifacts, code: &Rc<CodeObject>, child: &CallCtx, entries: u32) {
    let trips = art
        .callee_roundtrips
        .get()
        .saturating_add(child.dyn_py_calls);
    art.callee_roundtrips.set(trips);
    if entries >= GENERIC_RETIRE_MIN_ENTRIES
        && trips / entries >= CALLEE_ROUNDTRIP_RETIRE_RATIO
        && !code.jit_hint.is_not_jitable()
    {
        let key = Rc::as_ptr(code).cast::<CodeObject>();
        JIT.with(|cell| {
            let mut st = cell.borrow_mut();
            if let Some(ce) = st.cache.get_mut(&key) {
                ce.tier = Tier::NotJitable;
            }
            st.stats.generic_retires += 1;
        });
        code.jit_hint.mark_not_jitable();
    }
}

/// Charge one expensive round-trip (an interpreter call, a generic
/// attribute access, or a heavy native-to-native call) to the running
/// activation. Returns true once the activation has spent
/// [`INTERP_CALL_RETIRE_BUDGET`]: the code object is retired to tier-1
/// (its `jit_hint` fast-out gates every later entry and back edge) and
/// the caller should deopt the activation into the interpreter.
/// A generic call of a *native* callee (a builtin function or a bound
/// builtin method) from compiled code: the interpreter runs the same
/// call through its leaf paths without the activation shell, guard
/// re-validation and pin traffic, so an activation making many of them
/// is a net loss. Past the budget the code retires to the interpreter
/// (which takes over right after this call, through the `Boxed` exit).
fn charge_native_roundtrip(ctx: &mut CallCtx) -> bool {
    ctx.native_calls = ctx.native_calls.saturating_add(1);
    // The density check runs at each loop poll (see `wpjit_poll`); this
    // cap only catches calls piling up between polls.
    if ctx.native_calls < NATIVE_CALL_RETIRE_BUDGET || ctx.code_ptr.is_null() {
        return false;
    }
    retire_native_driver(ctx);
    true
}

/// Retire the activation's code as a thin native driver around native
/// callees (the interpreter's leaf calls serve such a loop better).
fn retire_native_driver(ctx: &CallCtx) {
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        if let Some(ce) = st.cache.get_mut(&ctx.code_ptr) {
            if !matches!(ce.tier, Tier::NotJitable) {
                ce.tier = Tier::NotJitable;
                st.stats.generic_retires += 1;
            }
        }
    });
    // SAFETY: the activation's code object outlives the activation (see
    // the field docs).
    unsafe { (*ctx.code_ptr).jit_hint.mark_not_jitable() };
}

/// Recompiles one code object may take after cold exits (see
/// `CacheEntry::recompile_at_osr`): one per later loop is the common
/// shape (a function running several loops over containers it builds in
/// turn), and the budget bounds compile cost for anything else.
const COLD_RECOMPILE_BUDGET: u8 = 3;

/// Generic native-callee calls one activation may make between two
/// loop polls before its code retires (see [`charge_native_roundtrip`]).
const NATIVE_CALL_RETIRE_BUDGET: u32 = 4096;

/// `str`-method calls one activation may make before its code retires.
/// Lower than the generic budget: tier-1 calls the very same bodies
/// through its leaf table with no argument marshaling, and a body that
/// side-exits every few hundred iterations never reaches a poll for the
/// density check to judge it.
const STR_METHOD_RETIRE_BUDGET: u32 = 256;
/// Generic native-callee calls one poll interval (`JIT_POLL_STRIDE`
/// loop-header iterations) may make: more than one per four native
/// iterations and the loop is a driver around its calls.
const NATIVE_CALLS_PER_POLL: u32 = (weavepy_jit::JIT_POLL_STRIDE / 4) as u32;

/// A compiled callee this size or smaller is served better by the
/// interpreter's inline activation than by a framed native call: its
/// body is a getter, a comparison or a constant return, and the call's
/// own buffers, pins, context and recursion tick dominate it.
const TINY_CALLEE_OPS: usize = 12;

/// Generic *interpreter* calls one poll interval may make before the
/// same judgment applies (see [`wpjit_poll`]). A call every fourth
/// native iteration is the break-even measured against the
/// interpreter's inline-activation call path.
const INTERP_CALLS_PER_POLL: u32 = (weavepy_jit::JIT_POLL_STRIDE / 4) as u32;

fn charge_roundtrip(ctx: &mut CallCtx) -> bool {
    ctx.interp_calls = ctx.interp_calls.saturating_add(1);
    // Measured (attr_access): a native loop that calls interpreted
    // methods still beats interpreting the loop — the round trip is no
    // dearer than the interpreter's own call — so a plain interpreter
    // call never retires the caller; only the deopt backoff does.
    #[allow(clippy::absurd_extreme_comparisons)] // budget currently 0 — see the constant
    if INTERP_CALL_RETIRE_BUDGET == 0
        || ctx.interp_calls < INTERP_CALL_RETIRE_BUDGET
        || ctx.code_ptr.is_null()
    {
        return false;
    }
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        if let Some(ce) = st.cache.get_mut(&ctx.code_ptr) {
            if !matches!(ce.tier, Tier::NotJitable) {
                ce.tier = Tier::NotJitable;
                st.stats.generic_retires += 1;
            }
        }
    });
    // SAFETY: the activation's code object outlives the activation (see
    // the field docs).
    unsafe { (*ctx.code_ptr).jit_hint.mark_not_jitable() };
    true
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
    // A native site's deferred-guard miss (see `wpjit_call_native_method`)
    // takes the surprise-receiver lane below.
    let token = token & !(weavepy_jit::METHOD_GUARD_AT_CALL | weavepy_jit::METHOD_NATIVE);
    native_stat(|s| s.method_calls.set(s.method_calls.get() + 1));
    let guard_miss = || {
        native_stat(|s| s.method_guard_misses.set(s.method_guard_misses.get() + 1));
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
            let probe = crate::object::StrKeyHashed {
                s: &entry.name,
                hash: entry.name_hash,
            };
            // SAFETY: a read between two native ops; nothing here runs
            // code (see `GilCell::peek`).
            attr_class_ok(inst, entry.ver)
                && unsafe { inst.attr_peek_has(probe.s, probe.hash) } == Some(false)
                && match &entry.callee {
                    MethodCallee::Py { func, code: burned } => match unsafe { func.code.peek() } {
                        Some(code) => Rc::ptr_eq(code, burned),
                        None => Rc::ptr_eq(&func.code.borrow(), burned),
                    },
                    // Only a native site's deferred-guard miss gets here.
                    MethodCallee::Native { .. } => false,
                }
        }
        _ => false,
    };
    if !guard_ok {
        // RFC 0074 — surprise-receiver lane: the burned resolution
        // doesn't apply (different class, shadowed name, swapped
        // `__code__`, or a non-instance receiver). Resolve and call
        // generically instead of deopting: a missing attribute raises
        // here exactly as the interpreter's `LOAD_ATTR` would.
        native_stat(|s| s.method_guard_misses.set(s.method_guard_misses.get() + 1));
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
    let MethodCallee::Py { func, code } = &entry.callee else {
        unreachable!("native methods returned above");
    };
    // A receiver with more values than the shadow bound allows misses the
    // in-line guard: once the class's names cover them, the bound grows
    // (see `arm_method_guard`).
    if entry.guarded_in_line.get() {
        if let Object::Instance(inst) = &recv {
            if !crate::gil::free_threading_enabled() {
                arm_method_guard(entry, inst.cls_raw());
            }
        }
    }

    // A pure-leaf method (a getter, a predicate) evaluates frameless.
    // SAFETY: `argc` marshaled entries are live (the function contract),
    // and `recv` outlives the evaluation.
    if let Some(status) = unsafe {
        try_pure_leaf_call(
            jf,
            ctx,
            interp,
            func,
            code,
            Some(&raw const recv),
            argc,
            expect_tag,
        )
    } {
        return status;
    }

    // A callback-free field update (`self.n += k; return self.n`) bound
    // exactly: the guards above and the update's own checks are all the
    // native activation would validate for it.
    if let Some(nc) = ctx
        .method_native
        .as_deref()
        .and_then(|t| t.get(token as usize))
        .and_then(Option::as_ref)
    {
        if nc.scalar_update.is_some()
            && code.arg_count == argc + 1
            && Rc::ptr_eq(&nc.func, func)
            && Rc::ptr_eq(&nc.code, code)
        {
            // SAFETY: the method guard above pinned the binding; the update
            // checks its receiver, argument lane, and observers itself.
            if let Some(value) =
                unsafe { native_scalar_field_update(jf, ctx, nc, &recv, argc as usize) }
            {
                // Compiled code runs the next ones in line.
                arm_update(entry, nc, &recv, argc);
                if expect_tag == SlotTag::Int as u32 {
                    jf.ret_bits = value as u64;
                    jf.ret_tag = SlotTag::Int as u32;
                    return CallStatus::Ok as i64;
                }
                // The store is complete: never repeat it.
                #[cfg(test)]
                crate::SCALAR_FIELD_UPDATE_BOXED_RETURNS.with(|hits| hits.set(hits.get() + 1));
                ctx.parked = Some(Object::Int(value));
                return CallStatus::Boxed as i64;
            }
        }
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
            if Rc::ptr_eq(&nc.func, func) && Rc::ptr_eq(&nc.code, code) {
                // SAFETY: `jf`/`ctx` are this activation's live buffers
                // (see the function contract) and `nc` came from this
                // thread's tier cache via the activation's resolved table.
                match unsafe { try_native_call(jf, ctx, interp, nc, argc, expect_tag, Some(&recv)) }
                {
                    Some(status) => return status,
                    None => {
                        native_stat(|s| s.fallbacks.set(s.fallbacks.get() + 1));
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
    let callee = Object::Function(func.clone());
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

/// Deliver a native method's or subscript's completed result `v` to
/// the compiled caller on its `expect_tag` lane: `None` for the
/// procedure lane, an unboxed scalar, or an object-lane pin (`None`
/// rides the nullable `-1`, so a procedure's result costs no pin). A
/// value the lane can't carry, or pin pressure, parks `v` (`Boxed`: the
/// caller resumes after the operation, which never runs again).
#[inline(always)]
fn deliver_native_result(jf: &mut JitFrame, ctx: &mut CallCtx, v: Object, expect_tag: u32) -> i64 {
    // The scalar arms own nothing, so they skip `v`'s drop glue.
    match (SlotTag::from_raw(expect_tag), &v) {
        (SlotTag::None, Object::None) => {
            std::mem::forget(v);
            return CallStatus::Ok as i64;
        }
        (SlotTag::Int, &Object::Int(value)) => {
            std::mem::forget(v);
            jf.ret_bits = value as u64;
            jf.ret_tag = expect_tag;
            return CallStatus::Ok as i64;
        }
        (SlotTag::Float, &Object::Float(value)) => {
            std::mem::forget(v);
            jf.ret_bits = value.to_bits();
            jf.ret_tag = expect_tag;
            return CallStatus::Ok as i64;
        }
        (SlotTag::ObjPin, Object::None) => {
            std::mem::forget(v);
            jf.ret_bits = u64::MAX;
            jf.ret_tag = expect_tag;
            return CallStatus::Ok as i64;
        }
        (SlotTag::ObjPin, _) => {
            if ctx.temporary_pin_limit_reached() {
                ctx.pin_pressure_exit = true;
            } else if ctx.pins.len() < RUNTIME_PIN_CAP {
                jf.ret_bits = ctx.pins.len() as u64;
                jf.ret_tag = expect_tag;
                ctx.pins.push(Pin::Obj(v));
                return CallStatus::Ok as i64;
            }
        }
        _ => {}
    }
    ctx.parked = Some(v);
    CallStatus::Boxed as i64
}

/// A bitwise view of one marshaled argument: scalars rebuild by value
/// and an object pin aliases its pinned object without taking a
/// reference (the pin owns it for the whole activation). The view must
/// never be dropped. `None` for a lane it can't alias (a list pin).
#[inline(always)]
fn arg_view(bits: u64, tag: u32, pins: &PinTable) -> Option<Object> {
    Some(match SlotTag::from_raw(tag) {
        SlotTag::Int => Object::Int(bits as i64),
        SlotTag::Float => Object::Float(f64::from_bits(bits)),
        SlotTag::Bool => Object::Bool(bits != 0),
        SlotTag::ObjPin if bits == u64::MAX => Object::None,
        SlotTag::ObjPin => match pins.get(bits as usize)? {
            // SAFETY: a bitwise alias, never dropped (see above).
            Pin::Obj(o) => unsafe { std::ptr::read(o) },
            Pin::List(..) => return None,
        },
        _ => return None,
    })
}

/// A native leaf body's direct call on bitwise views of the receiver pin
/// and the marshaled arguments (see [`arg_view`]): no references are
/// taken or released around the call, which runs no Python code. `None`
/// (nothing ran) when an argument has no view, the arity exceeds the
/// inline buffer, or the fast half declines.
///
/// # Safety
///
/// `argc` marshal entries are initialized (the helper contract).
#[inline(always)]
unsafe fn direct_native_call(
    jf: &JitFrame,
    ctx: &CallCtx,
    builtin: &crate::object::BuiltinFn,
    fast: Option<crate::leaf_builtins::Fast>,
    recv_pin: i64,
    argc: u32,
) -> Option<Result<Object, RuntimeError>> {
    const N: usize = 4;
    let n = argc as usize + 1;
    if n > N {
        return None;
    }
    let mut views = [const { std::mem::MaybeUninit::<Object>::uninit() }; N];
    match ctx.pins.get(usize::try_from(recv_pin).ok()?)? {
        // SAFETY: a bitwise alias, never dropped (the pin owns it).
        Pin::Obj(o) => views[0].write(unsafe { std::ptr::read(o) }),
        Pin::List(..) => return None,
    };
    for j in 0..argc as usize {
        // SAFETY: native code wrote `argc` entries.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        views[j + 1].write(arg_view(bits, tag, &ctx.pins)?);
    }
    // SAFETY: the first `n` entries were written; they are views, so the
    // `MaybeUninit` array never drops them.
    let args = unsafe { std::slice::from_raw_parts(views.as_ptr().cast::<Object>(), n) };
    match fast {
        Some(fast) => fast(args),
        None => Some(match builtin.call_kw.as_ref() {
            Some(ckw) => ckw(args, &[]),
            None => (builtin.call)(args),
        }),
    }
}

/// A native method's direct operation (`collections_native::fast_op`) on
/// the receiver pin and at most one marshaled argument, viewed in place
/// (see [`arg_view`]). `None` (nothing ran) sends the call to the full
/// body.
///
/// # Safety
///
/// `argc` marshal entries are initialized (the helper contract).
#[inline(always)]
unsafe fn direct_native_op(
    jf: &JitFrame,
    ctx: &CallCtx,
    op: u8,
    recv_pin: i64,
    argc: u32,
) -> Option<Object> {
    let Pin::Obj(recv) = ctx.pins.get(usize::try_from(recv_pin).ok()?)? else {
        return None;
    };
    match argc {
        0 => crate::stdlib::collections_native::fast_op(op, recv, None),
        1 => {
            // SAFETY: native code wrote one entry.
            let (bits, tag) = unsafe { (*jf.call_args, *jf.call_tags) };
            let arg = std::mem::ManuallyDrop::new(arg_view(bits, tag, &ctx.pins)?);
            crate::stdlib::collections_native::fast_op(op, recv, Some(&arg))
        }
        _ => None,
    }
}

/// A burned-in native leaf method (see [`MethodCallee::Native`]),
/// called directly on the receiver bound at its load and the marshaled
/// arguments. No Python runs on the direct path, so the activation stays
/// clean and no round trip is charged.
///
/// A fast half that declines (an argument whose conversion could run
/// Python), or an active profiler or tracer that must see the call,
/// needs the interpreter, and Python must see this activation's locals,
/// which live lane-packed in the native frame. While the receiver still
/// resolves the name to this builtin, the call rejects, and the
/// interpreter performs it (and the attribute load, which binds the same
/// builtin) on a materialized frame. A class rebound by the argument
/// evaluation since the load calls the builtin bound there through the
/// interpreter's profiled call path instead.
///
/// # Safety
///
/// Same contract as [`wpjit_call_method`]; `entry` lives in the
/// activation's method table.
unsafe fn call_native_method(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    entry: &MethodEntry,
    recv_pin: i64,
    argc: u32,
    expect_tag: u32,
) -> i64 {
    let MethodCallee::Native { op, .. } = &entry.callee else {
        return CallStatus::Reject as i64;
    };
    if *op != 0 && !crate::trace::any_observers_active() {
        // SAFETY: per the function contract.
        if let Some(v) = unsafe { direct_native_op(jf, ctx, *op, recv_pin, argc) } {
            return deliver_native_result(jf, ctx, v, expect_tag);
        }
    }
    // SAFETY: per the function contract.
    unsafe { call_native_body(jf, ctx, interp, entry, recv_pin, argc, expect_tag) }
}

/// [`call_native_method`] past its direct operation: the builtin's body
/// (or fast half) on views of the operands, else the interpreter.
///
/// # Safety
///
/// Same contract as [`call_native_method`].
#[cold]
#[inline(never)]
unsafe fn call_native_body(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    entry: &MethodEntry,
    recv_pin: i64,
    argc: u32,
    expect_tag: u32,
) -> i64 {
    let MethodCallee::Native { builtin, fast, .. } = &entry.callee else {
        return CallStatus::Reject as i64;
    };
    if !crate::trace::any_observers_active() {
        // SAFETY: per the function contract.
        match unsafe { direct_native_call(jf, ctx, builtin, *fast, recv_pin, argc) } {
            Some(Ok(v)) => return deliver_native_result(jf, ctx, v, expect_tag),
            Some(Err(err)) => {
                ctx.raised = Some(err);
                return CallStatus::Raised as i64;
            }
            None => {}
        }
    }
    let Some(recv) = ctx.pins.get(recv_pin as usize).map(Pin::to_object) else {
        return CallStatus::Reject as i64;
    };
    if matches!(&recv, Object::Instance(inst) if method_guard_ok(entry, inst)) {
        return CallStatus::Reject as i64;
    }
    let mut args: Vec<Object> = Vec::with_capacity(argc as usize);
    for j in 0..argc as usize {
        // SAFETY: native code wrote `argc` entries, and the buffers are
        // `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        args.push(unpack_pins(bits, tag, &ctx.pins));
    }
    ctx.dirty = true;
    let bound = Object::BoundMethod(Rc::new(crate::object::BoundMethod::new(
        recv,
        Object::Builtin(builtin.clone()),
    )));
    let res = call_with_activation_shell(interp, ctx, jf, |i| {
        i.call_c_profiled(&bound, &args, &[], &ctx.globals)
    });
    finish_interp_call(jf, ctx, interp, res, expect_tag)
}

/// The `wpjit_call_native_method` helper: [`wpjit_call_method`] for a
/// site that resolved a native leaf method (`METHOD_NATIVE` in the
/// token), kept apart so the common call pays none of the Python path's
/// setup. The method was guarded and bound at its load
/// (`TOp::GuardMethod`), or, when nothing ran in between, is guarded
/// here (`METHOD_GUARD_AT_CALL`): call the builtin bound there. A
/// deferred guard's miss is exactly the load the interpreter would
/// perform now, so it takes the generic helper's surprise-receiver lane.
///
/// # Safety
///
/// Same contract as [`wpjit_call_method`].
unsafe extern "C" fn wpjit_call_native_method(
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
    let guard_at_call = token & weavepy_jit::METHOD_GUARD_AT_CALL != 0;
    let index =
        (token & !(weavepy_jit::METHOD_GUARD_AT_CALL | weavepy_jit::METHOD_NATIVE)) as usize;
    // The common case in one pass: the entry's direct operation on the
    // receiver pin (read once) and at most one argument view.
    if let Some(
        entry @ MethodEntry {
            callee: MethodCallee::Native { op, .. },
            ..
        },
    ) = ctx.methods.get(index)
    {
        if let Some(Pin::Obj(recv)) = ctx.pins.get(recv_pin as usize) {
            let bound = !guard_at_call
                || matches!(recv, Object::Instance(inst) if method_guard_ok(entry, inst));
            if bound && *op != 0 && argc <= 1 && !crate::trace::any_observers_active() {
                let arg = if argc == 1 {
                    // SAFETY: native code wrote one entry.
                    let (bits, tag) = unsafe { (*jf.call_args, *jf.call_tags) };
                    arg_view(bits, tag, &ctx.pins).map(std::mem::ManuallyDrop::new)
                } else {
                    None
                };
                if *op == crate::stdlib::random_core::OP_RANDOM {
                    if argc == 0 {
                        if let Some(v) = crate::stdlib::random_core::random_fast(recv) {
                            return deliver_native_result(jf, ctx, v, expect_tag);
                        }
                    }
                } else if argc == 0 || arg.is_some() {
                    let arg = arg.as_deref();
                    if let Some(v) = crate::stdlib::collections_native::fast_op(*op, recv, arg) {
                        return deliver_native_result(jf, ctx, v, expect_tag);
                    }
                }
            }
        }
    }
    // SAFETY: per the function contract.
    unsafe { call_native_method_slow(frame, token, recv_pin, argc, expect_tag) }
}

/// [`wpjit_call_native_method`] past its direct operation (see there).
///
/// # Safety
///
/// Same contract as [`wpjit_call_method`].
#[cold]
#[inline(never)]
unsafe fn call_native_method_slow(
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
    let guard_at_call = token & weavepy_jit::METHOD_GUARD_AT_CALL != 0;
    let index =
        (token & !(weavepy_jit::METHOD_GUARD_AT_CALL | weavepy_jit::METHOD_NATIVE)) as usize;
    if let Some(
        entry @ MethodEntry {
            callee: MethodCallee::Native { .. },
            ..
        },
    ) = ctx.methods.get(index)
    {
        let bound = !guard_at_call
            || matches!(
                ctx.pins.get(recv_pin as usize),
                Some(Pin::Obj(Object::Instance(inst))) if method_guard_ok(entry, inst)
            );
        if bound {
            let entry: *const MethodEntry = entry;
            // SAFETY: the `&mut Interpreter` that entered native code is
            // dormant while the helper runs; the activation's method table
            // (and so the entry) lives until the activation ends.
            return unsafe {
                let interp = &mut *ctx.interp;
                call_native_method(jf, ctx, interp, &*entry, recv_pin, argc, expect_tag)
            };
        }
    }
    // SAFETY: per the function contract.
    unsafe { wpjit_call_method(frame, token, recv_pin, argc, expect_tag) }
}

/// The `wpjit_guard_method` helper (see `TOp::GuardMethod`): `0` when the
/// receiver pin is an instance of the class version the native method
/// site `token` burned, with no instance attribute shadowing the name;
/// `1` sends the load to the interpreter (nothing has run).
///
/// # Safety
///
/// Same live-activation contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_guard_method(frame: *mut JitFrame, pin: i64, token: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &*jf.ctx.cast::<CallCtx>() };
    let Some(entry) = usize::try_from(token).ok().and_then(|t| ctx.methods.get(t)) else {
        return 1;
    };
    let Some(Pin::Obj(Object::Instance(inst))) =
        usize::try_from(pin).ok().and_then(|p| ctx.pins.get(p))
    else {
        return 1;
    };
    i64::from(!method_guard_ok(entry, inst))
}

/// A burned method entry still describes `inst`: the class version it
/// was resolved against (which pins the MRO hit), and no instance
/// attribute shadowing the name.
#[inline(always)]
fn method_guard_ok(entry: &MethodEntry, inst: &crate::types::PyInstance) -> bool {
    attr_class_ok(inst, entry.ver)
        && (entry.no_dict
            // SAFETY: a read between two native ops; nothing here runs
            // code (see `GilCell::peek`).
            || unsafe { inst.attr_peek_has(&entry.name, entry.name_hash) } == Some(false))
}

/// The `wpjit_obj_getitem` helper: `container[index]` on an object-lane
/// container whose class resolved `__getitem__` to a native leaf method
/// (`deque.__getitem__`) at compile time. Shares the method helper's
/// shape: `token` names the burned resolution and the index rides
/// `call_args[0]`. Unlike a method call, a guard miss (another class, a
/// non-instance) or a fast half that declines rejects, so the
/// interpreter re-executes the `BINARY_SUBSCR` itself: nothing has run.
///
/// # Safety
///
/// Same contract as [`wpjit_call_method`]; `argc` is 1.
unsafe extern "C" fn wpjit_obj_getitem(
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
    if argc != 1 || crate::trace::any_observers_active() {
        return CallStatus::Reject as i64;
    }
    let Some(MethodEntry {
        callee: MethodCallee::Native { builtin, fast, op },
        ver,
        ..
    }) = ctx.methods.get(token as usize)
    else {
        return CallStatus::Reject as i64;
    };
    // Subscription looks the dunder up on the type: no instance
    // shadowing check.
    let recv = match ctx.pins.get(recv_pin as usize) {
        Some(Pin::Obj(recv @ Object::Instance(inst))) if attr_class_ok(inst, *ver) => recv,
        _ => return CallStatus::Reject as i64,
    };
    if *op != 0 {
        // SAFETY: native code wrote one entry (the index).
        let (bits, tag) = unsafe { (*jf.call_args, *jf.call_tags) };
        if let Some(index) = arg_view(bits, tag, &ctx.pins).map(std::mem::ManuallyDrop::new) {
            if let Some(v) = crate::stdlib::collections_native::fast_op(*op, recv, Some(&index)) {
                return deliver_native_result(jf, ctx, v, expect_tag);
            }
        }
    }
    let (builtin, fast) = (Rc::as_ptr(builtin), *fast);
    // SAFETY: the activation's method table keeps the builtin alive; one
    // marshaled argument (the index).
    match unsafe { direct_native_call(jf, ctx, &*builtin, fast, recv_pin, 1) } {
        Some(Ok(v)) => deliver_native_result(jf, ctx, v, expect_tag),
        Some(Err(err)) => {
            ctx.raised = Some(err);
            CallStatus::Raised as i64
        }
        // A declined fast half ran nothing: the interpreter re-executes.
        None => CallStatus::Reject as i64,
    }
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
/// (pin miss, pin-cap pressure, a `join` argument requiring the interpreter's
/// iterator protocol, or a `replace` count that can call Python).
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
    // The interpreter calls the same builtin body through its own leaf
    // table, with no argument marshaling: a loop whose work is `str`
    // methods belongs to tier-1 (see `wpjit_poll`'s density judgment).
    ctx.dyn_py_calls = ctx.dyn_py_calls.saturating_add(1);
    let recv = match ctx.pins.get(recv_pin as usize) {
        Some(Pin::Obj(o @ Object::Str(_))) => o.clone(),
        _ => return CallStatus::Reject as i64,
    };
    // Most string methods need only the receiver and up to three
    // positional arguments. Keep those handles on the stack; larger
    // calls still reach the builtin's ordinary arity validation.
    let mut inline_args: [Object; 4] = std::array::from_fn(|_| Object::None);
    let mut heap_args = Vec::new();
    let n_args = argc as usize + 1;
    let args = if n_args <= inline_args.len() {
        &mut inline_args[..n_args]
    } else {
        heap_args.resize(n_args, Object::None);
        heap_args.as_mut_slice()
    };
    args[0] = recv;
    for j in 0..argc as usize {
        // SAFETY: native code wrote `argc` entries, and the buffers are
        // `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        args[j + 1] = unpack_pins(bits, tag, &ctx.pins);
    }
    // `replace` can invoke Python through the count's `__index__`.
    // Resume before CALL so the interpreter installs the right frame
    // and observes callback mutations before executing subsequent code.
    if m == weavepy_jit::StrMethod::Replace
        && matches!(args.get(3), Some(Object::Instance(_) | Object::Foreign(_)))
    {
        return CallStatus::Reject as i64;
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
        Some(ckw) => ckw(args, &[]),
        None => (bf.call)(args),
    };
    // Pure native code ran: no Python, no guard invalidation, `ctx`
    // stays clean.
    match result {
        Err(err) => {
            ctx.raised = Some(err);
            CallStatus::Raised as i64
        }
        Ok(v) => {
            // A loop of `str` methods belongs to tier-1 (its leaf table
            // calls the same bodies without marshaling): past the budget
            // the activation hands back here, like the generic call
            // helpers' round-trip charge. The poll's density check only
            // sees loops that reach a poll; a body that side-exits more
            // often than that never does.
            ctx.native_calls = ctx.native_calls.saturating_add(1);
            if ctx.native_calls >= STR_METHOD_RETIRE_BUDGET && !ctx.code_ptr.is_null() {
                retire_native_driver(ctx);
                ctx.parked = Some(v);
                return CallStatus::Boxed as i64;
            }
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
                        if ctx.temporary_pin_limit_reached() {
                            ctx.pin_pressure_exit = true;
                        } else {
                            jf.ret_bits = ctx.pins.len() as u64;
                            jf.ret_tag = expect_tag;
                            ctx.pins.push(Pin::Obj(v));
                            return CallStatus::Ok as i64;
                        }
                    }
                }
                // `split`/`rsplit` — a fresh list of exact strings,
                // pinned on the object-element lane (`ForList`/
                // `ListGet` hand elements out as `Obj` pins).
                SlotTag::ListPin => {
                    if let Object::List(l) = &v {
                        if ctx.pins.len() < RUNTIME_PIN_CAP {
                            if ctx.temporary_pin_limit_reached() {
                                ctx.pin_pressure_exit = true;
                            } else {
                                jf.ret_bits = ctx.pins.len() as u64;
                                jf.ret_tag = expect_tag;
                                ctx.pins.push(Pin::List(l.clone(), JitType::Obj));
                                return CallStatus::Ok as i64;
                            }
                        }
                    }
                }
                SlotTag::None | SlotTag::Float | SlotTag::Boxed | SlotTag::Default => {}
            }
            // Lane surprise (`WStr` result, huge `int`, pin-cap
            // pressure): park the exact result and deopt after the
            // call — the interpreter resumes with it on the stack.
            // A temporary-pin limit also takes this completed-call exit.
            // Keep the exact result alive for reconstruction; neither the
            // method nor its argument production may run a second time.
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

/// The float `**` helper: libm `pow`, exactly as the interpreter's
/// `float_pow` computes it (native code admits only a positive base and
/// deopts on a non-finite result).
extern "C" fn wpjit_float_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}

/// Python-semantics `float` floor division (RFC 0069 WS2): CPython's
/// `float_divmod` quotient, sign discipline included. The compiled
/// guard deopts the zero-divisor case *before* the call (the
/// interpreter re-executes and raises the exact `ZeroDivisionError`),
/// so the error arm is unreachable-defensive.
extern "C" fn wpjit_float_floordiv(a: f64, b: f64) -> f64 {
    crate::py_float_divmod(a, b, "division by zero").map_or(f64::NAN, |(div, _)| div)
}

/// Python-semantics `float` modulo (RFC 0069 WS2): the remainder takes
/// the divisor's sign. Zero divisors deopt before the call, as with
/// [`wpjit_float_floordiv`].
extern "C" fn wpjit_float_mod(a: f64, b: f64) -> f64 {
    crate::py_float_divmod(a, b, "division by zero").map_or(f64::NAN, |(_, m)| m)
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
    let outcome: Result<u64, Pin> = {
        let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
            return 1;
        };
        // SAFETY: a read between two native ops; nothing here runs code
        // (see `GilCell::peek`).
        let Some(items) = (unsafe { list.peek() }) else {
            return 1;
        };
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
            (v @ (Object::Instance(_) | Object::Str(_) | Object::Tuple(_)), JitType::Obj) => {
                match pin_memo_hit(v, &ctx.pins, &ctx.pin_memo) {
                    Some(bits) => Ok(bits),
                    None => Err(Pin::Obj(v.clone())),
                }
            }
            // The nested lanes: a list element pins on the float-list or
            // int-list lane (its reads re-validate each element), reusing
            // the pin of a row read before.
            (Object::List(inner), lane @ (JitType::ListFloat | JitType::ListInt)) => {
                match list_pin_memo_hit(inner, &ctx.pins, &ctx.pin_memo) {
                    Some(bits) => Ok(bits),
                    None => Err(Pin::List(
                        inner.clone(),
                        lane.elem_lane().unwrap_or(JitType::Int),
                    )),
                }
            }
            _ => return 1,
        }
    };
    match outcome {
        Ok(bits) => {
            jf.ret_bits = bits;
            0
        }
        Err(p) => {
            if ctx.pins.len() >= RUNTIME_PIN_CAP {
                return 1;
            }
            jf.ret_bits = ctx.pins.len() as u64;
            pin_memo_note(&p, ctx.pins.len(), &mut ctx.pin_memo);
            ctx.pins.push(p);
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
    // SAFETY: as `wpjit_list_get` — nothing below runs code (the
    // displaced value is a scalar, checked before the store).
    let Some(items) = (unsafe { list.peek_mut() }) else {
        return 1;
    };
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
        // A list lane pins the cell's current list when it still has the
        // site's element lane (the list helpers re-check each element),
        // reusing the pin of the list the site read last.
        (v @ Object::List(l), Some(lane)) if lane.is_list() && entry_local_ok(v, lane) => {
            let elem = lane.elem_lane().unwrap_or(JitType::Unknown);
            let slot = idx as usize;
            if let Some(&pin) = ctx.cell_list_pins.get(slot) {
                if matches!(ctx.pins.get(pin as usize), Some(Pin::List(p, _)) if Rc::ptr_eq(p, l)) {
                    jf.ret_bits = u64::from(pin);
                    return 0;
                }
            }
            if ctx.pins.len() >= RUNTIME_PIN_CAP {
                return 1;
            }
            let pin = ctx.pins.len();
            ctx.pins.push(Pin::List(l.clone(), elem));
            if ctx.cell_list_pins.len() <= slot {
                ctx.cell_list_pins.resize(slot + 1, u32::MAX);
            }
            ctx.cell_list_pins[slot] = pin as u32;
            jf.ret_bits = pin as u64;
            return 0;
        }
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
/// (pre-staged in [`JitFrame::ret_bits`] and [`JitFrame::ret_tag`]) to a
/// pinned list. Validate the lane and any object pin before mutating
/// the list, so a failed append can be retried by the interpreter.
/// Never runs Python code or drops a displaced heap object.
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
    let v = match (elem, jf.ret_tag) {
        (JitType::Int | JitType::Obj, tag) if tag == SlotTag::Int as u32 => {
            Object::Int(jf.ret_bits as i64)
        }
        (JitType::Float | JitType::Obj, tag) if tag == SlotTag::Float as u32 => {
            Object::Float(f64::from_bits(jf.ret_bits))
        }
        (JitType::Obj, tag) if tag == SlotTag::Bool as u32 => Object::Bool(jf.ret_bits != 0),
        (JitType::Obj, tag) if tag == SlotTag::ObjPin as u32 => {
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
    // SAFETY: a push runs no code and drops no object, so the guard-free
    // view (live only while cells are unshared and nothing borrows the
    // list) ends before anything could borrow it.
    match unsafe { list.peek_mut() } {
        Some(items) => items.push(v),
        None => list.borrow_mut().push(v),
    }
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
    if a == b || SharedStr::ptr_eq(sa, sb) {
        return 1;
    }
    i64::from(sa == sb)
}

/// The `wpjit_is_obj` helper ([`weavepy_jit::TOp::IsObj`]): whether two
/// object-lane values (pin indices, `-1` for `None`) are the same object;
/// `2` deopts on a pin miss.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_is_obj(frame: *mut JitFrame, a: i64, b: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &*jf.ctx.cast::<CallCtx>() };
    let none = Object::None;
    let at = |p: i64| -> Option<Result<&Object, *const ()>> {
        if p == -1 {
            return Some(Ok(&none));
        }
        match ctx.pins.get(usize::try_from(p).ok()?)? {
            Pin::Obj(o) => Some(Ok(o)),
            Pin::List(l, _) => Some(Err(Rc::as_ptr(l).cast())),
        }
    };
    let (Some(x), Some(y)) = (at(a), at(b)) else {
        return 2;
    };
    let same = match (x, y) {
        (Ok(x), Ok(y)) => x.is_same(y),
        (Err(x), Err(y)) => x == y,
        (Ok(Object::List(l)), Err(y)) | (Err(y), Ok(Object::List(l))) => {
            Rc::as_ptr(l).cast::<()>() == y
        }
        _ => false,
    };
    i64::from(same)
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
    // A `str` or `int` key settles by native equality unless the table
    // compared it with a key of another kind.
    if let Some(probe) = crate::object::LeafProbe::new(key) {
        if let Ok(m) = d.try_borrow() {
            match m.get(&probe) {
                Some(v) => return Ok(Some(v.clone())),
                None if probe.miss_is_exact() => return Ok(None),
                None => {}
            }
        }
    }
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
    let Some(Pin::Obj(Object::Dict(d))) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    let int_key;
    let key: &Object = if key_tag == weavepy_jit::DICT_KEY_STR {
        match ctx.pins.get(key_bits as usize) {
            Some(Pin::Obj(o @ Object::Str(_))) => o,
            _ => return 1,
        }
    } else {
        int_key = Object::Int(key_bits);
        &int_key
    };
    let found = match dict_probe_native(d, key) {
        Ok(f) => f,
        Err(()) => return 1,
    };
    let Some(v) = found else {
        ctx.raised = Some(crate::error::key_error_object(key.clone()));
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
    // Python-free pre-probe: a deferral means the insert below could run
    // Python.
    if dict_probe_native(&d, &key).is_err() {
        return 1;
    }
    match crate::builtins::dict_insert(&d, key, v) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

/// The `wpjit_dict_del` helper: `del d[k]` on a pinned exact dict,
/// through the interpreter's own [`crate::builtins::dict_remove`]
/// chokepoint (and its removed-entry queue, as `delete_subscr`). Returns
/// `0`, or `1` to deopt *before* the delete: a missing key (the
/// interpreter raises the exact `KeyError`), a comparison that would
/// need Python, a displaced value the prompt-reap cascade would take
/// (the `wpjit_dict_set` discipline), or active C-API dict watchers.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_dict_del(
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
    if crate::capi_watchers::dicts_active() {
        return 1;
    }
    let Some((d, key)) = dict_pin_and_key(ctx, pin, key_bits, key_tag) else {
        return 1;
    };
    let old = match dict_probe_native(&d, &key) {
        Ok(Some(v)) => v,
        _ => return 1,
    };
    drop(old);
    match crate::builtins::dict_remove(&d, &key) {
        Ok(Some(_)) => 0,
        _ => 1,
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
    // SAFETY: this helper has the same live-activation contract.
    unsafe { wpjit_const_pin(frame, idx, false) }
}

/// Pin the code object's canonical tuple constant. Negative status deopts
/// before any side effect; nested values stay owned by the constant table.
///
/// # Safety
///
/// Same live-activation contract as [`wpjit_const_str`].
unsafe extern "C" fn wpjit_const_tuple(frame: *mut JitFrame, idx: i64) -> i64 {
    // SAFETY: this helper has the same live-activation contract.
    unsafe { wpjit_const_pin(frame, idx, true) }
}

/// Share the interpreter's materialized constant instead of allocating a
/// replacement on each activation. Constant indices have a fixed shape,
/// so string and tuple helpers can safely share the same pin memo.
///
/// # Safety
///
/// Same live-activation contract as [`wpjit_const_str`].
#[inline(always)]
unsafe fn wpjit_const_pin(frame: *mut JitFrame, idx: i64, tuple: bool) -> i64 {
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
    let Some(obj) = super::code_const_objects(code).get(idx as usize) else {
        return -1;
    };
    if !matches!(
        (tuple, obj),
        (true, Object::Tuple(_)) | (false, Object::Str(_))
    ) {
        return -1;
    }
    let obj = super::Interpreter::clone_operand(obj);
    let pin = ctx.pins.len() as u64;
    ctx.pins.push(Pin::Obj(obj));
    ctx.const_pins.push((idx, pin));
    pin as i64
}

/// Length of a pinned exact tuple. Subclasses and other objects deopt
/// without invoking __len__, allowing the interpreter to dispatch once.
///
/// # Safety
///
/// Same live-activation contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_tuple_len(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py for the live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(Object::Tuple(items))) => items.len() as i64,
        // The other exact builtin containers measure without code.
        Some(Pin::List(l, _) | Pin::Obj(Object::List(l))) => l.borrow().len() as i64,
        Some(Pin::Obj(Object::Dict(d))) => d.borrow().len() as i64,
        // An instance whose class's `__len__` is a registered native leaf
        // (`deque.__len__`): its body runs no Python and has no effects,
        // so anything but a plain length deopts for the interpreter to
        // call it again.
        Some(Pin::Obj(obj @ Object::Instance(_))) if !crate::gil::free_threading_enabled() => {
            // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
            let interp = unsafe { &*ctx.interp };
            match interp
                .leaf_instance_dunder(obj, "__len__")
                .map(|b| (b.call)(std::slice::from_ref(obj)))
            {
                Some(Ok(Object::Int(n))) if n >= 0 => n,
                _ => -1,
            }
        }
        _ => -1,
    }
}

#[cfg(test)]
thread_local! {
    static BORROWED_DYN_SCALAR_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static RESOLVED_DYN_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn borrowed_dyn_scalar_calls_for_test() -> u64 {
    BORROWED_DYN_SCALAR_CALLS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn resolved_dyn_calls_for_test() -> u64 {
    RESOLVED_DYN_CALLS.with(std::cell::Cell::get)
}

/// Guard a pinned exact integer before native arithmetic. All other
/// values, including integer subclasses and large integers, deopt without
/// invoking conversion or arithmetic hooks.
///
/// # Safety
///
/// Same live-activation contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_unbox_int(frame: *mut JitFrame, pin: i64) -> i64 {
    // SAFETY: see wpjit_call_py for the live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let Some(Pin::Obj(Object::Int(value))) = ctx.pins.get(pin as usize) else {
        return 1;
    };
    jf.ret_bits = *value as u64;
    0
}

/// Guard a pinned `float` before native float arithmetic, comparison, or
/// a store into a float-lane local (`weavepy_jit::TOp::UnboxFloat`). The
/// argument packs the pin with the promotion mode in its high bits: an
/// exact `float` always passes; an exact `int` passes converted under
/// the arithmetic mode, and within ±2^53 under the comparison mode.
/// Anything else (including subclasses, `bool`, and big integers) deopts
/// without invoking a hook.
///
/// # Safety
///
/// Same live-activation contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_unbox_float(frame: *mut JitFrame, packed: i64) -> i64 {
    if packed < 0 {
        return 1;
    }
    // SAFETY: see wpjit_call_py for the live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let pin = (packed & 0xFFFF_FFFF) as usize;
    let mode = (packed >> 32) as u8;
    let v = match ctx.pins.get(pin) {
        Some(Pin::Obj(Object::Float(f))) => *f,
        Some(Pin::Obj(Object::Int(i))) => match mode {
            weavepy_jit::UNBOX_FLOAT_ARITH => *i as f64,
            weavepy_jit::UNBOX_FLOAT_CMP if i.unsigned_abs() <= 1 << 53 => *i as f64,
            _ => return 1,
        },
        _ => return 1,
    };
    jf.ret_bits = v.to_bits();
    0
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
    let outcome: Result<u64, Pin> = {
        let Some(Pin::List(list, elem)) = ctx.pins.get(pin as usize) else {
            return 2;
        };
        if idx < 0 {
            return 2;
        }
        // SAFETY: a read between two native ops; nothing here runs code
        // (see `GilCell::peek`).
        let Some(items) = (unsafe { list.peek() }) else {
            return 2;
        };
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
            // A loop over the same few objects reuses their pins.
            (v @ (Object::Instance(_) | Object::Str(_) | Object::Tuple(_)), JitType::Obj) => {
                match pin_memo_hit(v, &ctx.pins, &ctx.pin_memo) {
                    Some(bits) => Ok(bits),
                    None => Err(Pin::Obj(v.clone())),
                }
            }
            // The nested lanes (see `wpjit_list_get`).
            (Object::List(inner), lane @ (JitType::ListFloat | JitType::ListInt)) => {
                match list_pin_memo_hit(inner, &ctx.pins, &ctx.pin_memo) {
                    Some(bits) => Ok(bits),
                    None => Err(Pin::List(
                        inner.clone(),
                        lane.elem_lane().unwrap_or(JitType::Int),
                    )),
                }
            }
            _ => return 2,
        }
    };
    let pinned = match outcome {
        Ok(bits) => Some(bits),
        Err(p) => (ctx.pins.len() < RUNTIME_PIN_CAP).then(|| {
            pin_memo_note(&p, ctx.pins.len(), &mut ctx.pin_memo);
            ctx.pins.push(p);
            (ctx.pins.len() - 1) as u64
        }),
    };
    match pinned {
        Some(bits) => {
            jf.ret_bits = bits;
            0
        }
        None => 2,
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
        Some(Pin::Obj(Object::Generator(_) | Object::Iter(_) | Object::LazyIter(_))) => 0,
        _ => 1,
    }
}

/// One step of a list, tuple or range iterator, without the generic
/// dispatch: the next item, or `None` once exhausted (the iterator then
/// detached as [`PyIterator::next_value`] leaves it). `None` outside for
/// any other iterator, or one already borrowed.
#[inline]
fn builtin_seq_step(cell: &Rc<GilRefCell<crate::object::PyIterator>>) -> Option<Option<Object>> {
    use crate::object::PyIterator;
    let mut it = cell.try_borrow_mut().ok()?;
    match &*it {
        PyIterator::List { .. } | PyIterator::Tuple { .. } | PyIterator::Range { .. } => {
            Some(it.next_value())
        }
        _ => None,
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
    // A native deque iterator's step runs no Python: no checkpoint (the
    // loop header's countdown poll covers the back edge), and the element
    // is read in place through the pin.
    if let Some(Pin::Obj(it @ Object::Instance(inst))) = ctx.pins.get(pin as usize) {
        if !crate::gil::free_threading_enabled()
            && crate::hot_gates::load() == 0
            && !crate::trace::any_observers_active()
            && super::Interpreter::default_getattribute(inst.cls_raw())
            && !crate::object::exotic_str_keys_possible()
        {
            if let Some((_, op)) = interp.core_leaf_next_op(inst) {
                if op != 0 {
                    if let Some(v) = crate::stdlib::collections_native::fast_op(op, it, None) {
                        if let Some(bits) =
                            pack_iter_elem(&v, elem_tag, &mut ctx.pins, &mut ctx.pin_memo)
                        {
                            jf.ret_bits = bits;
                            return 0;
                        }
                        // Consumed but outside the lane: resume at the store.
                        jf.ret_bits = if matches!(v, Object::None) {
                            u64::MAX
                        } else {
                            ctx.pins.push(Pin::Obj(v));
                            (ctx.pins.len() - 1) as u64
                        };
                        return 3;
                    }
                }
            }
        }
    }
    // A list, tuple or range iterator steps in place: no Python runs, so
    // the header's countdown poll covers the step, and nothing needs the
    // iterator cloned.
    let fast = match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(Object::Iter(cell))) => builtin_seq_step(cell),
        // A native adapter step that runs no code (`zip` of native
        // iterators, `itertools.repeat`, ...).
        Some(Pin::Obj(Object::LazyIter(l))) => interp.lazy_core_next(l),
        _ => None,
    };
    let (step, runs_python) = match fast {
        Some(step) => (Ok(step), false),
        None => {
            // The loop's poll point (the header's countdown poll covers the
            // native back edge; this covers the Python the step may run):
            // pending interpreter work and active observers route the loop
            // through the interpreter.
            crate::gil::yield_checkpoint();
            if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
                return 2;
            }
            let it = match ctx.pins.get(pin as usize) {
                Some(Pin::Obj(
                    o @ (Object::Generator(_) | Object::Iter(_) | Object::LazyIter(_)),
                )) => o.clone(),
                // An instance iterator whose `__next__` is a registered native
                // leaf (a `deque` iterator): stepped below without Python.
                Some(Pin::Obj(o @ Object::Instance(_)))
                    if !crate::gil::free_threading_enabled() =>
                {
                    o.clone()
                }
                _ => return 2,
            };
            // A builtin-iterator step is pure native code; everything else
            // (generator resume) runs arbitrary Python on behalf of this
            // activation.
            let runs_python = matches!(it, Object::Generator(_) | Object::LazyIter(_));
            if runs_python {
                ctx.dirty = true;
                // A generator resume from native code rebuilds a whole interpreter
                // activation, several times what the interpreter's own inline
                // resume costs: charged like an interpreter call, so a loop that
                // drives a generator retires at the next poll (see `wpjit_poll`).
                ctx.dyn_py_calls = ctx.dyn_py_calls.saturating_add(1);
            }
            let step = if let Object::Instance(inst) = &it {
                // The core loop's cached `__next__` resolution (by class version).
                // Any other `__next__` belongs to the interpreter (nothing was
                // consumed).
                if !super::Interpreter::default_getattribute(inst.cls_raw())
                    || crate::object::exotic_str_keys_possible()
                {
                    return 2;
                }
                match interp.leaf_next_step(&it, inst) {
                    None => return 2,
                    Some(Ok(v)) => Ok(Some(v)),
                    Some(Err(RuntimeError::PyException(e))) if e.type_name() == "StopIteration" => {
                        Ok(None)
                    }
                    Some(Err(e)) => Err(e),
                }
            } else {
                interp.iter_next(&it, &ctx.globals)
            };
            (step, runs_python)
        }
    };
    match step {
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
                        SlotTag::ObjPin => obj_ret_bits(&v, &mut ctx.pins, &mut ctx.pin_memo),
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
    crate::gc_trace::track(&Object::List(list.clone()));
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
fn pin_str(ctx: &CallCtx, pin: i64) -> Option<SharedStr> {
    match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(Object::Str(s))) => Some(s.clone()),
        _ => None,
    }
}

/// The `wpjit_str_concat` helper (RFC 0073 WS3): guarded exact-`str`
/// `+`. Allocates the joined `SharedStr` and pins it — the same
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
    crate::gc_trace::track(&obj);
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
    crate::gc_trace::track(&Object::List(fresh.clone()));
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::List(fresh, elem));
    idx
}

/// The `wpjit_list_from_range` helper: `list(range(start, stop))` —
/// the fresh `int` list pins on the `int` lane and is GC-tracked like
/// every list the interpreter builds. Negative deopts (cap pressure,
/// or a length past what the fast path builds — the interpreter then
/// raises exactly or builds it itself). Never runs Python code.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe extern "C" fn wpjit_list_from_range(frame: *mut JitFrame, start: i64, stop: i64) -> i64 {
    // Far past any sane fast-path build; the interpreter handles the rest.
    const MAX_LEN: i64 = 1 << 28;
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    if ctx.pins.len() >= RUNTIME_PIN_CAP {
        return -1;
    }
    let len = stop.saturating_sub(start).max(0);
    if len > MAX_LEN {
        return -1;
    }
    let items: Vec<Object> = (start..start + len).map(Object::Int).collect();
    let list = Rc::new(crate::sync::RefCell::new(items));
    let obj = Object::List(list.clone());
    // A short list of integers can't close a cycle (a long one is
    // registered without the scan as well).
    if len <= 32 {
        crate::gc_trace::track_inert(&obj);
    } else {
        crate::gc_trace::track(&obj);
    }
    if crate::stdlib::tracemalloc_real::is_tracking() {
        crate::stdlib::tracemalloc_real::track_new_object(&obj);
    }
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::List(list, JitType::Int));
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
    crate::gc_trace::track(&Object::List(fresh.clone()));
    let idx = ctx.pins.len() as i64;
    ctx.pins.push(Pin::List(fresh, elem));
    idx
}

/// `true` when a dict key is the string `name` (the per-access name
/// re-check that makes an indexed hit safe against `del`-driven index
/// shifts, mirroring the tier-1 caches).
fn key_is(key: &DictKey, name: &SharedStr) -> bool {
    // Instance-dict keys are interned on insert and the guard holds the
    // interned name: identity usually settles it.
    matches!(&key.0, Object::Str(s) if SharedStr::ptr_eq(s, name) || **s == **name)
}

/// A site guard's class check: the receiver's class still carries the
/// compiled `attr_version` (read without a borrow guard when only one
/// thread runs Python; nothing here runs code).
#[inline(always)]
fn attr_class_ok(inst: &crate::types::PyInstance, ver: u64) -> bool {
    if crate::gil::free_threading_enabled() {
        return inst.class.borrow().attr_version.get() == ver;
    }
    inst.cls_raw().attr_version.get() == ver
}

/// Reuse a pin only when it still owns the exact attribute result. A hint
/// from another activation can be out of range or name an unrelated object.
/// No equality protocol runs here, and the hint itself owns no object.
#[inline]
fn attr_result_pin(value: &Object, pins: &[Pin], hint: usize) -> Option<u64> {
    let Pin::Obj(pinned) = pins.get(hint)? else {
        return None;
    };
    let same = match (value, pinned) {
        (Object::Instance(a), Object::Instance(b)) => Rc::ptr_eq(a, b),
        (Object::Str(a), Object::Str(b)) => SharedStr::ptr_eq(a, b),
        (Object::Bytes(a), Object::Bytes(b)) => crate::shared_value::SharedSlice::ptr_eq(a, b),
        _ => false,
    };
    same.then_some(hint as u64)
}

#[cfg(test)]
mod scalar_update_guard_tests {
    use super::*;
    use crate::object::BuiltinFn;
    use crate::types::PyInstance;

    #[test]
    fn exotic_class_keys_disable_update_certification() {
        const CHILD: &str = "WEAVEPY_UPDATE_EXOTIC_KEYS_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tier2::scalar_update_guard_tests::exotic_class_keys_disable_update_certification",
                ])
                .env(CHILD, "1")
                .status()
                .expect("spawn exotic class-key test");
            assert!(status.success(), "exotic class-key child: {status}");
            return;
        }
        let object = crate::builtin_types::builtin_types().object_.clone();
        let owner = TypeObject::new_user("Owner", vec![object], DictData::default()).unwrap();
        let receiver = Object::Instance(Rc::new(PyInstance::new(owner.clone())));
        let version = owner.attr_version.get();
        assert!(scalar_update_class_value_stable(
            &receiver, "value", version
        ));
        assert_eq!(owner.getattribute_kind.get(), 1);
        // This process-global state is monotone. The child keeps it from
        // changing the eligibility of unrelated tests in the parent.
        crate::object::note_class_dict_key(&DictKey(Object::Int(0)));
        assert!(!scalar_update_class_value_stable(
            &receiver, "value", version
        ));
    }

    #[test]
    fn mutable_descriptor_classes_never_certify_an_update() {
        let object = crate::builtin_types::builtin_types().object_.clone();
        let descriptor_type =
            TypeObject::new_user("MutableValue", vec![object.clone()], DictData::default())
                .unwrap();
        let descriptor = Object::Instance(Rc::new(PyInstance::new(descriptor_type.clone())));
        let mut namespace = DictData::default();
        namespace.insert(DictKey(Object::from_static("value")), Object::Int(0));
        let owner = TypeObject::new_user("Owner", vec![object], namespace).unwrap();
        let receiver = Object::Instance(Rc::new(PyInstance::new(owner.clone())));
        let initial = owner.attr_version.get();
        assert!(scalar_update_class_value_stable(
            &receiver, "value", initial
        ));

        owner
            .dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("value")), descriptor);
        owner.bump_attr_version();
        let changed = owner.attr_version.get();
        assert_ne!(initial, changed);
        assert!(!scalar_update_class_value_stable(
            &receiver, "value", initial
        ));
        assert!(!scalar_update_class_value_stable(
            &receiver, "value", changed
        ));

        // Changing the descriptor's own class doesn't change Owner's token.
        // It must have been excluded even before acquiring the setter hook.
        descriptor_type.dict.borrow_mut().insert(
            DictKey(Object::from_static("__set__")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "__set__",
                binds_instance: true,
                call: Box::new(|_| Ok(Object::None)),
                call_kw: None,
            })),
        );
        descriptor_type.bump_attr_version();
        assert_eq!(owner.attr_version.get(), changed);
        assert!(!scalar_update_class_value_stable(
            &receiver, "value", changed
        ));

        owner
            .dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("value")), Object::None);
        owner.bump_attr_version();
        assert!(!scalar_update_class_value_stable(
            &receiver, "value", changed
        ));
        assert!(scalar_update_class_value_stable(
            &receiver,
            "value",
            owner.attr_version.get()
        ));
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<AttrGuard>(), 56);
    }
}

#[cfg(test)]
mod attr_pin_tests {
    use super::*;
    use crate::shared_value::SharedSlice;

    #[test]
    fn stale_hints_check_the_current_activation_and_object_identity() {
        let class = TypeObject::new_user("PinValue", vec![], DictData::default()).unwrap();
        let a = Object::Instance(Rc::new(crate::types::PyInstance::new(class.clone())));
        let b = Object::Instance(Rc::new(crate::types::PyInstance::new(class)));
        let outer = vec![Pin::Obj(a.clone()), Pin::Obj(b.clone())];
        let inner = vec![Pin::Obj(b.clone()), Pin::Obj(a.clone())];
        assert_eq!(attr_result_pin(&a, &outer, 0), Some(0));
        assert_eq!(attr_result_pin(&a, &inner, 0), None);
        assert_eq!(attr_result_pin(&a, &inner, 1), Some(1));
        assert_eq!(attr_result_pin(&a, &outer, 1), None);
        assert_eq!(attr_result_pin(&a, &[], 0), None);
        assert_eq!(attr_result_pin(&a, &outer, usize::MAX), None);
    }

    #[test]
    fn equal_text_and_bytes_need_distinct_pins() {
        let a = Object::Str(SharedStr::from("same content"));
        let b = Object::Str(SharedStr::from("same content"));
        let c = Object::Bytes(SharedSlice::from(b"same content".as_slice()));
        let d = Object::Bytes(SharedSlice::from(b"same content".as_slice()));
        let pins = vec![Pin::Obj(a.clone()), Pin::Obj(c.clone())];
        assert_eq!(attr_result_pin(&a, &pins, 0), Some(0));
        assert_eq!(attr_result_pin(&b, &pins, 0), None);
        assert_eq!(attr_result_pin(&c, &pins, 1), Some(1));
        assert_eq!(attr_result_pin(&d, &pins, 1), None);
        assert_eq!(attr_result_pin(&a, &pins, 1), None);
        let Object::Str(text) = &a else {
            unreachable!()
        };
        assert_eq!(SharedStr::strong_count(text), 2);
        drop(pins);
        assert_eq!(SharedStr::strong_count(text), 1);
    }
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
    // The common shape first: a scalar field of an indexed site, read
    // straight off the instance (the full path below re-derives it).
    if let (Some(Pin::Obj(Object::Instance(inst))), Some(g)) = (
        ctx.pins.get(pin as usize),
        ctx.attr_guards.get(site as usize),
    ) {
        match (g.storage, g.lane) {
            (AttrStorage::Indexed(key_idx), JitType::Int | JitType::Float | JitType::Bool) => {
                if attr_class_ok(inst, g.ver) {
                    // SAFETY: a read between two native ops; nothing here
                    // runs code (see `GilCell::peek`).
                    if let Some((k, v)) = unsafe { inst.attr_peek_index(key_idx as usize) } {
                        if key_is(k, &g.name) {
                            if let Some(bits) = pack(v, g.lane) {
                                jf.ret_bits = bits;
                                return 0;
                            }
                        }
                    }
                }
            }
            // A `__slots__` field at its usual position.
            (AttrStorage::Slot(key_idx), JitType::Int | JitType::Float | JitType::Bool) => {
                if attr_class_ok(inst, g.ver) {
                    // SAFETY: as above.
                    if let Some((k, v)) =
                        unsafe { inst.slots.peek() }.and_then(|s| s.get_index(key_idx as usize))
                    {
                        if key_is(k, &g.name) {
                            if let Some(bits) = pack(v, g.lane) {
                                jf.ret_bits = bits;
                                return 0;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // Scoped so the receiver borrow of `ctx.pins` ends before an
    // object-lane result appends a fresh pin (RFC 0070 WS1).
    let outcome: Result<u64, Object> = {
        let Some(Pin::Obj(Object::Instance(inst))) = ctx.pins.get(pin as usize) else {
            return 1;
        };
        let Some(g) = ctx.attr_guards.get(site as usize) else {
            return 1;
        };
        if !attr_class_ok(inst, g.ver) {
            return 1;
        }
        // RFC 0070 WS1 — the nullable object lane: `None` is the
        // machine value `-1`; an instance value reuses its last pin
        // when identity still matches, or gets a fresh runtime pin.
        // Any other value drifted from the compiled lane and deopts.
        let pinned_result = |v: &Object| {
            attr_result_pin(v, &ctx.pins, g.last_result_pin.get()).ok_or_else(|| v.clone())
        };
        let classify = |v: &Object| -> Option<Result<u64, Object>> {
            match (g.lane, v) {
                (JitType::Obj, Object::None) => Some(Ok(u64::MAX)),
                (JitType::Obj, Object::Instance(_)) => Some(pinned_result(v)),
                (JitType::Obj, _) => None,
                // RFC 0071 WS6 — `str`/`bytes` read lanes pin the
                // value; a drifted type deopts like any lane miss.
                (JitType::Str, Object::Str(_)) | (JitType::Bytes, Object::Bytes(_)) => {
                    Some(pinned_result(v))
                }
                (JitType::Str | JitType::Bytes, _) => None,
                _ => pack(v, g.lane).map(Ok),
            }
        };
        match g.storage {
            AttrStorage::Slot(key_idx) => {
                // SAFETY: a read between two native ops; nothing here
                // runs code (see `GilCell::peek`).
                let Some(slots) = (unsafe { inst.slots.peek() }) else {
                    return 1;
                };
                let indexed = slots
                    .get_index(key_idx as usize)
                    .filter(|(key, _)| key_is(key, &g.name))
                    .map(|(_, value)| value);
                // Deletion and differing population orders retain named
                // lookup. An unset slot resumes for AttributeError.
                let Some(v) = indexed.or_else(|| slots.get(&g.name)) else {
                    return 1;
                };
                match classify(v) {
                    Some(o) => o,
                    None => return 1,
                }
            }
            AttrStorage::Indexed(key_idx) => {
                // SAFETY: a read between two native ops; nothing here
                // runs code (see `GilCell::peek`).
                match unsafe { inst.attr_peek_index(key_idx as usize) } {
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
            let next = ctx.pins.len();
            ctx.pins.push(Pin::Obj(obj));
            ctx.attr_guards[site as usize].last_result_pin.set(next);
            jf.ret_bits = next as u64;
            0
        }
    }
}

#[derive(Clone, Copy)]
enum AttrChainMiss {
    Guard,
    SlotBorrow,
}

/// Inspect a field without retaining an owning intermediate result.
///
/// # Safety
/// The caller must retain the root instance and finish every use of the
/// returned reference before running Python, mutating fields, or releasing
/// the GIL. Both peek paths reject mutable borrows and unguarded VM access.
#[inline(always)]
unsafe fn chain_attr_peek<'a>(
    inst: &'a crate::types::PyInstance,
    guard: &AttrGuard,
) -> Result<&'a Object, AttrChainMiss> {
    if !attr_class_ok(inst, guard.ver) {
        return Err(AttrChainMiss::Guard);
    }
    match guard.storage {
        AttrStorage::Slot(index) => {
            // SAFETY: the caller keeps the entire chain read-only.
            let slots = unsafe { inst.slots.peek() }.ok_or(AttrChainMiss::SlotBorrow)?;
            slots
                .get_index(index as usize)
                .filter(|(name, _)| key_is(name, &guard.name))
                .map(|(_, value)| value)
                .or_else(|| slots.get(&guard.name))
                .ok_or(AttrChainMiss::Guard)
        }
        AttrStorage::Indexed(index) => {
            // SAFETY: the same callback-free interval as the slot read.
            let (name, value) =
                unsafe { inst.attr_peek_index(index as usize) }.ok_or(AttrChainMiss::Guard)?;
            if !key_is(name, &guard.name) {
                return Err(AttrChainMiss::Guard);
            }
            Ok(value)
        }
        AttrStorage::NewKey => Err(AttrChainMiss::Guard),
    }
}

#[inline(always)]
fn chain_attr_value(
    guard: &AttrGuard,
    value: &Object,
    pins: &[Pin],
) -> Option<Result<u64, Object>> {
    match (guard.lane, value) {
        (JitType::Obj, Object::None) => Some(Ok(u64::MAX)),
        (JitType::Obj, Object::Instance(_))
        | (JitType::Str, Object::Str(_))
        | (JitType::Bytes, Object::Bytes(_)) => Some(
            attr_result_pin(value, pins, guard.last_result_pin.get()).ok_or_else(|| value.clone()),
        ),
        (JitType::Obj | JitType::Str | JitType::Bytes, _) => None,
        _ => pack(value, guard.lane).map(Ok),
    }
}

/// Walk borrowed values in a loop so code size doesn't grow with every
/// possible mix of dictionary and slot storage along the chain.
#[inline(always)]
fn attr_chain_peek_result(
    mut inst: &crate::types::PyInstance,
    guards: &[AttrGuard],
    pins: &[Pin],
) -> Result<Result<u64, Object>, AttrChainMiss> {
    let (last, prefix) = guards.split_last().ok_or(AttrChainMiss::Guard)?;
    for guard in prefix {
        if guard.lane != JitType::Obj {
            return Err(AttrChainMiss::Guard);
        }
        // SAFETY: the root pin owns the graph, and this walk neither
        // mutates it nor calls Python. No borrowed reference escapes.
        let Object::Instance(next) = (unsafe { chain_attr_peek(inst, guard) })? else {
            return Err(AttrChainMiss::Guard);
        };
        inst = next;
    }
    // SAFETY: the same read-only interval; the final result alone may clone.
    let value = unsafe { chain_attr_peek(inst, last) }?;
    chain_attr_value(last, value, pins).ok_or(AttrChainMiss::Guard)
}

/// Inspect guarded storage while retaining any slot borrow for the read.
/// The callback and every caller must remain callback-free and read-only.
fn chain_attr_read<R>(
    inst: &crate::types::PyInstance,
    guard: &AttrGuard,
    read: impl FnOnce(&Object) -> R,
) -> Option<R> {
    if !attr_class_ok(inst, guard.ver) {
        return None;
    }
    match guard.storage {
        AttrStorage::Slot(index) => {
            let slots = inst.slots.try_borrow().ok()?;
            let value = slots
                .get_index(index as usize)
                .filter(|(name, _)| key_is(name, &guard.name))
                .map(|(_, value)| value)
                .or_else(|| slots.get(&guard.name))?;
            Some(read(value))
        }
        AttrStorage::Indexed(index) => {
            // SAFETY: every chain step is a read without Python callbacks.
            let (name, value) = unsafe { inst.attr_peek_index(index as usize) }?;
            key_is(name, &guard.name).then(|| read(value))
        }
        AttrStorage::NewKey => None,
    }
}

fn attr_chain_borrowed_result(
    inst: &crate::types::PyInstance,
    guards: &[AttrGuard],
    pins: &[Pin],
) -> Option<Result<u64, Object>> {
    let (guard, rest) = guards.split_first()?;
    chain_attr_read(inst, guard, |value| {
        if !rest.is_empty() {
            let (JitType::Obj, Object::Instance(next)) = (guard.lane, value) else {
                return None;
            };
            return attr_chain_borrowed_result(next, rest, pins);
        }
        chain_attr_value(guard, value, pins)
    })?
}

/// An out-of-GIL observer can revoke guardless reads. Keep the existing
/// guarded slot behavior in that mode, with every borrow alive through its
/// nested read. Ordinary chains take the compact borrowed walk above.
#[inline(always)]
fn attr_chain_result(
    inst: &crate::types::PyInstance,
    guards: &[AttrGuard],
    pins: &[Pin],
) -> Option<Result<u64, Object>> {
    match attr_chain_peek_result(inst, guards, pins) {
        Ok(value) => Some(value),
        Err(AttrChainMiss::Guard) => None,
        Err(AttrChainMiss::SlotBorrow) => {
            let value = attr_chain_borrowed_result(inst, guards, pins)?;
            #[cfg(test)]
            ATTR_CHAIN_BORROWED_COUNT.with(|count| count.set(count.get() + 1));
            Some(value)
        }
    }
}

#[cfg(test)]
thread_local! {
    static ATTR_CHAIN_TEST_COUNTS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    static ATTR_CHAIN_BORROWED_COUNT: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn attr_chain_counts_for_test() -> (u64, u64) {
    ATTR_CHAIN_TEST_COUNTS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn attr_chain_borrowed_count_for_test() -> u64 {
    ATTR_CHAIN_BORROWED_COUNT.with(Cell::get)
}

/// Read a bounded chain without creating intermediate owning pins. A miss
/// leaves all Python state unchanged and replays from the first attribute.
///
/// # Safety
/// Same live-buffer and GIL contract as [`wpjit_attr_get`].
unsafe extern "C" fn wpjit_attr_get_chain(
    frame: *mut JitFrame,
    pin: i64,
    first_site: i64,
    count: i64,
) -> i64 {
    if !(2..=weavepy_jit::MAX_ATTR_CHAIN_LEN as i64).contains(&count) {
        return 1;
    }
    let Ok(first) = usize::try_from(first_site) else {
        return 1;
    };
    let Some(end) = first.checked_add(count as usize) else {
        return 1;
    };
    // SAFETY: the caller supplies the same live activation as a single read.
    let jf = unsafe { &mut *frame };
    // The opaque ABI pointer originated from an aligned, live CallCtx.
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let outcome = {
        let Some(Pin::Obj(Object::Instance(inst))) = ctx.pins.get(pin as usize) else {
            return 1;
        };
        let Some(guards) = ctx.attr_guards.get(first..end) else {
            return 1;
        };
        let Some(outcome) = attr_chain_result(inst, guards, &ctx.pins) else {
            return 1;
        };
        outcome
    };
    match outcome {
        Ok(bits) => jf.ret_bits = bits,
        Err(value) => {
            if ctx.pins.len() >= RUNTIME_PIN_CAP {
                return 1;
            }
            let next = ctx.pins.len();
            ctx.pins.push(Pin::Obj(value));
            ctx.attr_guards[end - 1].last_result_pin.set(next);
            jf.ret_bits = next as u64;
        }
    }
    #[cfg(test)]
    ATTR_CHAIN_TEST_COUNTS.with(|counts| {
        let (dict, slots) = counts.get();
        counts.set(match ctx.attr_guards[first].storage {
            AttrStorage::Slot(_) => (dict, slots + 1),
            _ => (dict + 1, slots),
        });
    });
    0
}

/// Borrow a dynamic field only when the exact bytecode cache proves the read.
///
/// # Safety
/// The caller retains the original root and keeps every traversed reference
/// inside a read-only interval with no Python callbacks or GIL release.
#[inline(always)]
unsafe fn cached_chain_peek<'a>(
    receiver: &'a Object,
    code: &CodeObject,
    extension: &super::CodeConstObjects,
    pc: u32,
) -> Option<&'a Object> {
    use weavepy_compiler::{InlineCache as IC, OpCode};
    let instruction = code.instructions.get(pc as usize)?;
    if instruction.op != OpCode::LoadAttr {
        return None;
    }
    let Some(Object::Str(name)) = extension.name_objs.get(instruction.arg as usize) else {
        return None;
    };
    let cache = code.caches.get(pc);
    match receiver {
        Object::Instance(inst) if inst.cls_raw().native_kind.get() == 0 => {
            let version = inst.cls_raw().attr_version.get();
            match cache {
                IC::LoadAttrInstance { key_idx, ver } if ver == version => {
                    // SAFETY: the rooted walk is read-only and callback-free.
                    let (key, value) = unsafe { inst.attr_peek_index(key_idx as usize) }?;
                    key_is(key, name).then_some(value)
                }
                IC::LoadAttrSlot { key_idx, ver } if ver == version => {
                    // SAFETY: shared cells and conflicting borrows reject
                    // this peek instead of exposing an unguarded reference.
                    let slots = unsafe { inst.slots.peek() }?;
                    slots
                        .get_index(key_idx as usize)
                        .filter(|(key, _)| key_is(key, name))
                        .map(|(_, value)| value)
                        .or_else(|| slots.get(name.as_ref()))
                }
                _ => {
                    let index = extension
                        .attr_poly
                        .get()?
                        .get(pc as usize)?
                        .index(version)?;
                    // SAFETY: the same callback-free, rooted interval.
                    let (key, value) = unsafe { inst.attr_peek_index(index as usize) }?;
                    key_is(key, name).then_some(value)
                }
            }
        }
        Object::Module(module) => {
            let IC::LoadAttrModule { module_id, key_idx } = cache else {
                return None;
            };
            if crate::specialize::rc_id(&module.dict) != module_id
                || crate::object::module_class(module).is_some()
            {
                return None;
            }
            // SAFETY: the original root owns the whole read-only graph.
            let dict = unsafe { module.dict.peek() }?;
            let (key, value) = dict.get_index(key_idx as usize)?;
            key_is(key, name).then_some(value)
        }
        _ => None,
    }
}

#[cfg(test)]
thread_local! {
    static CACHED_ATTR_CHAIN_HITS: Cell<[u64; 2]> = const { Cell::new([0; 2]) };
    static CACHED_ATTR_PREFIX_HITS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn cached_attr_chain_hits_for_test() -> [u64; 2] {
    CACHED_ATTR_CHAIN_HITS.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn cached_attr_prefix_hits_for_test() -> u64 {
    CACHED_ATTR_PREFIX_HITS.with(Cell::get)
}

/// Read guarded and dynamically cached fields without owning intermediates.
/// Status 1 leaves accounting and pins untouched. Status 2 completes only the
/// guarded prefix using its original pin-reuse policy; lowering runs the dynamic
/// suffix through the original helpers. No Python runs inside this walk.
///
/// # Safety
/// Same live buffers and GIL contract as [`wpjit_attr_get`]. Native lowering must
/// publish the first read's bytecode PC in deopt_pc before entering this helper.
unsafe extern "C" fn wpjit_cached_attr_chain(
    frame: *mut JitFrame,
    pin: i64,
    first_site: i64,
    guarded: i64,
    total: i64,
    int_result: i64,
) -> i64 {
    if crate::gil::free_threading_enabled()
        || !(2..=weavepy_jit::MAX_CACHED_ATTR_CHAIN_LEN as i64).contains(&total)
        || !(0..total).contains(&guarded)
        || guarded > weavepy_jit::MAX_ATTR_CHAIN_LEN as i64
        || !matches!(int_result, 0 | 1)
    {
        return 1;
    }
    let Ok(first) = usize::try_from(first_site) else {
        return 1;
    };
    let Some(end) = first.checked_add(guarded as usize) else {
        return 1;
    };
    // SAFETY: native entry retains the frame, context, and code object.
    let jf = unsafe { &mut *frame };
    // The opaque pointer originated from an aligned, live CallCtx.
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    let code = unsafe { &*ctx.code_ptr };
    // The activation owns immutable names and cache storage through its code.
    // Fetch them once, and decline without initializing a missing extension.
    let Some(extension) = super::code_vm_ext_existing(code) else {
        return 1;
    };
    let Some(last_pc) = jf.deopt_pc.checked_add(total as u32 - 1) else {
        return 1;
    };
    if last_pc as usize >= code.instructions.len() {
        return 1;
    }
    enum Outcome {
        Complete(Object),
        Prefix(Result<u64, Object>),
    }
    #[cfg(test)]
    let mut read_kinds = [0; 5];
    let outcome = (|| {
        let Pin::Obj(root) = ctx.pins.get(pin as usize)? else {
            return None;
        };
        let guards = ctx.attr_guards.get(first..end)?;
        let mut value = root;
        for guard in guards {
            let Object::Instance(inst) = value else {
                return None;
            };
            if guard.lane != JitType::Obj {
                return None;
            }
            // SAFETY: the original root pin owns the read-only graph.
            value = unsafe { chain_attr_peek(inst, guard) }.ok()?;
        }
        // Keep only a borrowed reference to the completed guarded prefix. A
        // suffix miss can transfer this exact result instead of walking twice.
        let prefix = value;
        for offset in guarded as u32..total as u32 {
            // SAFETY: exact cache hits don't call Python or mutate fields.
            let next = unsafe { cached_chain_peek(value, code, extension, jf.deopt_pc + offset) };
            let Some(next) = next else {
                // Match the ordinary prefix's result lane and identity-based
                // pin reuse. No dynamic read has been charged or committed.
                let outcome = chain_attr_value(guards.last()?, prefix, &ctx.pins)?;
                return Some(Outcome::Prefix(outcome));
            };
            #[cfg(test)]
            {
                let kind = if matches!(value, Object::Module(_)) {
                    2
                } else {
                    0
                };
                read_kinds[kind] += 1;
            }
            value = next;
        }
        if int_result == 1 && !matches!(value, Object::Int(_)) {
            return None;
        }
        // Own only the final result; no graph reference escapes this closure.
        Some(Outcome::Complete(value.clone()))
    })();
    let result = match outcome {
        Some(Outcome::Complete(result)) => result,
        Some(Outcome::Prefix(outcome)) => {
            let bits = match outcome {
                Ok(bits) => bits,
                Err(value) => {
                    if ctx.pins.len() >= RUNTIME_PIN_CAP {
                        return 1;
                    }
                    let index = ctx.pins.len();
                    ctx.pins.push(Pin::Obj(value));
                    ctx.attr_guards[end - 1].last_result_pin.set(index);
                    index as u64
                }
            };
            jf.ret_bits = bits;
            jf.ret_tag = SlotTag::ObjPin as u32;
            #[cfg(test)]
            CACHED_ATTR_PREFIX_HITS.with(|hits| hits.set(hits.get() + 1));
            return 2;
        }
        None => return 1,
    };
    // A miss must leave the ordinary helpers their original accounting.
    // Preflight before any counter or pin mutation. The usual helpers handle
    // the precise completed-result boundary if retirement would be reached.
    let dynamic_reads = (total - guarded) as u32;
    #[allow(clippy::absurd_extreme_comparisons)] // current budget is zero
    let retires = INTERP_CALL_RETIRE_BUDGET != 0
        && ctx.interp_calls.saturating_add(dynamic_reads) >= INTERP_CALL_RETIRE_BUDGET;
    if retires
        || (int_result == 0 && !matches!(result, Object::None) && ctx.pins.len() >= RUNTIME_PIN_CAP)
    {
        return 1;
    }
    // All failure conditions have been checked. Preserve saturating charges
    // once per dynamic read, without allocating an intermediate owner.
    ctx.interp_calls = ctx.interp_calls.saturating_add(dynamic_reads);
    let (bits, tag) = if int_result == 1 {
        let Object::Int(integer) = result else {
            unreachable!("validated integer result");
        };
        (integer as u64, SlotTag::Int)
    } else if matches!(result, Object::None) {
        (u64::MAX, SlotTag::ObjPin)
    } else {
        let bits = ctx.pins.len() as u64;
        ctx.pins.push(Pin::Obj(result));
        (bits, SlotTag::ObjPin)
    };
    jf.ret_bits = bits;
    jf.ret_tag = tag as u32;
    #[cfg(test)]
    {
        CACHED_ATTR_CHAIN_HITS.with(|hits| {
            let mut counts = hits.get();
            counts[int_result as usize] += 1;
            hits.set(counts);
        });
        DYN_ATTR_NATIVE_READS.with(|reads| {
            let mut counts = reads.get();
            for (count, increment) in counts.iter_mut().zip(read_kinds) {
                *count += increment;
            }
            reads.set(counts);
        });
    }
    0
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
    // The common shape first: a scalar value over a scalar field of an
    // indexed site (no write barrier, nothing to reap).
    if let (AttrStorage::Indexed(key_idx), Some(Pin::Obj(Object::Instance(inst)))) =
        (g.storage, ctx.pins.get(pin as usize))
    {
        let v = match g.lane {
            JitType::Int => Some(Object::Int(jf.ret_bits as i64)),
            JitType::Float => Some(Object::Float(f64::from_bits(jf.ret_bits))),
            JitType::Bool => Some(Object::Bool(jf.ret_bits != 0)),
            _ => None,
        };
        if let Some(v) = v {
            if attr_class_ok(inst, g.ver) {
                // SAFETY: nothing below runs code while the view is live.
                if let Some((k, dst)) = unsafe { inst.attr_peek_index_mut(key_idx as usize, true) }
                {
                    if key_is(k, &g.name)
                        && matches!(
                            dst,
                            Object::Int(_) | Object::Float(_) | Object::Bool(_) | Object::None
                        )
                    {
                        *dst = v;
                        return 0;
                    }
                }
            }
        }
    }
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
    if !attr_class_ok(inst, g.ver) {
        return 1;
    }
    match g.storage {
        // RFC 0070 WS3 — a `__slots__` member. Tier-1's
        // `StoreAttrSlot` (like the generic
        // `member_set`) neither gc-tracks nor prompt-reaps the
        // displaced slot value, so the in-place overwrite is exactly
        // faithful.
        AttrStorage::Slot(key_idx) => {
            inst.note_slot_store(&v);
            let old = {
                let mut slots = inst.slots.borrow_mut();
                match slots.get_index_mut(key_idx as usize) {
                    Some((key, slot)) if key_is(key, &g.name) => Some(std::mem::replace(slot, v)),
                    _ => slots.insert_shared(&g.name, v),
                }
            };
            drop(old);
            0
        }
        AttrStorage::Indexed(key_idx) => {
            // Replacing an existing key's value leaves the key layout (and
            // so the stamp) alone, as the interpreter's indexed store does.
            // SAFETY: nothing below runs code while the view is live.
            let Some((k, dst)) =
                (unsafe { inst.attr_peek_index_mut(key_idx as usize, v.is_gc_atomic()) })
            else {
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
            let old = std::mem::replace(dst, v);
            drop(old);
            0
        }
        // RFC 0071 WS2 — the constructor-pattern store: a single-probe
        // insert-or-replace, exactly the tier-1 `StoreAttrNewKey`
        // execution. Watched instance dicts deopt so the generic path
        // fires the exact watcher events.
        AttrStorage::NewKey => {
            use indexmap::map::raw_entry_v1::{RawEntryApiV1, RawEntryMut};
            use std::hash::BuildHasher;

            if crate::capi_watchers::dicts_active() {
                return 1;
            }
            // The split layout (see `Interpreter::core_store_new_attr`).
            let v = if inst.dict.published().is_none() {
                // SAFETY: a read between two native ops.
                if unsafe { inst.dict.split_cell().peek() }.is_none() {
                    return 1;
                }
                match inst.split_store(&g.name, v) {
                    Ok(old) => {
                        drop(old);
                        return 0;
                    }
                    Err(v) => v,
                }
            } else {
                v
            };
            let mut dict = inst.dict_cell().borrow_mut();
            let atomic = v.is_gc_atomic();
            let dict = &mut *dict;
            let dict = if atomic {
                dict.map_mut_atomic_store()
            } else {
                &mut **dict
            };
            let probe = crate::object::LeafNameProbe::new(&g.name, g.name_hash);
            let hash = crate::fasthash::FxBuildHasher.hash_one(g.name_hash);
            let entry = dict
                .raw_entry_mut_v1()
                .from_key_hashed_nocheck(hash, &probe);
            // A hash-colliding user key could run Python during equality.
            // Resume before the store so the interpreter owns that callback.
            if probe.saw_exotic() {
                return 1;
            }
            let old = match entry {
                RawEntryMut::Occupied(mut entry) => {
                    let dst = entry.get_mut();
                    // The displaced-value discipline of the indexed arm.
                    Some(std::mem::replace(dst, v))
                }
                RawEntryMut::Vacant(entry) => {
                    // The guard already owns the interned key. Reuse the
                    // probe's hash and vacant entry instead of probing again.
                    entry.insert_hashed_nocheck(hash, DictKey(Object::Str(g.name.clone())), v);
                    None
                }
            };
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
    // A list-lane global's element lane rides the high word (see
    // `weavepy_jit::global_obj_list_code`).
    let elem = weavepy_jit::global_obj_list_elem(token);
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
    match (elem, obj) {
        (Some(elem), Object::List(l)) => ctx.pins.push(Pin::List(l, elem)),
        (Some(_), _) => return -2,
        (None, obj) => ctx.pins.push(Pin::Obj(obj)),
    }
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
    int_result: bool,
) -> Option<i64> {
    // A plain function or a bound method's function: the activation's
    // last resolution when it names the same function and code (see
    // `CallCtx::dyn_callee`).
    let plain = match callee {
        Object::Function(pf) => Some((pf, None)),
        // A deferred special-method dispatch (`redispatch_descriptor`)
        // re-resolves `__get__` at call time — interpreter territory.
        Object::BoundMethod(bm) if !bm.redispatch_descriptor => match &bm.function {
            Object::Function(pf) => Some((pf, Some(bm.receiver.clone()))),
            _ => return None,
        },
        _ => None,
    };
    if let Some((pf, recv)) = plain {
        let method = recv.is_some();
        // SAFETY: GIL-serialized raw read of the function's code cell;
        // only the pointer is compared.
        let code_ptr = unsafe { Rc::as_ptr(&*pf.code.as_ptr()) } as usize;
        let key = (Rc::as_ptr(pf) as usize, code_ptr, method);
        let nc = match ctx.dyn_callee.take() {
            Some((f, c, m, nc)) if (f, c, m) == key => nc,
            _ => {
                let fcode = pf.code.borrow().clone();
                JIT.with(|c| c.borrow().resolve_native_func(pf, &fcode, method))?
            }
        };
        // SAFETY: the resolved owners outlive the call, and the same
        // initialized argument-buffer contract applies to the shared
        // entry path.
        let r = unsafe { enter_dyn_native(jf, ctx, interp, &nc, argc, recv.as_ref(), int_result) };
        ctx.dyn_callee = Some((key.0, key.1, key.2, nc));
        return r;
    }
    let (nc, recv) = match callee {
        Object::Type(t) => {
            // A metaclass's `__call__` decides what calling the class
            // does: only `type`'s own is the constructor pipeline.
            if !t.metaclass_is_type() {
                return None;
            }
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
    // SAFETY: the resolved owners outlive the call, and the same initialized
    // argument-buffer contract applies to the shared entry path.
    unsafe { enter_dyn_native(jf, ctx, interp, &nc, argc, recv.as_ref(), int_result) }
}

/// Enter an already resolved dynamic callee and adapt its return lane.
///
/// # Safety
///
/// The owners and initialized buffers required by `try_dyn_native` remain
/// live. No JIT cache borrow may cross this call: guards can invoke Python.
unsafe fn enter_dyn_native(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    nc: &NativeCallee,
    argc: u32,
    recv: Option<&Object>,
    int_result: bool,
) -> Option<i64> {
    if nc.ctor.is_some() {
        // The constructor form allocates the instance and enters the
        // compiled `__init__`; the site's value is the instance pin.
        let status = unsafe { try_native_ctor(jf, ctx, interp, nc, argc, SlotTag::ObjPin as u32) }?;
        return Some(if status == CallStatus::Ok as i64 {
            finish_dyn_native_result(jf, ctx, int_result)
        } else {
            status
        });
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
    let status = unsafe { try_native_call(jf, ctx, interp, nc, argc, expect, recv) }?;
    Some(if status == CallStatus::Ok as i64 {
        finish_dyn_native_result(jf, ctx, int_result)
    } else {
        status
    })
}

/// Convert a completed native return to the generic call site's result
/// lane. Existing pins remain owned by the activation, even when an integer
/// is read from one; no local alias can observe a retired or reused pin.
#[inline(always)]
fn finish_dyn_native_result(jf: &mut JitFrame, ctx: &mut CallCtx, int_result: bool) -> i64 {
    if int_result {
        if jf.ret_tag == SlotTag::Int as u32 {
            return CallStatus::Ok as i64;
        }
        if jf.ret_tag == SlotTag::ObjPin as u32 {
            if let Some(Pin::Obj(Object::Int(value))) = ctx.pins.get(jf.ret_bits as usize) {
                jf.ret_bits = *value as u64;
                jf.ret_tag = SlotTag::Int as u32;
                return CallStatus::Ok as i64;
            }
        }
        ctx.parked = Some(unpack_pins(jf.ret_bits, jf.ret_tag, &ctx.pins));
        return CallStatus::Boxed as i64;
    }
    if jf.ret_tag != SlotTag::ObjPin as u32 {
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
                return CallStatus::Boxed as i64;
            }
        }
    }
    CallStatus::Ok as i64
}

enum BorrowedScalarCall {
    Completed(u64, u32),
    Resolved(NativeCallee),
    RetryPython,
}

/// Resolve a dynamic function once, borrowing exact-arity scalar leaves
/// without constructing an owning callee bundle. Other eligible functions
/// return an owning resolution for the ordinary native call machinery.
/// The activation pin holds the function; the borrowed thread-local
/// cache holds its code and native artifacts. Only guard-free scalar leaves
/// qualify: even namespace guard lookups can invoke Python for exotic keys.
/// These leaves cannot call Python, use contextual helpers, or mutate the pin
/// table. Every borrow ends before a result is pinned or the interpreter
/// handles a declined call.
///
/// # Safety
///
/// The live activation and initialized argument-buffer contract of
/// `wpjit_call_dyn` applies. This helper never publishes or mutates the caller's
/// pins, and only enters the engine's certified scalar-leaf subset.
unsafe fn try_borrowed_dyn_scalar(
    jf: &JitFrame,
    ctx: &CallCtx,
    interp: &mut super::Interpreter,
    callee_pin: i64,
    argc: u32,
) -> Option<BorrowedScalarCall> {
    let Some(Pin::Obj(Object::Function(func))) = ctx.pins.get(callee_pin as usize) else {
        return None;
    };
    // Do not hold a function-code borrow across the GIL checkpoint.
    let key = Rc::as_ptr(&func.code.borrow());
    JIT.with(|cell| {
        let state = cell.borrow();
        let Some(entry) = state.cache.get(&key) else {
            return Some(BorrowedScalarCall::RetryPython);
        };
        let Tier::Compiled(art) = &entry.tier else {
            return Some(BorrowedScalarCall::RetryPython);
        };
        let (cf, code) = (&art.cf, &art.code);
        if !native_callable(cf, code) {
            return Some(BorrowedScalarCall::RetryPython);
        }
        if !cf.is_scalar_leaf()
            || !art.snap.entries.is_empty()
            || !art.callees.is_empty()
            || !art.math.is_empty()
            || cf.n_locals as usize > SCALAR_LEAF_SLOTS
            || cf.max_stack as usize >= SCALAR_LEAF_SLOTS
            || code.arg_count != argc
        {
            // Reuse this lookup for the ordinary native call. Construct
            // owners here, then release the cache borrow before any guard
            // lookup, default binding, framed call, or Python fallback.
            #[cfg(test)]
            RESOLVED_DYN_CALLS.with(|count| count.set(count.get() + 1));
            return Some(BorrowedScalarCall::Resolved(NativeCallee {
                art: art.clone(),
                func: func.clone(),
                code: code.clone(),
                ctor: None,
            }));
        }
        // The cache is this thread's, and this checkpoint runs no Python.
        // Another thread may replace the function's code while it holds the
        // GIL, so validate the live identity again after reacquiring it.
        interp.gil_countdown = interp.gil_countdown.wrapping_sub(1);
        if interp.gil_countdown == 0 {
            interp.gil_countdown = crate::gil::GIL_CHECK_INTERVAL;
            crate::gil::yield_checkpoint();
        }
        if crate::hot_gates::load() != 0
            || crate::trace::any_observers_active()
            || code.jit_hint.is_not_jitable()
            || !Rc::ptr_eq(&func.code.borrow(), code)
        {
            return Some(BorrowedScalarCall::RetryPython);
        }
        for j in 0..argc as usize {
            let Some(lane) = cf.local_types.get(j).copied().flatten() else {
                return Some(BorrowedScalarCall::RetryPython);
            };
            // SAFETY: the caller initialized argc argument tags.
            if unsafe { *jf.call_tags.add(j) } != lane_tag(lane) {
                return Some(BorrowedScalarCall::RetryPython);
            }
        }
        let _guard = match crate::recursion::enter_with(ctx.depth_cell) {
            crate::recursion::Enter::Ok(guard) => guard,
            crate::recursion::Enter::Overflow => return Some(BorrowedScalarCall::RetryPython),
        };
        native_stat(|stats| {
            stats.calls.set(stats.calls.get() + 1);
            stats
                .scalar_leaf_calls
                .set(stats.scalar_leaf_calls.get() + 1);
        });
        #[cfg(test)]
        BORROWED_DYN_SCALAR_CALLS.with(|count| count.set(count.get() + 1));
        // SAFETY: certification, sizes, scalar argument lanes, exact arity,
        // guards, and recursion were checked above. The cache borrow keeps
        // the compiled entry alive; this body cannot reenter the interpreter.
        let enter = || unsafe { enter_scalar_leaf(cf, func, code, jf, argc as usize) };
        let result = if crate::stdlib::greenlet_native::on_greenlet_stack() {
            enter()
        } else {
            stacker::maybe_grow(512 * 1024, 8 * 1024 * 1024, enter)
        };
        Some(match result {
            Some((bits, tag)) => BorrowedScalarCall::Completed(bits, tag),
            None => {
                // Only pure numeric work ran. Release the cache borrow and
                // recursion guard before replaying it in the interpreter.
                native_stat(|stats| stats.deopts.set(stats.deopts.get() + 1));
                BorrowedScalarCall::RetryPython
            }
        })
    })
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
    // SAFETY: the wrapper preserves the live-buffer contract above.
    unsafe { call_dyn_impl(frame, callee_pin, argc, kwc, names, false) }
}

/// Generic call with an immediately consumed exact-integer result.
///
/// # Safety
///
/// Same live activation and initialized marshal buffers as [`wpjit_call_dyn`].
unsafe extern "C" fn wpjit_call_dyn_int(
    frame: *mut JitFrame,
    callee_pin: i64,
    argc: u32,
    kwc: u32,
    names: u32,
) -> i64 {
    // SAFETY: the wrapper preserves the same live-buffer contract.
    unsafe { call_dyn_impl(frame, callee_pin, argc, kwc, names, true) }
}

#[inline(always)]
unsafe fn call_dyn_impl(
    frame: *mut JitFrame,
    callee_pin: i64,
    argc: u32,
    kwc: u32,
    names: u32,
    int_result: bool,
) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` that entered native code is
    // dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    let retry_python = if kwc == 0 {
        // SAFETY: the same live buffers; the borrowed path releases every
        // borrow before this code updates the result or enters Python.
        match unsafe { try_borrowed_dyn_scalar(jf, ctx, interp, callee_pin, argc) } {
            Some(BorrowedScalarCall::Completed(bits, tag)) => {
                jf.ret_bits = bits;
                jf.ret_tag = tag;
                return finish_dyn_native_result(jf, ctx, int_result);
            }
            Some(BorrowedScalarCall::RetryPython) => true,
            Some(BorrowedScalarCall::Resolved(nc)) => {
                // SAFETY: resolution released its cache borrow, and this
                // activation's argument buffers remain initialized.
                if let Some(status) =
                    unsafe { enter_dyn_native(jf, ctx, interp, &nc, argc, None, int_result) }
                {
                    return status;
                }
                true
            }
            None => false,
        }
    } else {
        false
    };
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
    if kwc == 0 && !retry_python {
        // SAFETY: per the function contract — same live buffers.
        if let Some(status) = unsafe { try_dyn_native(jf, ctx, interp, &callee, argc, int_result) }
        {
            return status;
        }
    }
    // A keyword call of a warm pure leaf evaluates frameless on its
    // marshaled values (no Python runs, so nothing needs revalidating).
    if kwc > 0 {
        // SAFETY: per the function contract — same live buffers.
        if let Some(v) = unsafe { dyn_kw_pure_leaf(jf, ctx, interp, &callee, argc, kwc, names) } {
            return dyn_leaf_result(jf, ctx, v, int_result);
        }
    }
    // A keyword call of a plain function whose site carries the
    // interpreter's `CallPyKwNames` permutation binds through it, as the
    // `CALL_KW` handler's hit does: no name strings, no generic binder.
    // SAFETY: per the function contract — same live buffers.
    if let Some((f, locals)) = (kwc > 0)
        .then(|| unsafe { dyn_kw_site_bind(jf, ctx, &callee, argc, kwc, names) })
        .flatten()
    {
        // A pure-leaf callee evaluates frameless on the bound locals.
        if let Some(v) = interp.bound_leaf_eval(&f, &locals) {
            interp.recycle_scratch(locals);
            // SAFETY: as above.
            return unsafe { dyn_call_result(jf, ctx, Ok(v), false, false, int_result) };
        }
        // Not charged against the native driver: the interpreter's own
        // `CALL_KW` binds through this same permutation and activation,
        // so tier-1 would not run the call any cheaper.
        ctx.dirty = true;
        let called = call_with_activation_shell(interp, ctx, jf, |i| i.run_py_bound(&f, locals));
        // SAFETY: as above.
        return unsafe { dyn_call_result(jf, ctx, called, false, false, int_result) };
    }
    let n = (argc + kwc) as usize;
    let mut args: Vec<Object> = Vec::with_capacity(n);
    for j in 0..n {
        // SAFETY: native code wrote `argc + kwc` entries, and the
        // buffers are `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        args.push(unpack_pins(bits, tag, &ctx.pins));
    }
    // A builtin iterator type or an exact `itertools` class whose native
    // adapter builds without running code (`zip(xs, ys)`,
    // `enumerate(xs)`, `itertools.repeat(x, n)`).
    if kwc == 0 {
        if let Object::Type(t) = &callee {
            let built = if t.flags.is_builtin {
                crate::seqiter::builtin_ctor_pure(t, &args)
            } else if t.lazy_ctor.get() != 0 {
                crate::stdlib::itertools_mod::native_new_pure(t, &args)
            } else {
                None
            };
            if let Some(v) = built {
                return dyn_leaf_result(jf, ctx, v, int_result);
            }
        }
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
    // WS1's dirtiness discipline). A keyword call pays the generic
    // binder in tier-1 too, so only positional calls count against the
    // native driver.
    let charged = kwc == 0;
    if charged {
        note_generic_dyn_call(ctx);
    }
    ctx.dirty = true;
    let native_callee = matches!(&callee, Object::Builtin(_))
        || matches!(&callee, Object::BoundMethod(bm) if matches!(bm.function, Object::Builtin(_)));
    let called = call_with_activation_shell(interp, ctx, jf, |i| {
        i.call_object_with_globals(&callee, &args, &kwargs, &ctx.globals)
    });
    // SAFETY: as above.
    unsafe { dyn_call_result(jf, ctx, called, native_callee, charged, int_result) }
}

/// `call_dyn_impl`'s result protocol: park a raise, or deliver the
/// result unboxed (`int_result`) or pinned, parking it (`Boxed`) when a
/// round-trip charge or an invalidated guard ends the activation. An
/// uncharged call (one tier-1 would not run any cheaper) leaves the
/// native driver's call density alone.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`].
unsafe fn dyn_call_result(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    called: Result<Object, RuntimeError>,
    native_callee: bool,
    charged: bool,
    int_result: bool,
) -> i64 {
    // SAFETY: the `&mut Interpreter` that entered native code is
    // dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    match called {
        Err(err) => {
            ctx.raised = Some(err);
            CallStatus::Raised as i64
        }
        Ok(v) => {
            if charged && !native_callee {
                ctx.dyn_py_calls = ctx.dyn_py_calls.saturating_add(1);
            }
            if charge_roundtrip(ctx) || (charged && native_callee && charge_native_roundtrip(ctx)) {
                ctx.parked = Some(v);
                return CallStatus::Boxed as i64;
            }
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            );
            if still_valid {
                if int_result {
                    if let Object::Int(value) = &v {
                        jf.ret_bits = *value as u64;
                        jf.ret_tag = SlotTag::Int as u32;
                        return CallStatus::Ok as i64;
                    }
                } else if let Some(bits) = pin_any(v.clone(), &mut ctx.pins) {
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

/// A dynamic keyword call of a warm pure-leaf function (or a bound method
/// over one), evaluated frameless on the marshaled operands through the
/// call site's `CallPyKwNames` permutation (see
/// [`super::Interpreter::kw_pure_leaf_call`]): its result, or `None`
/// having done nothing observable.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]; the call site's pc is in
/// `jf.deopt_pc` (stored before every call helper runs).
unsafe fn dyn_kw_pure_leaf(
    jf: &JitFrame,
    ctx: &CallCtx,
    interp: &super::Interpreter,
    callee: &Object,
    argc: u32,
    kwc: u32,
    names: u32,
) -> Option<Object> {
    const MAX: usize = 9;
    // SAFETY: per the function contract, the activation keeps its code
    // object alive.
    let code = unsafe { &*ctx.code_ptr };
    let Some(Object::Tuple(name_items)) = crate::code_const_objects(code).get(names as usize)
    else {
        return None;
    };
    let (f, recv) = match callee {
        Object::Function(f) => (f, None),
        Object::BoundMethod(bm) if !bm.redispatch_descriptor => match &bm.function {
            Object::Function(f) => (f, Some(&bm.receiver)),
            _ => return None,
        },
        _ => return None,
    };
    let n = (argc + kwc) as usize;
    let eff_argc = argc as usize + usize::from(recv.is_some());
    if n + usize::from(recv.is_some()) > MAX || name_items.len() != kwc as usize {
        return None;
    }
    let mut vals: [Object; MAX] = std::array::from_fn(|_| Object::None);
    let offset = usize::from(recv.is_some());
    if let Some(r) = recv {
        vals[0] = r.clone();
    }
    for j in 0..n {
        // SAFETY: native code wrote `argc + kwc` entries, and the buffers
        // are `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        vals[offset + j] = unpack_pins(bits, tag, &ctx.pins);
    }
    interp.kw_pure_leaf_call(
        code,
        jf.deopt_pc as usize,
        f,
        name_items,
        &vals[..offset + n],
        eff_argc,
        ctx.depth_cell,
    )
}

/// Deliver a pure leaf's result at a dynamic call site: it ran no Python,
/// so the activation's guards still hold.
fn dyn_leaf_result(jf: &mut JitFrame, ctx: &mut CallCtx, v: Object, int_result: bool) -> i64 {
    if int_result {
        if let Object::Int(value) = &v {
            jf.ret_bits = *value as u64;
            jf.ret_tag = SlotTag::Int as u32;
            return CallStatus::Ok as i64;
        }
    } else if let Some(bits) = pin_any(v.clone(), &mut ctx.pins) {
        jf.ret_bits = bits;
        jf.ret_tag = SlotTag::ObjPin as u32;
        return CallStatus::Ok as i64;
    }
    ctx.parked = Some(v);
    CallStatus::Boxed as i64
}

/// Bind a dynamic keyword call's marshaled operands for a plain function
/// (or a bound method over one) through the call site's cached
/// `CallPyKwNames` permutation, re-verified against the live function
/// (`Interpreter::kw_names_bind_check`). Returns the function and its
/// bound locals, or `None` (nothing consumed) for the generic binder.
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]; the call site's pc is in
/// `jf.deopt_pc` (stored before every call helper runs).
unsafe fn dyn_kw_site_bind(
    jf: &mut JitFrame,
    ctx: &CallCtx,
    callee: &Object,
    argc: u32,
    kwc: u32,
    names: u32,
) -> Option<(Rc<crate::object::PyFunction>, Vec<Object>)> {
    use weavepy_compiler::InlineCache as IC;
    // SAFETY: per the function contract, the activation keeps its code
    // object alive.
    let code = unsafe { &*ctx.code_ptr };
    let IC::CallPyKwNames {
        func_id,
        perm,
        argc: ca,
        kwc: ck,
    } = code.caches.get(jf.deopt_pc)
    else {
        return None;
    };
    let Some(Object::Tuple(name_items)) = crate::code_const_objects(code).get(names as usize)
    else {
        return None;
    };
    let (f, recv) = match callee {
        Object::Function(f) => (f, None),
        Object::BoundMethod(bm) => match &bm.function {
            Object::Function(f) => (f, Some(&bm.receiver)),
            _ => return None,
        },
        _ => return None,
    };
    let (argc, kwc) = (argc as usize, kwc as usize);
    let eff_argc = argc + usize::from(recv.is_some());
    if ca as usize != eff_argc || ck as usize != kwc || name_items.len() != kwc {
        return None;
    }
    let (fcode, covered) =
        crate::Interpreter::kw_names_bind_check(f, func_id, perm, name_items, eff_argc)?;
    // SAFETY: the `&mut Interpreter` that entered native code is dormant
    // while the helper runs; only its vector pools are used here.
    let interp = unsafe { &*ctx.interp };
    let mut staged = interp.pooled_scratch();
    staged.extend(recv.cloned());
    for j in 0..argc + kwc {
        // SAFETY: native code wrote `argc + kwc` entries, and the
        // buffers are `max_call_args` wide.
        let (bits, tag) = unsafe { (*jf.call_args.add(j), *jf.call_tags.add(j)) };
        staged.push(unpack_pins(bits, tag, &ctx.pins));
    }
    let mut locals = interp.pooled_scratch();
    crate::Interpreter::kw_names_fill_locals(
        &mut locals,
        &mut staged,
        name_items,
        f,
        &fcode,
        perm,
        covered,
        kwc,
        eff_argc,
    );
    interp.recycle_scratch(staged);
    Some((f.clone(), locals))
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

#[cfg(test)]
thread_local! {
    static DYN_ATTR_NATIVE_READS: Cell<[u64; 5]> = const { Cell::new([0; 5]) };
}

#[cfg(test)]
pub(crate) fn dyn_attr_native_reads_for_test() -> [u64; 5] {
    DYN_ATTR_NATIVE_READS.with(Cell::get)
}

/// Read an object-lane field using the interpreter's callback-free caches.
/// An unproved read returns 3 before it can run a callback, so native code
/// materializes the original receiver and current locals and retries this
/// exact instruction. Simple getter paths can finish in the borrowed evaluator.
/// Unproved getters run with a real caller frame and write-through locals.
/// Success pins the same result as the generic lane; cap pressure parks that
/// completed result and returns 2. No new object lanes or pin reuse are added.
///
/// # Safety
/// Same live-buffer and GIL contract as [`wpjit_call_py`]. The lowering must
/// publish this read's bytecode PC in `deopt_pc` before invoking the helper.
unsafe extern "C" fn wpjit_dyn_attr_get(frame: *mut JitFrame, pin: i64, name: i64) -> i64 {
    if crate::gil::free_threading_enabled() {
        return 3;
    }
    // SAFETY: the native entry owns these live buffers. Its erased context
    // pointer originated from an aligned CallCtx, as in the other helpers.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // An object pin already owns the receiver for the entire callback-free
    // lookup. Only the specialized list pin needs a temporary Object wrapper.
    let temporary_receiver;
    let receiver = match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(receiver)) => receiver,
        Some(pin) => {
            temporary_receiver = pin.to_object();
            &temporary_receiver
        }
        None => return 3,
    };
    let Ok(name_idx) = u32::try_from(name) else {
        return 3;
    };
    // SAFETY: the activation retains the code object until native return.
    let code = unsafe { &*ctx.code_ptr };
    let (value, kind) = match receiver {
        Object::Instance(inst) if inst.cls_raw().native_kind.get() == 0 => (
            // The site's split-layout shortcut first (the interpreter's
            // own fastest read), then its inline cache.
            super::code_vm_ext(code)
                // SAFETY: the view is cloned before anything else runs.
                .and_then(|ext| unsafe { super::field_slot_hit(ext, jf.deopt_pc as usize, inst) })
                .map(super::clone_hot)
                .or_else(|| {
                    super::Interpreter::leaf_load_attr_recv(code, receiver, jf.deopt_pc, name_idx)
                }),
            0,
        ),
        Object::Type(cls) if super::Interpreter::plain_metaclass(cls) => (
            super::Interpreter::leaf_load_type_attr(code, cls, jf.deopt_pc, name_idx),
            1,
        ),
        Object::Module(module) if crate::object::module_class(module).is_none() => (
            super::Interpreter::leaf_load_attr_recv(code, receiver, jf.deopt_pc, name_idx),
            2,
        ),
        // Exact built-in values have no instance overrides. Their method
        // table only constructs native callables; binding doesn't invoke
        // them. Keep method capture (including dict.items and fromkeys)
        // native without admitting arbitrary descriptor execution.
        Object::List(_)
        | Object::Dict(_)
        | Object::Tuple(_)
        | Object::Str(_)
        | Object::Bytes(_)
        | Object::ByteArray(_)
        | Object::Set(_)
        | Object::FrozenSet(_)
        | Object::Range(_)
        | Object::Int(_)
        | Object::Long(_)
        | Object::Bool(_)
        | Object::Float(_)
        | Object::Complex(_) => {
            let Some(name) = code.names.get(name_idx as usize) else {
                return 3;
            };
            let method = crate::builtins::lookup_method(receiver, name).map(|method| {
                if matches!(&method, Object::Builtin(b) if !b.binds_instance) {
                    method
                } else {
                    Object::BoundMethod(Rc::new(crate::object::BoundMethod::new(
                        receiver.clone(),
                        method,
                    )))
                }
            });
            (method, 3)
        }
        _ => return 3,
    };
    let (value, kind) = match value {
        Some(value) => (value, kind),
        None => {
            // SAFETY: the entering interpreter is dormant. The borrowed
            // evaluator neither calls Python nor mutates interpreter state.
            let interp = unsafe { &*ctx.interp };
            // SAFETY: the activation retains this thread's depth cell.
            let depth = unsafe { (*ctx.depth_cell).get() };
            let Some(value) = interp.leaf_getter_read(code, receiver, name_idx, depth) else {
                return 3;
            };
            (value, 4)
        }
    };
    // Retain the existing roundtrip and pin-cap behavior. No Python ran, so
    // this read cannot invalidate the activation's globals or callees and
    // doesn't mark the context dirty or recheck those unrelated guards.
    if charge_roundtrip(ctx) {
        ctx.parked = Some(value);
        return 2;
    }
    // Consume the completed value once. On cap pressure retain that same
    // value for the interpreter; None still uses its allocation-free sentinel.
    let bits = match value {
        Object::None => u64::MAX,
        value if ctx.pins.len() < RUNTIME_PIN_CAP => {
            let index = ctx.pins.len();
            ctx.pins.push(Pin::Obj(value));
            index as u64
        }
        value => {
            ctx.parked = Some(value);
            return 2;
        }
    };
    jf.ret_bits = bits;
    #[cfg(test)]
    DYN_ATTR_NATIVE_READS.with(|reads| {
        let mut counts = reads.get();
        counts[kind] += 1;
        reads.set(counts);
    });
    #[cfg(not(test))]
    let _ = kind;
    0
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
    let Ok(name_idx) = u32::try_from(name) else {
        return 3;
    };
    // SAFETY: the activation keeps its code object alive. Own the shared
    // name before dispatch, since a user hook may replace its function's code.
    let code = unsafe { &*ctx.code_ptr };
    let Some(Object::Str(attr)) = super::code_name_obj(code, name_idx) else {
        return 3;
    };
    let attr = attr.clone();
    // SAFETY: native code staged the value in slot 0.
    let (bits, tag) = unsafe { (*jf.call_args, *jf.call_tags) };
    let value = unpack_pins(bits, tag, &ctx.pins);
    // A plain instance whose site cache proves an ordinary field store
    // (the interpreter's core-loop store): no Python runs, so neither the
    // dirtiness discipline nor a guard recheck applies.
    if let Object::Instance(inst) = &recv {
        if !crate::gil::free_threading_enabled()
            && !matches!(value, Object::BoundMethod(_))
            && super::Interpreter::core_store_attr(
                code,
                inst,
                jf.deopt_pc as usize,
                name_idx,
                &value,
            )
        {
            // The store moved the value's bits into the instance.
            std::mem::forget(value);
            return 0;
        }
    }
    ctx.dirty = true;
    match interp.store_attr_shared_name(&recv, &attr, value) {
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
    // A registered native `__bool__`/`__len__` (or neither): the
    // interpreter's cached answer, which runs no Python (read through the
    // pin: the table owns the object throughout).
    if let Some(Pin::Obj(obj @ Object::Instance(_))) = ctx.pins.get(pin as usize) {
        if !crate::gil::free_threading_enabled() {
            if let Some(b) = interp.leaf_instance_truth(obj) {
                jf.ret_bits = u64::from(b);
                return 0;
            }
        }
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
    // A Python `__bool__`/`__len__` may inspect its caller's frame, whose
    // locals live lane-packed in the native activation: the interpreter
    // re-executes the test with a materialized frame.
    if matches!(v, Object::Instance(_)) {
        return 3;
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

/// The two operands a generic operation staged in marshal slots 0 and 1.
///
/// # Safety
///
/// Native code staged both entries and their tags.
unsafe fn dyn_operands(jf: &JitFrame, ctx: &CallCtx) -> (Object, Object) {
    // SAFETY: per the function contract.
    let (a_bits, a_tag, b_bits, b_tag) = unsafe {
        (
            *jf.call_args,
            *jf.call_tags,
            *jf.call_args.add(1),
            *jf.call_tags.add(1),
        )
    };
    (
        unpack_pins(a_bits, a_tag, &ctx.pins),
        unpack_pins(b_bits, b_tag, &ctx.pins),
    )
}

/// Deliver a generic operation's completed result `v` as an object pin
/// (status `0`), or park it and deopt after the operation (`2`) when it
/// invalidated a guard or the pin table is full.
fn deliver_dyn_result(
    jf: &mut JitFrame,
    ctx: &mut CallCtx,
    interp: &mut super::Interpreter,
    v: Object,
) -> i64 {
    let still_valid = guards_hold(
        interp,
        &ctx.globals,
        &ctx.builtins,
        &ctx.guard_snapshot,
        &ctx.callees,
        &ctx.math,
    );
    if still_valid {
        if ctx.temporary_pin_limit_reached() {
            // Leave to reclaim the temporaries (see `relax_pin_limit`).
            ctx.pin_pressure_exit = true;
        } else if let Some(bits) = pin_any(v.clone(), &mut ctx.pins) {
            jf.ret_bits = bits;
            return 0;
        }
    }
    ctx.parked = Some(v);
    2
}

/// The `wpjit_dyn_binop` helper (`weavepy_jit::TOp::DynBinary`): run the
/// interpreter's own `BINARY_OP` dispatch for `arg` (the operator and the
/// in-place flag) on the two staged operands — the full `__op__` /
/// `__rop__` protocol, which may run Python code — and hand back the
/// result as an object pin. Status `1` raised (the error parked on the
/// context); `2` completed but parked (deopt after the operation).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]; both operands are staged.
unsafe extern "C" fn wpjit_dyn_binop(frame: *mut JitFrame, arg: i64, _unused: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // SAFETY: native code staged both operands.
    let (a, b) = unsafe { dyn_operands(jf, ctx) };
    let arg = arg as u32;
    // SAFETY: `BinOpKind` is `repr(u8)` and the compiler only emits valid
    // kinds (the interpreter's `binary_op_step` decodes it the same way).
    let kind: weavepy_compiler::BinOpKind = unsafe { std::mem::transmute(arg as u8) };
    let globals = ctx.globals.clone();
    ctx.dirty = true;
    let r = if arg & weavepy_compiler::BINARY_OP_INPLACE_FLAG != 0 {
        interp.dispatch_inplace_op(&a, &b, kind, &globals)
    } else {
        interp.dispatch_binary_op(&a, &b, kind, &globals)
    };
    match r {
        Err(err) => {
            ctx.raised = Some(err);
            1
        }
        Ok(v) => {
            interp.record_alloc(&v);
            deliver_dyn_result(jf, ctx, interp, v)
        }
    }
}

/// The `wpjit_dyn_getitem` helper (`weavepy_jit::TOp::DynGetItem`): the
/// interpreter's own subscript (a user `__getitem__`, `__missing__`, a
/// class's `__class_getitem__`) on the two staged operands, its result
/// handed back as an object pin. Statuses as [`wpjit_dyn_binop`].
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]; both operands are staged.
unsafe extern "C" fn wpjit_dyn_getitem(frame: *mut JitFrame, _arg: i64, _unused: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // SAFETY: native code staged both operands.
    let (container, index) = unsafe { dyn_operands(jf, ctx) };
    ctx.dirty = true;
    match interp.subscr_get_public(&container, &index) {
        Err(err) => {
            ctx.raised = Some(err);
            1
        }
        Ok(v) => deliver_dyn_result(jf, ctx, interp, v),
    }
}

/// The `wpjit_dyn_setitem` helper (`weavepy_jit::TOp::DynSetItem`): the
/// interpreter's own item store on the staged value, container, and
/// index. Status `0` stored; `1` raised; `2` stored but a guard broke
/// (deopt after the store, with no result to place).
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]; three operands are staged.
unsafe extern "C" fn wpjit_dyn_setitem(frame: *mut JitFrame, _arg: i64, _unused: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // SAFETY: native code staged three operands.
    let [value, container, index] = std::array::from_fn(|k| unsafe {
        unpack_pins(*jf.call_args.add(k), *jf.call_tags.add(k), &ctx.pins)
    });
    ctx.dirty = true;
    match interp.subscr_set_public(&container, &index, value) {
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
                ctx.parked = None;
                2
            }
        }
    }
}

/// The `wpjit_dyn_unary` helper (`weavepy_jit::TOp::DynUnary`): the
/// interpreter's unary operation for the `UNARY_OP` oparg `arg` (an
/// operand's `__neg__`, say) on the staged operand. Statuses as
/// [`wpjit_dyn_binop`].
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]; one operand is staged.
unsafe extern "C" fn wpjit_dyn_unary(frame: *mut JitFrame, arg: i64, _unused: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // SAFETY: native code staged the operand.
    let v = unsafe { unpack_pins(*jf.call_args, *jf.call_tags, &ctx.pins) };
    // SAFETY: `UnaryKind` is `repr(u8)` and the compiler only emits
    // valid kinds (the interpreter decodes `UNARY_OP` the same way).
    let kind: weavepy_compiler::UnaryKind = unsafe { std::mem::transmute(arg as u8) };
    ctx.dirty = true;
    match interp.op_unary(&v, kind) {
        Err(err) => {
            ctx.raised = Some(err);
            1
        }
        Ok(r) => deliver_dyn_result(jf, ctx, interp, r),
    }
}

/// The `wpjit_dyn_compare` helper (`weavepy_jit::TOp::DynCompare`): the
/// interpreter's rich comparison for `arg` on the two staged operands. With
/// the oparg's to-`bool` flag the result is its truth (`0`/`1` in
/// `ret_bits`), as `COMPARE_OP` computes it; otherwise an object pin.
/// Statuses as [`wpjit_dyn_binop`].
///
/// # Safety
///
/// Same contract as [`wpjit_call_py`]; both operands are staged.
unsafe extern "C" fn wpjit_dyn_compare(frame: *mut JitFrame, arg: i64, _unused: i64) -> i64 {
    // SAFETY: see wpjit_call_py — same live-buffer contract.
    let jf = unsafe { &mut *frame };
    #[allow(clippy::cast_ptr_alignment)]
    let ctx = unsafe { &mut *jf.ctx.cast::<CallCtx>() };
    // SAFETY: the `&mut Interpreter` is dormant while the helper runs.
    let interp = unsafe { &mut *ctx.interp };
    // SAFETY: native code staged both operands.
    let (a, b) = unsafe { dyn_operands(jf, ctx) };
    let arg = arg as u32;
    let to_bool = arg & weavepy_compiler::COMPARE_OP_TO_BOOL_FLAG != 0;
    // SAFETY: `CompareKind` is `repr(u8)`; decoded as `compare_op_step`
    // decodes it.
    let kind: weavepy_compiler::CompareKind =
        unsafe { std::mem::transmute((arg & !weavepy_compiler::COMPARE_OP_TO_BOOL_FLAG) as u8) };
    let globals = ctx.globals.clone();
    ctx.dirty = true;
    let r = interp.rich_compare_obj(&a, &b, kind, &globals);
    let r = match r {
        Ok(v) if to_bool => match v {
            Object::Bool(t) => Ok(t),
            Object::Instance(_) | Object::MappingProxyObj(_) => interp.obj_truthy(&v, &globals),
            other => Ok(other.is_truthy()),
        }
        .map(Object::Bool),
        other => other,
    };
    match r {
        Err(err) => {
            ctx.raised = Some(err);
            1
        }
        Ok(Object::Bool(t)) if to_bool => {
            let still_valid = guards_hold(
                interp,
                &ctx.globals,
                &ctx.builtins,
                &ctx.guard_snapshot,
                &ctx.callees,
                &ctx.math,
            );
            if still_valid {
                jf.ret_bits = u64::from(t);
                0
            } else {
                ctx.parked = Some(Object::Bool(t));
                2
            }
        }
        Ok(v) => deliver_dyn_result(jf, ctx, interp, v),
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
    // An instance whose class's `__iter__` is a registered native leaf
    // (`deque.__iter__`) builds its iterator without Python and without
    // effects: a failure deopts for the interpreter to raise it.
    if let Object::Instance(_) = &recv {
        if crate::gil::free_threading_enabled() {
            return -1;
        }
        let Some(b) = interp.leaf_instance_dunder(&recv, "__iter__") else {
            return -1;
        };
        return match (b.call)(std::slice::from_ref(&recv)) {
            Ok(it) => {
                let idx = ctx.pins.len() as i64;
                ctx.pins.push(Pin::Obj(it));
                idx
            }
            Err(_) => -1,
        };
    }
    // `make_iter` dispatches Python for exactly these receiver
    // shapes; a fresh compile would double `__iter__`'s side effects
    // on a post-hoc deopt, so they never enter the helper.
    if matches!(recv, Object::Type(_) | Object::MappingProxyObj(_)) {
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
fn pack_iter_elem(
    v: &Object,
    tag: i64,
    pins: &mut PinTable,
    memo: &mut [u32; PIN_MEMO],
) -> Option<u64> {
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
        SlotTag::ObjPin => match v {
            // Generic pair loops can keep primitive values boxed, just
            // like opaque call results. These immutable leaves can't
            // retain user finalizers while the activation holds its pins.
            // Keep the existing fallback for other non-instance objects.
            Object::Int(_)
            | Object::Long(_)
            | Object::Float(_)
            | Object::Bool(_)
            | Object::Str(_)
            | Object::WStr(_)
            | Object::Bytes(_)
            // Containers too (a list of rows, a list of pairs): the pin
            // shares them with the iterable they came from.
            | Object::Tuple(_)
            | Object::List(_)
            | Object::Dict(_) => pin_any(v.clone(), pins),
            _ => obj_ret_bits(v, pins, memo),
        },
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
enum NativeBytePair {
    Unsupported,
    Exhausted,
    Value(i64, u8),
}

/// Read exact native byte enumeration without constructing a Python tuple.
/// Keep both shared cursors current so an interpreter continuation sees the
/// same state. Unsupported shapes and counter overflow consume nothing.
#[inline]
fn next_enumerated_byte(it: &Object) -> NativeBytePair {
    let Object::Iter(it) = it else {
        return NativeBytePair::Unsupported;
    };
    // Guard-free while cells are unshared: this step runs no code and
    // touches no cell besides these two, so neither view can meet a
    // conflicting borrow before it ends with this function.
    // SAFETY: as above.
    if let Some(state) = unsafe { it.peek_mut() } {
        let PyIterator::Enumerate {
            inner,
            count,
            count_big: None,
        } = state
        else {
            return NativeBytePair::Unsupported;
        };
        // SAFETY: as above; `inner` is a different cell.
        return match unsafe { inner.peek_mut() } {
            Some(source) => step_enumerated_byte(count, source),
            None => NativeBytePair::Unsupported,
        };
    }
    let Ok(mut state) = it.try_borrow_mut() else {
        return NativeBytePair::Unsupported;
    };
    let PyIterator::Enumerate {
        inner,
        count,
        count_big: None,
    } = &mut *state
    else {
        return NativeBytePair::Unsupported;
    };
    let Ok(mut source) = inner.try_borrow_mut() else {
        return NativeBytePair::Unsupported;
    };
    step_enumerated_byte(count, &mut source)
}

/// One step of `enumerate(<bytes iterator>)`: both cursors advance
/// together, or neither does.
#[inline]
fn step_enumerated_byte(count: &mut i64, source: &mut PyIterator) -> NativeBytePair {
    let Some(next_count) = count.checked_add(1) else {
        return NativeBytePair::Unsupported;
    };
    let PyIterator::Bytes { data, index } = source else {
        return NativeBytePair::Unsupported;
    };
    let Some(&byte) = data.get(*index) else {
        return NativeBytePair::Exhausted;
    };
    let current = *count;
    *index += 1;
    *count = next_count;
    NativeBytePair::Value(current, byte)
}

#[derive(Debug)]
enum NativeObjectPair {
    Unsupported,
    Exhausted,
    Value(i64, Object),
}

/// Read an exact tuple iterator's immutable leaves into object lanes.
/// Check pin capacity before advancing either cursor: the caller pins
/// the returned value immediately, without running Python or adding other
/// pins. `None` needs no pin. Other values retain generic iteration and its
/// existing lifetime handling.
#[inline]
fn next_enumerated_tuple(it: &Object, pin_available: bool) -> NativeObjectPair {
    let Object::Iter(it) = it else {
        return NativeObjectPair::Unsupported;
    };
    let Ok(mut state) = it.try_borrow_mut() else {
        return NativeObjectPair::Unsupported;
    };
    let PyIterator::Enumerate {
        inner,
        count,
        count_big: None,
    } = &mut *state
    else {
        return NativeObjectPair::Unsupported;
    };
    let Some(next_count) = count.checked_add(1) else {
        return NativeObjectPair::Unsupported;
    };
    let Ok(mut source) = inner.try_borrow_mut() else {
        return NativeObjectPair::Unsupported;
    };
    let PyIterator::Tuple { items, index } = &mut *source else {
        return NativeObjectPair::Unsupported;
    };
    let Some(value) = items.get(*index) else {
        return NativeObjectPair::Exhausted;
    };
    if !matches!(value, Object::None)
        && (!pin_available
            || !matches!(
                value,
                Object::Int(_)
                    | Object::Long(_)
                    | Object::Float(_)
                    | Object::Bool(_)
                    | Object::Str(_)
                    | Object::WStr(_)
                    | Object::Bytes(_)
            ))
    {
        return NativeObjectPair::Unsupported;
    }
    let value = super::Interpreter::clone_operand(value);
    let current = *count;
    *index += 1;
    *count = next_count;
    NativeObjectPair::Value(current, value)
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
    // A `dict.items()` step yields the key and value straight into the
    // two lanes, without the tuple; no Python runs, so the header's poll
    // covers it.
    let items = match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(Object::Iter(cell))) => cell
            .try_borrow_mut()
            .ok()
            .and_then(|mut it| it.next_item_pair()),
        _ => None,
    };
    if let Some(step) = items {
        return match step {
            Err(err) => {
                ctx.raised = Some(err);
                4
            }
            Ok(None) => 1,
            Ok(Some((k, v))) => {
                let b1 = pack_iter_elem(&k, tag1, &mut ctx.pins, &mut ctx.pin_memo);
                let b2 =
                    b1.and_then(|_| pack_iter_elem(&v, tag2, &mut ctx.pins, &mut ctx.pin_memo));
                if let (Some(b1), Some(b2)) = (b1, b2) {
                    jf.ret_bits = b1;
                    // SAFETY: the marshal buffer is at least one slot
                    // wide (`max_call_args.max(1)`).
                    unsafe { *jf.call_args = b2 };
                    return 0;
                }
                // Not in the lanes: the pair surrenders as the tuple the
                // erased `UNPACK_SEQUENCE` consumes.
                ctx.pins.push(Pin::Obj(Object::new_tuple_array([k, v])));
                jf.ret_bits = (ctx.pins.len() - 1) as u64;
                3
            }
        };
    }
    // An `enumerate` or two-source `zip` over native iterators whose step
    // runs no code: the pair straight into the lanes, as above.
    let pair = match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(Object::LazyIter(l))) => crate::seqiter::lazy_pure_pair(l),
        Some(Pin::Obj(it @ Object::Iter(_))) => crate::seqiter::enumerate_pure_pair(it),
        _ => None,
    };
    if let Some((k, v)) = pair {
        let b1 = pack_iter_elem(&k, tag1, &mut ctx.pins, &mut ctx.pin_memo);
        let b2 = b1.and_then(|_| pack_iter_elem(&v, tag2, &mut ctx.pins, &mut ctx.pin_memo));
        if let (Some(b1), Some(b2)) = (b1, b2) {
            jf.ret_bits = b1;
            // SAFETY: the marshal buffer is at least one slot wide.
            unsafe { *jf.call_args = b2 };
            return 0;
        }
        ctx.pins.push(Pin::Obj(Object::new_tuple_array([k, v])));
        jf.ret_bits = (ctx.pins.len() - 1) as u64;
        return 3;
    }
    // The loop's poll point — see `wpjit_iter_next`.
    crate::gil::yield_checkpoint();
    if crate::hot_gates::load() != 0 || crate::trace::any_observers_active() {
        return 2;
    }
    // A list or tuple iterator (a row of `(index, weight)` pairs) steps
    // in place: no Python runs, and the element unpacks like the
    // generic step's below.
    let seq_step = match ctx.pins.get(pin as usize) {
        Some(Pin::Obj(Object::Iter(cell))) => {
            cell.try_borrow_mut()
                .ok()
                .and_then(|mut it| match &mut *it {
                    // The common step inline (`next_value` is a large
                    // function); exhaustion detaches through it.
                    crate::object::PyIterator::List { items, index, .. } => {
                        let next = items.borrow().get(*index).cloned();
                        Some(match next {
                            Some(v) => {
                                *index += 1;
                                Some(v)
                            }
                            None => it.next_value(),
                        })
                    }
                    crate::object::PyIterator::Tuple { items, index } => {
                        let next = items.get(*index).cloned();
                        if next.is_some() {
                            *index += 1;
                        }
                        Some(next)
                    }
                    _ => None,
                })
        }
        _ => None,
    };
    if let Some(step) = seq_step {
        let Some(v) = step else {
            return 1;
        };
        if let Object::Tuple(items) = &v {
            if let [a, b] = &items[..] {
                let packed1 = pack_iter_elem(a, tag1, &mut ctx.pins, &mut ctx.pin_memo);
                let packed2 =
                    packed1.and_then(|_| pack_iter_elem(b, tag2, &mut ctx.pins, &mut ctx.pin_memo));
                if let (Some(b1), Some(b2)) = (packed1, packed2) {
                    jf.ret_bits = b1;
                    // SAFETY: the marshal buffer is at least one slot
                    // wide (`max_call_args.max(1)`).
                    unsafe { *jf.call_args = b2 };
                    return 0;
                }
            }
        }
        // Not in the lanes: the erased `UNPACK_SEQUENCE` consumes it.
        jf.ret_bits = if matches!(v, Object::None) {
            u64::MAX
        } else {
            ctx.pins.push(Pin::Obj(v));
            (ctx.pins.len() - 1) as u64
        };
        return 3;
    }
    if tag1 == SlotTag::Int as i64 && tag2 == SlotTag::Int as i64 {
        let pair = match ctx.pins.get(pin as usize) {
            Some(Pin::Obj(it)) => next_enumerated_byte(it),
            _ => NativeBytePair::Unsupported,
        };
        match pair {
            NativeBytePair::Value(index, byte) => {
                jf.ret_bits = index as u64;
                // SAFETY: the marshal buffer has at least one slot,
                // as required by this helper's live-buffer contract.
                unsafe { *jf.call_args = u64::from(byte) };
                return 0;
            }
            NativeBytePair::Exhausted => return 1,
            NativeBytePair::Unsupported => {}
        }
    } else if tag1 == SlotTag::Int as i64 && tag2 == SlotTag::ObjPin as i64 {
        let pair = match ctx.pins.get(pin as usize) {
            Some(Pin::Obj(it)) => next_enumerated_tuple(it, ctx.pins.len() < RUNTIME_PIN_CAP),
            _ => NativeObjectPair::Unsupported,
        };
        match pair {
            NativeObjectPair::Value(index, value) => {
                let bits = if matches!(value, Object::None) {
                    u64::MAX
                } else {
                    debug_assert!(ctx.pins.len() < RUNTIME_PIN_CAP);
                    let bits = ctx.pins.len() as u64;
                    ctx.pins.push(Pin::Obj(value));
                    bits
                };
                jf.ret_bits = index as u64;
                // SAFETY: the helper's marshal buffer contains one slot.
                unsafe { *jf.call_args = bits };
                return 0;
            }
            NativeObjectPair::Exhausted => return 1,
            NativeObjectPair::Unsupported => {}
        }
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
        // A generator resume from native code rebuilds a whole interpreter
        // activation, several times what the interpreter's own inline
        // resume costs: charged like an interpreter call, so a loop that
        // drives a generator retires at the next poll (see `wpjit_poll`).
        ctx.dyn_py_calls = ctx.dyn_py_calls.saturating_add(1);
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
                        let packed1 =
                            pack_iter_elem(&items[0], tag1, &mut ctx.pins, &mut ctx.pin_memo);
                        let packed2 = packed1.and_then(|_| {
                            pack_iter_elem(&items[1], tag2, &mut ctx.pins, &mut ctx.pin_memo)
                        });
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

/// Attempt native entry after the ordinary argument binder has finished.
/// Keep this eligibility check out of the caller so argument counts and layout
/// flags don't stay live across its inlined keyword/default binding code.
/// All execution guards remain in `try_call_native_direct`.
#[inline(never)]
pub(crate) fn try_call_native_bound(
    interp: &mut super::Interpreter,
    f: &Rc<PyFunction>,
    code: &Rc<CodeObject>,
    bound: &[Object],
) -> Option<Result<Object, RuntimeError>> {
    if code.has_varargs
        || code.has_varkeywords
        || code.kwonly_count != 0
        || !f.closure.is_empty()
        || code.is_generator
        || code.is_coroutine
        || code.is_async_generator
    {
        return None;
    }
    let args = bound.get(..code.arg_count as usize)?;
    try_call_native_direct(interp, f, code, args)
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
    if code.jit_hint.is_not_jitable() || jit_off_for_process() {
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
    if !cf.interp_entry {
        return None;
    }
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
        cf: StdRc::as_ptr(&entry.art.cf),
        guard_snapshot: entry.art.snap.clone(),
        globals: f.globals.clone(),
        builtins: f.builtins.clone(),
        // Direct-callable code is cell-free (`native_callable`).
        cells: crate::object::empty_cells(),
        parked: None,
        raised: None,
        const_pins: Vec::new(),
        pins,
        entry_pin_count,
        pin_pressure_exit: false,
        pin_limit: RUNTIME_PIN_SOFT_LIMIT,
        pins_counted: 0,
        last_ref_pins: 0,
        obj_globals: entry.art.obj_globals.clone(),
        obj_global_pins: Vec::new(),
        attr_guards: entry.art.attr_guards.clone(),
        methods: entry.art.methods.clone(),
        math: entry.art.math.clone(),
        dirty: false,
        interp_calls: 0,
        dyn_py_calls: 0,
        native_calls: 0,
        polls: 0,
        child: None,
        depth_cell: crate::recursion::depth_cell(),
        code_ptr: key,
        native: entry.native.clone(),
        method_native: entry.method_native.clone(),
        table_gen: current_compile_gen(),
        // The frameless direct entry pushes no interpreter frame —
        // keep the activation observable to callee-side stack walkers.
        frameless_code: Some(code.clone()),
        dyn_callee: None,
        pin_memo: [u32::MAX; PIN_MEMO],
        introspected: Cell::new(false),
        inspected_locals: std::cell::RefCell::new(None),
        cell_list_pins: Vec::new(),
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
    end_introspection(&ctx);

    native_stat(|s| s.direct_calls.set(s.direct_calls.get() + 1));
    // A direct callee that keeps calling back into the interpreter is
    // retired like a native-to-native one (deltablue's `incremental_add`
    // ran each `satisfy` through an activation shell and a framed call,
    // 6% of the benchmark's instructions).
    note_callee_exit(&entry.art, code, &ctx);

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
                if ctx.pin_pressure_exit {
                    debug_assert_eq!(status, JitStatus::Deopt);
                    st.stats.pin_pressure_exits += 1;
                } else if matches!(status, JitStatus::Deopt) {
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
                art: entry.art.clone(),
                func: f.clone(),
                code: code.clone(),
                ctor: None,
            };
            finish_deopted_callee(interp, &nc, &mut ctx, locals_buf, spill, tags, &jf, pending)
        }
    };
    // Reap the activation's pins (after the
    // pin-based unpacks above).
    drain_activation_pins(interp, &mut ctx.pins);
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
    if frame.code.jit_hint.is_not_jitable() || jit_off_for_process() {
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
        let mut probe_iter = |depth: u32| probe_stack_iter_lane(frame_ref, depth);
        let mut probe_pairs = |slot: u32| probe_pair_lanes(frame_ref, slot);
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
                stack_iter: &mut probe_iter,
                pairs: &mut probe_pairs,
            },
        )
    });
    let Some(entry) = entry else {
        return JitEntry::Skip;
    };
    // A loop-free body that round-trips into the interpreter runs no
    // faster natively (see `CompiledFrame::interp_entry`).
    if !entry.cf.interp_entry {
        return JitEntry::Skip;
    }

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

/// Below this much remaining straight-line scalar work, a first activation
/// can finish in the interpreter before compilation is likely to pay off.
/// Calls, nested loops, and unknown iteration counts keep the usual policy.
const FIRST_RANGE_WORK_BUDGET: u64 = 64 * 1024;

fn short_scalar_range(frame: &super::Frame) -> bool {
    use weavepy_compiler::{BinOpKind, OpCode, BINARY_OP_INPLACE_FLAG};
    let code = &frame.code;
    if code.is_generator
        || code.is_coroutine
        || code.is_async_generator
        || !code.exception_table.is_empty()
    {
        return false;
    }
    let [Object::Iter(iterator)] = frame.stack.as_slice() else {
        return false;
    };
    let remaining = match &*iterator.borrow() {
        PyIterator::Range {
            current,
            stop,
            step: 1,
        } => i128::from(*stop) - i128::from(*current),
        _ => return false,
    };
    let Ok(remaining) = u64::try_from(remaining) else {
        return false;
    };
    let pc = frame.pc as usize;
    let instructions = &code.instructions;
    if instructions.get(pc).is_none_or(|i| i.op != OpCode::ForIter) {
        return false;
    }
    let mut backedges = instructions
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i.op, OpCode::ForIter | OpCode::JumpBackward));
    // Only one loop, with its one backedge targeting this exact header.
    if backedges.next().map(|(pos, _)| pos) != Some(pc) {
        return false;
    }
    let Some((end, backedge)) = backedges.next() else {
        return false;
    };
    if backedge.op != OpCode::JumpBackward
        || backedges.next().is_some()
        || (end + 1).checked_sub(backedge.arg as usize) != Some(pc)
    {
        return false;
    }
    let Some(work) = remaining.checked_mul((end + 1 - pc) as u64) else {
        return false;
    };
    if work >= FIRST_RANGE_WORK_BUDGET {
        return false;
    }
    let locals = frame.locals.borrow();
    instructions[pc + 1..end].iter().all(|i| match i.op {
        OpCode::Nop | OpCode::StoreFast => true,
        OpCode::LoadFast => matches!(
            locals.get(i.arg as usize),
            Some(Object::Int(_) | Object::Float(_) | Object::Bool(_))
        ),
        OpCode::LoadConst => matches!(
            code.constants.get(i.arg as usize),
            Some(weavepy_compiler::Constant::Int(_) | weavepy_compiler::Constant::Float(_))
        ),
        OpCode::BinaryOp => matches!(
            i.arg & !BINARY_OP_INPLACE_FLAG,
            n if n == BinOpKind::Add as u32
                || n == BinOpKind::Sub as u32
                || n == BinOpKind::Mult as u32
        ),
        _ => false,
    })
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
        if st.range_budget {
            let key = Rc::as_ptr(&frame.code).cast::<CodeObject>();
            if let Some(entry) = st.cache.get_mut(&key) {
                if matches!(entry.tier, Tier::Cold) && short_scalar_range(frame) {
                    entry.defer_osr = true;
                    // The next call compiles (see `get_compiled`), lean
                    // ones included: they otherwise compile only at the
                    // warm-up checkpoint, many calls on. The lean count
                    // moves there, so the calls it held are credited.
                    let hint = &frame.code.jit_hint;
                    let at = lean_warm_at();
                    entry.calls = entry
                        .calls
                        .wrapping_add(hint.lean_entries())
                        .wrapping_sub(at - 1);
                    hint.warm_next_lean(at);
                    return None;
                }
            }
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
        let mut probe_iter = |depth: u32| probe_stack_iter_lane(frame_ref, depth);
        let mut probe_pairs = |slot: u32| probe_pair_lanes(frame_ref, slot);
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
                stack_iter: &mut probe_iter,
                pairs: &mut probe_pairs,
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
    // A region loop entered once per iteration of the interpreted code
    // around it is worth entering only for enough remaining iterations.
    if cf.region_roots.contains(&(pc as u32)) && short_live_loop(&frame.stack) {
        return JitEntry::Skip;
    }
    let Some(osr) = cf.osr_entries.iter().find(|e| e.pc == pc) else {
        // A loop whose code stays interpreted is expected to find no
        // entry: its nested regions enter at their own headers.
        if cf.stayed_heads.contains(&(pc as u32)) {
            return JitEntry::Skip;
        }
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
                        // An unbound scalar-lane local written before
                        // any native read from this entry (an inlined
                        // comprehension's target, an accumulator bound
                        // after the loop): the packed placeholder is
                        // never read.
                        || (matches!(o, Object::Unbound)
                            && matches!(ty, JitType::Int | JitType::Float | JitType::Bool)
                            && !osr.unassigned_reads.contains(&(slot as u32)))
                });
                if !ok {
                    if crate::hot_gates::env_flags::jit_trace() {
                        eprintln!(
                            "jit osr refuse {:?} pc {}: local {} {:?} holds {}",
                            frame.code.name,
                            pc,
                            slot,
                            ty,
                            locals
                                .get(slot)
                                .map_or("?".to_owned(), |o| o.type_name_owned())
                        );
                    }
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
        if crate::hot_gates::env_flags::jit_trace() {
            eprintln!("jit osr refuse {:?} pc {}: live loops", frame.code.name, pc);
        }
        return fail(&frame.code);
    };
    // The iterators are consumed by the decomposition: native code owns
    // the loops from here (a deopt reconstructs fresh iterators).
    frame.stack.clear();
    JIT.with(|cell| cell.borrow_mut().stats.osr_entries += 1);
    enter_compiled(interp, frame, &entry, pc, &synth, None)
}

/// Whether the innermost live loop iterator (the top of an OSR request's
/// stack) has fewer than `weavepy_jit::MIN_REGION_TRIPS` items left.
fn short_live_loop(stack: &[Object]) -> bool {
    let Some(Object::Iter(it)) = stack.last() else {
        return false;
    };
    let left = match &*it.borrow() {
        PyIterator::Range {
            current,
            stop,
            step: 1,
        } => stop.saturating_sub(*current),
        PyIterator::List { items, index, .. } => {
            (items.borrow().len() as i64).saturating_sub(*index as i64)
        }
        _ => return false,
    };
    left < weavepy_jit::MIN_REGION_TRIPS as i64
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
            // A passed-through iterator is never stepped natively.
            if !lp.passthrough && !matches!(o, Object::Generator(_) | Object::Iter(_)) {
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
    if frame.code.jit_hint.is_not_jitable() || jit_off_for_process() {
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
        let mut probe_iter = |depth: u32| probe_stack_iter_lane(frame_ref, depth);
        let mut probe_pairs = |slot: u32| probe_pair_lanes(frame_ref, slot);
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
                stack_iter: &mut probe_iter,
                pairs: &mut probe_pairs,
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
    let mut pins: PinTable = take_pins();
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
        cf: StdRc::as_ptr(&entry.cf),
        guard_snapshot: entry.guard_snapshot.clone(),
        globals: frame.globals.clone(),
        builtins: frame.builtins.clone(),
        cells: frame.cells.clone(),
        parked: None,
        raised: None,
        const_pins: Vec::new(),
        pins,
        entry_pin_count,
        pin_pressure_exit: false,
        pin_limit: RUNTIME_PIN_SOFT_LIMIT,
        pins_counted: 0,
        last_ref_pins: 0,
        obj_globals: entry.obj_globals.clone(),
        obj_global_pins: Vec::new(),
        attr_guards: entry.attr_guards.clone(),
        methods: entry.methods.clone(),
        math: entry.math.clone(),
        dirty: false,
        interp_calls: 0,
        dyn_py_calls: 0,
        native_calls: 0,
        polls: 0,
        child: None,
        depth_cell: crate::recursion::depth_cell(),
        code_ptr: Rc::as_ptr(&frame.code).cast::<CodeObject>(),
        native: entry.native.clone(),
        method_native: entry.method_native.clone(),
        table_gen: current_compile_gen(),
        // Framed entry: this activation's `Frame` shell is on the
        // spine already.
        frameless_code: None,
        dyn_callee: None,
        pin_memo: [u32::MAX; PIN_MEMO],
        introspected: Cell::new(false),
        inspected_locals: std::cell::RefCell::new(None),
        cell_list_pins: Vec::new(),
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
    let jf_ptr: *mut JitFrame = &raw mut jf;
    // SAFETY: as above.
    let status = with_native_frame(&frame.locals, jf_ptr, || unsafe { cf.enter(jf_ptr) });
    end_introspection(&ctx);

    // A cold exit is an expected hand-off, never a deopt charge.
    let cold_exit = matches!(status, JitStatus::Deopt) && cf.cold_exits.contains(&jf.deopt_pc);
    note_native_exit(frame, &jf, status, ctx.pin_pressure_exit, cold_exit);

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
                interp_calls: ctx.interp_calls,
                dyn_py_calls: ctx.dyn_py_calls,
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
    // Entry and runtime pins can both hold replaced temporaries: reap the
    // ones dying with the activation (after every pin-based rebuild
    // above, so nothing is unpacked from a drained table).
    drain_activation_pins(interp, &mut ctx.pins);
    put_pins(std::mem::take(&mut ctx.pins));
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
fn note_native_exit(
    frame: &super::Frame,
    jf: &JitFrame,
    status: JitStatus,
    pin_pressure_exit: bool,
    cold_exit: bool,
) {
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        st.stats.native_entries += 1;
        if cold_exit {
            st.stats.cold_exits += 1;
            let key = Rc::as_ptr(&frame.code).cast::<CodeObject>();
            if let Some(ce) = st.cache.get_mut(&key) {
                if ce.cold_recompiles < COLD_RECOMPILE_BUDGET {
                    ce.recompile_at_osr = true;
                }
            }
            return;
        }
        if pin_pressure_exit {
            debug_assert_eq!(status, JitStatus::Deopt);
            st.stats.pin_pressure_exits += 1;
            // Count the physical entry for diagnostics, but do not dilute
            // the generic-call ratio with chunks of one activation or
            // retire healthy code for requesting temporary cleanup.
            return;
        }
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
            if crate::hot_gates::env_flags::jit_trace() {
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
    // An inspected activation's frame already holds its locals, plus
    // whatever the inspection wrote (see `sync_native_locals`).
    if !ctx.introspected.get() {
        let mut locals = frame.locals.borrow_mut();
        for (slot, &bits) in locals_buf.iter().enumerate() {
            if let Some(ty) = cf.local_types[slot] {
                if let Some(dst) = locals.get_mut(slot) {
                    *dst = unpack_ty(bits, ty, &ctx.pins);
                }
            }
        }
    }
    let raised = matches!(status, JitStatus::Raised);
    // A deopt-after-call carries the parked, already-computed result:
    // `rebuild_stack` slots it in at the exiting op's native depth
    // (below any open self-or-null marker) and the interpreter resumes
    // after the call.
    let parked = if raised { None } else { ctx.parked.take() };
    rebuild_stack(
        interp, frame, entry, locals_buf, spill, tags, jf, &ctx.pins, parked,
    );
    if raised {
        // As though the CALL instruction just executed and
        // raised: pc points past it (`handle_exception` uses
        // `pc - 1` as the raise site).
        frame.pc = jf.deopt_pc + 1;
        let err = ctx.raised.take().unwrap_or_else(|| {
            RuntimeError::Internal("JIT Raised exit without a parked exception".to_owned())
        });
        JitEntry::Raised(err)
    } else {
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
    parked: Option<Object>,
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
    for s in cf
        .len_spans
        .iter()
        .filter(|s| s.live_from < jf.deopt_pc && jf.deopt_pc < s.live_to)
    {
        let callee = erased_global(entry, &frame.code, s.token);
        inserts.push((s.interp_depth, callee));
        inserts.push((s.interp_depth + 1, Object::Unbound));
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
            .map_or(Object::None, |guard| guard.expected.clone());
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
    if crate::hot_gates::env_flags::jit_trace() {
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
    // A deopt-after-call's parked, already-computed result is the
    // value the exiting op would have pushed: it occupies the next
    // *native* slot, so interpreter-side inserts recorded above that
    // depth (notably the `Unbound` self-or-null marker of a
    // method-form `DynAttrGet` whose null span is open at `deopt_pc`)
    // must land *above* it, not below. Pushing it after the trailing
    // inserts inverted `[method, Unbound]` into `[Unbound, method]`,
    // and the consuming CALL then invoked `Unbound` ("'NoneType'
    // object is not callable" once the pin table hit its cap in a
    // hot allocating loop; test_dictviews test_deeply_nested_repr).
    if let Some(v) = parked {
        while next < inserts.len() && inserts[next].0 as usize == frame.stack.len() {
            frame.stack.push(inserts[next].1.clone());
            next += 1;
        }
        frame.stack.push(v);
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
    /// Pin-table size at the first native entry, used as the baseline
    /// for the temporary-pin soft limit across native suspensions.
    entry_pin_count: usize,
    dirty: bool,
    /// Interpreter round-trips so far (see `CallCtx::interp_calls`).
    interp_calls: u32,
    /// See `CallCtx::dyn_py_calls`.
    dyn_py_calls: u32,
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
    for s in cf
        .len_spans
        .iter()
        .filter(|s| s.live_from < pc && pc < s.live_to)
    {
        let callee = erased_global(entry, code, s.token);
        inserts.push((s.interp_depth, PlanSlot::Obj(callee)));
        inserts.push((s.interp_depth + 1, PlanSlot::Obj(Object::Unbound)));
    }
    for s in cf
        .math_spans
        .iter()
        .filter(|s| s.live_from < pc && pc < s.live_to)
    {
        let f = entry
            .math
            .get(s.token as usize)
            .map_or(Object::None, |guard| guard.expected.clone());
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
#[inline]
pub(crate) fn materialize_parked(frame: &mut super::Frame) {
    if frame.parked_native.is_some() {
        materialize_parked_native(frame);
    }
}

#[cold]
#[inline(never)]
fn materialize_parked_native(frame: &mut super::Frame) {
    let Some(mut act) = frame.parked_native.take() else {
        return;
    };
    if crate::hot_gates::env_flags::jit_trace() {
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
    // The reconstructed frame owns every live value. Retire obsolete pins
    // without running Python while a materialization caller may hold a
    // generator-state borrow or lack an installed interpreter frame.
    defer_activation_pins(&mut act.pins);
    // Buffers return to the pools.
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
        cf: StdRc::as_ptr(&entry.cf),
        guard_snapshot: entry.guard_snapshot.clone(),
        globals: frame.globals.clone(),
        builtins: frame.builtins.clone(),
        cells: frame.cells.clone(),
        parked: None,
        raised: None,
        const_pins: std::mem::take(&mut act.const_pins),
        pins,
        entry_pin_count: act.entry_pin_count,
        pin_pressure_exit: false,
        pin_limit: RUNTIME_PIN_SOFT_LIMIT,
        pins_counted: 0,
        last_ref_pins: 0,
        obj_globals: entry.obj_globals.clone(),
        obj_global_pins: std::mem::take(&mut act.obj_global_pins),
        attr_guards: entry.attr_guards.clone(),
        methods: entry.methods.clone(),
        math: entry.math.clone(),
        dirty: act.dirty,
        interp_calls: act.interp_calls,
        dyn_py_calls: act.dyn_py_calls,
        native_calls: 0,
        polls: 0,
        child: None,
        depth_cell: crate::recursion::depth_cell(),
        code_ptr: Rc::as_ptr(&frame.code).cast::<CodeObject>(),
        native: entry.native.clone(),
        method_native: entry.method_native.clone(),
        table_gen: current_compile_gen(),
        // Framed entry (generator resume): the resumed `Frame`'s shell
        // is on the spine already.
        frameless_code: None,
        dyn_callee: None,
        pin_memo: [u32::MAX; PIN_MEMO],
        introspected: Cell::new(false),
        inspected_locals: std::cell::RefCell::new(None),
        cell_list_pins: Vec::new(),
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
    let jf_ptr: *mut JitFrame = &raw mut jf;
    // SAFETY: as above.
    let status = with_native_frame(&frame.locals, jf_ptr, || unsafe { cf.enter(jf_ptr) });
    end_introspection(&ctx);
    let cold_exit = matches!(status, JitStatus::Deopt) && cf.cold_exits.contains(&jf.deopt_pc);
    note_native_exit(frame, &jf, status, ctx.pin_pressure_exit, cold_exit);
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
    drain_activation_pins(interp, &mut ctx.pins);
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
    JIT_PROCESS_GATE.store(1, std::sync::atomic::Ordering::Relaxed);
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        st.enabled = true;
        st.threshold = threshold.max(1);
        st.range_budget = false;
        set_lean_warm_from_threshold(st.threshold);
    });
}

#[cfg(test)]
pub(crate) fn pin_pressure_exits_for_test() -> u64 {
    JIT.with(|cell| cell.borrow().stats.pin_pressure_exits)
}

#[cfg(test)]
pub(crate) unsafe fn cached_chain_peek_for_test<'a>(
    receiver: &'a Object,
    code: &CodeObject,
    pc: u32,
) -> Option<&'a Object> {
    let extension = super::code_vm_ext_existing(code)?;
    // SAFETY: callers uphold cached_chain_peek's read-only GIL contract.
    unsafe { cached_chain_peek(receiver, code, extension, pc) }
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

#[cfg(test)]
pub(crate) fn code_compiled_for_test(code: &Rc<CodeObject>) -> bool {
    JIT.with(|cell| {
        cell.borrow()
            .cache
            .get(&Rc::as_ptr(code))
            .is_some_and(|entry| matches!(entry.tier, Tier::Compiled(_)))
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

/// Test hook: `wpjit_call_method` invocations on the current thread (a
/// direct method call never reaches it).
#[cfg(test)]
pub(crate) fn method_helper_calls_for_test() -> u64 {
    NATIVE_CALL_STATS.with(|s| s.method_calls.get())
}

/// Test hook for frameless interpreter-to-native entries, counted separately
/// from framed entries and native-to-native calls.
#[cfg(test)]
pub(crate) fn direct_call_count_for_test() -> u64 {
    NATIVE_CALL_STATS.with(|s| s.direct_calls.get())
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
        let (ncalls, nfallbacks, ndeopts, mcalls, mfallbacks, mmisses, direct, leaves) =
            NATIVE_CALL_STATS.with(|n| {
                (
                    n.calls.get(),
                    n.fallbacks.get(),
                    n.deopts.get(),
                    n.method_calls.get(),
                    n.method_call_fallbacks.get(),
                    n.method_guard_misses.get(),
                    n.direct_calls.get(),
                    n.scalar_leaf_calls.get(),
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
             - pin-pressure exits: **{}**\n\
             - cold exits: **{}**\n\
             - entry-guard failures: **{}**\n\
             - native-to-native calls: **{}**\n\
             - scalar leaf calls: **{}**\n\
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
            s.pin_pressure_exits,
            s.cold_exits,
            s.entry_guard_failures,
            ncalls,
            leaves,
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

#[cfg(test)]
mod native_pair_tests {
    use super::{
        next_enumerated_byte, next_enumerated_tuple, NativeBytePair, NativeObjectPair, Object,
        PyIterator,
    };
    use crate::sync::{Rc, RefCell};

    fn enumeration(data: &[u8], count: i64) -> (Object, Rc<RefCell<PyIterator>>) {
        let inner = Rc::new(RefCell::new(PyIterator::Bytes {
            data: crate::shared_value::SharedSlice::from(data),
            index: 0,
        }));
        let outer = Object::Iter(Rc::new(RefCell::new(PyIterator::Enumerate {
            inner: inner.clone(),
            count,
            count_big: None,
        })));
        (outer, inner)
    }

    #[test]
    fn shares_cursors_with_interpreted_steps_and_stays_exhausted() {
        let (outer, inner) = enumeration(&[10, 20, 30], -2);
        assert!(matches!(
            inner.borrow_mut().next_value(),
            Some(Object::Int(10))
        ));
        assert_eq!(next_enumerated_byte(&outer), NativeBytePair::Value(-2, 20));
        let Object::Iter(cursor) = &outer else {
            unreachable!()
        };
        let Some(Object::Tuple(pair)) = cursor.borrow_mut().next_value() else {
            panic!("interpreted continuation must produce a pair");
        };
        assert!(matches!(&pair[..], [Object::Int(-1), Object::Int(30)]));
        for _ in 0..3 {
            assert_eq!(next_enumerated_byte(&outer), NativeBytePair::Exhausted);
        }
        assert!(matches!(
            &*cursor.borrow(),
            PyIterator::Enumerate { count: 0, .. }
        ));
        assert!(matches!(
            &*inner.borrow(),
            PyIterator::Bytes { index: 3, .. }
        ));
    }

    #[test]
    fn counter_overflow_leaves_the_value_for_generic_promotion() {
        let (outer, inner) = enumeration(&[11, 22, 33], i64::MAX - 1);
        assert_eq!(
            next_enumerated_byte(&outer),
            NativeBytePair::Value(i64::MAX - 1, 11)
        );
        assert_eq!(next_enumerated_byte(&outer), NativeBytePair::Unsupported);
        assert!(matches!(
            &*inner.borrow(),
            PyIterator::Bytes { index: 1, .. }
        ));
        let Object::Iter(cursor) = &outer else {
            unreachable!()
        };
        let Some(Object::Tuple(pair)) = cursor.borrow_mut().next_value() else {
            panic!("overflow fallback lost a byte");
        };
        assert!(matches!(&pair[0], Object::Int(value) if *value == i64::MAX));
        assert!(matches!(&pair[1], Object::Int(22)));
        assert_eq!(next_enumerated_byte(&outer), NativeBytePair::Unsupported);
        let Some(Object::Tuple(pair)) = cursor.borrow_mut().next_value() else {
            panic!("big-counter continuation lost a byte");
        };
        assert!(matches!(&pair[0], Object::Long(value)
            if **value == num_bigint::BigInt::from(i64::MAX) + 1));
        assert!(matches!(&pair[1], Object::Int(33)));
    }

    #[test]
    fn unsupported_sources_do_not_advance_either_cursor() {
        let inner = Rc::new(RefCell::new(PyIterator::Bytes {
            data: crate::shared_value::SharedSlice::from([42_u8].as_slice()),
            index: 0,
        }));
        let wrapped = Rc::new(RefCell::new(PyIterator::Shared(inner.clone())));
        let outer = Object::Iter(Rc::new(RefCell::new(PyIterator::Enumerate {
            inner: wrapped,
            count: 7,
            count_big: None,
        })));
        assert_eq!(
            next_enumerated_byte(&Object::None),
            NativeBytePair::Unsupported
        );
        assert_eq!(next_enumerated_byte(&outer), NativeBytePair::Unsupported);
        assert!(matches!(
            &*inner.borrow(),
            PyIterator::Bytes { index: 0, .. }
        ));
        let Object::Iter(cursor) = outer else {
            unreachable!()
        };
        assert!(matches!(
            &*cursor.borrow(),
            PyIterator::Enumerate { count: 7, .. }
        ));
    }

    #[test]
    fn a_live_borrow_does_not_consume_a_byte() {
        let (outer, inner) = enumeration(&[42], 0);
        let borrowed = inner.borrow();
        assert_eq!(next_enumerated_byte(&outer), NativeBytePair::Unsupported);
        drop(borrowed);
        assert_eq!(next_enumerated_byte(&outer), NativeBytePair::Value(0, 42));
    }

    fn tuple_enumeration(items: Vec<Object>, count: i64) -> (Object, Rc<RefCell<PyIterator>>) {
        let inner = Rc::new(RefCell::new(PyIterator::Tuple {
            items: crate::object::TupleStorage::from_vec(items),
            index: 0,
        }));
        let outer = Object::Iter(Rc::new(RefCell::new(PyIterator::Enumerate {
            inner: inner.clone(),
            count,
            count_big: None,
        })));
        (outer, inner)
    }

    #[test]
    fn tuple_pairs_preserve_identity_shared_cursors_and_exhaustion() {
        let value = Object::from_str("retained value");
        let (outer, inner) =
            tuple_enumeration(vec![Object::Int(5), value.clone(), Object::None], -2);
        assert!(matches!(
            inner.borrow_mut().next_value(),
            Some(Object::Int(5))
        ));
        let NativeObjectPair::Value(-2, next) = next_enumerated_tuple(&outer, true) else {
            panic!("native enumeration lost the shared cursor");
        };
        assert!(next.is_same(&value));
        let Object::Iter(cursor) = &outer else {
            unreachable!()
        };
        let Some(Object::Tuple(pair)) = cursor.borrow_mut().next_value() else {
            panic!("interpreted continuation must produce a pair");
        };
        assert!(matches!(&pair[..], [Object::Int(-1), Object::None]));
        for _ in 0..3 {
            assert!(matches!(
                next_enumerated_tuple(&outer, false),
                NativeObjectPair::Exhausted
            ));
        }
        assert!(matches!(
            &*cursor.borrow(),
            PyIterator::Enumerate { count: 0, .. }
        ));
        assert!(matches!(
            &*inner.borrow(),
            PyIterator::Tuple { index: 3, .. }
        ));
    }

    #[test]
    fn tuple_pin_pressure_preserves_values_and_allows_none() {
        let (outer, inner) = tuple_enumeration(vec![Object::Int(42), Object::None], 0);
        assert!(matches!(
            next_enumerated_tuple(&outer, false),
            NativeObjectPair::Unsupported
        ));
        assert!(matches!(
            &*inner.borrow(),
            PyIterator::Tuple { index: 0, .. }
        ));
        assert!(matches!(
            next_enumerated_tuple(&outer, true),
            NativeObjectPair::Value(0, Object::Int(42))
        ));
        assert!(matches!(
            next_enumerated_tuple(&outer, false),
            NativeObjectPair::Value(1, Object::None)
        ));
        assert!(matches!(
            next_enumerated_tuple(&outer, false),
            NativeObjectPair::Exhausted
        ));
    }

    #[test]
    fn tuple_mutable_values_and_counter_overflow_keep_generic_continuations() {
        let mutable = Object::List(Rc::new(RefCell::new(vec![Object::Int(1)])));
        let (outer, inner) = tuple_enumeration(vec![mutable.clone(), Object::Int(7)], 7);
        assert!(matches!(
            next_enumerated_tuple(&outer, true),
            NativeObjectPair::Unsupported
        ));
        assert!(matches!(
            &*inner.borrow(),
            PyIterator::Tuple { index: 0, .. }
        ));
        let Object::Iter(cursor) = &outer else {
            unreachable!()
        };
        let Some(Object::Tuple(pair)) = cursor.borrow_mut().next_value() else {
            panic!("mutable value must remain available to generic iteration");
        };
        assert!(matches!(pair[0], Object::Int(7)));
        assert!(pair[1].is_same(&mutable));
        assert!(matches!(
            next_enumerated_tuple(&outer, true),
            NativeObjectPair::Value(8, Object::Int(7))
        ));

        let (outer, inner) = tuple_enumeration(vec![Object::Int(11), Object::Int(22)], i64::MAX);
        assert!(matches!(
            next_enumerated_tuple(&outer, true),
            NativeObjectPair::Unsupported
        ));
        assert!(matches!(
            &*inner.borrow(),
            PyIterator::Tuple { index: 0, .. }
        ));
        let Object::Iter(cursor) = &outer else {
            unreachable!()
        };
        let Some(Object::Tuple(pair)) = cursor.borrow_mut().next_value() else {
            panic!("overflow fallback lost the tuple value");
        };
        assert!(matches!(
            &pair[..],
            [Object::Int(i64::MAX), Object::Int(11)]
        ));
        assert!(matches!(
            next_enumerated_tuple(&outer, true),
            NativeObjectPair::Unsupported
        ));
        let Some(Object::Tuple(pair)) = cursor.borrow_mut().next_value() else {
            panic!("big-counter continuation lost the tuple value");
        };
        assert!(matches!(&pair[0], Object::Long(value)
            if **value == num_bigint::BigInt::from(i64::MAX) + 1));
        assert!(matches!(pair[1], Object::Int(22)));
    }

    #[test]
    fn tuple_borrows_and_wrappers_do_not_consume_values() {
        let (outer, inner) = tuple_enumeration(vec![Object::Int(42)], 0);
        let borrowed = inner.borrow();
        assert!(matches!(
            next_enumerated_tuple(&outer, true),
            NativeObjectPair::Unsupported
        ));
        drop(borrowed);
        let Object::Iter(cursor) = &outer else {
            unreachable!()
        };
        let borrowed = cursor.borrow();
        assert!(matches!(
            next_enumerated_tuple(&outer, true),
            NativeObjectPair::Unsupported
        ));
        drop(borrowed);
        let wrapped = Object::Iter(Rc::new(RefCell::new(PyIterator::Enumerate {
            inner: Rc::new(RefCell::new(PyIterator::Shared(inner.clone()))),
            count: 0,
            count_big: None,
        })));
        assert!(matches!(
            next_enumerated_tuple(&wrapped, true),
            NativeObjectPair::Unsupported
        ));
        assert!(matches!(
            next_enumerated_tuple(&Object::None, true),
            NativeObjectPair::Unsupported
        ));
        assert!(matches!(
            &*inner.borrow(),
            PyIterator::Tuple { index: 0, .. }
        ));
        assert!(matches!(
            next_enumerated_tuple(&outer, true),
            NativeObjectPair::Value(0, Object::Int(42))
        ));
    }
}

#[cfg(test)]
mod startup_compilation_tests {
    use super::{
        budget_import_compilation, compile_allowed, import_compilation_budget,
        startup_compilation_scope, IMPORT_THRESHOLD_FACTOR,
    };

    // Inside a start-up or import scope, a code object compiles only once
    // its counter reaches `IMPORT_THRESHOLD_FACTOR` times the threshold.
    const BUDGET: u32 = IMPORT_THRESHOLD_FACTOR;

    #[test]
    fn import_budget_is_nested_thread_local_and_unwinds() {
        assert!(!import_compilation_budget());
        {
            let _outer = budget_import_compilation();
            assert!(!compile_allowed(BUDGET - 1, 1));
            assert!(compile_allowed(BUDGET, 1));
            {
                let _inner = budget_import_compilation();
                assert!(import_compilation_budget());
            }
            assert!(import_compilation_budget());
            std::thread::spawn(|| assert!(!import_compilation_budget()))
                .join()
                .unwrap();
            let caught = std::panic::catch_unwind(|| {
                let _inner = budget_import_compilation();
                panic!("import failure");
            });
            assert!(caught.is_err());
            assert!(import_compilation_budget());
            {
                let _startup = startup_compilation_scope();
                assert!(!import_compilation_budget());
                {
                    let _nested_import = budget_import_compilation();
                    assert!(!import_compilation_budget());
                    assert!(!compile_allowed(BUDGET - 1, 1));
                    assert!(compile_allowed(BUDGET, 1));
                }
                assert!(!compile_allowed(BUDGET - 1, 1));
                assert!(compile_allowed(BUDGET, 1));
            }
            assert!(import_compilation_budget());
        }
        assert!(!import_compilation_budget());
    }

    #[test]
    fn startup_admission_is_nested_and_unwinds() {
        assert!(compile_allowed(u32::MAX, 1));
        {
            let _outer = startup_compilation_scope();
            assert!(!compile_allowed(BUDGET - 1, 1));
            assert!(compile_allowed(BUDGET, 1));
            {
                let _inner = startup_compilation_scope();
                assert!(!compile_allowed(BUDGET - 1, 1));
                assert!(compile_allowed(BUDGET, 1));
            }
            assert!(!compile_allowed(BUDGET - 1, 1));
            assert!(compile_allowed(BUDGET, 1));
            let caught = std::panic::catch_unwind(|| {
                let _inner = startup_compilation_scope();
                panic!("startup failure");
            });
            assert!(caught.is_err());
            assert!(!compile_allowed(BUDGET - 1, 1));
            assert!(compile_allowed(BUDGET, 1));
        }
        assert!(compile_allowed(u32::MAX, 1));
    }
}
