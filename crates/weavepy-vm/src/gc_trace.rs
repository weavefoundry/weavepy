//! Tracing cycle collector — RFC 0024.
//!
//! `Rc<…>` doesn't collect cycles; without help, programs that
//! build self-referential structures (`n.self = n`) leak forever.
//! CPython solved this with a generational tracing collector
//! sitting on top of refcounting; we follow the same design.
//!
//! The collector is **process-global** (see [`with_state`]): after
//! RFC 0025 the heap is `Arc`-rooted and `Object` is `Send + Sync`,
//! so objects — and the cycles they form — routinely span OS threads.
//! A single shared `GcState` is the only design that can break a
//! cross-thread cycle, and it mirrors CPython's one-collector-per-
//! interpreter model. `Arc<TrackedHandle>` gives the collector and
//! the weakref registry shared ownership of each slot; the `Arc` is
//! genuinely `Send + Sync` now, so no Clippy suppression is needed for
//! it.
//!
//! The model:
//!
//! - Three **generations** (0/1/2). Most allocations land in 0;
//!   survivors of one collection promote up.
//! - **Tri-color marking** (white/grey/black). White = not yet
//!   visited. Grey = visited, children pending. Black = visited,
//!   children traced.
//! - The **`Traverse` trait** is the per-type "walk my child
//!   refs" callback. Containers implement it (list, dict, set,
//!   tuple, instance, frame, generator, coroutine, type,
//!   bound-method, function); leaf types like `int`/`float`/
//!   `str` skip it.
//! - Allocation is *opt-in*. Containers call
//!   [`GcState::track`] to add themselves; leaf types don't.
//!   A type's flags decide whether tracking is needed at
//!   construction time.
//! - The **eval breaker** triggers a collection when the
//!   generation-0 counter exceeds the threshold (default 2000, as in CPython 3.14).
//!   Collections also happen on explicit `gc.collect()`.
//!
//! Today's implementation is *non-incremental*: a full
//! mark-sweep over the targeted generation runs to completion
//! before the eval loop resumes. Real-world heaps in our test
//! corpus are small enough (low thousands of tracked objects)
//! that the pause is sub-millisecond. Incremental marking is
//! deferred to a future RFC.
//!
//! ## Cycle detection without `Drop`-driven collection
//!
//! Because `Rc<…>` keeps cycles alive, we can't rely on
//! `Drop` to discover them. Instead we use the standard CPython
//! trick:
//!
//! 1. For each tracked object, compute a **gc_refs** counter
//!    initialised from the object's outer (Python-visible)
//!    strong refcount. (We approximate via `Rc::strong_count`,
//!    which is conservative — every Rust-side stash counts —
//!    so the false-positive rate is "we keep more than CPython
//!    would.")
//! 2. Walk every tracked object's `Traverse` impl. For each
//!    child reference *that points to another tracked object
//!    in the same generation*, decrement that child's
//!    `gc_refs`.
//! 3. After the walk, any tracked object with `gc_refs > 0` is
//!    reachable from outside the tracked set; mark it black
//!    and propagate.
//! 4. The remaining white objects form a cycle. They are moved
//!    to the unreachable list, finalisers run (PEP 442), and
//!    the cycle is broken by clearing each container.
//!
//! The mechanism intentionally trades precision for simplicity:
//! it's correct (never collects a still-reachable object) but
//! occasionally too conservative (a transient Rust borrow shows
//! up as `gc_refs > 0`, so the cycle survives one more
//! generation than it strictly has to).

use crate::fasthash::ObjectIdHasher;
use crate::shared_value::ThinArc;
use crate::sync::Rc as HandleRc;
use crate::sync::RefCell;
use std::hash::BuildHasherDefault;
use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};

use crate::object::Object;
use crate::weak_object::WeakObject;
use crate::weakref_registry::{id_of, ObjectId};

/// A set of object ids (addresses), hashed with the address mixer.
type IdSet = std::collections::HashSet<ObjectId, BuildHasherDefault<ObjectIdHasher>>;

type GcIndex = std::collections::HashMap<
    ObjectId,
    HandleRc<TrackedHandle>,
    BuildHasherDefault<ObjectIdHasher>,
>;

/// The standard CPython generation count (3) and default
/// thresholds (CPython 3.14's): gen 0 collects when 2000 net tracked allocations
/// have happened; gen 1 every 10 gen 0 collections; gen 2
/// every 10 gen 1 collections.
pub const N_GENERATIONS: usize = 3;
// Color and generation are bounded states; reference counts remain full-width.
const _: () = assert!(N_GENERATIONS > 0 && N_GENERATIONS <= u8::MAX as usize + 1);
const MAX_GENERATION: u8 = (N_GENERATIONS - 1) as u8;
pub const DEFAULT_THRESHOLDS: [usize; N_GENERATIONS] = [2000, 10, 10];

/// Upper bound on the number of mark-sweep passes a single
/// [`GcState::collect`] runs to reach a fixpoint. Convergence is normally 2–3
/// passes (one to clear the bulk, one or two to drop subgraphs a transient
/// reference pinned); the cap only guards against pathological churn. Also
/// bounds the collect→finalize→collect retry loop the interpreter runs to
/// settle `__del__` chains within a single `gc.collect()`.
pub const MAX_COLLECT_PASSES: usize = 16;

/// `gc.DEBUG_SAVEALL`: instead of freeing unreachable objects, append them to
/// `gc.garbage` so a debugging session can inspect what would have been
/// collected. Mirrors CPython's `gc.set_debug(gc.DEBUG_SAVEALL)`.
const DEBUG_SAVEALL: i64 = 0x20;

/// Walk all child references reachable through `obj`. Used by
/// the GC's mark phase. Container types should implement this;
/// leaf types do nothing.
pub trait Traverse {
    /// Call `visit(child)` once for every directly-owned
    /// `Object` reference. The callback may inspect or even
    /// recurse into children; the GC does its own bookkeeping.
    fn traverse(&self, visit: &mut dyn FnMut(&Object));
}

/// Optional finaliser hook. Containers that want PEP 442
/// resurrection-aware finalisation implement this.
pub trait Finalize {
    fn finalize(&self);
}

/// A tracked object's entry: a non-owning handle on the object plus the
/// bookkeeping that outlives a collection. The collector never keeps an
/// object alive. An entry whose object died stays in its generation until
/// the next collection or prune of that generation drops it; its weak
/// handle keeps the allocation reserved until then, so the address the
/// index keys it by can't be reused by another object meanwhile.
#[allow(missing_debug_implementations)]
pub struct TrackedHandle {
    /// The tracked object, held weakly.
    pub object: WeakObject,
    /// Identity, computed from `id_of(object)` at `track` time.
    pub id: ObjectId,
    /// [`color::Frozen`] while `gc.freeze()` holds the entry, otherwise
    /// [`color::White`]. Collections keep their marking state in the
    /// candidates they build, not here.
    pub color: AtomicU8,
    /// Generation index (0..N_GENERATIONS). Survivors are promoted by
    /// incrementing this.
    pub generation: AtomicU8,
    /// Position of this handle within its owning `Vec` —
    /// `generations[generation].handles` normally, or the `frozen`
    /// list when `color == Frozen`. Maintained by every site that
    /// pushes, drains, or rebuilds those vectors so that
    /// [`GcState::untrack_id`] can `swap_remove` in O(1).
    pub slot: CachedSlot,
    /// Has this object's `__del__` already *run* to completion? CPython
    /// guarantees a finaliser runs at most once.
    pub finalized: AtomicBool,
    /// Has this object's `__del__` been *queued* by a collection but not yet
    /// run? While set, the object is excluded from the `collected` count:
    /// its finalizer (drained after `gc.collect()` returns) may resurrect
    /// it, and CPython only counts objects that are actually reclaimed.
    pub finalize_queued: AtomicBool,
}

/// A compact vector-position hint, with separate absent and uncached states.
///
/// Positions too large to cache use the pointer-search fallback. Neither
/// decoded sentinel can index a live `Vec<Arc<TrackedHandle>>`, whose
/// allocation is bounded by `isize::MAX`.
#[derive(Debug)]
pub struct CachedSlot(AtomicU32);

impl CachedSlot {
    const fn encode(slot: usize) -> u32 {
        if slot == usize::MAX {
            u32::MAX
        } else if slot >= (u32::MAX - 1) as usize {
            u32::MAX - 1
        } else {
            slot as u32
        }
    }

    const fn decode(slot: u32) -> usize {
        if slot == u32::MAX {
            usize::MAX
        } else if slot == u32::MAX - 1 {
            usize::MAX - 1
        } else {
            slot as usize
        }
    }

    pub const fn new(slot: usize) -> Self {
        Self(AtomicU32::new(Self::encode(slot)))
    }

    #[inline]
    pub fn load(&self, order: Ordering) -> usize {
        Self::decode(self.0.load(order))
    }

    #[inline]
    pub fn store(&self, slot: usize, order: Ordering) {
        self.0.store(Self::encode(slot), order);
    }

    #[inline]
    pub fn swap(&self, slot: usize, order: Ordering) -> usize {
        Self::decode(self.0.swap(Self::encode(slot), order))
    }
}

#[allow(non_upper_case_globals)]
pub mod color {
    pub const White: u8 = 0;
    pub const Grey: u8 = 1;
    pub const Black: u8 = 2;
    pub const Frozen: u8 = 3;
}

impl TrackedHandle {
    /// A generation-`generation` entry for `object`, or `None` for a value
    /// with no heap allocation of its own.
    pub fn new(object: &Object, generation: usize) -> Option<Self> {
        assert!(generation < N_GENERATIONS, "invalid collector generation");
        Some(Self {
            id: id_of(object),
            object: WeakObject::new(object)?,
            color: AtomicU8::new(color::White),
            generation: AtomicU8::new(generation as u8),
            slot: CachedSlot::new(0),
            finalized: AtomicBool::new(false),
            finalize_queued: AtomicBool::new(false),
        })
    }
}

/// Swap-remove the handle at `slot` from `vec`, fixing up the slot
/// index of whatever handle gets moved into the vacated position.
/// O(1): the only handle whose position changes is the one swapped
/// in from the end, and its `slot` field is corrected here so the
/// per-handle position invariant holds after the call.
#[inline]
fn swap_remove_handle(vec: &mut Vec<HandleRc<TrackedHandle>>, slot: usize) {
    if slot >= vec.len() {
        return;
    }
    vec.swap_remove(slot);
    if let Some(moved) = vec.get(slot) {
        moved.slot.store(slot, Ordering::Release);
    }
}

/// Correctness fallback for [`GcState::untrack_id`]: when a handle's cached
/// `slot` no longer points at it (a concurrent `swap_remove`/promotion on
/// another OS thread moved it before this thread acquired the vector lock),
/// locate it by pointer identity and `swap_remove` it. Returns `true` if the
/// handle was found and removed. O(n) in the generation length, but only ever
/// taken on the rare stale-cache path — the common case stays O(1).
#[inline]
fn remove_handle_by_ptr(
    vec: &mut Vec<HandleRc<TrackedHandle>>,
    handle: &HandleRc<TrackedHandle>,
) -> bool {
    if let Some(pos) = vec.iter().position(|h| HandleRc::ptr_eq(h, handle)) {
        swap_remove_handle(vec, pos);
        true
    } else {
        false
    }
}

#[derive(Default)]
struct Generation {
    /// All tracked handles in this generation. Append-only
    /// during normal allocation; rewritten in place when
    /// objects are promoted or moved to the unreachable list.
    handles: Vec<HandleRc<TrackedHandle>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GcStats {
    pub collections: u64,
    pub collected: u64,
    pub uncollectable: u64,
}

/// One object under examination by a collection: a strong reference for
/// the collection's duration, its entry (absent for a temporary candidate
/// discovered through an untracked container), and the marking state.
struct Cand {
    obj: Object,
    id: ObjectId,
    entry: Option<HandleRc<TrackedHandle>>,
    gc_refs: std::cell::Cell<i64>,
    color: std::cell::Cell<u8>,
}

impl Cand {
    fn new(obj: Object, id: ObjectId, entry: Option<HandleRc<TrackedHandle>>) -> Self {
        Self {
            obj,
            id,
            entry,
            gc_refs: std::cell::Cell::new(0),
            color: std::cell::Cell::new(color::White),
        }
    }

    fn is_white(&self) -> bool {
        self.color.get() == color::White
    }

    /// Whether the object still owes a `__del__` (or a generator close).
    fn pending_finalizer(&self) -> bool {
        has_finalizer(&self.obj)
            && !self
                .entry
                .as_ref()
                .is_some_and(|e| e.finalized.load(Ordering::Acquire))
    }
}

/// How many entries of the older generations each young collection or
/// prune examines for dead objects, so a dead old object's allocation is
/// released within a bounded number of young collections rather than at
/// the next full one.
const OLD_PRUNE_BUDGET: usize = 512;

/// Public state of the cycle GC.
///
/// A single instance lives in a process-global `LazyLock` (see
/// [`with_state`]) and is shared by every OS thread, mirroring
/// CPython's one-collector-per-interpreter model. This is required
/// for correctness: the heap is `Arc`-rooted and a cycle's links can
/// be allocated on different threads, so only a shared tracked-set can
/// ever observe and break such a cycle. All fields are `Sync` (interior
/// `GilCell`s + atomics), so concurrent access is memory-safe; the GIL
/// additionally serializes mutators.
///
/// The collector owns nothing: its entries are weak, so objects die by
/// reference count exactly when CPython's would, and collections only
/// have to find and break the cycles.
#[allow(missing_debug_implementations)]
pub struct GcState {
    generations: RefCell<[Generation; N_GENERATIONS]>,
    /// Insert-only miss-filter over `index`, maintained at
    /// [`Self::track_now`] and consulted by the usually-miss `is_tracked`
    /// probes. Rebuilt from `index` once most of its bits name objects
    /// long gone.
    tracked_filter: crate::hot_filter::RebuildableBloom,
    /// Id → handle index over every tracked object (all generations plus
    /// the frozen set): keeps `track` dedupe and `is_tracked` O(1).
    index: RefCell<GcIndex>,
    /// Re-entrancy guard: a nested collection would see torn generation
    /// lists. Atomic so the whole `GcState` is `Sync`.
    collecting: AtomicBool,
    /// Per-generation thresholds. Gen 0's threshold is
    /// "allocations since last gen 0 collection"; gens 1 and 2
    /// are "collections of the previous gen since last
    /// collection of this gen".
    thresholds: RefCell<[usize; N_GENERATIONS]>,
    /// Live counters: how many allocations / collection ticks
    /// have happened since the last collection of each
    /// generation.
    counts: RefCell<[usize; N_GENERATIONS]>,
    /// Lock-free mirror of `counts[0]` and `thresholds[0]`, packed as
    /// `count << 32 | threshold`, for the allocation sites' due check.
    gen0_gauge: AtomicU64,
    /// Containers born holding only atomic values (see
    /// [`container_can_cycle`]). They cannot take part in a cycle while
    /// that holds, so they are not registered with the collector — only
    /// remembered here, so that a collection can re-examine them and
    /// promote any that has since acquired a non-atomic element.
    deferred: RefCell<Vec<DeferredContainer>>,
    /// Length at which [`GcState::sweep_deferred`] compacts `deferred`.
    deferred_limit: AtomicUsize,
    /// Instances born since the last collection, held weakly: most die
    /// young, and registering one with the collector (an index entry and
    /// a handle) costs far more than its life. Their births count toward
    /// the gen-0 threshold as eager registration's would, and every
    /// collection, reflective API and finalization pass first hands the
    /// collector the ones still alive ([`GcState::flush_young`]), so it
    /// sees exactly the population eager tracking would have shown it.
    young: RefCell<Vec<crate::sync::Weak<crate::types::PyInstance>>>,
    /// Functions born since the last collection, held weakly as
    /// [`Self::young`] holds instances (a `def` or `lambda` run in a loop
    /// makes one per pass). A flush also hands the collector each live
    /// one's globals dict, which [`track`] would have tracked at birth.
    young_fns: RefCell<Vec<crate::sync::Weak<crate::object::PyFunction>>>,
    /// Frozen handles. `gc.freeze()` moves all tracked objects
    /// here; they are skipped by future collections until
    /// `gc.unfreeze()` runs.
    frozen: RefCell<Vec<HandleRc<TrackedHandle>>>,
    /// `gc.garbage` — uncollectable objects (cycles whose
    /// finalisers refused to release).
    pub garbage: RefCell<Vec<Object>>,
    /// `gc.callbacks` — list of user callbacks invoked at
    /// cycle start/stop.
    pub callbacks: RefCell<Vec<Object>>,
    /// Per-generation aggregate stats.
    pub stats: RefCell<[GcStats; N_GENERATIONS]>,
    /// `gc.set_debug` flag. Drives `gc.DEBUG_*` printing.
    pub debug: AtomicI64,
    enabled: AtomicBool,
    /// Bumped on every change to the tracked-object set so
    /// callers can know when to invalidate caches.
    pub tracked_version: AtomicUsize,
    /// Total tracked-object population (entries, live or not yet pruned).
    pub tracked_count: AtomicUsize,
    /// Ids whose `__del__` has been run (or queued) by a finalizing
    /// collection or teardown. Persists past the point where the handle
    /// leaves the tracked set so `gc.is_finalized()` still answers `True`
    /// for an object its finalizer resurrected (PEP 442 / `test_is_finalized`).
    finalized_ids: RefCell<crate::fasthash::FxHashSet<ObjectId>>,
    /// Where the next prune of the older generations resumes.
    old_prune_cursor: AtomicUsize,
}

impl Default for GcState {
    fn default() -> Self {
        Self::new()
    }
}

impl GcState {
    pub fn new() -> Self {
        Self {
            generations: RefCell::new(Default::default()),
            tracked_filter: crate::hot_filter::RebuildableBloom::new(),
            index: RefCell::new(GcIndex::default()),
            collecting: AtomicBool::new(false),
            thresholds: RefCell::new(DEFAULT_THRESHOLDS),
            counts: RefCell::new([0; N_GENERATIONS]),
            gen0_gauge: AtomicU64::new(DEFAULT_THRESHOLDS[0] as u64),
            deferred: RefCell::new(Vec::new()),
            deferred_limit: AtomicUsize::new(DEFERRED_FLOOR),
            young: RefCell::new(Vec::new()),
            young_fns: RefCell::new(Vec::new()),
            frozen: RefCell::new(Vec::new()),
            garbage: RefCell::new(Vec::new()),
            callbacks: RefCell::new(Vec::new()),
            stats: RefCell::new([GcStats::default(); N_GENERATIONS]),
            debug: AtomicI64::new(0),
            enabled: AtomicBool::new(true),
            tracked_version: AtomicUsize::new(0),
            tracked_count: AtomicUsize::new(0),
            finalized_ids: RefCell::new(crate::fasthash::FxHashSet::default()),
            old_prune_cursor: AtomicUsize::new(0),
        }
    }

    /// Reinitialise the collector's locks in a `fork(2)` child, preserving the
    /// inherited tracked-object state. An `Object` whose last `Arc` is
    /// released on a peer thread can drop without that thread holding the
    /// GIL; if such a peer vanishes mid-`borrow` in the fork, the inherited
    /// `parking_lot` lock would wedge the child's very first allocation
    /// (`test_threading.test_reinit_tls_after_fork`). Rebuild every field's
    /// lock in place and clear the re-entrancy guard — CPython's
    /// `PyOS_AfterFork_Child` reinitialises the runtime's locks for the same
    /// reason.
    ///
    /// # Safety
    ///
    /// `this` must point at the process-global collector on the lone
    /// surviving thread of a fork child, so the in-place lock rebuilds cannot
    /// race and the payloads (last mutated under the GIL the forking thread
    /// holds) are consistent.
    pub unsafe fn reinit_after_fork_in_child(this: *mut Self) {
        unsafe {
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).generations));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).index));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).thresholds));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).counts));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).deferred));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).young));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).young_fns));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).frozen));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).garbage));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).callbacks));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).stats));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).finalized_ids));
            // A peer may have vanished mid-collection with this set.
            (*this).collecting.store(false, Ordering::Release);
        }
    }

    /// Record that `id`'s finalizer has been run (or queued). Survives the
    /// handle's removal from the tracked set so `gc.is_finalized` keeps
    /// answering `True` for a resurrected object.
    pub fn note_finalized(&self, id: ObjectId) {
        self.finalized_ids.borrow_mut().insert(id);
    }

    /// Has `id`'s finalizer already run? Backs `gc.is_finalized`.
    pub fn was_finalized(&self, id: ObjectId) -> bool {
        self.finalized_ids.borrow().contains(&id)
    }

    /// Record that `id`'s finalizer has finished running: set `finalized`,
    /// clear the `finalize_queued` deferral flag, and remember it for
    /// `gc.is_finalized`. Called by the interpreter the moment a queued
    /// `__del__` returns, so the next collection treats a non-resurrected
    /// object as plain dead garbage (and a resurrected one is never
    /// re-finalized).
    pub fn complete_finalizer(&self, id: ObjectId) {
        self.note_finalized(id);
        if let Some(h) = self.handle_for(id) {
            h.finalized.store(true, Ordering::Release);
            h.finalize_queued.store(false, Ordering::Release);
        }
    }

    /// Track `obj` for cycle detection. Idempotent — if `obj`
    /// is already tracked, this is a no-op.
    pub fn track(&self, obj: &Object) {
        // A container holding only atomic values cannot anchor a cycle
        // yet, so it stays off the GC's books until it holds something
        // that could close one. `defer_container` remembers it so the
        // next collection can promote it first.
        if !container_can_cycle(obj) && self.defer_container(obj) {
            return;
        }
        if let Object::Instance(inst) = obj {
            if self.nurse(&self.young, inst) {
                return;
            }
        }
        self.track_now(obj);
    }

    /// [`Self::track`] for a function: young (see [`Self::young_fns`]), or
    /// registered at once together with its globals dict.
    fn track_function(&self, obj: &Object, f: &crate::Rc<crate::object::PyFunction>) {
        if self.nurse(&self.young_fns, f) {
            return;
        }
        self.track(&Object::Dict(f.globals.clone()));
        self.track_now(obj);
    }

    /// Register a newborn instance (or function) in its young set (see
    /// [`Self::young`]). `false` leaves it to eager registration.
    fn nurse<T: 'static>(
        &self,
        set: &RefCell<Vec<crate::sync::Weak<T>>>,
        obj: &crate::Rc<T>,
    ) -> bool {
        let over = {
            // SAFETY: nothing below runs code while the set is borrowed
            // (a dead predecessor's release only frees memory).
            let Some(young) = (unsafe { set.peek_mut() }) else {
                // A flush is walking the set, or cells are shared.
                return false;
            };
            // Churn (a loop that builds an object and drops the previous
            // one) leaves its dead predecessors at the tail: reclaim them
            // here, so the set neither grows nor parks dead allocations.
            let n = young.len();
            for i in (n.saturating_sub(2)..n).rev() {
                if young[i].strong_count() == 0 {
                    young.swap_remove(i);
                }
            }
            young.push(crate::Rc::downgrade(obj));
            young.len() >= YOUNG_CAP
        };
        // As in `track_now`: an id recycled from a finalized object starts
        // un-finalized (a finalizer that runs from here on is recorded).
        // SAFETY: a read that runs no code.
        if unsafe { self.finalized_ids.peek() }.is_none_or(|f| !f.is_empty()) {
            let id = crate::Rc::as_ptr(obj) as usize as ObjectId;
            self.finalized_ids.borrow_mut().remove(&id);
        }
        self.note_gen0_alloc();
        if over {
            self.flush_young();
        }
        true
    }

    /// Register the young instances and functions still alive with the
    /// collector.
    fn flush_young(&self) {
        if let Ok(mut fns) = self.young_fns.try_borrow_mut() {
            if !fns.is_empty() {
                let fns = std::mem::take(&mut *fns);
                for w in fns {
                    if let Some(f) = w.upgrade() {
                        self.track(&Object::Dict(f.globals.clone()));
                        self.register(&Object::Function(f), false);
                    }
                }
            }
        }
        let young = match self.young.try_borrow_mut() {
            Ok(mut young) if !young.is_empty() => std::mem::take(&mut *young),
            _ => return,
        };
        for w in young {
            if let Some(inst) = w.upgrade() {
                self.register(&Object::Instance(inst), false);
            }
        }
    }

    /// The young instances and functions still alive (the dead ones are
    /// dropped).
    fn young_live(&self) -> usize {
        let fns = self.young_fns.try_borrow_mut().map_or(0, |mut fns| {
            fns.retain(|w| w.strong_count() > 0);
            fns.len()
        });
        let Ok(mut young) = self.young.try_borrow_mut() else {
            return fns;
        };
        young.retain(|w| w.strong_count() > 0);
        young.len() + fns
    }

    /// Whether the instance or function `id` is in a young set.
    fn is_young(&self, id: ObjectId) -> bool {
        fn holds<T: 'static>(set: &RefCell<Vec<crate::sync::Weak<T>>>, id: ObjectId) -> bool {
            set.try_borrow().is_ok_and(|young| {
                young
                    .iter()
                    .any(|w| w.as_ptr() as usize as ObjectId == id && w.strong_count() > 0)
            })
        }
        holds(&self.young, id) || holds(&self.young_fns, id)
    }

    /// [`Self::track`] for a container its builder knows holds only
    /// atomic values (a `str.split` result): deferred whatever its size,
    /// without the element scan, which `track` caps at `SCAN_CAP`
    /// elements and so registers every longer list eagerly.
    pub fn track_inert(&self, obj: &Object) {
        debug_assert!(match obj {
            Object::List(l) => l.borrow().iter().all(element_is_inert),
            _ => false,
        });
        if !self.defer_container(obj) {
            self.track_now(obj);
        }
    }

    /// Remember `obj` weakly instead of tracking it. Returns `false` when
    /// the kind has no deferred form (the caller tracks it as before).
    ///
    /// A deferred birth does not advance the gen-0 allocation counter.
    /// That counter paces automatic collections against the rate cyclic
    /// garbage can appear, and nothing deferred here can be part of a
    /// cycle. A population that genuinely accumulates is paced by
    /// `DEFERRED_CAP` instead.
    fn defer_container(&self, obj: &Object) -> bool {
        let Some(weak) = DeferredContainer::new(obj) else {
            return false;
        };
        let over = {
            let Ok(mut deferred) = self.deferred.try_borrow_mut() else {
                // A sweep is already walking the list (it upgrades and can
                // re-enter through a promotion). Track eagerly rather than
                // queue onto a list we cannot touch.
                return false;
            };
            // The churn shape — a loop that builds a container and drops
            // the previous one — leaves its dead predecessors just below
            // the tail: reclaim them here, so steady churn neither grows
            // the list nor parks dead allocations until a sweep.
            let n = deferred.len();
            for i in (n.saturating_sub(2)..n).rev() {
                if deferred[i].is_dead() {
                    deferred.swap_remove(i);
                }
            }
            deferred.push(weak);
            deferred.len() >= self.deferred_limit.load(Ordering::Relaxed)
        };
        if over {
            self.sweep_deferred(false);
        }
        true
    }

    /// Re-examine the deferred containers: drop the ones that have died,
    /// and hand the collector every one that has since acquired a
    /// non-atomic element — `promote_all` forces the handover even for
    /// those still holding only scalars (`gc.get_objects`, which must
    /// enumerate them the way CPython does).
    ///
    /// Every edge of a reference cycle points at a container, so a cycle
    /// member always holds a non-atomic value and is always promoted
    /// here. Running this before a collection's mark phase is therefore
    /// enough for the collector to see every cycle it would have seen
    /// with eager tracking.
    fn sweep_deferred(&self, promote_all: bool) {
        self.flush_young();
        let mut promote: Vec<Object> = Vec::new();
        {
            let Ok(mut deferred) = self.deferred.try_borrow_mut() else {
                return;
            };
            deferred.retain(|entry| {
                let Some(obj) = entry.upgrade() else {
                    return false;
                };
                if promote_all || container_can_cycle(&obj) {
                    promote.push(obj);
                    return false;
                }
                true
            });
            let live = deferred.len();
            if live >= DEFERRED_CAP {
                // The set has stopped being a churn buffer: hand it all
                // over, so these allocations resume pacing collections.
                promote.extend(deferred.drain(..).filter_map(|e| e.upgrade()));
            }
            self.deferred_limit.store(
                DEFERRED_FLOOR.max(deferred.len().saturating_mul(2)),
                Ordering::Relaxed,
            );
        }
        for obj in promote {
            self.track_now(&obj);
        }
    }

    /// Hand every deferred container to the collector. For the reflective
    /// APIs, which must enumerate the same population CPython does.
    pub fn promote_all_deferred(&self) {
        self.sweep_deferred(true);
    }

    /// [`Self::track`] without the deferral filter: register `obj` with the
    /// collector unconditionally. For objects the caller has already decided
    /// must be tracked (a promoted deferral, `gc.is_tracked`, `gc.get_objects`).
    pub fn track_now(&self, obj: &Object) {
        if self.register(obj, true) {
            self.note_gen0_alloc();
        }
    }

    /// Enter `obj` in the index and generation 0 (`false`: it was already
    /// there, or has no heap allocation of its own). A `fresh` object's id
    /// may be a finalized one's, recycled; a young instance's own
    /// finalizer may already have run, and its entry says so.
    fn register(&self, obj: &Object, fresh: bool) -> bool {
        let new_id = id_of(obj);
        {
            let mut index = self.index.borrow_mut();
            // One probe decides the dedupe and the insert. An entry whose
            // object died can't sit at this address: its weak handle keeps
            // the allocation reserved until the entry is pruned.
            let entry = match index.entry(new_id) {
                std::collections::hash_map::Entry::Occupied(_) => return false,
                std::collections::hash_map::Entry::Vacant(e) => e,
            };
            let Some(handle) = TrackedHandle::new(obj, 0) else {
                return false;
            };
            let handle = HandleRc::new(handle);
            // Publish to the miss-filter *before* the insert becomes
            // observable (we hold the index borrow).
            self.tracked_filter.insert(new_id);
            entry.insert(handle.clone());
            // `finalized_ids` is keyed by object id (a pointer), which the
            // allocator recycles. A freshly tracked object at a recycled
            // address must start *un*-finalized (`test_is_finalized`).
            // Almost always empty (only `__del__`-bearing objects ever land
            // there).
            if !self.finalized_ids.borrow().is_empty() {
                if fresh {
                    self.finalized_ids.borrow_mut().remove(&new_id);
                } else if self.finalized_ids.borrow().contains(&new_id) {
                    handle.finalized.store(true, Ordering::Release);
                }
            }
            let mut gens = self.generations.borrow_mut();
            handle.slot.store(gens[0].handles.len(), Ordering::Release);
            gens[0].handles.push(handle);
        }
        serial_add(&self.tracked_count, 1);
        serial_add(&self.tracked_version, 1);
        true
    }

    /// Stop tracking `obj`. Backs the explicit `gc._untrack(obj)` extension
    /// and the C-API `PyObject_GC_UnTrack`.
    pub fn untrack_id(&self, id: ObjectId) {
        // A registered object is found by one probe; only then scan the
        // young sets (an object is in at most one of the three), which
        // can hold thousands of entries.
        let handle = self.index.borrow_mut().remove(&id);
        if let Some(handle) = handle {
            self.untrack_registered(handle);
            return;
        }
        // (From the newest: an untrack usually follows its birth closely.)
        if let Ok(mut fns) = self.young_fns.try_borrow_mut() {
            if let Some(i) = fns
                .iter()
                .rposition(|w| w.as_ptr() as usize as ObjectId == id)
            {
                fns.swap_remove(i);
                let mut counts = self.counts.borrow_mut();
                counts[0] = counts[0].saturating_sub(1);
                self.sync_gen0_gauge(counts[0], None);
                return;
            }
        }
        if let Ok(mut young) = self.young.try_borrow_mut() {
            if let Some(i) = young
                .iter()
                .rposition(|w| w.as_ptr() as usize as ObjectId == id)
            {
                young.swap_remove(i);
                let mut counts = self.counts.borrow_mut();
                counts[0] = counts[0].saturating_sub(1);
                self.sync_gen0_gauge(counts[0], None);
            }
        }
    }

    /// [`Self::untrack_id`] for an object found in the index.
    fn untrack_registered(&self, handle: HandleRc<TrackedHandle>) {
        {
            let mut counts = self.counts.borrow_mut();
            counts[0] = counts[0].saturating_sub(1);
            self.sync_gen0_gauge(counts[0], None);
        }
        // O(1) removal via the handle's cached `slot`. The cached position is
        // only valid under the owning vector's lock; fall back to a pointer
        // search if it's stale.
        if handle.color.load(Ordering::Acquire) == color::Frozen {
            let mut frozen = self.frozen.borrow_mut();
            let slot = handle.slot.load(Ordering::Acquire);
            if frozen
                .get(slot)
                .is_some_and(|h| HandleRc::ptr_eq(h, &handle))
            {
                swap_remove_handle(&mut frozen, slot);
            } else {
                remove_handle_by_ptr(&mut frozen, &handle);
            }
        } else {
            let mut gens = self.generations.borrow_mut();
            let g = usize::from(
                handle
                    .generation
                    .load(Ordering::Acquire)
                    .min(MAX_GENERATION),
            );
            let slot = handle.slot.load(Ordering::Acquire);
            if gens[g]
                .handles
                .get(slot)
                .is_some_and(|h| HandleRc::ptr_eq(h, &handle))
            {
                swap_remove_handle(&mut gens[g].handles, slot);
            } else if !remove_handle_by_ptr(&mut gens[g].handles, &handle) {
                for gg in 0..N_GENERATIONS {
                    if gg != g && remove_handle_by_ptr(&mut gens[gg].handles, &handle) {
                        break;
                    }
                }
            }
        }
        serial_add(&self.tracked_count, usize::MAX);
        serial_add(&self.tracked_version, 1);
    }

    /// Drop the entries of objects that have died from `handles`, and from
    /// the index; `budget` caps how many entries are examined, starting at
    /// `start` (wrapping). Returns how many were dropped and where the
    /// examination stopped.
    fn prune_dead_in(
        index: &mut GcIndex,
        handles: &mut Vec<HandleRc<TrackedHandle>>,
        start: usize,
        budget: usize,
    ) -> (usize, usize) {
        let mut removed = 0;
        let mut i = if start < handles.len() { start } else { 0 };
        let mut examined = 0;
        while examined < budget && i < handles.len() {
            examined += 1;
            if handles[i].object.is_dead() {
                let h = handles.swap_remove(i);
                index.remove(&h.id);
                if let Some(moved) = handles.get(i) {
                    moved.slot.store(i, Ordering::Release);
                }
                removed += 1;
            } else {
                i += 1;
            }
        }
        (removed, i)
    }

    /// Drop the dead young entries, plus a bounded slice of the older
    /// generations'. Returns the young generation's live population.
    fn prune(&self) -> usize {
        let (removed, live) = {
            let mut index = self.index.borrow_mut();
            let mut gens = self.generations.borrow_mut();
            let (mut removed, _) =
                Self::prune_dead_in(&mut index, &mut gens[0].handles, 0, usize::MAX);
            // The older generations, a slice at a time: the cursor runs
            // over gen 1 then gen 2 as one sequence.
            let cursor = self.old_prune_cursor.load(Ordering::Relaxed);
            let n1 = gens[1].handles.len();
            let (g, start) = if cursor < n1 {
                (1, cursor)
            } else {
                (2, cursor - n1)
            };
            let (r, stop) =
                Self::prune_dead_in(&mut index, &mut gens[g].handles, start, OLD_PRUNE_BUDGET);
            removed += r;
            let next = if stop >= gens[g].handles.len() {
                if g == 1 {
                    gens[1].handles.len()
                } else {
                    0
                }
            } else if g == 1 {
                stop
            } else {
                gens[1].handles.len() + stop
            };
            self.old_prune_cursor.store(next, Ordering::Relaxed);
            (removed, gens[0].handles.len())
        };
        if removed > 0 {
            self.tracked_count.fetch_sub(
                removed.min(self.tracked_count.load(Ordering::Acquire)),
                Ordering::AcqRel,
            );
            serial_add(&self.tracked_version, 1);
        }
        live
    }

    pub fn is_tracked(&self, id: ObjectId) -> bool {
        // Usually-miss probe: two relaxed loads instead of the index
        // borrow. A stale filter bit just takes the precise path.
        if !self.tracked_filter.may_contain(id) {
            return false;
        }
        self.index.borrow().contains_key(&id)
    }

    /// [`Self::is_tracked`] for an instance or function, which may be
    /// young.
    pub fn is_tracked_instance(&self, id: ObjectId) -> bool {
        self.is_tracked(id) || self.is_young(id)
    }

    /// O(1) handle lookup by object id (any generation or frozen).
    pub fn handle_for(&self, id: ObjectId) -> Option<HandleRc<TrackedHandle>> {
        if !self.tracked_filter.may_contain(id) {
            return None;
        }
        self.index.borrow().get(&id).cloned()
    }

    /// Snapshot every live tracked object that still carries an unrun
    /// `__del__`. The interpreter's shutdown pass walks this list to
    /// finalize objects that are still alive at exit. The per-handle
    /// `finalized` flag guarantees each `__del__` runs at most once.
    pub fn finalization_candidates(&self) -> Vec<(HandleRc<TrackedHandle>, Object)> {
        self.flush_young();
        let mut out = Vec::new();
        let mut consider = |h: &HandleRc<TrackedHandle>| {
            // A finalizer already queued by a collection (but not yet
            // drained) must not be listed again.
            if h.finalized.load(Ordering::Acquire) || h.finalize_queued.load(Ordering::Acquire) {
                return;
            }
            if let Some(obj) = h.object.upgrade() {
                if has_finalizer(&obj) {
                    out.push((h.clone(), obj));
                }
            }
        };
        for gen in self.generations.borrow().iter() {
            gen.handles.iter().for_each(&mut consider);
        }
        self.frozen.borrow().iter().for_each(&mut consider);
        out
    }

    /// Number of tracked objects in each generation.
    pub fn counts(&self) -> [usize; N_GENERATIONS] {
        // A deferred container is an allocation an eagerly-tracking build
        // would have counted, so report it as one: drop the ones that have
        // died, then add those still live. `gc.get_count()` and
        // `_testinternalcapi.get_tracked_heap_size()` then read exactly as
        // they would have (`test_gc.test_heap_size`).
        self.sweep_deferred(false);
        let mut counts = *self.counts.borrow();
        counts[0] = counts[0].saturating_add(self.deferred.borrow().len());
        counts
    }

    pub fn thresholds(&self) -> [usize; N_GENERATIONS] {
        *self.thresholds.borrow()
    }

    pub fn set_thresholds(&self, t: [usize; N_GENERATIONS]) {
        *self.thresholds.borrow_mut() = t;
        self.sync_gen0_gauge(self.counts.borrow()[0], Some(t[0]));
    }

    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn bump_count(&self, gen: usize) {
        let mut counts = self.counts.borrow_mut();
        counts[gen] = counts[gen].saturating_add(1);
        if gen == 0 {
            self.sync_gen0_gauge(counts[0], None);
        }
    }

    /// Account one gen-0 allocation and, on crossing `threshold0`,
    /// schedule the automatic collection for the next safe point — not
    /// every allocation site polls `maybe_auto_collect` itself
    /// (`MAKE_FUNCTION`, `list(it)`, …).
    fn note_gen0_alloc(&self) {
        // SAFETY: nothing runs while the counts are updated.
        match unsafe { self.counts.peek_mut() } {
            Some(counts) => {
                counts[0] = counts[0].saturating_add(1);
                self.sync_gen0_gauge(counts[0], None);
            }
            None => self.bump_count(0),
        }
        let gauge = self.gen0_gauge.load(Ordering::Relaxed);
        let threshold = gauge & 0xffff_ffff;
        if threshold != 0 && (gauge >> 32) == threshold && self.is_enabled() {
            crate::hot_gates::set(crate::hot_gates::GC_DUE);
        }
    }

    /// Republish the gen-0 gauge after a write to `counts[0]` or
    /// `thresholds[0]` (see the field docs).
    #[inline]
    fn sync_gen0_gauge(&self, count: usize, threshold: Option<usize>) {
        let th = match threshold {
            Some(t) => t as u64,
            None => self.gen0_gauge.load(Ordering::Relaxed) & 0xffff_ffff,
        };
        self.gen0_gauge
            .store(((count as u64) << 32) | th, Ordering::Relaxed);
    }

    /// Whether the next [`Self::maybe_auto_collect`] would consider a
    /// collection (the young-generation counter has reached its
    /// threshold). Lets an allocation site that cannot run finalizers
    /// hand the allocation to one that can.
    pub fn auto_collect_due(&self) -> bool {
        if !self.is_enabled() || self.collecting.load(Ordering::Acquire) {
            return false;
        }
        let gauge = self.gen0_gauge.load(Ordering::Relaxed);
        let threshold = gauge & 0xffff_ffff;
        threshold != 0 && (gauge >> 32) >= threshold
    }

    /// Threshold-driven automatic collection (CPython's `gc_alloc`
    /// path): when the gen-0 counter passes `threshold0`, collect the
    /// *oldest* generation whose own counter has also passed its
    /// threshold. Returns whether a collection ran. Callers must be at a
    /// safe point (no outstanding container borrows).
    ///
    /// CPython's counter is allocations *minus deallocations* of tracked
    /// objects. Deaths aren't counted as they happen here, so the counter
    /// is corrected when it trips: the dead young entries are pruned and
    /// the count restarts from the survivors, unless they alone make up
    /// half the threshold (which also bounds the pruning work at two
    /// entries per allocation).
    pub fn maybe_auto_collect(&self) -> bool {
        if !self.is_enabled() || self.collecting.load(Ordering::Acquire) {
            return false;
        }
        let (due, eligible, threshold0) = {
            let counts = self.counts.borrow();
            let thresholds = self.thresholds.borrow();
            if thresholds[0] == 0 {
                return false;
            }
            let mut gen = 0;
            if counts[1] + 1 >= thresholds[1] {
                gen = 1;
                if counts[2] + 1 >= thresholds[2] {
                    gen = 2;
                }
            }
            (counts[0] >= thresholds[0], gen, thresholds[0])
        };
        if !due {
            return false;
        }
        let live = self.prune() + self.young_live();
        if live < threshold0 / 2 {
            let mut counts = self.counts.borrow_mut();
            counts[0] = live;
            self.sync_gen0_gauge(live, None);
            return false;
        }
        // Automatic young collection: a single pass (see `collect_impl`'s
        // `exact` discussion).
        self.collect_impl(eligible, false);
        true
    }

    /// Total population (across all generations + frozen).
    pub fn population(&self) -> usize {
        self.flush_young();
        let gens = self.generations.borrow();
        let mut n = 0;
        for g in gens.iter() {
            n += g.handles.len();
        }
        n + self.frozen.borrow().len()
    }

    /// Snapshot all live tracked objects. Used by
    /// `gc.get_objects(generation=...)`.
    pub fn snapshot(&self, generation: Option<usize>) -> Vec<Object> {
        // `gc.get_objects()` enumerates every container CPython tracks,
        // including the all-scalar ones whose tracking we defer.
        self.promote_all_deferred();
        let gens = self.generations.borrow();
        let mut out = Vec::new();
        let mut push = |h: &HandleRc<TrackedHandle>| out.extend(h.object.upgrade());
        match generation {
            Some(g) if g < N_GENERATIONS => gens[g].handles.iter().for_each(&mut push),
            _ => gens
                .iter()
                .for_each(|g| g.handles.iter().for_each(&mut push)),
        }
        if generation.is_none() {
            self.frozen.borrow().iter().for_each(&mut push);
        }
        out
    }

    /// `gc.freeze()` — mark every currently-tracked object as
    /// frozen so it is ignored by future collections.
    pub fn freeze_all(&self) {
        self.flush_young();
        let mut gens = self.generations.borrow_mut();
        let mut frozen = self.frozen.borrow_mut();
        for g in gens.iter_mut() {
            for h in g.handles.drain(..) {
                h.color.store(color::Frozen, Ordering::Release);
                h.slot.store(frozen.len(), Ordering::Release);
                frozen.push(h);
            }
        }
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
    }

    /// `gc.unfreeze()` — move every frozen object back to
    /// generation 0.
    pub fn unfreeze_all(&self) {
        // Lock order: generations before frozen, matching `freeze_all`.
        let mut gens = self.generations.borrow_mut();
        let mut frozen = self.frozen.borrow_mut();
        for h in frozen.drain(..) {
            h.color.store(color::White, Ordering::Release);
            h.generation.store(0, Ordering::Release);
            h.slot.store(gens[0].handles.len(), Ordering::Release);
            gens[0].handles.push(h);
        }
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
    }

    pub fn freeze_count(&self) -> usize {
        self.frozen.borrow().len()
    }

    /// Collect generations `0..=upto`. Returns the number of
    /// objects reclaimed.
    ///
    /// Runs regardless of `gc.isenabled()`: CPython's `gc.disable()` only
    /// suppresses the *automatic*, threshold-driven collections (see
    /// [`Self::maybe_auto_collect`]); an explicit `gc.collect()` always runs a
    /// full sweep. The re-entrancy guard still applies — a collection
    /// triggered from inside a collection is a no-op.
    pub fn collect(&self, upto: usize) -> usize {
        self.collect_impl(upto, true)
    }

    /// Run the cycle collector's mark phase across all generations and fire
    /// the weakref callbacks of every unreachable, non-finalizable object,
    /// *without* the destructive teardown of a real collection. See
    /// [`Self::collect_generation`]'s `weakref_only` discussion.
    pub fn fire_dead_weakrefs(&self) {
        self.sweep_deferred(false);
        if self
            .collecting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.collect_generation(N_GENERATIONS - 1, true);
        self.collecting.store(false, Ordering::Release);
    }

    /// Shared collection body. `exact` selects between the two cost/precision
    /// profiles:
    ///
    /// * `true` — an explicit `gc.collect()`: iterate the mark-sweep to a
    ///   fixpoint, reproducing CPython's "one call reclaims all current
    ///   cyclic garbage" guarantee that `test_gc`'s exact-count assertions
    ///   depend on.
    /// * `false` — a threshold-driven automatic young collection: a single
    ///   pass (leftover garbage waits for the next trigger), keeping the
    ///   per-allocation cost flat.
    fn collect_impl(&self, upto: usize, exact: bool) -> usize {
        // Promote any deferred container that can now anchor a cycle, so
        // the mark phase sees the whole candidate population. Before the
        // re-entrancy claim: promotion calls `track_now`.
        self.sweep_deferred(false);
        // Atomic claim: overlapping collections over the shared heap would
        // subtract the same internal edges twice.
        if self
            .collecting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return 0;
        }
        // Drop this thread's parked C-dropped clones before seeding
        // reachability: each inflates its object's strong count, which the
        // mark phase reads as an external root. Not while an extension frame
        // is live on this thread: a queued clone may back a body pointer C
        // still borrows.
        if !crate::vm_singletons::cext_call_active() {
            drop(crate::vm_singletons::drain_pending_cext_drops());
        }
        let gen = upto.min(N_GENERATIONS - 1);
        // Reachability is seeded from strong counts, so a transient
        // Rust-side reference can make a dead object — and everything
        // reachable only through it — look live for a single pass.
        // Repeating until a pass collects nothing (exact only) reclaims
        // all current cyclic garbage, as CPython's one call does.
        let passes = if exact { MAX_COLLECT_PASSES } else { 1 };
        let mut collected = 0usize;
        for _ in 0..passes {
            let n = self.collect_generation(gen, false);
            collected += n;
            if n == 0 {
                break;
            }
        }
        {
            let mut stats = self.stats.borrow_mut();
            stats[gen].collections = stats[gen].collections.saturating_add(1);
            stats[gen].collected = stats[gen].collected.saturating_add(collected as u64);
        }
        {
            // CPython resets the counters of every collected generation and
            // credits one "tick" to the next older one — that tick is what
            // eventually promotes a gen-1 / gen-2 collection.
            let mut counts = self.counts.borrow_mut();
            for c in counts.iter_mut().take(gen + 1) {
                *c = 0;
            }
            if gen + 1 < N_GENERATIONS {
                counts[gen + 1] = counts[gen + 1].saturating_add(1);
            }
            self.sync_gen0_gauge(counts[0], None);
        }
        // The tracked-id filter only ever gains bits; once most of them
        // name objects long gone, rebuild it from the live index.
        let live = self.index.borrow().len();
        let stale = self.tracked_filter.inserts_since_rebuild();
        if gen == N_GENERATIONS - 1 || stale > 4096.max(live.saturating_mul(4)) {
            self.rebuild_tracked_filter();
        }
        self.collecting.store(false, Ordering::Release);
        // Referents without a death hook (see `weakref_registry`) are
        // noticed here.
        crate::weakref_registry::sweep_dead_targets();
        collected
    }

    /// Rebuild the tracked-id miss filter from the live index. Holding the
    /// index borrow serializes it against every `track`.
    pub fn rebuild_tracked_filter(&self) {
        let index = self.index.borrow();
        self.tracked_filter.rebuild(index.keys().copied());
    }

    /// Collect a specific generation. Used by [`Self::collect`].
    ///
    /// `weakref_only` runs the identical mark phase but stops once the
    /// unreachable set is known: it fires the weakref callbacks of the dead,
    /// non-finalizable objects and returns *without* running finalizers,
    /// clearing fields or rebuilding generations. It is used from a
    /// blocking `Thread.join` to fire a dead `ThreadPoolExecutor`'s
    /// `weakref_cb` without the destructive teardown of a full collection
    /// (RFC 0040: `test_shutdown`).
    fn collect_generation(&self, gen: usize, weakref_only: bool) -> usize {
        // Phase 1: the live objects of this generation and the younger ones,
        // each held strongly for the collection's duration. Entries whose
        // object died are dropped on the way.
        let mut cands = self.snapshot_for_collection(gen);
        let n_real = cands.len();
        if n_real == 0 {
            return 0;
        }
        let mut by_id: GcIdMap = cands.iter().enumerate().map(|(i, c)| (c.id, i)).collect();

        // Phase 2: promote untracked nodes reachable from the candidates to
        // temporary candidates for this pass only. CPython GC-tracks
        // iterators, tuples, frames, tracebacks, cells, bound methods and
        // descriptor wrappers; we keep them off the books for speed and
        // discover the ones a cycle actually routes through here, so their
        // internal edges are accounted. Temporaries take part in the
        // subtract/mark walk but are never reclaimed or tracked.
        let mut scanned = 0usize;
        while scanned < cands.len() {
            let parent_is_iter = matches!(cands[scanned].obj, Object::Iter(_));
            let parent_is_frame = matches!(cands[scanned].obj, Object::Frame(_));
            let mut found: Vec<Object> = Vec::new();
            traverse_object(&cands[scanned].obj, &mut |child| {
                if promotes_temporarily(child, parent_is_iter, parent_is_frame)
                    && !by_id.contains_key(&id_of(child))
                {
                    found.push(child.clone());
                }
            });
            for child in found {
                let cid = id_of(&child);
                if let std::collections::hash_map::Entry::Vacant(e) = by_id.entry(cid) {
                    e.insert(cands.len());
                    cands.push(Cand::new(child, cid, None));
                }
            }
            scanned += 1;
        }

        // Phase 3: seed gc_refs from the outer refcount, after discovery (an
        // iterator synthesises a fresh wrapper for its buffer on each
        // traverse, alive only during the visit). Each candidate holds one
        // reference of its own.
        for c in &cands {
            c.gc_refs.set(strong_count_for(&c.obj) as i64 - 1);
        }
        // Subtract internal references, self-references included.
        for c in &cands {
            traverse_object(&c.obj, &mut |child| {
                if let Some(&i) = by_id.get(&id_of(child)) {
                    let t = &cands[i];
                    t.gc_refs.set(t.gc_refs.get() - 1);
                }
            });
        }

        // Phase 4: anything with gc_refs > 0 is reachable from outside; mark
        // it black and propagate.
        let mut grey: Vec<usize> = Vec::new();
        for (i, c) in cands.iter().enumerate() {
            if c.gc_refs.get() > 0 {
                c.color.set(color::Grey);
                grey.push(i);
            }
        }
        if std::env::var_os("WP_ROOT_DBG").is_some() {
            for &i in &grey {
                let c = &cands[i];
                eprintln!(
                    "[root] id={} gc_refs={} sc={} {}",
                    c.id,
                    c.gc_refs.get(),
                    strong_count_for(&c.obj),
                    c.obj.type_name_owned()
                );
            }
        }
        Self::propagate_black(&cands, &by_id, &mut grey);

        // Phase 5: white real candidates are unreachable cyclic garbage.
        let unreachable: Vec<usize> = (0..n_real).filter(|&i| cands[i].is_white()).collect();

        // CPython's `handle_weakrefs`: a weakref that is *itself* part of the
        // cyclic trash has its callback cleared without invocation — only
        // weakrefs rooted outside the dying subgraph observe the deaths
        // (test_callbacks_on_callback).
        let trash_ids: IdSet = unreachable.iter().map(|&i| cands[i].id).collect();
        let wrapper_is_trash = |slot: &crate::sync::Rc<crate::weakref_registry::WeakRefSlot>| {
            slot.py_ref
                .borrow()
                .as_ref()
                .and_then(crate::sync::Weak::upgrade)
                .is_none_or(|inst| {
                    trash_ids.contains(&(crate::sync::Rc::as_ptr(&inst) as usize as u64))
                })
        };

        if weakref_only {
            let mut weakref_callbacks = Vec::new();
            for &i in &unreachable {
                if cands[i].pending_finalizer() {
                    continue;
                }
                for (slot, cb) in crate::weakref_registry::notify_clear(cands[i].id) {
                    if let Some(cb) = cb {
                        if !wrapper_is_trash(&slot) {
                            weakref_callbacks.push((slot, cb));
                        }
                    }
                }
            }
            queue_weakref_callbacks(weakref_callbacks, |_| false);
            return 0;
        }

        // CPython clears weakrefs to the *entire* unreachable set
        // (`handle_weakrefs`) BEFORE running any finalizer, so a weakref
        // watching an object a finalizer later revives stays cleared
        // (`test_io.test_garbage_collection`).
        let mut weakref_callbacks = Vec::new();
        for &i in &unreachable {
            for (slot, cb) in crate::weakref_registry::notify_clear(cands[i].id) {
                if let Some(cb) = cb {
                    if !wrapper_is_trash(&slot) {
                        weakref_callbacks.push((slot, cb));
                    }
                }
            }
        }

        // Objects whose `__del__` hasn't run yet are queued for
        // finalization and survive this pass: the finalizer (drained after
        // the collection) may resurrect them, and CPython only counts
        // objects it actually reclaims. The next collection, by which point
        // the finalizer has set `finalized`, reclaims the ones it didn't.
        let (deferred, maybe_dead): (Vec<usize>, Vec<usize>) = unreachable
            .iter()
            .partition(|&&i| cands[i].pending_finalizer());
        for &i in &deferred {
            if let Some(e) = &cands[i].entry {
                if !e.finalize_queued.swap(true, Ordering::AcqRel) {
                    run_finalizer(&cands[i].obj);
                }
            }
        }
        // CPython runs `finalize_garbage` before `delete_garbage`, so a
        // pending finalizer always sees its own class, closure cells, and
        // referents intact: protect the deferred objects' whole subgraphs.
        let mut protect: Vec<usize> = Vec::new();
        for &i in &deferred {
            cands[i].color.set(color::Grey);
            protect.push(i);
        }
        Self::propagate_black(&cands, &by_id, &mut protect);

        let dead: Vec<usize> = maybe_dead
            .into_iter()
            .filter(|&i| cands[i].is_white())
            .collect();
        let collected = dead.len();
        // Temporarily promoted iterators and immutable containers that ended
        // up white are cyclic garbage too, freed by refcount once the cycle's
        // mutable anchor is cleared below. CPython counts each (`test_tuple`
        // asserts the closing tuple is counted alongside its list).
        let reported = collected
            + cands[n_real..]
                .iter()
                .filter(|c| {
                    c.is_white()
                        && matches!(
                            c.obj,
                            Object::Iter(_) | Object::Tuple(_) | Object::FrozenSet(_)
                        )
                })
                .count();

        // Break the cycles by clearing the reclaimed objects' fields — or,
        // under `gc.DEBUG_SAVEALL`, park them in `gc.garbage` intact.
        if self.debug.load(Ordering::Acquire) & DEBUG_SAVEALL != 0 {
            let mut garbage = self.garbage.borrow_mut();
            for &i in &dead {
                garbage.push(cands[i].obj.clone());
            }
        } else {
            // Instances whose `__dict__` is shared are deferred; once every
            // other dead object has released its references, a dict held
            // only by dead holders is down to one owner and the retry
            // clears it (a live holder keeps it intact).
            let mut shared_dict_holders: Vec<usize> = Vec::new();
            for &i in &dead {
                if !clear_object_fields(&cands[i].obj) {
                    shared_dict_holders.push(i);
                }
            }
            for i in shared_dict_holders {
                clear_object_fields(&cands[i].obj);
            }
        }

        // Queue the weakref callbacks (after finalisers and cyclic clears,
        // matching CPython's order) for the interpreter's next safe point.
        queue_weakref_callbacks(weakref_callbacks, |slot| {
            slot.py_ref
                .borrow()
                .as_ref()
                .and_then(crate::sync::Weak::upgrade)
                .is_none_or(|inst| {
                    trash_ids.contains(&(crate::sync::Rc::as_ptr(&inst) as usize as u64))
                })
        });

        // Phase 6: rebuild the generation lists. Survivors of generation
        // `g` move to generation min(g+1, N_GENERATIONS-1); the dead leave
        // the index. Dropping `cands` then frees the dead by refcount.
        self.rebuild_generations(gen, &cands[..n_real]);
        self.tracked_count.fetch_sub(
            collected.min(self.tracked_count.load(Ordering::Acquire)),
            Ordering::AcqRel,
        );
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
        drop(cands);
        reported
    }

    /// Blacken everything reachable from the grey candidates in `grey`.
    fn propagate_black(cands: &[Cand], by_id: &GcIdMap, grey: &mut Vec<usize>) {
        while let Some(i) = grey.pop() {
            cands[i].color.set(color::Black);
            traverse_object(&cands[i].obj, &mut |child| {
                if let Some(&j) = by_id.get(&id_of(child)) {
                    if cands[j].is_white() {
                        cands[j].color.set(color::Grey);
                        grey.push(j);
                    }
                }
            });
        }
    }

    /// The live objects of generations `0..=upto` as collection candidates;
    /// entries whose object died are dropped from those generations and the
    /// index.
    fn snapshot_for_collection(&self, upto: usize) -> Vec<Cand> {
        let mut index = self.index.borrow_mut();
        let mut gens = self.generations.borrow_mut();
        let mut out = Vec::new();
        let mut pruned = 0usize;
        for g in gens.iter_mut().take(upto.min(N_GENERATIONS - 1) + 1) {
            g.handles.retain(|h| match h.object.upgrade() {
                Some(obj) => {
                    out.push(Cand::new(obj, h.id, Some(h.clone())));
                    true
                }
                None => {
                    index.remove(&h.id);
                    pruned += 1;
                    false
                }
            });
            for (i, h) in g.handles.iter().enumerate() {
                h.slot.store(i, Ordering::Release);
            }
        }
        if pruned > 0 {
            self.tracked_count.fetch_sub(
                pruned.min(self.tracked_count.load(Ordering::Acquire)),
                Ordering::AcqRel,
            );
        }
        out
    }

    fn rebuild_generations(&self, upto: usize, cands: &[Cand]) {
        // Lock order MUST match `track` (index before generations).
        let mut index = self.index.borrow_mut();
        let mut gens = self.generations.borrow_mut();
        for g in 0..=upto.min(N_GENERATIONS - 1) {
            gens[g].handles.clear();
        }
        for c in cands {
            let Some(h) = &c.entry else { continue };
            if c.is_white() {
                index.remove(&h.id);
                continue;
            }
            let g = h.generation.load(Ordering::Acquire);
            let new_g = g.saturating_add(1).min(MAX_GENERATION);
            h.generation.store(new_g, Ordering::Release);
            let new_g = usize::from(new_g);
            h.slot.store(gens[new_g].handles.len(), Ordering::Release);
            gens[new_g].handles.push(h.clone());
        }
    }
}

/// Id → candidate position, for one collection.
type GcIdMap = std::collections::HashMap<ObjectId, usize, BuildHasherDefault<ObjectIdHasher>>;

/// Whether a collection promotes `child`, an untracked node reached from a
/// candidate, to a temporary candidate (see `collect_generation`'s phase 2).
fn promotes_temporarily(child: &Object, parent_is_iter: bool, parent_is_frame: bool) -> bool {
    match child {
        // Iterators, immutable containers, dict views, slices, closure
        // cells, tracebacks, frames, bound methods and descriptor wrappers
        // are all GC-tracked by CPython, and a cycle can route through any
        // of them (`obj.x = iter(set_containing_obj)`, `l = []; t = (l,);
        // l.append(t)`, a recursive closure's `function -> cell ->
        // function`, an exception's `__traceback__ -> frame -> f_locals`).
        Object::Iter(_)
        | Object::Tuple(_)
        | Object::FrozenSet(_)
        | Object::DictView(_)
        | Object::Slice(_)
        | Object::Cell(_)
        | Object::Traceback(_)
        | Object::Frame(_)
        | Object::BoundMethod(_)
        | Object::StaticMethod(_)
        | Object::ClassMethod(_)
        | Object::Property(_) => true,
        // Lists are tracked at creation, so an *untracked* list is only an
        // iterator's private snapshot buffer.
        Object::List(_) => parent_is_iter,
        // An untracked exception (the `group -> __context__ -> __traceback__
        // -> frame -> f_locals -> group` loop), or an instance whose
        // tracking is still deferred, whose edge to its class can close a
        // cycle (`A.a = A(); del A`).
        Object::Instance(i) => i.cls().flags.is_exception || i.is_gc_deferred(),
        // A frame's `f_locals` cache carries the frame's only edges to its
        // locals.
        Object::Dict(_) => parent_is_frame,
        _ => false,
    }
}

/// Queue the collected weakref callbacks for the interpreter's next safe
/// point, skipping those whose wrapper `is_trash`.
fn queue_weakref_callbacks(
    callbacks: Vec<(
        crate::sync::Rc<crate::weakref_registry::WeakRefSlot>,
        Object,
    )>,
    is_trash: impl Fn(&crate::sync::Rc<crate::weakref_registry::WeakRefSlot>) -> bool,
) {
    for (slot, cb) in callbacks {
        if is_trash(&slot) {
            continue;
        }
        let wr = slot
            .py_ref
            .borrow()
            .as_ref()
            .and_then(crate::sync::Weak::upgrade)
            .map(crate::object::Object::Instance);
        if let Some(wr) = wr {
            crate::vm_singletons::push_pending_weakref_callback(cb, wr);
        }
    }
}

/// Process-global hook counting `Object` clones held *only* by the C-API
/// layer's pin caches (parked argument-pinned identity boxes, dead
/// scalar/tuple pins) — infrastructure references `sys.getrefcount` must
/// discount (RFC 0076 WS1). Registered once by `weavepy-capi` at init,
/// same additive-hook pattern as `register_instance_body_free`; inert
/// (always 0) in a pure-VM build.
static PIN_CLONE_COUNT: std::sync::OnceLock<fn(&Object) -> usize> = std::sync::OnceLock::new();

/// Register the pin-clone counter hook. Idempotent.
pub fn register_pin_clone_count(f: fn(&Object) -> usize) {
    let _ = PIN_CLONE_COUNT.set(f);
}

/// Clones of `obj` held only by C-API pin caches (0 without the hook).
pub fn pin_clone_count(obj: &Object) -> usize {
    match PIN_CLONE_COUNT.get() {
        Some(f) => f(obj),
        None => 0,
    }
}

/// Process-global hook counting **surplus raw C references** to `obj`'s
/// faithful body — `ob_refcnt` beyond the single `Rc` pin the C layer
/// holds while any C reference exists. An extension bumping the body with
/// the inline `Py_INCREF` macro (numpy's `NpyIter_Copy` increfs its
/// operands this way) never creates an `Rc` clone, so without this the
/// reference is invisible to `sys.getrefcount`
/// (test_nditer's test_iter_refcount — RFC 0076 WS1).
static EXTRA_C_REFS: std::sync::OnceLock<fn(&Object) -> usize> = std::sync::OnceLock::new();

/// Register the surplus-C-refs counter hook. Idempotent.
pub fn register_extra_c_refs(f: fn(&Object) -> usize) {
    let _ = EXTRA_C_REFS.set(f);
}

/// Raw C references to `obj` beyond the pin `Rc` (0 without the hook).
pub fn extra_c_refs(obj: &Object) -> usize {
    match EXTRA_C_REFS.get() {
        Some(f) => f(obj),
        None => 0,
    }
}

/// `Rc::strong_count`-like accessor that knows about every
/// container Object variant.
pub fn strong_count_for(obj: &Object) -> usize {
    use crate::sync::Rc;
    match obj {
        Object::List(l) => Rc::strong_count(l),
        Object::Dict(d) => Rc::strong_count(d),
        Object::Set(s) => Rc::strong_count(s),
        Object::FrozenSet(s) => Rc::strong_count(s),
        Object::Tuple(t) => ThinArc::strong_count(t),
        Object::Instance(i) => Rc::strong_count(i),
        Object::Function(f) => Rc::strong_count(f),
        Object::Builtin(b) => Rc::strong_count(b),
        Object::BoundMethod(b) => Rc::strong_count(b),
        Object::Generator(g) => Rc::strong_count(g),
        Object::Coroutine(g) => Rc::strong_count(g),
        Object::AsyncGenerator(g) => Rc::strong_count(g),
        Object::ByteArray(b) => Rc::strong_count(b),
        // Not cycle-capable, but `sys.getrefcount(b"...")` parity matters
        // to ctypes' keepalive tests (test_internals.test_c_char_p).
        Object::Bytes(b) => ThinArc::strong_count(b),
        Object::Iter(i) => Rc::strong_count(i),
        Object::Frame(f) => Rc::strong_count(f),
        Object::Traceback(t) => Rc::strong_count(t),
        Object::MemoryView(m) => Rc::strong_count(m),
        Object::MappingProxy(d) => Rc::strong_count(d),
        Object::MappingProxyObj(o) => Rc::strong_count(o),
        Object::DictView(v) => Rc::strong_count(v),
        Object::SimpleNamespace(d) => Rc::strong_count(d),
        Object::Cell(c) => Rc::strong_count(c),
        Object::Module(m) => Rc::strong_count(m),
        Object::Type(t) => Rc::strong_count(t),
        Object::Code(c) => Rc::strong_count(c),
        // Tracked only when user attributes give it cycle-capable edges.
        Object::File(f) => Rc::strong_count(f),
        // Promoted transiently when a cycle routes through one.
        Object::Slice(s) => Rc::strong_count(s),
        Object::Property(p) => Rc::strong_count(p),
        Object::StaticMethod(m) | Object::ClassMethod(m) => Rc::strong_count(m),
        Object::SlotDescriptor(d) => Rc::strong_count(d),
        Object::LazyIter(l) => Rc::strong_count(l),
        Object::AsyncGenAwait(a) => Rc::strong_count(a),
        // Leaf types — no internal refs to trace.
        _ => 1,
    }
}

/// Walk the immediate children of a container object, calling
/// `visit(child)` for each. Containers without children no-op.
///
/// Uses `try_borrow` throughout: collections can now run from the
/// interpreter's allocation sites, and a container that is mid-borrow
/// at that instant is simply skipped. That is *conservative* under the
/// refcount-seeded reachability model — an unvisited child keeps its
/// external `gc_refs` and therefore survives the pass.
pub fn traverse_object(obj: &Object, visit: &mut dyn FnMut(&Object)) {
    match obj {
        Object::List(l) => {
            let Ok(v) = l.try_borrow() else { return };
            for item in v.iter() {
                visit(item);
            }
        }
        Object::Tuple(t) => {
            for item in t.iter() {
                visit(item);
            }
        }
        Object::Dict(d) | Object::MappingProxy(d) | Object::SimpleNamespace(d) => {
            let Ok(m) = d.try_borrow() else { return };
            for (k, v) in m.iter() {
                visit(&k.0);
                visit(v);
            }
        }
        Object::MappingProxyObj(inner) => visit(inner),
        Object::Set(s) => {
            let Ok(m) = s.try_borrow() else { return };
            for k in m.iter() {
                visit(&k.0);
            }
        }
        Object::FrozenSet(s) => {
            for k in s.iter() {
                visit(&k.0);
            }
        }
        Object::Instance(i) => {
            // CPython's `subtype_traverse` visits `Py_TYPE(self)` for heap
            // types: a user class is itself GC-tracked and an instance holds a
            // strong ref to it, so a class reachable *only* through its
            // instances (e.g. `A.a = A(); del A`) must see that edge subtracted
            // or it never collects. Built-in types are immortal and untracked,
            // so skip them (the `by_id` lookup would miss anyway).
            let cls = i.cls();
            if !cls.flags.is_builtin {
                visit(&Object::Type(cls));
            }
            // A namespace dict that is itself a GC candidate — a
            // `types.ModuleType('foo')` instance's `__dict__`, tracked in
            // tandem with the functions whose `__globals__` it becomes —
            // is one strong edge from the instance, and its own candidacy
            // accounts for the contents. Walking the contents here too
            // would subtract every entry twice; *not* visiting the dict
            // object would leave it looking externally referenced, and a
            // `dict -> instance -> class -> method -> __globals__` cycle
            // in a dead ModuleType namespace would be immortal
            // (test_module.test_clear_dict_in_ref_cycle).
            if i.dict.published().is_none() {
                // Split values: the instance's own children.
                if let Ok(split) = i.dict.split_cell().try_borrow() {
                    for (k, v) in split.iter() {
                        visit(&k.0);
                        visit(v);
                    }
                }
            } else if let Some(dict) = i.dict.get_shared() {
                let dict_obj = Object::Dict(dict);
                if is_tracked(id_of(&dict_obj)) {
                    visit(&dict_obj);
                } else if let Object::Dict(dict) = &dict_obj {
                    if let Ok(m) = dict.try_borrow() {
                        for (k, v) in m.iter() {
                            visit(&k.0);
                            visit(v);
                        }
                    }
                }
            }
            if let Ok(slots) = i.slots.try_borrow() {
                for (k, v) in slots.iter() {
                    visit(&k.0);
                    visit(v);
                }
            }
            // A built-in *container* subclass (`class C(list)`, `D(dict)`,
            // `S(set)`, …) keeps its payload in `native`; that container is
            // an internal, separately-untracked detail of the instance, so
            // its elements are the instance's real children. Walk them so
            // the collector sees cycles routed through subclass storage and
            // prompt reclamation can follow such a chain (a leaf `native`
            // like an `int`/`str` subclass simply has no children).
            if let Some(native) = i.native.get() {
                traverse_object(native, visit);
            }
            // A C extension type (RFC 0044) may hold child references in
            // C-managed memory invisible to the dict walk above; give its
            // registered `tp_traverse` bridge a chance to surface them.
            run_external_traverse(obj, visit);
        }
        Object::Module(m) => {
            // The module holds exactly one strong edge: its namespace
            // dict. `track()` enrolls that dict as its own candidate
            // (whose traversal covers the entries), so visiting the
            // contents here as well would double-subtract them. If the
            // dict was never tracked (pre-dating that pairing), the
            // `by_id` miss makes this visit harmless and its entries
            // simply count as externally referenced — conservative.
            visit(&Object::Dict(m.dict.clone()));
        }
        Object::Cell(c) => {
            let Ok(v) = c.try_borrow() else { return };
            visit(&v);
        }
        Object::File(f) => {
            // User attributes on a stream (`f.f = f`) are its only
            // cycle-capable edges; the fixed fields hold no objects.
            if let Ok(attrs) = f.extra_attrs.try_borrow() {
                for (_, v) in attrs.iter() {
                    visit(v);
                }
            }
        }
        Object::BoundMethod(b) => {
            visit(&b.function);
            visit(&b.receiver);
        }
        Object::MemoryView(m) => {
            // CPython's `memory_traverse` visits `view->obj`: an exporter
            // that (transitively) owns the view closes a cycle
            // (test_picklebuffer.test_cycle routes one through
            // `PickleBuffer._view`).
            if let Ok(exp) = m.exporter.try_borrow() {
                if let Some(exp) = exp.as_ref() {
                    visit(exp);
                }
            }
        }
        Object::Slice(s) => {
            visit(&s.start);
            visit(&s.stop);
            visit(&s.step);
        }
        Object::Property(p) => {
            for member in [&p.fget, &p.fset, &p.fdel] {
                if let Ok(v) = member.try_borrow() {
                    visit(&v);
                }
            }
            if let Ok(doc) = p.doc.try_borrow() {
                visit(&doc);
            }
        }
        Object::StaticMethod(o) | Object::ClassMethod(o) => {
            visit(&o.func());
            if let Ok(d) = o.dict.try_borrow() {
                for (k, v) in d.iter() {
                    visit(&k.0);
                    visit(v);
                }
            }
        }
        Object::DictView(v) => {
            // A dict view holds only the *dict* (a shared `Rc`), so the
            // cycle edge to subtract is `view -> dict`; the dict's own
            // traversal accounts `dict -> entries` (bug #3680, test_dict
            // `test_container_iterator`). Visiting the entries here would
            // subtract edges the view doesn't actually hold.
            visit(&Object::Dict(v.dict.clone()));
        }
        Object::Type(t) => {
            // Class dict + base list. Without this, classes that
            // close over a method that closes over the class
            // (a very common pattern via decorators) leak.
            if let Ok(dict) = t.dict.try_borrow() {
                for (k, v) in dict.iter() {
                    visit(&k.0);
                    visit(v);
                }
            }
            for base in t.bases.borrow().iter() {
                visit(&Object::Type(base.clone()));
            }
            // The MRO holds strong refs — including one to the class
            // itself (every class self-cycles through `mro[0]`). The
            // collector must subtract these internal edges or a class
            // can never collapse to gc_refs == 0.
            if let Ok(mro) = t.mro.try_borrow() {
                for entry in mro.iter() {
                    visit(&Object::Type(entry.clone()));
                }
            }
            if let Ok(meta) = t.metaclass.try_borrow() {
                if let Some(meta) = meta.as_ref() {
                    visit(&Object::Type(meta.clone()));
                }
            }
            // The cached instantiation plan holds strong refs to the
            // resolved `__new__`/`__init__` (usually aliases of the dict
            // entries visited above, but still *extra* edges). Without
            // subtracting them, a class whose `__init__` was ever called
            // keeps that function externally reachable, and a
            // `dict -> instance -> class -> __init__ -> __globals__`
            // exec cycle never collapses
            // (test_module.test_clear_dict_in_ref_cycle).
            if let Ok(plan) = t.instance_plan.try_borrow() {
                if let Some((_, plan)) = plan.as_ref() {
                    for slot in [&plan.user_new, &plan.init_fn] {
                        let Some(f) = slot else { continue };
                        visit(f);
                        // A classmethod-form `__new__` is cached as a
                        // plan-private BoundMethod over the class — that
                        // wrapper is never itself a tracked candidate, so
                        // its edges (function + the class receiver) are
                        // this class's edges.
                        if let Object::BoundMethod(bm) = f {
                            visit(&bm.function);
                            visit(&bm.receiver);
                        }
                    }
                    // The plain-construction fast path's own handle on the
                    // same `__init__` is one more such edge.
                    if let Some((init, _)) = &plan.lean_init {
                        visit(&Object::Function(init.clone()));
                    }
                }
            }
        }
        Object::Function(f) => {
            // CPython `func_traverse` visits globals, defaults, kwdefaults,
            // closure, __dict__ and the slot values (annotations, qualname,
            // …). The `f -> __globals__ -> f` self-cycle that `exec(src, d)`
            // builds (`test_function`) closes through `globals`, so it must
            // be walked. A module-level function's globals is the module
            // namespace dict, which isn't a tracked candidate on its own —
            // the `by_id` lookup simply misses it, so visiting is harmless.
            visit(&Object::Dict(f.globals.clone()));
            // `func_builtins` too. For the main interpreter this edge is
            // moot (its builtins dict is rooted from Rust for the process
            // lifetime), but a destroyed sub-interpreter's builtins dict
            // is held *only* by that interpreter's functions: without the
            // edge, every one of them counted as an external root, the
            // dict stayed Black, and everything reachable from it —
            // `__loader__` (its `_frozen_importlib.BuiltinImporter`), its
            // `open`, its `__import__` — pinned the whole module graph of
            // every closed interpreter for the life of the process.
            visit(&Object::Dict(f.builtins.clone()));
            for d in &f.defaults {
                visit(d);
            }
            for (_, v) in &f.kw_defaults {
                visit(v);
            }
            for cell in &f.closure {
                visit(cell);
            }
            // The lean call paths cache the closure's cells as a frame
            // `cells` vector (`PyFunction::closure_cells`): a second strong
            // handle on every cell, owned by this function. Unvisited, it
            // made each cell of a called closure look externally held, so
            // no cycle through one was ever collected (attrs' slotted
            // `_ClassBuilder` pinned the class it replaced). While a live
            // activation shares the vector, its handles are that frame's,
            // not this function's: leave them counted as external.
            if let Some(cells) = f.closure_cells.get() {
                if crate::sync::Rc::strong_count(cells) == 1 {
                    for cell in cells.iter() {
                        visit(&Object::Cell(cell.clone()));
                    }
                }
            }
            if let Some(attrs_rc) = f.attrs.try_borrow().ok().and_then(|a| a.clone()) {
                if let Ok(attrs) = attrs_rc.try_borrow() {
                    for (k, v) in attrs.iter() {
                        visit(&k.0);
                        visit(v);
                    }
                }
            }
            // (An unplanted seed holds only the shared name strings.)
            if let Ok(slots) = f.slots_raw.try_borrow() {
                for (k, v) in slots.iter() {
                    visit(&k.0);
                    visit(v);
                }
            }
        }
        Object::LazyIter(l) => l.gc_referents(visit),
        Object::Builtin(_)
        | Object::Generator(_)
        | Object::Coroutine(_)
        | Object::AsyncGenerator(_)
        | Object::Iter(_)
        | Object::Frame(_)
        | Object::Traceback(_) => {
            // The fields of these variants are private to the
            // module that defined them; the GC cooperates with
            // them via the external `*_traverse` helper, but
            // we don't crash if no helper is registered. (See
            // the `register_traverse` extension hook below.)
            run_external_traverse(obj, visit);
        }
        _ => {}
    }
}

/// Called from `traverse_object` to give container types whose
/// fields are private to other modules (functions, generators,
/// frames, ...) a chance to participate. The hook table is
/// populated at interpreter init via [`register_traverse`].
///
/// The table holds plain function pointers, so it's `Send +
/// Sync` and lives in a `OnceLock`. Each thread sees the same
/// table — registrations are a global, additive operation.
fn run_external_traverse(obj: &Object, visit: &mut dyn FnMut(&Object)) {
    // The table is append-only and read lock-free (see `HookTable`); a
    // hook may re-enter the collector, and this function, on the same
    // thread.
    for (matches, traverse) in TRAVERSE_TABLE.iter() {
        if matches(obj) {
            traverse(obj, visit);
        }
    }
}

/// A small append-only registry of `(matches, hook)` function-pointer
/// pairs with lock-free reads: hooks are registered at interpreter
/// init and polled on every collection walk, so readers must not take a
/// lock (nor allocate) per object.
const HOOK_TABLE_CAP: usize = 16;

struct HookTable<H: Copy> {
    len: std::sync::atomic::AtomicUsize,
    slots: [std::sync::OnceLock<(fn(&Object) -> bool, H)>; HOOK_TABLE_CAP],
}

impl<H: Copy> HookTable<H> {
    const CAP: usize = HOOK_TABLE_CAP;

    const fn new() -> Self {
        Self {
            len: std::sync::atomic::AtomicUsize::new(0),
            slots: [const { std::sync::OnceLock::new() }; HOOK_TABLE_CAP],
        }
    }

    fn push(&self, matches: fn(&Object) -> bool, hook: H) {
        let i = self.len.fetch_add(1, Ordering::AcqRel);
        assert!(i < Self::CAP, "too many GC hook registrations");
        let _ = self.slots[i].set((matches, hook));
    }

    #[inline]
    fn iter(&self) -> impl Iterator<Item = (fn(&Object) -> bool, H)> + '_ {
        let n = self.len.load(Ordering::Acquire).min(Self::CAP);
        // A slot past a registration in flight is still unset; skip it.
        self.slots[..n].iter().filter_map(|s| s.get().copied())
    }
}

static TRAVERSE_TABLE: HookTable<fn(&Object, &mut dyn FnMut(&Object))> = HookTable::new();

/// Register a traverse callback. Called once per Object variant
/// whose fields are not directly visible to `traverse_object`.
pub fn register_traverse(
    matches: fn(&Object) -> bool,
    traverse: fn(&Object, &mut dyn FnMut(&Object)),
) {
    TRAVERSE_TABLE.push(matches, traverse);
}

static CLEAR_TABLE: HookTable<fn(&Object)> = HookTable::new();

/// Called from `clear_object_fields` to let a type whose child
/// references live in module-private (or C-managed) memory break its
/// cycles during the collector's clear phase. The companion of
/// [`register_traverse`] (RFC 0044, WS4).
fn run_external_clear(obj: &Object) {
    for (matches, clear) in CLEAR_TABLE.iter() {
        if matches(obj) {
            clear(obj);
        }
    }
}

/// Register a clear callback, mirroring [`register_traverse`]. Invoked
/// during the collector's clear phase so a matching object can drop the
/// child references it holds outside the VM's view.
pub fn register_clear(matches: fn(&Object) -> bool, clear: fn(&Object)) {
    CLEAR_TABLE.push(matches, clear);
}

/// Drain a container's child references in place. Used during
/// the GC's clear phase to break cycles.
///
/// Returns `false` when the clear was *deferred*: an instance whose
/// `__dict__` is shared with another holder (`ref = obj.__dict__`,
/// then `obj` dies) keeps that dict intact — CPython's `subtype_clear`
/// only drops the instance's *reference* to the dict, and the dict
/// lives on for whoever else holds it (test_mailbox
/// `test_type_specific_attributes_removed_on_conversion` snapshots
/// `cls(msg).__dict__` of a temporary). The caller may retry once the
/// other dead holders have released their references.
pub fn clear_object_fields(obj: &Object) -> bool {
    // `try_borrow_mut` throughout: clear targets are unreachable, but
    // collections can run from allocation sites and the drop path —
    // a momentarily-borrowed container is left for the next pass
    // rather than panicking the interpreter.
    match obj {
        Object::List(l) => {
            if let Ok(mut v) = l.try_borrow_mut() {
                v.clear();
            }
        }
        Object::Dict(d) | Object::MappingProxy(d) | Object::SimpleNamespace(d) => {
            if let Ok(mut m) = d.try_borrow_mut() {
                m.clear();
            }
        }
        Object::Set(s) => {
            if let Ok(mut m) = s.try_borrow_mut() {
                m.clear();
            }
        }
        Object::Instance(i) => {
            // Drop any C-held child references (RFC 0044) *first*: a readied
            // extension type's `tp_clear` breaks cycles routed through
            // C-managed memory that the dict/slots clears below can't see, and
            // it typically reads its identity (`self._id`, …) back out of the
            // instance dict to find its side-table slot — so it must run while
            // that dict is still intact.
            run_external_clear(obj);
            if let Ok(mut slots) = i.slots.try_borrow_mut() {
                *slots = crate::types::SlotStorage::default();
            }
            if i.dict.published().is_none() {
                let values = i
                    .dict
                    .split_cell()
                    .try_borrow_mut()
                    .map(|mut s| s.take())
                    .unwrap_or_default();
                drop(values);
            }
            if i.dict.strong_count() > 1 {
                // Shared `__dict__`: leave its contents to the other
                // holder (see the doc comment).
                return false;
            }
            if let Some(dict) = i.dict.published() {
                if let Ok(mut m) = dict.try_borrow_mut() {
                    m.clear();
                }
            }
        }
        Object::ByteArray(b) => {
            if let Ok(mut v) = b.try_borrow_mut() {
                v.clear();
            }
        }
        Object::File(f) => {
            // Break `f.attr = f`-style cycles; the subsequent `Rc` drop runs
            // `PyFile::drop`, which closes the fd and queues the unclosed-file
            // `ResourceWarning` (test_io `test_garbage_collection`).
            if let Ok(mut attrs) = f.extra_attrs.try_borrow_mut() {
                attrs.clear();
            }
        }
        Object::Cell(c) => {
            if let Ok(mut v) = c.try_borrow_mut() {
                *v = Object::None;
            }
        }
        Object::MemoryView(m) => {
            // Drop the `view->obj` edge (CPython `memory_clear` releases
            // the buffer). The backing bytes stay valid — only the
            // exporter reference participates in cycles.
            if let Ok(mut exp) = m.exporter.try_borrow_mut() {
                *exp = None;
            }
        }
        Object::Function(f) => {
            // Break the function's outgoing edges (CPython `func_clear`).
            // `globals` is intentionally left alone: it's a shared namespace
            // dict (a module's `__dict__` or the `exec` target), reclaimed as
            // its own candidate if it too is unreachable — clearing it here
            // could wipe a live module.
            if let Some(attrs_rc) = f.attrs.try_borrow().ok().and_then(|a| a.clone()) {
                if let Ok(mut attrs) = attrs_rc.try_borrow_mut() {
                    attrs.clear();
                }
            }
            if let Ok(mut slots) = f.slots_raw.try_borrow_mut() {
                slots.clear();
            }
            if let Ok(mut seed) = f.slot_seed.try_borrow_mut() {
                seed.take();
            }
        }
        Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g) => {
            // Dropping the suspended frame box breaks the cycle
            // (the finalizer — close() — has already run by the
            // time clear is reached; see collect phase 5c).
            if let Ok(mut st) = g.state.try_borrow_mut() {
                *st = crate::object::GeneratorState::Finished;
            }
        }
        Object::Type(t) => {
            // An unreachable class: drop the dict entries and the MRO
            // (which holds the self-`Rc` every class is born with).
            // `bases` is an immutable Vec, but base edges point up to
            // parents that hold children only weakly, so they never
            // form a cycle on their own.
            if let Ok(mut dict) = t.dict.try_borrow_mut() {
                dict.clear();
            }
            if let Ok(mut mro) = t.mro.try_borrow_mut() {
                mro.clear();
            }
            if let Ok(mut meta) = t.metaclass.try_borrow_mut() {
                *meta = None;
            }
        }
        _ => {}
    }
    true
}

/// Look up `__del__` on the object's type and queue the
/// finalizer for invocation. Errors are swallowed and routed
/// through `sys.unraisablehook` upstream (the interpreter loop
/// owns that channel; here we just push the obj onto the
/// pending queue).
fn run_finalizer(obj: &Object) {
    if has_finalizer(obj) {
        crate::vm_singletons::push_pending_finalizer(obj.clone());
    }
}

/// True iff `obj` needs finalization when it becomes garbage:
/// instances whose class defines `__del__`, and generator-family
/// objects that haven't finished (closing them runs `finally`
/// blocks — CPython's `gen_dealloc` behavior).
fn has_finalizer(obj: &Object) -> bool {
    match obj {
        // The class's cached `__del__` verdict (reset whenever `__del__`
        // or the MRO changes), not an MRO walk per tracked instance.
        Object::Instance(inst) => inst.cls().instances_need_finalize(),
        // A generator that has never been started has nothing to clean
        // up: `close()` on it runs no Python. A *coroutine* still counts
        // unstarted — finalizing one that was never awaited emits the
        // RuntimeWarning.
        Object::Generator(g) => !g.is_finished() && !g.is_unstarted(),
        Object::Coroutine(g) | Object::AsyncGenerator(g) => !g.is_finished(),
        _ => false,
    }
}

/// The cycle collector is **process-global**, not per-thread.
///
/// RFC 0025 made the entire VM heap `Arc`-rooted: `Object` is `Send +
/// Sync` and a container allocated on one OS thread can be referenced
/// from another. A per-thread collector therefore cannot work — it
/// would never see (and so never break) a cycle whose links were
/// allocated on different threads, and a background `gc.collect()`
/// thread (CPython's documented pattern, exercised by
/// `test_weakref`/`test_gc`) would only ever sweep its own empty
/// state while the mutator thread's garbage grew without bound.
///
/// A single shared `GcState` matches CPython's one-collector-per-
/// interpreter model. It is safe because every mutation of a tracked
/// object and every collection happens under the GIL (so accesses are
/// serialized), and `GcState`'s interior `GilCell`s make each borrow
/// memory-safe even if that invariant is ever violated. The state is
/// never dropped (statics have no drop glue); process teardown
/// finalizes survivors via [`GcState::finalization_candidates`].
static GC_STATE: std::sync::LazyLock<GcState> = std::sync::LazyLock::new(GcState::new);

/// The process-wide state's miss filter (see [`GcState::tracked_filter`]),
/// for the drop-path probes that must not take the state's borrows.
#[inline]
fn process_tracked_filter() -> &'static crate::hot_filter::RebuildableBloom {
    &GC_STATE.tracked_filter
}

/// Run a closure with the shared, process-global GC state.
pub fn with_state<R>(f: impl FnOnce(&GcState) -> R) -> R {
    f(&GC_STATE)
}

/// Convenience: track `obj` in the shared, process-global GC.
pub fn track(obj: &Object) {
    if let Object::Instance(inst) = obj {
        // The instance may have been born with deferred tracking; its
        // `__dict__` no longer needs to guard it.
        inst.clear_deferred_tracking();
    }
    // A module's namespace dict outlives the module object whenever
    // functions defined in it survive (their `__globals__`), so it must
    // be a collection candidate in its own right — a
    // `dict -> instance -> class -> method -> __globals__` cycle in a
    // dead module's namespace is otherwise immortal
    // (test_module.test_clear_dict_in_ref_cycle). The module's own
    // traversal visits the dict *object* (its single strong edge), and
    // the dict candidate accounts for the contents.
    // Likewise, a function's `__globals__` dict is the closing edge of
    // every `namespace -> object -> function -> __globals__` cycle. A
    // `types.ModuleType('foo')` namespace (an instance-internal dict
    // that never went through BuildMap) would otherwise never be a
    // candidate. CPython tracks every dict; we pair the tracking with
    // the objects that make the dict cycle-capable.
    // (A young function's dict is tracked when the function is flushed.)
    if let Object::Module(m) = obj {
        let dict = Object::Dict(m.dict.clone());
        with_state(|s| s.track(&dict));
    } else if let Object::Function(f) = obj {
        return with_state(|s| s.track_function(obj, f));
    }
    with_state(|s| s.track(obj));
}

/// Track a list or dict that native code filled before publishing it (the
/// unpickler's results) as if it had been tracked at birth, while still
/// empty: it joins the deferred containers. Every collection promotes the
/// deferred containers that can anchor a cycle before its mark phase, so
/// the collector still sees every cycle.
pub fn track_built(obj: &Object) {
    if !with_state(|s| s.defer_container(obj)) {
        track(obj);
    }
}

/// [`track`] for a fresh list, dict or set its builder knows holds only
/// atomic values (a `list(range(..))`): such a container is deferred
/// whatever it holds, so the element scan [`container_can_cycle`] would
/// make first is skipped.
pub fn track_inert(obj: &Object) {
    debug_assert!(!container_can_cycle(obj));
    with_state(|s| {
        if !s.defer_container(obj) {
            s.track_now(obj);
        }
    });
}

/// `counter += n` (wrapping) for a collector counter: a plain load and
/// store while the GIL serializes every writer, a locked read-modify-write
/// only in free-threaded mode.
#[inline(always)]
fn serial_add(counter: &AtomicUsize, n: usize) {
    // (The debug unit-test binary runs interpreters on concurrent threads
    // with no GIL between them: it keeps the locked form too.)
    if cfg!(debug_assertions) || crate::gil::free_threading_enabled() {
        counter.fetch_add(n, Ordering::AcqRel);
    } else {
        let v = counter.load(Ordering::Relaxed);
        counter.store(v.wrapping_add(n), Ordering::Release);
    }
}

/// Deferred instance tracking.
///
/// CPython tracks every instance of a Python-defined class at
/// allocation. A tracked object here costs an index entry and a weak
/// handle, which a temporary whose dict only ever holds ints doesn't
/// need. An instance that holds only atomic values ([`Object::is_gc_atomic`])
/// cannot take part in a cycle, so a fresh plain instance starts
/// *untracked* and its `__dict__` records the instance address as its
/// deferred owner. The first store that could put a non-atomic value
/// into the instance — any generic dict mutation (`DictData`'s
/// `DerefMut`), a slot store of a non-atomic value, a `__dict__`
/// replacement — tracks it through this function; so do `gc.is_tracked`
/// and weakref creation. An instance whose owner record is still set when
/// it dies is freed by its last `Arc` drop.
///
/// `owner` is the address recorded by [`crate::object::DictData`]; it is
/// 0 when the record was already taken. The instance is alive: it clears
/// the record from its `Drop` before its memory is released.
pub fn track_deferred_owner(owner: usize) {
    if owner == 0 {
        return;
    }
    let ptr = owner as *const crate::types::PyInstance;
    // SAFETY: `owner` was recorded from `Rc::as_ptr` of a live instance
    // by `PyInstance::new_deferred`, and every path that could release
    // the instance clears the record first (`PyInstance::drop`), so the
    // allocation is live and the strong count can be bumped.
    let inst = unsafe {
        crate::Rc::increment_strong_count(ptr);
        crate::Rc::from_raw(ptr)
    };
    // The dict's copy of the record was taken by the caller; clear the
    // instance's so `is_gc_deferred` stops claiming the collector has
    // never seen it.
    inst.deferred.set(false);
    track(&Object::Instance(inst));
}

/// True while a collection (mark/sweep or weakref-only pass) is in
/// flight on the process-global collector. Used by the capi boundary to
/// suppress side-effectful bookkeeping (e.g. the C-drop reap queue) for
/// objects the collector itself marshals through transient C boxes.
pub fn collector_active() -> bool {
    with_state(|s| s.collecting.load(Ordering::Acquire))
}

/// True for the whole span of a `gc.collect()` *orchestration* — the
/// mark/sweep passes plus the interpreter-side drains of the `__del__`
/// finalizers those passes queued. CPython keeps `gcstate->collecting`
/// set while `finalize_garbage` invokes finalizers, and faulthandler's
/// bpo-44466 "Garbage-collecting" marker keys on exactly that; WeavePy's
/// collector can't call Python, so the finalizer phase happens outside
/// [`collector_active`]'s window and is tracked separately here.
static COLLECT_FINALIZER_PHASE: AtomicBool = AtomicBool::new(false);

pub fn set_collect_finalizer_phase(on: bool) {
    COLLECT_FINALIZER_PHASE.store(on, Ordering::Release);
}

/// The faulthandler dump's view: is a garbage collection in progress on
/// this process right now? Lock-free (plain atomic loads), so it is safe
/// to call from the fatal-signal handler.
pub fn collection_in_progress() -> bool {
    COLLECT_FINALIZER_PHASE.load(Ordering::Acquire) || collector_active()
}

/// Convenience: stop tracking `obj` (by identity) in the shared,
/// process-global GC. The inverse of [`track`]; backs the C-API
/// `PyObject_GC_UnTrack` (RFC 0044, WS4).
pub fn untrack(obj: &Object) {
    let id = crate::weakref_registry::id_of(obj);
    with_state(|s| s.untrack_id(id));
}

/// [`untrack`] by identity.
pub fn untrack_id(id: ObjectId) {
    with_state(|s| s.untrack_id(id));
}

/// Reinitialise the process-global cycle collector's locks in a `fork(2)`
/// child. See [`GcState::reinit_after_fork_in_child`].
///
/// # Safety
///
/// Must run only on the lone surviving thread of a fork child.
pub unsafe fn reinit_after_fork_in_child() {
    // Launder the static's address into a raw `*mut` (forcing init via the
    // deref) so the field rebuilds don't go through a `&T -> *mut T` cast.
    let state: &GcState = &GC_STATE;
    unsafe {
        GcState::reinit_after_fork_in_child(std::ptr::from_ref(state).cast_mut());
    }
}

/// A value that can never (transitively) hold a reference back to a
/// container, and therefore can never be part of a reference cycle.
/// Mutable byte/scalar leaves qualify; everything else is treated as
/// potentially-cyclic so the collector errs toward tracking.
pub fn is_atomic(obj: &Object) -> bool {
    matches!(
        obj,
        Object::None
            | Object::Unbound
            | Object::Bool(_)
            | Object::Int(_)
            | Object::Long(_)
            | Object::Float(_)
            | Object::Complex(_)
            | Object::Str(_)
            | Object::Bytes(_)
            | Object::ByteArray(_)
            | Object::Range(_)
    )
}

/// True if the freshly-built container `obj` holds at least one
/// non-atomic element and could therefore participate in a reference
/// cycle. A `list`/`dict`/`set` of only scalar leaves (ints, strs,
/// floats, …) can never close a cycle, so the collector skips it —
/// this is CPython's container-untracking optimization applied at
/// construction time, and it keeps numeric/string-heavy workloads off
/// the GC's books entirely.
/// The smallest deferral population worth compacting: below this, the
/// sweep's fixed cost outweighs what it reclaims.
const DEFERRED_FLOOR: usize = 4096;

/// How many young instances wait before the set is handed to the
/// collector regardless (collections normally flush it well before).
const YOUNG_CAP: usize = 8192;

/// How many containers may stay deferred at once. Past this the sweep
/// hands the whole set to the collector, which both bounds the weak
/// references parked here and restores the allocation pacing a program
/// that *accumulates* scalar containers would otherwise lose:
/// `test_gc.test_bug1055820c` grows a list of empty lists and requires a
/// collection to trigger within 10000 appends.
const DEFERRED_CAP: usize = 4096;

/// A weak reference to a container whose tracking is deferred (see
/// [`GcState::deferred`]). One variant per container kind rather than a
/// weak `Object`, because `Object`'s payload `Arc` is what has to stay
/// weak — holding the `Object` itself would pin the container alive.
enum DeferredContainer {
    List(crate::sync::Weak<crate::RefCell<Vec<Object>>>),
    Dict(crate::sync::Weak<crate::RefCell<crate::object::DictData>>),
    Set(crate::sync::Weak<crate::RefCell<crate::object::SetData>>),
}

impl DeferredContainer {
    /// A weak handle on `obj`, or `None` for a kind that is never deferred.
    fn new(obj: &Object) -> Option<Self> {
        match obj {
            Object::List(l) => Some(Self::List(crate::Rc::downgrade(l))),
            Object::Dict(d) => Some(Self::Dict(crate::Rc::downgrade(d))),
            Object::Set(s) => Some(Self::Set(crate::Rc::downgrade(s))),
            _ => None,
        }
    }

    /// Whether the container has died.
    #[inline]
    fn is_dead(&self) -> bool {
        match self {
            Self::List(w) => w.strong_count() == 0,
            Self::Dict(w) => w.strong_count() == 0,
            Self::Set(w) => w.strong_count() == 0,
        }
    }

    /// The container, if it is still alive.
    fn upgrade(&self) -> Option<Object> {
        match self {
            Self::List(w) => w.upgrade().map(Object::List),
            Self::Dict(w) => w.upgrade().map(Object::Dict),
            Self::Set(w) => w.upgrade().map(Object::Set),
        }
    }
}

/// An element that cannot route a cycle back out of the container
/// holding it: an atomic value, or an instance whose own tracking is
/// still deferred — which by that deferral's invariant holds nothing but
/// atomic values itself, so `container -> instance -> scalars` is as far
/// as the chain goes. The first non-atomic store into such an instance
/// tracks it, and the sweep then re-reads its holders and promotes them.
#[inline]
fn element_is_inert(obj: &Object) -> bool {
    match obj {
        Object::Instance(i) => i.is_gc_deferred(),
        other => is_atomic(other),
    }
}

fn container_can_cycle(obj: &Object) -> bool {
    // A container past this size is registered without inspection: the
    // scan is per *element* while the registration it saves is per
    // *container*, so beyond a point it stops paying for itself — and an
    // unbounded scan would make `track` O(len) for `list(range(1e6))`.
    const SCAN_CAP: usize = 32;
    match obj {
        Object::List(l) => l
            .try_borrow()
            .map(|v| v.len() > SCAN_CAP || v.iter().any(|x| !element_is_inert(x)))
            .unwrap_or(true),
        Object::Set(s) => s
            .try_borrow()
            .map(|m| m.len() > SCAN_CAP || m.iter().any(|k| !element_is_inert(&k.0)))
            .unwrap_or(true),
        Object::Dict(d) => d
            .try_borrow()
            .map(|m| {
                m.len() > SCAN_CAP
                    || m.iter()
                        .any(|(k, v)| !element_is_inert(&k.0) || !element_is_inert(v))
            })
            .unwrap_or(true),
        // A tuple can only anchor a cycle through a non-atomic element. An
        // empty or all-scalar tuple (the interned `()`, `(1, 2)`, …) can never
        // close one, so it stays off the GC's books.
        Object::Tuple(t) => t.len() > SCAN_CAP || t.iter().any(|x| !element_is_inert(x)),
        // Any other container kind: be conservative and track.
        _ => true,
    }
}

/// Track a freshly-created mutable container (`list`/`dict`/`set`) with
/// the cycle collector, but only when it can actually participate in a
/// cycle (see [`container_can_cycle`]). Returns `true` when the object
/// was added to the tracked set, so the caller can decide whether to
/// run a threshold-driven young collection at the allocation site.
/// Track a memoryview that just recorded a buffer exporter. Only a
/// mutable-container exporter (an instance, list, …) can route a cycle
/// back through the view, so scalar exporters (`bytes`) stay untracked.
pub fn track_memoryview_exporter(mv: &Object, exporter: &Object) {
    debug_assert!(matches!(mv, Object::MemoryView(_)));
    if !is_atomic(exporter) {
        track(mv);
    }
}

pub fn track_if_cyclic(obj: &Object) -> bool {
    if container_can_cycle(obj) {
        track(obj);
        true
    } else {
        false
    }
}

/// Whether `id` may be tracked: the miss-filter probe (no false
/// negatives — every tracked object's bits are set at track time; a bit
/// left by an untracked object is a harmless false positive).
#[inline]
pub fn maybe_tracked(id: ObjectId) -> bool {
    process_tracked_filter().may_contain(id)
}

/// See [`GcState::auto_collect_due`].
#[inline]
pub fn auto_collect_due() -> bool {
    with_state(GcState::auto_collect_due)
}

/// Convenience: threshold-driven automatic collection on the shared GC
/// (see [`GcState::maybe_auto_collect`]). Returns whether a collection
/// ran; the caller should then drain pending finalizers.
pub fn maybe_auto_collect() -> bool {
    with_state(GcState::maybe_auto_collect)
}

/// Convenience: find a tracked handle by object id (O(1) via the
/// id index, which covers all generations plus the frozen set).
pub fn find_handle(id: ObjectId) -> Option<HandleRc<TrackedHandle>> {
    with_state(|s| s.handle_for(id))
}

/// Convenience: is `id` currently tracked by the cycle GC?
pub fn is_tracked(id: ObjectId) -> bool {
    with_state(|s| s.is_tracked(id))
}

/// Convenience: claim `id`'s finalizer (so a later collection
/// won't double-run `__del__`). Returns false if it was already
/// claimed or the object isn't tracked.
pub fn mark_finalized(id: ObjectId) -> bool {
    with_state(|s| s.note_finalized(id));
    match find_handle(id) {
        Some(h) => !h.finalized.swap(true, Ordering::AcqRel),
        None => false,
    }
}

/// Convenience: has `id`'s finalizer already run on the current thread?
/// Backs `gc.is_finalized`.
pub fn was_finalized(id: ObjectId) -> bool {
    with_state(|s| s.was_finalized(id))
}

/// Convenience: mark `id`'s finalizer as finished on the current thread's GC
/// (see [`GcState::complete_finalizer`]).
pub fn complete_finalizer(id: ObjectId) {
    with_state(|s| s.complete_finalizer(id));
}

/// Convenience: the live tracked objects with an unrun `__del__` (see
/// [`GcState::finalization_candidates`]).
pub fn finalization_candidates() -> Vec<(HandleRc<TrackedHandle>, Object)> {
    with_state(|s| s.finalization_candidates())
}

/// Rebuild the tracked-id miss filter from the live tracked set (see
/// [`GcState::rebuild_tracked_filter`]).
pub fn rebuild_tracked_filter() {
    with_state(GcState::rebuild_tracked_filter);
}

thread_local! {
    /// Set when work for the interpreter's next safe point was queued on
    /// this thread: a finalizer resurrected by a dying object's `Drop`, a
    /// weakref callback, an unclosed resource's warning. The dispatch
    /// loops poll it between instructions, so the work runs before the
    /// instruction after the one that dropped the object — CPython runs
    /// it inside that instruction's `Py_DECREF`.
    static MAYBE_DEAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A pointer to the calling thread's pending-work flag, for a dispatch
/// loop that polls it after every instruction: one thread-local lookup
/// per loop entry instead of one per poll. Valid for the life of the
/// calling thread; only ever dereferenced on that thread.
#[inline]
pub(crate) fn maybe_dead_flag() -> *const std::cell::Cell<bool> {
    MAYBE_DEAD.with(std::ptr::from_ref)
}

/// Note that the current thread queued work for its next safe point.
/// Teardown-safe: a thread whose locals are gone has no next safe point.
#[inline]
pub fn mark_maybe_dead() {
    let _ = MAYBE_DEAD.try_with(|c| c.set(true));
}

/// Consume the pending-work flag, returning whether it was set.
#[inline]
pub fn take_maybe_dead() -> bool {
    MAYBE_DEAD.with(|c| c.replace(false))
}

/// Convenience: run a full collection on the shared GC. Returns the
/// number of objects collected.
pub fn collect_all() -> usize {
    with_state(|s| s.collect(N_GENERATIONS - 1))
}

/// Convenience: run a partial collection of generations
/// `0..=upto`.
pub fn collect_upto(upto: usize) -> usize {
    with_state(|s| s.collect(upto))
}

/// Convenience: fire dead objects' weakref callbacks via a non-destructive
/// mark pass on the shared GC (see [`GcState::fire_dead_weakrefs`]). Used
/// from a blocking `Thread.join` to unblock idle `ThreadPoolExecutor`
/// workers without the teardown risk of a full collection.
pub fn fire_dead_weakrefs() {
    with_state(|s| s.fire_dead_weakrefs());
    crate::weakref_registry::sweep_dead_targets();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Rc;
    use crate::sync::RefCell;

    use crate::object::DictData;

    #[test]
    fn compact_positions_keep_absent_and_uncached_states_distinct() {
        for value in [0, 1, (u32::MAX - 2) as usize, usize::MAX] {
            let hint = CachedSlot::new(value);
            assert_eq!(hint.load(Ordering::Acquire), value);
            assert_eq!(hint.swap(3, Ordering::AcqRel), value);
            assert_eq!(hint.load(Ordering::Acquire), 3);
            hint.store(value, Ordering::Release);
            assert_eq!(hint.load(Ordering::Acquire), value);
        }
        for value in [(u32::MAX - 1) as usize, usize::MAX - 1] {
            let hint = CachedSlot::new(value);
            assert_eq!(hint.load(Ordering::Acquire), usize::MAX - 1);
            assert_eq!(hint.swap(usize::MAX, Ordering::AcqRel), usize::MAX - 1);
            assert_eq!(hint.load(Ordering::Acquire), usize::MAX);
        }
        #[cfg(target_pointer_width = "64")]
        {
            let hint = CachedSlot::new(u32::MAX as usize + 100);
            assert_eq!(hint.load(Ordering::Acquire), usize::MAX - 1);
            assert_eq!(std::mem::size_of::<TrackedHandle>(), 40);
        }
    }

    #[test]
    fn uncacheable_generation_and_frozen_positions_use_identity_fallback() {
        for frozen in [false, true] {
            let state = GcState::new();
            let roots: Vec<_> = (0..3)
                .map(|_| Object::Dict(Rc::new(RefCell::new(DictData::default()))))
                .collect();
            for root in &roots {
                state.track_now(root);
            }
            if frozen {
                state.freeze_all();
            }
            let first = state.handle_for(id_of(&roots[0])).unwrap();
            first.slot.store((u32::MAX - 1) as usize, Ordering::Release);
            state.untrack_id(first.id);
            assert!(!state.is_tracked(first.id));
            let moved = state.handle_for(id_of(&roots[2])).unwrap();
            assert_eq!(moved.slot.load(Ordering::Acquire), 0);
            let second = state.handle_for(id_of(&roots[1])).unwrap();
            // A cacheable but stale position must also validate identity.
            second.slot.store(0, Ordering::Release);
            state.untrack_id(second.id);
            assert!(!state.is_tracked(second.id));
            assert!(state.is_tracked(moved.id));
            assert_eq!(state.freeze_count(), usize::from(frozen));
            state.untrack_id(moved.id);
            assert!(!state.is_tracked(moved.id));
            assert_eq!(state.freeze_count(), 0);
        }
    }

    #[test]
    fn gc_index_distributes_aligned_object_addresses() {
        use std::hash::BuildHasher;
        let hasher = BuildHasherDefault::<ObjectIdHasher>::default();
        // Exercise regular allocation strides at multiple address regions.
        // The unmodified word hash uses at most 512 of these 8192 buckets.
        for region in [0u64, 0x1_0000_0000, 0x6000_0000_0000] {
            for stride in [16u64, 32, 96, 192, 4096] {
                let mut buckets = [false; 8192];
                for index in 0..4096 {
                    let id = region + index * stride;
                    buckets[(hasher.hash_one(id) & 8191) as usize] = true;
                }
                assert!(buckets.into_iter().filter(|used| *used).count() > 2048);
            }
        }
    }

    #[test]
    fn live_roots_promote_freeze_and_untrack() {
        let state = GcState::new();
        let roots: Vec<Object> = (0..4)
            .map(|_| Object::Dict(Rc::new(RefCell::new(DictData::default()))))
            .collect();
        for root in &roots {
            // See `track_and_untrack` on `track_now`.
            state.track_now(root);
        }
        for (collection, expected) in [(0, 1), (1, 2), (2, 2), (2, 2)] {
            assert_eq!(state.collect(collection), 0);
            for (slot, root) in roots.iter().enumerate() {
                let handle = state.handle_for(id_of(root)).unwrap();
                assert_eq!(handle.generation.load(Ordering::Acquire), expected);
                assert_eq!(handle.color.load(Ordering::Acquire), color::White);
                assert_eq!(handle.slot.load(Ordering::Acquire), slot);
            }
        }
        state.freeze_all();
        assert_eq!(state.freeze_count(), roots.len());
        assert_eq!(state.collect(2), 0);
        for root in &roots {
            let handle = state.handle_for(id_of(root)).unwrap();
            assert_eq!(handle.color.load(Ordering::Acquire), color::Frozen);
        }
        state.untrack_id(id_of(&roots[1]));
        assert!(!state.is_tracked(id_of(&roots[1])));
        assert_eq!(state.freeze_count(), 3);
        let moved = state.handle_for(id_of(&roots[3])).unwrap();
        assert_eq!(moved.slot.load(Ordering::Acquire), 1);
        state.unfreeze_all();
        assert_eq!(state.freeze_count(), 0);
        for (slot, index) in [0, 3, 2].into_iter().enumerate() {
            let handle = state.handle_for(id_of(&roots[index])).unwrap();
            assert_eq!(handle.generation.load(Ordering::Acquire), 0);
            assert_eq!(handle.color.load(Ordering::Acquire), color::White);
            assert_eq!(handle.slot.load(Ordering::Acquire), slot);
        }
        state.untrack_id(id_of(&roots[3]));
        assert!(!state.is_tracked(id_of(&roots[3])));
        let moved = state.handle_for(id_of(&roots[2])).unwrap();
        assert_eq!(moved.slot.load(Ordering::Acquire), 1);
    }

    #[test]
    fn generation_constructor_rejects_out_of_range_indices() {
        for generation in [N_GENERATIONS, u8::MAX as usize + 1, usize::MAX] {
            let result = std::panic::catch_unwind(|| TrackedHandle::new(&Object::None, generation));
            assert!(result.is_err());
        }
    }

    #[test]
    fn track_and_untrack() {
        let s = GcState::new();
        let d = Object::Dict(Rc::new(RefCell::new(DictData::default())));
        // `track_now`, not `track`: an empty dict holds nothing that could
        // close a cycle, so `track` would defer it (see `defer_container`).
        // This test is about the index's bookkeeping, not that policy.
        s.track_now(&d);
        assert!(s.is_tracked(id_of(&d)));
        s.untrack_id(id_of(&d));
        assert!(!s.is_tracked(id_of(&d)));
    }

    #[test]
    fn collect_clears_simple_cycle() {
        let s = GcState::new();
        let dict = Rc::new(RefCell::new(DictData::default()));
        let outer = Object::Dict(dict.clone());
        s.track_now(&outer);
        // The dict references itself: a 1-cycle.
        dict.borrow_mut().insert(
            crate::object::DictKey(Object::from_static("self")),
            outer.clone(),
        );
        let weak = Rc::downgrade(&dict);
        drop(outer);
        drop(dict);
        // Only the cycle keeps the dict alive; the collector holds nothing.
        assert!(weak.upgrade().is_some());
        assert_eq!(s.collect(2), 1);
        assert!(weak.upgrade().is_none());
        assert_eq!(s.population(), 0);
    }

    #[test]
    fn tracked_objects_die_by_refcount() {
        let s = GcState::new();
        let dict = Rc::new(RefCell::new(DictData::default()));
        let obj = Object::Dict(dict.clone());
        s.track_now(&obj);
        let weak = Rc::downgrade(&dict);
        drop(obj);
        drop(dict);
        // No collection ran: the entry is stale but owns nothing.
        assert!(weak.upgrade().is_none());
        assert_eq!(s.population(), 1);
        assert_eq!(s.collect(0), 0);
        assert_eq!(s.population(), 0);
    }

    #[test]
    fn freeze_unfreeze_round_trip() {
        let s = GcState::new();
        let d = Object::Dict(Rc::new(RefCell::new(DictData::default())));
        // See `track_and_untrack` on `track_now`.
        s.track_now(&d);
        s.freeze_all();
        assert_eq!(s.freeze_count(), 1);
        s.unfreeze_all();
        assert_eq!(s.freeze_count(), 0);
    }
}
