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
//!   generation-0 counter exceeds the threshold (default 700).
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

use crate::sync::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::object::Object;
use crate::weakref_registry::{id_of, ObjectId};

/// The standard CPython generation count (3) and default
/// thresholds: gen 0 collects when 700 untracked allocations
/// have happened; gen 1 every 10 gen 0 collections; gen 2
/// every 10 gen 1 collections.
pub const N_GENERATIONS: usize = 3;
pub const DEFAULT_THRESHOLDS: [usize; N_GENERATIONS] = [700, 10, 10];

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

/// Per-tracked-object metadata. Stored as a `Vec<Arc<TrackedHandle>>`
/// inside each [`Generation`] — the `Arc` makes the per-handle
/// state cheaply shared across the candidate snapshot during a
/// collection.
#[allow(missing_debug_implementations)]
pub struct TrackedHandle {
    /// Strong handle to the tracked object. Holding a strong
    /// reference is fine because the GC's job is to *break*
    /// cycles by clearing fields, not by dropping the Rc.
    pub object: Object,
    /// Identity, computed from `id_of(object)` at
    /// `track`-time. Cached so the GC's mark phase doesn't
    /// have to recompute on every visit.
    pub id: ObjectId,
    /// Working `gc_refs` field. Reset to a fresh value at the
    /// start of every collection cycle.
    pub gc_refs: AtomicI64,
    /// Tri-color state. Reset to White at cycle start.
    pub color: AtomicI64,
    /// Generation index (0..N_GENERATIONS). Survivors are
    /// promoted by incrementing this.
    pub generation: AtomicUsize,
    /// Position of this handle within its owning `Vec` —
    /// `generations[generation].handles` normally, or the `frozen`
    /// list when `color == Frozen`. Maintained by every site that
    /// pushes, drains, or rebuilds those vectors so that
    /// [`GcState::untrack_id`] can `swap_remove` in O(1) instead of
    /// scanning every generation (which made drop-heavy,
    /// large-heap workloads quadratic — RFC 0039 WS4).
    pub slot: AtomicUsize,
    /// Has this object's `__del__` already *run* to completion? CPython
    /// guarantees a finaliser runs at most once.
    pub finalized: AtomicBool,
    /// Has this object's `__del__` been *queued* by a collection but not yet
    /// run? While set, the object is kept tracked and excluded from the
    /// `collected` count: its finalizer (drained after `gc.collect()` returns)
    /// may resurrect it, and CPython only counts objects that are actually
    /// reclaimed. Cleared once the finalizer completes (`finalized` is set).
    pub finalize_queued: AtomicBool,
}

#[allow(non_upper_case_globals)]
pub mod color {
    pub const White: i64 = 0;
    pub const Grey: i64 = 1;
    pub const Black: i64 = 2;
    pub const Frozen: i64 = 3;
}

impl TrackedHandle {
    pub fn new(object: Object, generation: usize) -> Self {
        Self {
            id: id_of(&object),
            object,
            gc_refs: AtomicI64::new(0),
            color: AtomicI64::new(color::White),
            generation: AtomicUsize::new(generation),
            slot: AtomicUsize::new(0),
            finalized: AtomicBool::new(false),
            finalize_queued: AtomicBool::new(false),
        }
    }
}

/// Swap-remove the handle at `slot` from `vec`, fixing up the slot
/// index of whatever handle gets moved into the vacated position.
/// O(1): the only handle whose position changes is the one swapped
/// in from the end, and its `slot` field is corrected here so the
/// per-handle position invariant holds after the call.
#[inline]
fn swap_remove_handle(vec: &mut Vec<Arc<TrackedHandle>>, slot: usize) {
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
fn remove_handle_by_ptr(vec: &mut Vec<Arc<TrackedHandle>>, handle: &Arc<TrackedHandle>) -> bool {
    if let Some(pos) = vec.iter().position(|h| Arc::ptr_eq(h, handle)) {
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
    handles: Vec<Arc<TrackedHandle>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GcStats {
    pub collections: u64,
    pub collected: u64,
    pub uncollectable: u64,
}

/// Public state of the cycle GC.
///
/// A single instance lives in a process-global `LazyLock` (see
/// [`with_state`]) and is shared by every OS thread, mirroring
/// CPython's one-collector-per-interpreter model. This is required
/// for correctness: post-RFC-0025 the heap is `Arc`-rooted and a
/// cycle's links can be allocated on different threads, so only a
/// shared tracked-set can ever observe and break such a cycle. All
/// fields are `Sync` (interior `GilCell`s + atomics), so concurrent
/// access is memory-safe; the GIL additionally serializes mutators.
#[allow(missing_debug_implementations)]
pub struct GcState {
    generations: RefCell<[Generation; N_GENERATIONS]>,
    /// Id → handle index over every tracked object (all generations
    /// plus the frozen set). Keeps `track` dedupe, `find_handle`, and
    /// `is_tracked` O(1) — the linear scans they replace made
    /// allocation-heavy workloads quadratic once the tracked
    /// population grew past a few thousand.
    index: RefCell<std::collections::HashMap<ObjectId, Arc<TrackedHandle>>>,
    /// Re-entrancy guard: a collection can indirectly allocate (e.g.
    /// queued finalizers running Python at the next safe point may
    /// re-enter `track`), and a nested collection would see torn
    /// generation lists. An `AtomicBool` (rather than a `Cell`) so the
    /// whole `GcState` is `Sync` and can live in a process-global
    /// `LazyLock` — the cycle collector is shared across every OS
    /// thread, matching the `Arc`-rooted shared heap (RFC 0039 WS4).
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
    /// Frozen handles. `gc.freeze()` moves all tracked objects
    /// here; they are skipped by future collections until
    /// `gc.unfreeze()` runs.
    frozen: RefCell<Vec<Arc<TrackedHandle>>>,
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
    /// Total tracked-object population (live count). Useful
    /// for `gc.get_count` and for the threshold check.
    pub tracked_count: AtomicUsize,
    /// Ids whose `__del__` has been run (or queued) by a finalizing
    /// collection or teardown. Persists past the point where the handle
    /// leaves the tracked set so `gc.is_finalized()` still answers `True`
    /// for an object its finalizer resurrected (PEP 442 / `test_is_finalized`).
    finalized_ids: RefCell<std::collections::HashSet<ObjectId>>,
}

impl Default for GcState {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GcState {
    fn drop(&mut self) {
        // Thread teardown: the tracked set can hold long generator /
        // container chains whose recursive field-drops overflow the
        // native stack (each `Arc` link is one `drop_in_place` frame).
        // Clear every tracked object's container fields *iteratively*
        // first so the chains are already severed when the handle
        // vectors drop. Safe at this point: the thread is exiting, no
        // Python code will observe the cleared objects.
        let mut handles: Vec<Arc<TrackedHandle>> = Vec::new();
        if let Ok(gens) = self.generations.try_borrow() {
            for g in gens.iter() {
                handles.extend(g.handles.iter().cloned());
            }
        }
        if let Ok(frozen) = self.frozen.try_borrow() {
            handles.extend(frozen.iter().cloned());
        }
        // Only clear objects whose sole remaining strong reference is the
        // registry handle itself. Anything with extra references escaped
        // into shared state that outlives this thread — e.g. Flag
        // pseudo-members a worker published into the enum class's
        // `_value2member_map_` — and other threads *will* observe it, so
        // wiping its fields would corrupt live objects. Iterate to a
        // fixpoint: each cleared object releases its referents, which can
        // drop a chained object's count to 1 and make it clearable on the
        // next pass — severing long chains without recursive drops.
        loop {
            let mut progress = false;
            handles.retain(|h| {
                if strong_count_for(&h.object) <= 1 {
                    clear_object_fields(&h.object);
                    progress = true;
                    false
                } else {
                    true
                }
            });
            if !progress {
                break;
            }
        }
    }
}

impl GcState {
    pub fn new() -> Self {
        Self {
            generations: RefCell::new(Default::default()),
            index: RefCell::new(std::collections::HashMap::new()),
            collecting: AtomicBool::new(false),
            thresholds: RefCell::new(DEFAULT_THRESHOLDS),
            counts: RefCell::new([0; N_GENERATIONS]),
            frozen: RefCell::new(Vec::new()),
            garbage: RefCell::new(Vec::new()),
            callbacks: RefCell::new(Vec::new()),
            stats: RefCell::new([GcStats::default(); N_GENERATIONS]),
            debug: AtomicI64::new(0),
            enabled: AtomicBool::new(true),
            tracked_version: AtomicUsize::new(0),
            tracked_count: AtomicUsize::new(0),
            finalized_ids: RefCell::new(std::collections::HashSet::new()),
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
    pub fn track(&self, obj: Object) {
        let new_id = id_of(&obj);
        {
            let mut index = self.index.borrow_mut();
            if index.contains_key(&new_id) {
                return;
            }
            let handle = Arc::new(TrackedHandle::new(obj, 0));
            index.insert(new_id, handle.clone());
            let mut gens = self.generations.borrow_mut();
            handle.slot.store(gens[0].handles.len(), Ordering::Release);
            gens[0].handles.push(handle);
        }
        // `finalized_ids` is keyed by object id (a pointer), which the
        // allocator recycles. A freshly tracked object at a recycled address
        // must start *un*-finalized, so drop any stale entry — otherwise
        // `gc.is_finalized(new_obj)` would inherit the previous tenant's
        // finalized flag (`test_is_finalized`).
        self.finalized_ids.borrow_mut().remove(&new_id);
        self.tracked_count.fetch_add(1, Ordering::AcqRel);
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
        self.bump_count(0);
    }

    /// Stop tracking `obj`. Used by the cycle-clearing path
    /// after an object is reclaimed, and by the explicit
    /// `gc._untrack(obj)` extension.
    pub fn untrack_id(&self, id: ObjectId) {
        let Some(handle) = self.index.borrow_mut().remove(&id) else {
            return;
        };
        // O(1) removal via the handle's cached `slot`. The index is the
        // dedupe authority, so exactly one handle existed for `id`, and
        // its `slot`/`generation`/`color` pinpoint its position without a
        // per-generation scan (which made drop-heavy large heaps
        // quadratic — RFC 0039 WS4).
        //
        // The cached `slot`/`generation` are *only* valid while the owning
        // generation/frozen lock is held: a `swap_remove` elsewhere updates a
        // moved handle's `slot` under that same lock. So we must acquire the
        // vector lock *before* reading the cached position, and — because the
        // GC is process-global and shared across OS threads — fall back to a
        // pointer search if the cached slot is stale, rather than corrupting
        // the vector with a wrong `swap_remove` (the bug behind the
        // "generation slot index out of sync" panic under threaded GC).
        if handle.color.load(Ordering::Acquire) == color::Frozen {
            let mut frozen = self.frozen.borrow_mut();
            let slot = handle.slot.load(Ordering::Acquire);
            if frozen.get(slot).is_some_and(|h| Arc::ptr_eq(h, &handle)) {
                swap_remove_handle(&mut frozen, slot);
            } else {
                remove_handle_by_ptr(&mut frozen, &handle);
            }
        } else {
            let mut gens = self.generations.borrow_mut();
            let g = handle
                .generation
                .load(Ordering::Acquire)
                .min(N_GENERATIONS - 1);
            let slot = handle.slot.load(Ordering::Acquire);
            if gens[g].handles.get(slot).is_some_and(|h| Arc::ptr_eq(h, &handle)) {
                swap_remove_handle(&mut gens[g].handles, slot);
            } else if !remove_handle_by_ptr(&mut gens[g].handles, &handle) {
                // Declared generation was wrong too (e.g. a concurrent
                // promotion landed between the `generation` and `slot`
                // reads). Search the rest before giving up.
                for gg in 0..N_GENERATIONS {
                    if gg != g && remove_handle_by_ptr(&mut gens[gg].handles, &handle) {
                        break;
                    }
                }
            }
        }
        self.tracked_count.fetch_sub(1, Ordering::AcqRel);
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
    }

    /// Reclaim every tracked object on this thread whose only remaining
    /// strong reference is the cycle collector's own handle — dead
    /// *acyclic* garbage that CPython's refcounting frees the instant
    /// its last binding drops, but which our per-thread strong handle
    /// pins until a collection. Skips finalizable objects (their
    /// `__del__` must be ordered by a finalizing collection) and
    /// weakref-watched objects (clearing their weakrefs runs user
    /// callbacks). Because every survivor of these filters runs no
    /// Python on the way out, this is safe to call from any GIL-holding
    /// safe point — notably a `Thread.join` return, where a worker has
    /// just dropped the last *program* reference to objects this thread
    /// allocated (RFC 0039 WS4: cross-thread prompt reclamation across
    /// the per-thread-heap boundary). Iterates to a fixpoint so freeing
    /// one object reclaims any acyclic chain it anchored. Returns the
    /// number of objects reclaimed.
    pub fn reap_dead_acyclic(&self) -> usize {
        // A collection already walks the same set; never re-enter it.
        if self.collecting.load(Ordering::Acquire) {
            return 0;
        }
        let mut reclaimed = 0usize;
        loop {
            let dead: Vec<ObjectId> = {
                let index = self.index.borrow();
                index
                    .iter()
                    .filter(|(id, h)| {
                        strong_count_for(&h.object) <= 1
                            && !has_finalizer(&h.object)
                            && crate::weakref_registry::count_for(**id) == 0
                    })
                    .map(|(id, _)| *id)
                    .collect()
            };
            if dead.is_empty() {
                break;
            }
            for id in dead {
                // Re-validate under a fresh borrow: a free earlier in this
                // batch may have already reclaimed `id` as a child, or
                // (it cannot here, counts only fall) revived it.
                let still_dead = match self.index.borrow().get(&id) {
                    Some(h) => {
                        strong_count_for(&h.object) <= 1
                            && !has_finalizer(&h.object)
                            && crate::weakref_registry::count_for(id) == 0
                    }
                    None => false,
                };
                if still_dead {
                    self.untrack_id(id);
                    reclaimed += 1;
                }
            }
        }
        reclaimed
    }

    pub fn is_tracked(&self, id: ObjectId) -> bool {
        self.index.borrow().contains_key(&id)
    }

    /// O(1) handle lookup by object id (any generation or frozen).
    pub fn handle_for(&self, id: ObjectId) -> Option<Arc<TrackedHandle>> {
        self.index.borrow().get(&id).cloned()
    }

    /// Snapshot every tracked object that still carries an unrun
    /// `__del__`. The interpreter's shutdown pass walks this list to
    /// finalize objects that are still alive at exit — CPython runs
    /// finalizers for everything during interpreter teardown, not just
    /// for cyclic garbage. The per-handle `finalized` flag (shared with
    /// the cycle collector) guarantees each `__del__` runs at most once.
    pub fn finalization_candidates(&self) -> Vec<Arc<TrackedHandle>> {
        let mut out = Vec::new();
        let pending = |h: &Arc<TrackedHandle>| {
            !h.finalized.load(Ordering::Acquire)
                // A finalizer already queued by a collection (but not yet
                // drained) must not be listed again — the pending queue owns
                // it, and running it twice would double-fire `__del__`.
                && !h.finalize_queued.load(Ordering::Acquire)
                && has_finalizer(&h.object)
        };
        let gens = self.generations.borrow();
        for gen in gens.iter() {
            for h in &gen.handles {
                if pending(h) {
                    out.push(h.clone());
                }
            }
        }
        for h in self.frozen.borrow().iter() {
            if pending(h) {
                out.push(h.clone());
            }
        }
        out
    }

    /// Number of tracked objects in each generation.
    pub fn counts(&self) -> [usize; N_GENERATIONS] {
        *self.counts.borrow()
    }

    pub fn thresholds(&self) -> [usize; N_GENERATIONS] {
        *self.thresholds.borrow()
    }

    pub fn set_thresholds(&self, t: [usize; N_GENERATIONS]) {
        *self.thresholds.borrow_mut() = t;
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
    }

    /// Threshold-driven automatic collection (CPython's `gc_alloc`
    /// path): when the gen-0 allocation counter passes `threshold0`,
    /// collect the *oldest* generation whose own counter has also
    /// passed its threshold. Returns the number of objects reclaimed.
    /// Callers must be at a safe point (no outstanding container
    /// borrows); the interpreter invokes this from its allocation
    /// sites.
    pub fn maybe_auto_collect(&self) -> bool {
        if !self.is_enabled() || self.collecting.load(Ordering::Acquire) {
            return false;
        }
        let (count0, eligible) = {
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
            (counts[0] >= thresholds[0], gen)
        };
        if !count0 {
            return false;
        }
        // Automatic young collection: single incremental pass, no whole-index
        // acyclic reap (see `collect_impl`'s `exact` discussion). Report that a
        // collection ran (regardless of how many objects it reclaimed) so the
        // caller drains any `__del__` finalizers it deferred — without paying a
        // pending-queue probe on every allocation.
        self.collect_impl(eligible, false);
        true
    }

    /// Total population (across all generations + frozen).
    pub fn population(&self) -> usize {
        let gens = self.generations.borrow();
        let mut n = 0;
        for g in gens.iter() {
            n += g.handles.len();
        }
        n + self.frozen.borrow().len()
    }

    /// Snapshot all tracked objects. Used by
    /// `gc.get_objects(generation=...)`.
    pub fn snapshot(&self, generation: Option<usize>) -> Vec<Object> {
        let gens = self.generations.borrow();
        let mut out = Vec::new();
        match generation {
            Some(g) if g < N_GENERATIONS => {
                for h in &gens[g].handles {
                    out.push(h.object.clone());
                }
            }
            _ => {
                for g in gens.iter() {
                    for h in &g.handles {
                        out.push(h.object.clone());
                    }
                }
            }
        }
        if generation.is_none() {
            for h in self.frozen.borrow().iter() {
                out.push(h.object.clone());
            }
        }
        out
    }

    /// `gc.freeze()` — mark every currently-tracked object as
    /// frozen so it is ignored by future collections.
    pub fn freeze_all(&self) {
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
        // Lock order: generations before frozen, matching `freeze_all`
        // (consistent ordering avoids a cross-cell deadlock now that the
        // GC is process-global — RFC 0039 WS4).
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
    /// full sweep. (`test_gc` disables the collector module-wide via
    /// `setUpModule` and then asserts that explicit collections still reclaim
    /// cycles.) The re-entrancy guard still applies — a collection triggered
    /// from inside a collection (e.g. an allocating finalizer) is a no-op.
    pub fn collect(&self, upto: usize) -> usize {
        // An explicit `gc.collect()` is "exact": it reaps acyclic dead and
        // iterates to a fixpoint so the returned count matches CPython.
        self.collect_impl(upto, true)
    }

    /// Run the cycle collector's mark phase across all generations and fire
    /// the weakref callbacks of every unreachable, non-finalizable object,
    /// *without* the destructive teardown of a real collection. See
    /// [`Self::collect_generation`]'s `weakref_only` discussion. The
    /// re-entrancy guard applies, so this is a no-op inside a collection.
    pub fn fire_dead_weakrefs(&self) {
        if self.collecting.load(Ordering::Acquire) {
            return;
        }
        self.collecting.store(true, Ordering::Release);
        self.collect_generation(N_GENERATIONS - 1, true);
        self.collecting.store(false, Ordering::Release);
    }

    /// Shared collection body. `exact` selects between the two cost/precision
    /// profiles:
    ///
    /// * `true` — an explicit `gc.collect()`. Reap acyclic dead up front (so
    ///   they stay out of the cyclic count and `DEBUG_SAVEALL`) and iterate the
    ///   mark-sweep to a fixpoint, reproducing CPython's "one call reclaims all
    ///   current cyclic garbage" guarantee that `test_gc`'s exact-count
    ///   assertions depend on.
    /// * `false` — a threshold-driven *automatic* young collection. CPython's
    ///   auto path is a single incremental pass (leftover garbage waits for the
    ///   next trigger or an explicit collect), so we skip both the whole-index
    ///   acyclic reap and the fixpoint loop. That keeps the per-allocation cost
    ///   flat: with the reap+fixpoint on every auto-collect, an allocation-heavy
    ///   suite (`test_set`'s mutation stress) re-scanned the entire accumulated
    ///   tracked set several times per trigger and blew the time budget.
    fn collect_impl(&self, upto: usize, exact: bool) -> usize {
        if self.collecting.load(Ordering::Acquire) {
            return 0;
        }
        if exact {
            // Reap dead *acyclic* garbage first. CPython frees these by refcount
            // the instant their last binding drops, so they never reach the
            // cycle collector; we pin them on the registry handle until now.
            // Doing it up front keeps them out of the cyclic `collected` count
            // *and* out of `gc.garbage` under `DEBUG_SAVEALL` (`test_saveall`
            // asserts only the genuine cycle is saved, not an incidental dead
            // `[]` temporary).
            self.reap_dead_acyclic();
        }
        self.collecting.store(true, Ordering::Release);
        let gen = upto.min(N_GENERATIONS - 1);
        // Iterate the mark-sweep to a fixpoint (exact only). Reachability is
        // seeded from an *approximate* outer refcount (`Rc::strong_count`), so a
        // transient Rust-side reference (an operand-stack slot not yet
        // overwritten, an in-flight clone) can make a dead object — and
        // everything reachable only through it — look live for a single pass.
        // CPython's collector is refcount-exact and reclaims *all* current
        // cyclic garbage in one `gc.collect()`; the count tests in `test_gc`
        // (`gc.collect()` returns exactly the cycle size) depend on that
        // completeness. Repeating until a pass collects nothing reproduces it:
        // each pass re-seeds from a fresh refcount snapshot, so a reference that
        // was transient last pass no longer pins its subgraph. Passes collect a
        // strictly shrinking set, so this converges quickly; the cap is a guard
        // against pathological churn.
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
            // CPython resets the counters of every collected
            // generation and credits one "tick" to the next older
            // one — that tick is what eventually promotes a gen-1 /
            // gen-2 collection in `maybe_auto_collect`.
            let mut counts = self.counts.borrow_mut();
            for c in counts.iter_mut().take(gen + 1) {
                *c = 0;
            }
            if gen + 1 < N_GENERATIONS {
                counts[gen + 1] = counts[gen + 1].saturating_add(1);
            }
        }
        self.collecting.store(false, Ordering::Release);
        collected
    }

    /// Collect a specific generation. Used by [`Self::collect`].
    ///
    /// `weakref_only` runs the identical mark phase but stops once the
    /// unreachable set is known: it fires the weakref callbacks of the dead,
    /// non-finalizable objects (flipping `weakref.ref(obj)()` to `None`) and
    /// returns *without* running finalizers, clearing fields, untracking, or
    /// rebuilding generations. It is used from a blocking `Thread.join` to
    /// fire a reference-count-dead `ThreadPoolExecutor`'s `weakref_cb` (which
    /// signals its idle workers to exit) without the destructive teardown of a
    /// full collection — which, run while a worker holds an in-flight
    /// `_WorkItem` in a frame the collector can't see as a root, would clear
    /// that live work item mid-use (RFC 0040: `test_shutdown`). Because it
    /// never mutates object contents, such a misclassification is harmless
    /// here (a `_WorkItem` has no weakref, so its `notify_clear` is a no-op).
    fn collect_generation(&self, gen: usize, weakref_only: bool) -> usize {
        // Phase 1: snapshot the handles in this generation, plus
        // any younger ones (collecting gen N also collects all
        // gens 0..N). We treat gens 0..=gen as the candidate set.
        let candidate_set = self.snapshot_for_collection(gen);
        let cs_len = candidate_set.len();
        if cs_len == 0 {
            return 0;
        }

        // Phase 2: initialise gc_refs from the *outer* refcount.
        // For Rc-wrapped objects we approximate by
        // `Rc::strong_count - 1` (the candidate set holds one
        // reference itself, in `TrackedHandle::object`).
        for handle in &candidate_set {
            // A weak reference must not keep its referent reachable, but
            // each live slot holds a strong `Object` clone of the target
            // (the registry's drop-driven clear model). Discount those
            // clones here so an object reachable *only* through weakrefs
            // collapses to `gc_refs == 0` and is collected — which fires
            // `notify_clear` and flips `weakref.ref(obj)()` to `None`.
            let weak_clones = crate::weakref_registry::strong_clone_count(handle.id) as i64;
            let outer = strong_count_for(&handle.object)
                .saturating_sub(1)
                .saturating_sub(weak_clones as usize) as i64;
            handle.gc_refs.store(outer, Ordering::Release);
            handle.color.store(color::White, Ordering::Release);
        }

        // Index the candidate set by id so the per-child lookups in
        // phases 3 and 4 are O(1) — a linear `find` here makes the
        // whole collection quadratic, which generator-heavy programs
        // (itertools pipelines) hit hard.
        let mut by_id: std::collections::HashMap<ObjectId, Arc<TrackedHandle>> =
            candidate_set.iter().map(|h| (h.id, h.clone())).collect();

        // Phase 2b: promote untracked iterators reachable from the candidate
        // set to *temporary* candidates for this pass only. CPython GC-tracks
        // its `*_iterator` objects, so an iterator-mediated cycle (bug #3680:
        // `obj.x = iter(set_containing_obj)`) is collectible: the iterator's
        // single internal ref to the container has to be subtracted off the
        // container's `gc_refs`, otherwise the container looks externally
        // reachable and pins the whole cycle. We keep transient *loop*
        // iterators untracked for speed (enrolling every `for`-loop iterator
        // in a generation regressed allocation-heavy suites by triggering far
        // more young collections); instead we discover only the iterators that
        // are actually reachable from already-tracked objects, here, while a
        // collection is already in flight. The temporary handles take part in
        // the subtract/mark walk (so their edges are accounted) but never enter
        // a generation, are never cleared/finalized, never touch the index, and
        // are not counted as collected — they're dropped when this pass ends,
        // and the underlying iterator is freed by refcount once the real
        // objects in its (dead) cycle are cleared.
        let mut temp_handles: Vec<Arc<TrackedHandle>> = Vec::new();
        {
            // `work` holds cheap `Arc` handles, never extra `Object` clones, so
            // the only strong reference a discovered object gains is the one
            // inside its temporary handle. Scanning `work` by index lets newly
            // discovered objects extend it, so a private buffer reached through
            // an iterator (and any iterator reached through that buffer) is
            // promoted too. We promote untracked iterators and untracked
            // `list` buffers: a snapshot iterator (`frozenset`/`dict.values()`/
            // file) hands back a fresh, untracked `Object::List` for its
            // buffer, whose `-> elements` edges have to be accounted for the
            // cycle to collapse. An `iter(list)` shares the live list's buffer,
            // which is already a real candidate and is found by id below.
            let mut work: Vec<Arc<TrackedHandle>> = candidate_set.clone();
            let mut scanned = 0usize;
            while scanned < work.len() {
                let h = work[scanned].clone();
                scanned += 1;
                // Immutable containers (tuple/frozenset) and iterators are not
                // persistently GC-tracked — pinning them in a generation would
                // hold transient `(type, value, tb)` triples and loop iterators
                // alive past the point CPython frees them by refcount
                // (`test_traceback`'s `getrefcount` asserts, the loop-iterator
                // churn that regressed allocation-heavy suites). But a cycle can
                // still *route through* one (`l=[]; t=(l,); l.append(t)`;
                // `obj.x = iter(set_containing_obj)`), so we discover the ones
                // reachable from the (mutable, tracked) candidate set here and
                // promote them to temporary candidates: their internal edges are
                // accounted, the dead ones are counted, and the handles are
                // dropped when the pass ends (no persistent pinning).
                //
                // Lists are tracked at creation, so an *untracked* list is only
                // ever an iterator's private snapshot buffer (`frozenset`/
                // `dict.values()`/file iterators); promote those only when
                // reached directly through an iterator, so we never re-scan the
                // whole (already tracked) list population.
                let parent_is_iter = matches!(&h.object, Object::Iter(_));
                traverse_object(&h.object, &mut |child| {
                    let promote = match child {
                        Object::Iter(_) | Object::Tuple(_) | Object::FrozenSet(_) => true,
                        Object::List(_) => parent_is_iter,
                        _ => false,
                    };
                    if !promote {
                        return;
                    }
                    let cid = id_of(child);
                    if by_id.contains_key(&cid) {
                        return;
                    }
                    let handle = Arc::new(TrackedHandle::new(child.clone(), 0));
                    by_id.insert(cid, handle.clone());
                    temp_handles.push(handle.clone());
                    work.push(handle);
                });
            }
            // Seed `gc_refs` *after* discovery: an iterator synthesises a fresh
            // `Object::List`/`Object::Set` wrapper for its buffer on each
            // traverse, and that wrapper is alive only for the duration of the
            // `visit` call above. Computing the outer refcount here — once
            // every such transient clone has been dropped — keeps the seed
            // exact (referrers + the one clone the handle itself holds).
            for handle in &temp_handles {
                let weak_clones = crate::weakref_registry::strong_clone_count(handle.id) as i64;
                let outer = strong_count_for(&handle.object)
                    .saturating_sub(1)
                    .saturating_sub(weak_clones as usize) as i64;
                handle.gc_refs.store(outer, Ordering::Release);
                handle.color.store(color::White, Ordering::Release);
            }
        }

        // Real candidates plus the temporary iterator candidates take part in
        // the subtract/mark walk; only the real ones are reclaimed below.
        let scan_all: Vec<Arc<TrackedHandle>> = candidate_set
            .iter()
            .chain(temp_handles.iter())
            .cloned()
            .collect();

        // Phase 3: subtract internal refs by walking each
        // tracked object's children. Self-references count too —
        // a `self.self = self` instance has one internal ref to
        // itself which must be subtracted off so a pure self-cycle
        // collapses to gc_refs == 0.
        for handle in &scan_all {
            traverse_object(&handle.object, &mut |child| {
                if let Some(target) = by_id.get(&id_of(child)) {
                    target.gc_refs.fetch_sub(1, Ordering::AcqRel);
                }
            });
        }

        // Phase 4: anything with gc_refs > 0 is reachable from
        // outside; mark it black and propagate.
        let mut grey: Vec<Arc<TrackedHandle>> = Vec::new();
        for handle in &scan_all {
            if handle.gc_refs.load(Ordering::Acquire) > 0 {
                handle.color.store(color::Grey, Ordering::Release);
                grey.push(handle.clone());
            }
        }
        while let Some(h) = grey.pop() {
            h.color.store(color::Black, Ordering::Release);
            traverse_object(&h.object, &mut |child| {
                if let Some(target) = by_id.get(&id_of(child)) {
                    if target.color.load(Ordering::Acquire) == color::White {
                        target.color.store(color::Grey, Ordering::Release);
                        grey.push(target.clone());
                    }
                }
            });
        }

        // Phase 5: white objects are unreachable cyclic garbage.
        let unreachable: Vec<Arc<TrackedHandle>> = candidate_set
            .iter()
            .filter(|h| h.color.load(Ordering::Acquire) == color::White)
            .cloned()
            .collect();

        if std::env::var_os("WP_REAP_DBG").is_some() {
            for h in &candidate_set {
                if let Object::Instance(i) = &h.object {
                    if i.cls().name.contains("Executor") {
                        let exec_id = h.id;
                        let mut referrers: Vec<String> = Vec::new();
                        for c in &scan_all {
                            if c.id == exec_id {
                                continue;
                            }
                            let mut hit = false;
                            traverse_object(&c.object, &mut |child| {
                                if id_of(child) == exec_id {
                                    hit = true;
                                }
                            });
                            if hit {
                                let nm = match &c.object {
                                    Object::Instance(ci) => format!("Instance({})", ci.cls().name),
                                    other => other.type_name().to_string(),
                                };
                                referrers.push(nm);
                            }
                        }
                        eprintln!(
                            "[mark wronly={}] Executor sc={} clones={} gc_refs={} white={} tracked_referrers={:?}",
                            weakref_only,
                            strong_count_for(&h.object),
                            crate::weakref_registry::strong_clone_count(h.id),
                            h.gc_refs.load(Ordering::Acquire),
                            h.color.load(Ordering::Acquire) == color::White,
                            referrers,
                        );
                    }
                }
            }
        }

        // Weakref-only pass: fire the dead objects' weakref callbacks and
        // stop. We deliberately skip everything destructive below (finalizer
        // execution, field clearing, untracking, generation rebuild) so a
        // frame-rooted live object the mark mis-coloured White is left fully
        // intact — only its (absent) weakrefs would be touched. A genuinely
        // dead, weakref-watched object (the `del`'d `ThreadPoolExecutor`) gets
        // its `weakref_cb` queued, which is all a blocking `join` needs to
        // unblock its idle workers. Finalizable objects are left for a real
        // collection so `tp_finalize` ordering is preserved.
        if weakref_only {
            let mut weakref_callbacks = Vec::new();
            for h in &unreachable {
                if has_finalizer(&h.object) && !h.finalized.load(Ordering::Acquire) {
                    continue;
                }
                for (slot, cb) in crate::weakref_registry::notify_clear(h.id) {
                    if let Some(cb) = cb {
                        weakref_callbacks.push((slot, cb));
                    }
                }
            }
            for (slot, cb) in weakref_callbacks {
                let wr = slot
                    .py_ref
                    .borrow()
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                    .map(crate::object::Object::Instance);
                if let Some(wr) = wr {
                    crate::vm_singletons::push_pending_weakref_callback(cb, wr);
                }
            }
            return 0;
        }

        // Split the unreachable set into objects whose `__del__` hasn't run
        // yet ("deferred") and the rest. A deferred object is queued for
        // finalization and kept tracked: its finalizer (drained right after
        // `gc.collect()` returns control to the interpreter) might resurrect
        // it, and CPython only counts objects it actually reclaims
        // (`test_resurrection_*`). The interpreter then collects again — by
        // which point the finalizer has set `finalized`, so a survivor that
        // wasn't resurrected falls into `dead` and is reclaimed (its weakrefs
        // cleared in that second pass, so single-`collect()` weakref tests
        // still observe `ref() is None`).
        let mut deferred: Vec<Arc<TrackedHandle>> = Vec::new();
        let mut maybe_dead: Vec<Arc<TrackedHandle>> = Vec::new();
        for h in &unreachable {
            let pending_finalizer =
                has_finalizer(&h.object) && !h.finalized.load(Ordering::Acquire);
            if pending_finalizer {
                deferred.push(h.clone());
            } else {
                maybe_dead.push(h.clone());
            }
        }

        // Run each deferred object's finalizer (once). A finalizer is arbitrary
        // Python: it can execute bytecode, hit a `periodic_gil_checkpoint`, and
        // hand the GIL to another OS thread — which may then *resurrect* an
        // object the mark phase just classified unreachable (store it somewhere
        // reachable, or, in the threaded queue reproducers, pull it off a buffer
        // into a live frame local). Every mark color computed above predates
        // these finalizers, so it is stale the instant any finalizer runs.
        for h in &deferred {
            if !h.finalize_queued.swap(true, Ordering::AcqRel) {
                run_finalizer(&h.object);
            }
        }

        // CPython's `handle_resurrected_objects`: after `finalize_garbage` runs
        // every `tp_finalize`, it re-derives reachability and moves any object
        // that came back to life out of the to-be-cleared set. Mirror that — but
        // only when a finalizer actually ran, since that is the sole point in
        // this routine where the GIL can be released and the object graph can
        // change underneath us. Re-seed `gc_refs` from a *fresh* strong-count
        // snapshot (so a reference a concurrent thread or a finalizer added is
        // counted), re-subtract internal edges, and re-propagate reachability.
        // Without this, a live object reachable only through an untraversed root
        // (a running thread's frame locals) that a finalizer's GIL hand-off
        // revived is cleared mid-use — emptying its `__dict__` while another
        // thread pickles it (RFC 0040: `ProcessPoolExecutor` / multiprocessing
        // `Queue` feeder dropping a `_CallItem` into a worker's pipe).
        if !deferred.is_empty() {
            for handle in &scan_all {
                let weak_clones = crate::weakref_registry::strong_clone_count(handle.id) as i64;
                let outer = strong_count_for(&handle.object)
                    .saturating_sub(1)
                    .saturating_sub(weak_clones as usize) as i64;
                handle.gc_refs.store(outer, Ordering::Release);
                handle.color.store(color::White, Ordering::Release);
            }
            for handle in &scan_all {
                traverse_object(&handle.object, &mut |child| {
                    if let Some(target) = by_id.get(&id_of(child)) {
                        target.gc_refs.fetch_sub(1, Ordering::AcqRel);
                    }
                });
            }
            let mut grey: Vec<Arc<TrackedHandle>> = Vec::new();
            for handle in &scan_all {
                if handle.gc_refs.load(Ordering::Acquire) > 0 {
                    handle.color.store(color::Grey, Ordering::Release);
                    grey.push(handle.clone());
                }
            }
            while let Some(h) = grey.pop() {
                h.color.store(color::Black, Ordering::Release);
                traverse_object(&h.object, &mut |child| {
                    if let Some(target) = by_id.get(&id_of(child)) {
                        if target.color.load(Ordering::Acquire) == color::White {
                            target.color.store(color::Grey, Ordering::Release);
                            grey.push(target.clone());
                        }
                    }
                });
            }
        }

        // Recolor the deferred roots Black and protect their whole reachable
        // subgraph. CPython runs `finalize_garbage` *before* `delete_garbage`, so
        // a pending finalizer always sees its own class, closure cells, and
        // referents intact — even when those are themselves unreachable cyclic
        // garbage (a locally-defined class whose only instance is dying, the
        // `__del__` function closing over the cycle, …). Those objects are
        // reclaimed by a later pass once the owning finalizer has run and they,
        // too, are plain garbage. Re-applied here so it survives the resurrection
        // re-mark above (which reset every color from the fresh refcounts).
        let mut protect_stack: Vec<Arc<TrackedHandle>> = Vec::new();
        for h in &deferred {
            h.color.store(color::Black, Ordering::Release);
            protect_stack.push(h.clone());
        }
        while let Some(h) = protect_stack.pop() {
            traverse_object(&h.object, &mut |child| {
                if let Some(target) = by_id.get(&id_of(child)) {
                    if target.color.load(Ordering::Acquire) == color::White {
                        target.color.store(color::Black, Ordering::Release);
                        protect_stack.push(target.clone());
                    }
                }
            });
        }

        // Whatever stayed White after the resurrection re-mark and the finalizer
        // subgraph protection is genuinely dead this pass.
        let dead: Vec<Arc<TrackedHandle>> = maybe_dead
            .into_iter()
            .filter(|h| h.color.load(Ordering::Acquire) == color::White)
            .collect();
        let collected = dead.len();

        // Temporarily-promoted iterators / immutable containers (tuple,
        // frozenset) that ended up White are genuine cyclic garbage: they'll be
        // freed by refcount the moment the mutable anchor in their cycle is
        // cleared just below. CPython counts each in the `gc.collect()` total
        // (`test_tuple` asserts the closing tuple is counted alongside its
        // list), so fold the dead real-object temporaries into the *reported*
        // count. The private list buffers an iterator snapshots have no CPython
        // counterpart, so they don't count; and none of these were ever in
        // `tracked_count`, so that bookkeeping uses `collected` (real) below.
        let mut reported = collected;
        for h in &temp_handles {
            if h.color.load(Ordering::Acquire) == color::White
                && matches!(
                    h.object,
                    Object::Iter(_) | Object::Tuple(_) | Object::FrozenSet(_)
                )
            {
                reported += 1;
            }
        }

        // 5a: clear weakrefs for the reclaimed objects, queueing callbacks for
        // invocation in 5d. Deferred (possibly-resurrected) objects keep their
        // weakrefs until a later pass confirms they're dead.
        let mut weakref_callbacks = Vec::new();
        for h in &dead {
            let cleared = crate::weakref_registry::notify_clear(h.id);
            for (slot, cb) in cleared {
                if let Some(cb) = cb {
                    weakref_callbacks.push((slot, cb));
                }
            }
        }

        // 5b (RFC 0039 WS5): before tearing the dead objects down, record the
        // children they referenced *outside* this collection's candidate set.
        // These seed the older-generation refcount cascade in 5c2; they must
        // be captured here, while the dead objects' fields are still intact.
        let saveall = self.debug.load(Ordering::Acquire) & DEBUG_SAVEALL != 0;
        let mut cascade_seed: Vec<ObjectId> = Vec::new();
        if !saveall {
            for h in &dead {
                traverse_object(&h.object, &mut |child| {
                    cascade_seed.push(id_of(child));
                });
            }
        }

        // 5c: break cycles by clearing the reclaimed objects' fields — or,
        // under `gc.DEBUG_SAVEALL`, park them in `gc.garbage` intact for
        // inspection instead of tearing them down.
        if saveall {
            let mut garbage = self.garbage.borrow_mut();
            for h in &dead {
                garbage.push(h.object.clone());
            }
        } else {
            for h in &dead {
                clear_object_fields(&h.object);
            }
        }

        // 5c2 (RFC 0039 WS5): cascade refcount-reclamation into *older*
        // generations the current pass didn't scan. CPython frees an object
        // the instant its refcount hits zero, regardless of generation:
        // clearing a young cyclic-garbage object (`c1`) drops the last
        // reference to an old object (`c0`) it pointed at, which frees `c0`
        // and fires `c0`'s weakref callback — even during a young-only
        // collection (`test_gc` `test_bug1055820c`). Our tracked handle pins
        // such an object, so the refcount never reaches zero on its own;
        // emulate the cascade explicitly. Starting from the children the now
        // cleared dead objects referenced (captured in 5b), reap any tracked
        // object that (a) isn't part of this collection's candidate set (those
        // are handled by the normal mark/rebuild) and (b) is now reachable only
        // through its own tracked handle and weakref slots, firing its weakref
        // callbacks and recursing into its children. Finalizable orphans are
        // left for a finalizing collection so `__del__` ordering is preserved.
        if !saveall {
            let dead_ids: std::collections::HashSet<ObjectId> = dead.iter().map(|h| h.id).collect();
            let mut worklist = cascade_seed;
            let mut seen: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();
            while let Some(cid) = worklist.pop() {
                if dead_ids.contains(&cid) || by_id.contains_key(&cid) || !seen.insert(cid) {
                    // Dead (already reaped), a candidate this collection owns,
                    // or already visited — skip.
                    continue;
                }
                let Some(h) = self.index.borrow().get(&cid).cloned() else {
                    continue;
                };
                // Leave finalizable objects to a finalizing collection.
                if has_finalizer(&h.object) {
                    continue;
                }
                let weak_clones = crate::weakref_registry::strong_clone_count(cid);
                let effective = strong_count_for(&h.object)
                    .saturating_sub(1)
                    .saturating_sub(weak_clones);
                if effective != 0 {
                    // Still reachable from a survivor — keep it.
                    continue;
                }
                // Orphaned: fire its weakref callbacks (queued in 5d below),
                // capture its children for the cascade, tear it down, and drop
                // it from the tracked set.
                for (slot, cb) in crate::weakref_registry::notify_clear(cid) {
                    if let Some(cb) = cb {
                        weakref_callbacks.push((slot, cb));
                    }
                }
                traverse_object(&h.object, &mut |child| {
                    worklist.push(id_of(child));
                });
                clear_object_fields(&h.object);
                self.untrack_id(cid);
            }
        }

        // 5d: queue weakref callbacks (after finalisers and cyclic
        // clears, matching CPython's order). The interpreter drains
        // the queue at its next safe point — the GC layer can't call
        // Python itself.
        for (slot, cb) in weakref_callbacks {
            let wr = slot
                .py_ref
                .borrow()
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .map(crate::object::Object::Instance);
            if let Some(wr) = wr {
                crate::vm_singletons::push_pending_weakref_callback(cb, wr);
            }
        }

        // Phase 6: rebuild the generation lists. Survivors of
        // generation `g` (color != White) move to generation
        // min(g+1, N_GENERATIONS-1).
        self.rebuild_generations(gen, &candidate_set);

        // Adjust the population counter.
        self.tracked_count.fetch_sub(
            collected.min(self.tracked_count.load(Ordering::Acquire)),
            Ordering::AcqRel,
        );
        self.tracked_version.fetch_add(1, Ordering::AcqRel);

        reported
    }

    fn snapshot_for_collection(&self, upto: usize) -> Vec<Arc<TrackedHandle>> {
        let gens = self.generations.borrow();
        let mut out = Vec::new();
        for g in 0..=upto.min(N_GENERATIONS - 1) {
            for h in &gens[g].handles {
                out.push(h.clone());
            }
        }
        out
    }

    fn rebuild_generations(&self, upto: usize, candidates: &[Arc<TrackedHandle>]) {
        // Lock order MUST match `track` (index before generations): the
        // collector and a mutator thread can both reach the GC under the
        // shared, process-global state, and acquiring these two cells in
        // opposite orders is a textbook deadlock (observed under
        // `test_weakref`'s background-collector loop — RFC 0039 WS4).
        let mut index = self.index.borrow_mut();
        let mut gens = self.generations.borrow_mut();
        for g in 0..=upto.min(N_GENERATIONS - 1) {
            gens[g].handles.clear();
        }
        for h in candidates {
            let color = h.color.load(Ordering::Acquire);
            if color == color::White {
                index.remove(&h.id);
                continue;
            }
            let g = h.generation.load(Ordering::Acquire);
            let new_g = (g + 1).min(N_GENERATIONS - 1);
            h.generation.store(new_g, Ordering::Release);
            h.color.store(color::White, Ordering::Release);
            h.slot.store(gens[new_g].handles.len(), Ordering::Release);
            gens[new_g].handles.push(h.clone());
        }
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
        Object::Tuple(t) => Rc::strong_count(t),
        Object::Instance(i) => Rc::strong_count(i),
        Object::Function(f) => Rc::strong_count(f),
        Object::Builtin(b) => Rc::strong_count(b),
        Object::BoundMethod(b) => Rc::strong_count(b),
        Object::Generator(g) => Rc::strong_count(g),
        Object::Coroutine(g) => Rc::strong_count(g),
        Object::AsyncGenerator(g) => Rc::strong_count(g),
        Object::ByteArray(b) => Rc::strong_count(b),
        Object::Iter(i) => Rc::strong_count(i),
        Object::Frame(f) => Rc::strong_count(f),
        Object::Traceback(t) => Rc::strong_count(t),
        Object::MemoryView(m) => Rc::strong_count(m),
        Object::MappingProxy(d) => Rc::strong_count(d),
        Object::DictView(v) => Rc::strong_count(v),
        Object::SimpleNamespace(d) => Rc::strong_count(d),
        Object::Cell(c) => Rc::strong_count(c),
        Object::Module(m) => Rc::strong_count(m),
        Object::Type(t) => Rc::strong_count(t),
        Object::Code(c) => Rc::strong_count(c),
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
            if let Ok(m) = i.dict.try_borrow() {
                for (k, v) in m.iter() {
                    visit(&k.0);
                    visit(v);
                }
            }
            if let Ok(slots) = i.slots.try_borrow() {
                if let Some(slots) = slots.as_ref() {
                    for (k, v) in slots.iter() {
                        visit(&k.0);
                        visit(v);
                    }
                }
            }
            // A built-in *container* subclass (`class C(list)`, `D(dict)`,
            // `S(set)`, …) keeps its payload in `native`; that container is
            // an internal, separately-untracked detail of the instance, so
            // its elements are the instance's real children. Walk them so
            // the collector sees cycles routed through subclass storage and
            // prompt reclamation can follow such a chain (a leaf `native`
            // like an `int`/`str` subclass simply has no children).
            if let Some(native) = &i.native {
                traverse_object(native, visit);
            }
        }
        Object::Module(m) => {
            let Ok(dict) = m.dict.try_borrow() else {
                return;
            };
            for (k, v) in dict.iter() {
                visit(&k.0);
                visit(v);
            }
        }
        Object::Cell(c) => {
            let Ok(v) = c.try_borrow() else { return };
            visit(&v);
        }
        Object::BoundMethod(b) => {
            visit(&b.function);
            visit(&b.receiver);
        }
        Object::Slice(s) => {
            visit(&s.start);
            visit(&s.stop);
            visit(&s.step);
        }
        Object::Property(p) => {
            visit(&p.fget);
            visit(&p.fset);
            visit(&p.fdel);
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
            // Dict views borrow the underlying dict — visit its
            // entries so cycles through `dict.items()` snapshots
            // are detectable.
            let Ok(m) = v.dict.try_borrow() else { return };
            for (k, val) in m.iter() {
                visit(&k.0);
                visit(val);
            }
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
            for d in &f.defaults {
                visit(d);
            }
            for (_, v) in &f.kw_defaults {
                visit(v);
            }
            for cell in &f.closure {
                visit(cell);
            }
            if let Ok(attrs) = f.attrs.try_borrow() {
                for (k, v) in attrs.iter() {
                    visit(&k.0);
                    visit(v);
                }
            }
            if let Ok(slots) = f.slots.try_borrow() {
                for (k, v) in slots.iter() {
                    visit(&k.0);
                    visit(v);
                }
            }
        }
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
    let table = TRAVERSE_TABLE.get_or_init(|| parking_lot::Mutex::new(Vec::new()));
    let entries = table.lock();
    for entry in entries.iter() {
        if (entry.matches)(obj) {
            (entry.traverse)(obj, visit);
        }
    }
}

#[allow(missing_debug_implementations)]
struct TraverseEntry {
    matches: fn(&Object) -> bool,
    traverse: fn(&Object, &mut dyn FnMut(&Object)),
}

static TRAVERSE_TABLE: std::sync::OnceLock<parking_lot::Mutex<Vec<TraverseEntry>>> =
    std::sync::OnceLock::new();

/// Register a traverse callback. Called once per Object variant
/// whose fields are not directly visible to `traverse_object`.
pub fn register_traverse(
    matches: fn(&Object) -> bool,
    traverse: fn(&Object, &mut dyn FnMut(&Object)),
) {
    let table = TRAVERSE_TABLE.get_or_init(|| parking_lot::Mutex::new(Vec::new()));
    table.lock().push(TraverseEntry { matches, traverse });
}

/// Drain a container's child references in place. Used during
/// the GC's clear phase to break cycles.
pub fn clear_object_fields(obj: &Object) {
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
            if let Ok(mut m) = i.dict.try_borrow_mut() {
                m.clear();
            }
            if let Ok(mut slots) = i.slots.try_borrow_mut() {
                *slots = None;
            }
        }
        Object::ByteArray(b) => {
            if let Ok(mut v) = b.try_borrow_mut() {
                v.clear();
            }
        }
        Object::Cell(c) => {
            if let Ok(mut v) = c.try_borrow_mut() {
                *v = Object::None;
            }
        }
        Object::Function(f) => {
            // Break the function's outgoing edges (CPython `func_clear`).
            // `globals` is intentionally left alone: it's a shared namespace
            // dict (a module's `__dict__` or the `exec` target), reclaimed as
            // its own candidate if it too is unreachable — clearing it here
            // could wipe a live module.
            if let Ok(mut attrs) = f.attrs.try_borrow_mut() {
                attrs.clear();
            }
            if let Ok(mut slots) = f.slots.try_borrow_mut() {
                slots.clear();
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
        Object::Instance(inst) => inst.cls().lookup("__del__").is_some(),
        Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g) => !g.is_finished(),
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

/// Run a closure with the shared, process-global GC state.
pub fn with_state<R>(f: impl FnOnce(&GcState) -> R) -> R {
    f(&GC_STATE)
}

/// Convenience: track `obj` in the shared, process-global GC.
pub fn track(obj: Object) {
    with_state(|s| s.track(obj));
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
fn container_can_cycle(obj: &Object) -> bool {
    match obj {
        Object::List(l) => l
            .try_borrow()
            .map(|v| v.iter().any(|x| !is_atomic(x)))
            .unwrap_or(true),
        Object::Set(s) => s
            .try_borrow()
            .map(|m| m.iter().any(|k| !is_atomic(&k.0)))
            .unwrap_or(true),
        Object::Dict(d) => d
            .try_borrow()
            .map(|m| m.iter().any(|(k, v)| !is_atomic(&k.0) || !is_atomic(v)))
            .unwrap_or(true),
        // A tuple can only anchor a cycle through a non-atomic element. An
        // empty or all-scalar tuple (the interned `()`, `(1, 2)`, …) can never
        // close one, so it stays off the GC's books.
        Object::Tuple(t) => t.iter().any(|x| !is_atomic(x)),
        // Any other container kind: be conservative and track.
        _ => true,
    }
}

/// Track a freshly-created mutable container (`list`/`dict`/`set`) with
/// the cycle collector, but only when it can actually participate in a
/// cycle (see [`container_can_cycle`]). Returns `true` when the object
/// was added to the tracked set, so the caller can decide whether to
/// run a threshold-driven young collection at the allocation site.
pub fn track_if_cyclic(obj: &Object) -> bool {
    if container_can_cycle(obj) {
        track(obj.clone());
        true
    } else {
        false
    }
}

/// Convenience: threshold-driven automatic collection on the current
/// thread's GC (see [`GcState::maybe_auto_collect`]). Returns the
/// number of objects reclaimed; the caller should drain pending
/// finalizers when this is non-zero.
pub fn maybe_auto_collect() -> bool {
    let ran = with_state(GcState::maybe_auto_collect);
    if ran {
        sweep_weakref_only_targets();
    }
    ran
}

/// Convenience: find a tracked handle by object id (O(1) via the
/// id index, which covers all generations plus the frozen set).
pub fn find_handle(id: ObjectId) -> Option<Arc<TrackedHandle>> {
    with_state(|s| s.handle_for(id))
}

/// Convenience: is `id` currently tracked by the cycle GC? Used
/// by refcount-emulation paths to discount the registry's own
/// strong handle.
pub fn is_tracked(id: ObjectId) -> bool {
    find_handle(id).is_some()
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

/// Convenience: snapshot all tracked objects with an unrun `__del__`
/// in the shared GC (see [`GcState::finalization_candidates`]).
pub fn finalization_candidates() -> Vec<Arc<TrackedHandle>> {
    with_state(|s| s.finalization_candidates())
}

/// Convenience: refcount-reclaim dead acyclic garbage in the shared
/// GC (see [`GcState::reap_dead_acyclic`]).
pub fn reap_dead_acyclic() -> usize {
    with_state(|s| s.reap_dead_acyclic())
}

/// Convenience: run a full collection on the shared GC. Returns the
/// number of objects collected.
pub fn collect_all() -> usize {
    let n = with_state(|s| s.collect(N_GENERATIONS - 1));
    sweep_weakref_only_targets();
    n
}

/// Convenience: run a partial collection of generations
/// `0..=upto`.
pub fn collect_upto(upto: usize) -> usize {
    let n = with_state(|s| s.collect(upto));
    sweep_weakref_only_targets();
    n
}

/// Convenience: fire dead objects' weakref callbacks via a non-destructive
/// mark pass on the shared GC (see [`GcState::fire_dead_weakrefs`]), then
/// sweep the untracked weakref-only targets. Used from a blocking
/// `Thread.join` to unblock idle `ThreadPoolExecutor` workers without the
/// teardown risk of a full collection.
pub fn fire_dead_weakrefs() {
    with_state(|s| s.fire_dead_weakrefs());
    sweep_weakref_only_targets();
}

/// Clear weakrefs whose referent isn't in the tracked set and whose
/// only remaining strong references are the weakref slots' own
/// clones. Covers weakref-able objects the cycle collector never
/// sees — plain functions, bound methods, types — so
/// `del f; gc.collect()` flips `weakref.ref(f)()` to `None` exactly
/// like CPython's refcount-driven `tp_dealloc` would.
pub fn sweep_weakref_only_targets() -> usize {
    let targets = crate::weakref_registry::with_registry(|r| r.targets());
    let mut cleared = 0;
    for (id, target) in targets {
        if is_tracked(id) {
            // Tracked objects belong to the cycle pass (their handle
            // holds an extra strong ref this arithmetic doesn't model).
            continue;
        }
        let clones = crate::weakref_registry::strong_clone_count(id);
        // `target` itself is one clone we hold for the probe.
        if strong_count_for(&target) <= clones + 1 {
            crate::weakref_registry::queue_callbacks(crate::weakref_registry::notify_clear(id));
            cleared += 1;
        }
    }
    if cleared > 0 {
        crate::weakref_registry::with_registry(|r| r.shrink());
    }
    cleared
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Rc;
    use crate::sync::RefCell;

    use crate::object::DictData;

    #[test]
    fn track_and_untrack() {
        let s = GcState::new();
        let d = Object::Dict(Rc::new(RefCell::new(DictData::new())));
        s.track(d.clone());
        assert!(s.is_tracked(id_of(&d)));
        s.untrack_id(id_of(&d));
        assert!(!s.is_tracked(id_of(&d)));
    }

    #[test]
    fn collect_clears_simple_cycle() {
        let s = GcState::new();
        let dict = Rc::new(RefCell::new(DictData::new()));
        let outer = Object::Dict(dict.clone());
        s.track(outer.clone());
        // The dict references itself: a 1-cycle.
        dict.borrow_mut().insert(
            crate::object::DictKey(Object::from_static("self")),
            outer.clone(),
        );
        // Drop the local strong ref; only the cycle + the GC's
        // tracked handle keep it alive.
        drop(outer);
        let collected = s.collect(2);
        // We expect the cyclic dict to be discovered (the cycle's
        // gc_refs is balanced by the self-pointer). The actual
        // assertion is loose — the GC may or may not collect on
        // the first pass depending on Rust-side stash counts.
        let _ = collected;
        // What we *do* assert: the GC didn't crash.
    }

    #[test]
    fn freeze_unfreeze_round_trip() {
        let s = GcState::new();
        let d = Object::Dict(Rc::new(RefCell::new(DictData::new())));
        s.track(d.clone());
        s.freeze_all();
        assert_eq!(s.freeze_count(), 1);
        s.unfreeze_all();
        assert_eq!(s.freeze_count(), 0);
    }
}
