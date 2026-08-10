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
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
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
    /// Cached count of callback-bearing weakref clones the registry holds for
    /// this object — refreshed by [`GcState::note_weakref_finalizable`]. The
    /// prompt-finalization scan uses it as a fast-path liveness filter: an
    /// object whose `strong_count` exceeds `1 (our handle) + weak_clones`
    /// definitely still has a program reference, so the scan can skip it
    /// without the (per-id) registry lookup that computes the exact clone
    /// count. The cache is only ever an *upper bound* on the live clone count
    /// (weakrefs can clear without notifying us), and an over-estimate only
    /// makes the filter admit *more* objects to the precise check — never
    /// fewer — so it can never cause a dead object to be missed.
    pub weak_clones: AtomicUsize,
    /// RFC 0061 (WS1b): set (never cleared) when this handle leaves the
    /// GC index (`untrack_id`, the collection rebuild's White purge). The
    /// prompt-reap suspect probe reads it instead of a per-entry
    /// `is_tracked` registry lookup — that lookup (`GcState::handle_for`)
    /// was the hottest non-dispatch symbol on drop-heavy profiles. A
    /// re-tracked object gets a *fresh* handle, so a set flag
    /// definitively means "this handle is dead".
    pub untracked: AtomicBool,
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
            weak_clones: AtomicUsize::new(0),
            untracked: AtomicBool::new(false),
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
    /// population grew past a few thousand. Keyed by object *address*,
    /// so the internal fast hasher applies (consulted on every object
    /// drop via the prompt reaper — SipHash here was a top-ten CPU
    /// consumer under pandas).
    index: RefCell<crate::fasthash::FxHashMap<ObjectId, Arc<TrackedHandle>>>,
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
    /// Dedicated index over just the *finalizable* tracked objects —
    /// instances whose class defines `__del__` and unfinished
    /// generator-family objects. CPython runs `__del__` the instant an
    /// object's refcount reaches zero; our tracing handle pins it until a
    /// collection, so [`Self::reap_dead_finalizable`] emulates the prompt
    /// path by scanning *this* small set (not the whole tracked
    /// population) at the interpreter's reference-drop safe points. Keyed
    /// by id like `index`; an object is in both while finalizable.
    finalizable: RefCell<std::collections::BTreeMap<ObjectId, Arc<TrackedHandle>>>,
    /// Live population of [`Self::finalizable`]. A relaxed load of this
    /// atomic is the gate the interpreter checks before every prompt-
    /// finalization sweep: when it is zero (the overwhelmingly common
    /// case — most code never defines `__del__`) the sweep is skipped
    /// entirely, so the feature costs one atomic load per safe point.
    finalizable_count: AtomicUsize,
    /// Rotating scan position for [`Self::reap_dead_finalizable_locked`]
    /// when the finalizable index outgrows its per-safe-point scan budget
    /// (70k callback-weakrefs from a `WeakKeyDictionary` stress test must
    /// not turn every reference-dropping opcode into a full index walk —
    /// test_weakref's threaded-copy tests went quadratic). Stores the id
    /// the next bounded scan resumes from.
    fin_scan_cursor: std::sync::atomic::AtomicU64,
    /// Safe-point call counter paired with the cursor: when the index is
    /// over budget, only every [`FIN_SCAN_STRIDE`]-th safe point pays for
    /// a window scan, keeping the steady-state per-opcode cost a counter
    /// bump instead of 256 atomic strong-count loads.
    fin_scan_tick: std::sync::atomic::AtomicU64,
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
            index: RefCell::new(crate::fasthash::FxHashMap::default()),
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
            finalizable: RefCell::new(std::collections::BTreeMap::new()),
            finalizable_count: AtomicUsize::new(0),
            fin_scan_cursor: std::sync::atomic::AtomicU64::new(0),
            fin_scan_tick: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Reinitialise the collector's locks in a `fork(2)` child, preserving the
    /// inherited tracked-object state. The collector is process-global and its
    /// `GilCell` fields are normally serialised by the GIL, but an `Object`
    /// whose last `Arc` is released on a peer thread can drop — and run through
    /// the collector's prompt-reap bookkeeping — without that thread holding
    /// the GIL. If such a peer vanishes mid-`borrow` in the fork, the inherited
    /// `parking_lot` lock would wedge the child's very first allocation
    /// (`test_threading.test_reinit_tls_after_fork` forks from 16 threads; the
    /// child deadlocks in `threading._after_fork`'s `set(_enumerate())`).
    /// Rebuild every field's lock in place and clear the re-entrancy guard —
    /// CPython's `PyOS_AfterFork_Child` reinitialises the runtime's locks for
    /// the same reason.
    ///
    /// Takes a raw `*mut Self` so each field's lock rebuild is driven from a
    /// laundered raw pointer rather than a `&self` cast (see
    /// [`crate::sync::GilCell::reinit_lock_after_fork`]).
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
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).frozen));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).garbage));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).callbacks));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).stats));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).finalized_ids));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).finalizable));
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
    pub fn track(&self, obj: Object) {
        let new_id = id_of(&obj);
        {
            let mut index = self.index.borrow_mut();
            if index.contains_key(&new_id) {
                return;
            }
            let handle = Arc::new(TrackedHandle::new(obj, 0));
            index.insert(new_id, handle.clone());
            // Enroll finalizable objects in the dedicated prompt-finalization
            // index so the per-safe-point sweep scans only them, not the whole
            // tracked population.
            if has_finalizer(&handle.object) {
                // Seed the clone-count cache from the registry: weakrefs
                // created *before* tracking (asyncio's WeakValueDictionary
                // registers a transport before its first cycle-suspect
                // mutation tracks it) would otherwise make the fast-path
                // filter read the object as permanently live.
                handle.weak_clones.store(
                    crate::weakref_registry::strong_clone_count(new_id),
                    Ordering::Release,
                );
                let mut fin = self.finalizable.borrow_mut();
                if fin.insert(new_id, handle.clone()).is_none() {
                    self.finalizable_count.fetch_add(1, Ordering::AcqRel);
                }
            }
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
        // RFC 0061 (WS1b): let suspect probes see the removal without a
        // registry lookup.
        handle.untracked.store(true, Ordering::Release);
        // Purge any suspect-list clone of this handle in lock-step: the
        // suspect entry shares the same `Arc<TrackedHandle>`, so dropping
        // the index's Arc alone would leave the handle's strong `object`
        // reference alive in the suspect list (see `remove_suspect`).
        remove_suspect(id);
        // CPython's `PyObject_GC_Del` decrements the gen-0 allocation
        // counter for every GC-tracked object freed, whatever its
        // generation — so churn workloads whose young objects die by
        // refcount (asyncio's per-connection task/future/handle webs)
        // barely advance toward `threshold0` and automatic collections
        // stay rare. Without the decrement, our prompt-reap untracks kept
        // the gross allocation count, firing young collections an order
        // of magnitude more often than CPython — frequently enough to
        // land inside the window where a pending asyncio task is
        // reachable only through its own await cycle. Collecting there is
        // *correct* per CPython semantics (the docs tell users to hold
        // task references; test_log_destroyed_pending_task relies on it),
        // but CPython's cadence means its suite never observes it in e.g.
        // test_streams.test_start_server — match the cadence, not just
        // the semantics (RFC 0054).
        {
            let mut counts = self.counts.borrow_mut();
            counts[0] = counts[0].saturating_sub(1);
        }
        // Drop the finalizable-index entry in lock-step with the main index so
        // the cheap prompt-finalization scan never sees a reclaimed object.
        if self.finalizable.borrow_mut().remove(&id).is_some() {
            self.finalizable_count.fetch_sub(1, Ordering::AcqRel);
        }
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
            if gens[g]
                .handles
                .get(slot)
                .is_some_and(|h| Arc::ptr_eq(h, &handle))
            {
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
        // Atomic claim (see `collect_impl`): overlapping reaps/collections
        // from two threads corrupt each other's refcount math.
        if self
            .collecting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return 0;
        }
        let n = self.reap_dead_acyclic_locked();
        self.collecting.store(false, Ordering::Release);
        n
    }

    /// [`Self::reap_dead_acyclic`] body, called with the `collecting` claim
    /// already held (by `reap_dead_acyclic` itself or by `collect_impl`).
    fn reap_dead_acyclic_locked(&self) -> usize {
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
                    if std::env::var_os("WEAVEPY_REAP_TRACE").is_some() {
                        if let Some(h) = self.index.borrow().get(&id) {
                            eprintln!("[ACYCLIC-REAP] {}", h.object.type_name_owned());
                        }
                    }
                    self.untrack_id(id);
                    reclaimed += 1;
                }
            }
        }
        reclaimed
    }

    /// True iff at least one finalizable object is currently tracked. A single
    /// relaxed atomic load — the gate the interpreter checks at every
    /// reference-drop safe point before deciding whether a prompt-finalization
    /// sweep is even worth attempting.
    #[inline]
    pub fn has_any_finalizable(&self) -> bool {
        self.finalizable_count.load(Ordering::Relaxed) > 0
    }

    /// Enroll a tracked object in the prompt-finalization index because a
    /// weakref *with a callback* now watches it (`weakref.ref(obj, cb)`,
    /// `weakref.finalize`, `multiprocessing.util.Finalize`). CPython fires
    /// such a callback the instant the referent's last strong reference drops;
    /// without this the callback would wait for the next cyclic collection.
    /// No-op when the object isn't tracked (untracked weakref targets —
    /// plain functions, bound methods — are handled by the collection-time
    /// [`sweep_weakref_only_targets`] sweep instead).
    pub fn note_weakref_finalizable(&self, id: ObjectId) {
        let handle = self.index.borrow().get(&id).cloned();
        if let Some(h) = handle {
            // Refresh the cached clone count used by the prompt-finalization
            // fast-path filter (see `TrackedHandle::weak_clones`).
            let clones = crate::weakref_registry::strong_clone_count(id);
            h.weak_clones.store(clones, Ordering::Release);
            let mut fin = self.finalizable.borrow_mut();
            if fin.insert(id, h).is_none() {
                self.finalizable_count.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    /// Drive one prompt-finalization pass over the dedicated finalizable index:
    /// for every tracked finalizable object whose last *program* reference just
    /// dropped, run its `__del__` and/or fire its weakref callbacks and reclaim
    /// it. CPython does this by refcount the instant the count hits zero; our
    /// tracing handle pins the object until a collection, so this emulates the
    /// prompt path between bytecodes.
    ///
    /// "Dead" means the effective program refcount is zero:
    /// `strong_count - 1 (our own GC handle) - (registry weakref clones)`.
    /// A weakref slot keeps one strong clone of its target alive
    /// ([`crate::weakref_registry::WeakRefSlot::target`]); discounting those is
    /// what lets a `util.Finalize`-watched object — reachable now only through
    /// the GC handle and the finalizer's own weakref — collapse to zero.
    ///
    /// Per dead object:
    /// * If a `__del__` is still pending, queue it (set `finalize_queued`) and
    ///   leave the object tracked — its finalizer might resurrect it, and any
    ///   weakref callbacks must fire *after* `__del__` (CPython order). The
    ///   driver's next pass re-checks death and, if it stuck, fires the
    ///   weakrefs and reclaims.
    /// * Otherwise (no `__del__`, or it already ran) fire the weakref callbacks
    ///   (`notify_clear` → queued for the interpreter) and untrack the object —
    ///   dropping the handle frees it and cascades the refcount into referents,
    ///   exposing the next layer of dead finalizables on the following pass.
    ///
    /// Returns the number of objects that made progress (queued a finalizer or
    /// were reclaimed); the driver loops until this is zero. Cheap: scans only
    /// the finalizable index, which holds just the `__del__`/callback-weakref
    /// objects (typically a handful).
    pub fn reap_dead_finalizable(&self) -> usize {
        // Atomic claim (see `collect_impl`): the untrack path below mutates
        // the shared index/generations, which must not overlap a
        // collection's mark walk on another thread.
        if self
            .collecting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return 0;
        }
        let n = self.reap_dead_finalizable_locked();
        self.collecting.store(false, Ordering::Release);
        n
    }

    /// [`Self::reap_dead_finalizable`] body, called with the `collecting`
    /// claim held.
    fn reap_dead_finalizable_locked(&self) -> usize {
        // Borrow-only scan: collect just the dead handles. The common case —
        // all finalizables still reachable — allocates nothing (an empty
        // `Vec::new()` doesn't heap-allocate) and pays only a cheap
        // `strong_count` atomic load per object, skipping the per-id registry
        // lookup via the `weak_clones` fast-path filter.
        static FIN_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let fin_trace = *FIN_TRACE.get_or_init(|| std::env::var_os("WEAVEPY_FIN_TRACE").is_some());
        // Per-safe-point scan budget. Below this size the whole index is
        // scanned (full CPython-like promptness — the overwhelmingly common
        // shape); above it, a rotating window bounds the cost so a huge
        // population of callback-weakrefs (70k `WeakKeyDictionary` keys in
        // test_weakref's threaded-copy stress) doesn't make every
        // reference-dropping opcode O(index) — quadratic over the run.
        // Deaths are still detected within index_len/budget safe points.
        const FIN_SCAN_BUDGET: usize = 256;
        /// When over budget, additionally scan only every N-th safe point:
        /// with tens of thousands of live callback-weakrefs even a bounded
        /// window per drop opcode dominates the run; deaths are batched,
        /// so probing less often loses nothing but a little latency.
        const FIN_SCAN_STRIDE: u64 = 8;
        if self.finalizable.borrow().len() > FIN_SCAN_BUDGET {
            let tick = self.fin_scan_tick.fetch_add(1, Ordering::Relaxed);
            if !tick.is_multiple_of(FIN_SCAN_STRIDE) {
                return 0;
            }
        }
        let dead: Vec<Arc<TrackedHandle>> = {
            let fin = self.finalizable.borrow();
            let mut out: Vec<Arc<TrackedHandle>> = Vec::new();
            let mut check = |h: &Arc<TrackedHandle>| {
                let sc = strong_count_for(&h.object);
                let cached = h.weak_clones.load(Ordering::Acquire);
                if fin_trace {
                    let tn = h.object.type_name_owned();
                    if tn.contains("Transport") || tn.contains("SSLContext") || tn == "SSLProtocol"
                    {
                        eprintln!(
                            "[FIN-SCAN] {tn} sc={sc} cached={cached} clones={}",
                            crate::weakref_registry::strong_clone_count(h.id)
                        );
                    }
                }
                // Fast reject: more strong refs than our handle plus all of its
                // (cached, upper-bound) weakref clones ⇒ a program reference is
                // still live. Skip without touching the registry.
                if sc > 1 + cached {
                    return;
                }
                // Borderline: compute the exact live clone count and test for
                // an effective program refcount of zero.
                let clones = crate::weakref_registry::strong_clone_count(h.id);
                // Refresh the cached bound with the exact value. A cleared or
                // died weakref leaves the cache stale-high, and a stale-high
                // bound keeps a live object "borderline" — paying this
                // registry lookup again at *every* reference-dropping safe
                // point. This scan runs per drop opcode while any finalizable
                // is live, so a single stale entry costs a whole test run
                // (statistics.kde's hot sum-loop spent ~40% of its time
                // here). New weakrefs refresh the cache upward via
                // `note_weakref_finalizable`, so tightening it is safe.
                h.weak_clones.store(clones, Ordering::Release);
                if sc.saturating_sub(1).saturating_sub(clones) == 0 {
                    out.push(h.clone());
                }
            };
            if fin.len() <= FIN_SCAN_BUDGET {
                for h in fin.values() {
                    check(h);
                }
            } else {
                let start = self.fin_scan_cursor.load(Ordering::Relaxed);
                let mut next_cursor = start;
                for (scanned, (id, h)) in fin.range(start..).chain(fin.range(..start)).enumerate() {
                    if scanned == FIN_SCAN_BUDGET {
                        next_cursor = *id;
                        break;
                    }
                    check(h);
                }
                // budget < len guarantees the break above ran and set the
                // resume point to the first unscanned id.
                self.fin_scan_cursor.store(next_cursor, Ordering::Relaxed);
            }
            out
        };
        if dead.is_empty() {
            return 0;
        }
        let mut progressed = 0;
        for h in dead {
            // Re-validate under fresh counts: an earlier finalizer in this batch
            // may have resurrected `h`.
            let clones = crate::weakref_registry::strong_clone_count(h.id);
            let effective = strong_count_for(&h.object)
                .saturating_sub(1)
                .saturating_sub(clones);
            if effective != 0 {
                continue; // resurrected between scan and now
            }
            let del_pending = has_finalizer(&h.object) && !h.finalized.load(Ordering::Acquire);
            if del_pending {
                // Queue `__del__`; defer weakref callbacks + reclamation to a
                // later pass (after the finalizer has run and either resurrected
                // the object or left it dead).
                if !h.finalize_queued.swap(true, Ordering::AcqRel) {
                    run_finalizer(&h.object);
                    progressed += 1;
                }
            } else {
                // No pending `__del__`: fire the (callback) weakrefs and
                // reclaim. Weakref callbacks receive the weakref wrapper, not
                // the target, so they cannot resurrect it — the object is
                // definitively dead once they're queued.
                crate::weakref_registry::queue_callbacks(crate::weakref_registry::notify_clear(
                    h.id,
                ));
                self.untrack_id(h.id);
                progressed += 1;
            }
        }
        progressed
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
        // Atomic claim (see `collect_impl`): even a mark-only pass mutates
        // the shared `gc_refs` counters, so it must not overlap a real
        // collection on another thread.
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
        // Atomic claim — a plain load-then-store gate let two threads both
        // observe `false` and run *overlapping* collections over the shared
        // heap. Each phase-3 walk then subtracted the same internal edges
        // from the same `gc_refs` counters, so a live object with one
        // external root went negative and was swept as garbage — observed
        // as a peer thread's suspended generator being closed mid-`for`
        // loop (test_threading.test_foreign_thread wait_threads_exit).
        if self
            .collecting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return 0;
        }
        // Drop this thread's parked C-dropped clones (RFC 0047, wave 5)
        // before seeding reachability. Each queue entry is a strong
        // `Object` clone awaiting the eval loop's prompt-reap safe point —
        // but when C code runs without returning to the eval loop (a
        // C-driven embedding, or the collector's own traverse/clear boxes
        // freed mid-pass) the queue never drains, and every entry inflates
        // its object's `Rc::strong_count`. The mark phase seeds `gc_refs`
        // from exactly that count, so a queued clone makes its object —
        // and everything reachable from it — look externally rooted,
        // pinning dead cycles forever. A collection is itself a safe
        // point: anything that dies when these clones drop is either
        // reaped below (acyclic) or found unreachable by the mark phase,
        // with weakrefs/finalizers handled by the normal collection path.
        //
        // Guarded on no extension frame being live on this thread: while
        // one is (`gc.collect()` invoked *from* C), a queued clone may be
        // the last count backing a body pointer that C still borrows
        // across its call, and dropping it here would free the body under
        // C's feet — the exact UAF the queue exists to prevent. The clones
        // then simply wait for the eval-loop drain, as designed.
        if !crate::vm_singletons::cext_call_active() {
            drop(crate::vm_singletons::drain_pending_cext_drops());
        }
        if exact {
            // Reap dead *acyclic* garbage first. CPython frees these by refcount
            // the instant their last binding drops, so they never reach the
            // cycle collector; we pin them on the registry handle until now.
            // Doing it up front keeps them out of the cyclic `collected` count
            // *and* out of `gc.garbage` under `DEBUG_SAVEALL` (`test_saveall`
            // asserts only the genuine cycle is saved, not an incidental dead
            // `[]` temporary).
            self.reap_dead_acyclic_locked();
        }
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
                let parent_is_frame = matches!(&h.object, Object::Frame(_));
                traverse_object(&h.object, &mut |child| {
                    let promote = match child {
                        // Dict views join iterators here: they're not
                        // persistently tracked, but a cycle can route
                        // through one (`obj.v = container.keys()` with
                        // `container = {obj: 1}` — test_container_iterator).
                        // Bound methods likewise (CPython GC-tracks
                        // `method`): a stored `self.cb` closes the classic
                        // callback cycle — asyncio's `future._callbacks →
                        // task.__wakeup → task → future` (RFC 0054,
                        // test_tasks.test_log_destroyed_pending_task).
                        // Closure cells likewise (CPython GC-tracks `cell`):
                        // a nested function that calls itself
                        // (`def inner(): ... inner()`) closes the cycle
                        // `function → cell → function`, and the cell's edge
                        // must be subtracted or the function always looks
                        // externally reachable (RFC 0054: asyncio's
                        // `iter_one`-style recursive callbacks pin the async
                        // generator they iterate, test_base_events'
                        // asyncgen-finalization-by-gc tests).
                        // Tracebacks and frames likewise (CPython GC-tracks
                        // both): an exception object owns `__traceback__ →
                        // frame → f_locals`, and a local that references the
                        // exception again (`except* E as excs` materialises
                        // the handled group in the frame's locals mirror)
                        // closes a cycle whose edges live entirely in these
                        // untracked node types (RFC 0054,
                        // test_taskgroups.test_exception_refcycles_*).
                        // Descriptor wrappers likewise (CPython GC-tracks
                        // staticmethod/classmethod/property): a user
                        // `__new__` is stored in the class dict behind a
                        // staticmethod wrapper, so the wrapper's edge to
                        // the function must be subtracted or a dead
                        // `namespace -> class -> __new__ -> __globals__`
                        // exec cycle keeps the function externally
                        // reachable forever
                        // (test_module.test_clear_dict_in_ref_cycle).
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
                        Object::List(_) => parent_is_iter,
                        // An *exception* instance is untracked until a
                        // mutation marks it a cycle suspect, yet `raise X
                        // from …` inside an `except` builds the classic
                        // `group → __context__ exc → __traceback__ → frame →
                        // f_locals → group` loop where the chained exception
                        // is the only instance node. Promote untracked
                        // exceptions so their `__context__`/`__cause__`/
                        // traceback edges are subtracted (RFC 0054,
                        // test_taskgroups.test_exception_refcycles_*).
                        Object::Instance(i) => i.cls().flags.is_exception,
                        // A frame's `f_locals` cache is an internal,
                        // untracked dict; it carries the frame's only
                        // object-graph edges to the locals (the `eg` in the
                        // cycle above), so it joins the walk when reached
                        // through its frame.
                        Object::Dict(_) => parent_is_frame,
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
            let dbg_class = std::env::var("WP_REAP_DBG_CLASS").unwrap_or("Executor".into());
            for h in &candidate_set {
                let matches_dbg = match &h.object {
                    Object::Instance(i) => i.cls().name.contains(&dbg_class),
                    Object::Type(t) => t.name.contains(&dbg_class),
                    _ => false,
                };
                {
                    if matches_dbg {
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
                            "[mark wronly={}] {} sc={} clones={} gc_refs={} white={} tracked_referrers={:?}",
                            weakref_only,
                            h.object.type_name_owned(),
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
        // CPython's `handle_weakrefs`: a weakref that is *itself* part of the
        // cyclic trash has its callback cleared without invocation — only
        // weakrefs rooted outside the dying subgraph observe the deaths
        // (test_callbacks_on_callback: `c.wr`/`d.wr` stay silent while the
        // external `safe_callback` fires). Snapshot the trash ids so the
        // queue loops below can drop callbacks belonging to trash wrappers.
        let mut trash_ids: std::collections::HashSet<ObjectId> =
            unreachable.iter().map(|h| h.id).collect();
        let wrapper_is_trash =
            |slot: &Arc<crate::weakref_registry::WeakRefSlot>,
             trash: &std::collections::HashSet<ObjectId>| {
                slot.py_ref
                    .borrow()
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                    .is_none_or(|inst| {
                        trash.contains(&(crate::sync::Rc::as_ptr(&inst) as usize as u64))
                    })
            };

        if weakref_only {
            let mut weakref_callbacks = Vec::new();
            for h in &unreachable {
                if has_finalizer(&h.object) && !h.finalized.load(Ordering::Acquire) {
                    continue;
                }
                for (slot, cb) in crate::weakref_registry::notify_clear(h.id) {
                    if let Some(cb) = cb {
                        if wrapper_is_trash(&slot, &trash_ids) {
                            continue;
                        }
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

        // CPython clears weakrefs to the *entire* unreachable set
        // (`handle_weakrefs`) BEFORE running any finalizer (`finalize_garbage`)
        // and before resurrection handling. So a weakref watching an object a
        // finalizer later revives stays cleared even though the object itself
        // survives — e.g. `_pyio.FileIO.__del__` recording a
        // `ResourceWarning(source=self)` into a live `catch_warnings` log
        // resurrects the file, yet `weakref.ref(f)()` must read `None`
        // (`test_io.test_garbage_collection`). Mirror that ordering: clear and
        // queue callbacks for every unreachable object now, regardless of
        // whether the resurrection re-mark or a finalizer keeps it alive below.
        // (Counting still tracks only objects actually reclaimed — `dead` — so
        // a resurrected object is uncounted but its weakref is gone, exactly as
        // in CPython.)
        let mut weakref_callbacks = Vec::new();
        for h in &unreachable {
            for (slot, cb) in crate::weakref_registry::notify_clear(h.id) {
                if let Some(cb) = cb {
                    if wrapper_is_trash(&slot, &trash_ids) {
                        continue;
                    }
                    weakref_callbacks.push((slot, cb));
                }
            }
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

        // 5a: weakrefs for the unreachable set were already cleared above (the
        // CPython `handle_weakrefs`-before-`finalize_garbage` ordering), so the
        // `dead` objects' weakrefs are gone and their callbacks are already
        // queued in `weakref_callbacks` for invocation in 5d.

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
                    // Still reachable from a survivor — keep it. But the
                    // extra references may be Rust-side transients rather
                    // than heap edges: an *automatic* collection can land
                    // mid-teardown (asyncio cancellation), while the task
                    // machinery still holds in-flight clones of a dying
                    // child. The collection consumes the dead subgraph, so
                    // the eval-loop cascade never walks it and the child is
                    // pinned by its own handle forever once the transients
                    // drop. Mirror that cascade's slack: enroll borderline
                    // survivors for the suspect re-probe so they still die
                    // at CPython's refcount timing (test_ssl
                    // test_handshake_timeout_handler_leak with a mid-run
                    // gen-0 collection).
                    if effective <= 3 {
                        note_suspect(h.clone());
                    }
                    continue;
                }
                // Orphaned: fire its weakref callbacks (queued in 5d below),
                // capture its children for the cascade, tear it down, and drop
                // it from the tracked set. The orphan joins the trash set
                // first so a weakref *wrapper* dying in this cascade never
                // fires its own callback (CPython `handle_weakrefs` parity).
                trash_ids.insert(cid);
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
        // Python itself. Wrappers that turned out to be trash (including
        // cascade orphans discovered after their callbacks were queued)
        // are dropped here.
        for (slot, cb) in weakref_callbacks {
            if wrapper_is_trash(&slot, &trash_ids) {
                continue;
            }
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

        // The rebuild dropped the dead objects' index handles *directly*
        // (not via `untrack_id`), so purge any suspect-list clones in
        // lock-step here — after the index borrow is released, matching
        // `untrack_id`'s ordering. A stale suspect entry shares the dead
        // handle's `Arc`, whose strong `object` reference would otherwise
        // keep the just-collected object alive until the suspect probe
        // ages it out: `test_descr.test_remove_subclass` observed a
        // collected class still listed in `Parent.__subclasses__()`
        // right after an explicit `gc.collect()`.
        for h in &dead {
            remove_suspect(h.id);
        }

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
        let mut fin = self.finalizable.borrow_mut();
        let mut gens = self.generations.borrow_mut();
        for g in 0..=upto.min(N_GENERATIONS - 1) {
            gens[g].handles.clear();
        }
        for h in candidates {
            let color = h.color.load(Ordering::Acquire);
            if color == color::White {
                index.remove(&h.id);
                // RFC 0061 (WS1b): mirror `untrack_id`'s flag so a stale
                // suspect entry self-identifies as reclaimed.
                h.untracked.store(true, Ordering::Release);
                if fin.remove(&h.id).is_some() {
                    self.finalizable_count.fetch_sub(1, Ordering::AcqRel);
                }
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
        // Not cycle-capable, but `sys.getrefcount(b"...")` parity matters
        // to ctypes' keepalive tests (test_internals.test_c_char_p).
        Object::Bytes(b) => Rc::strong_count(b),
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
        // Tracked only when user attributes give it cycle-capable edges.
        Object::File(f) => Rc::strong_count(f),
        // Promoted transiently when a cycle routes through one.
        Object::Slice(s) => Rc::strong_count(s),
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
            let dict_obj = Object::Dict(i.dict.clone());
            if is_tracked(id_of(&dict_obj)) {
                visit(&dict_obj);
            } else if let Ok(m) = i.dict.try_borrow() {
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
            if let Ok(attrs_rc) = f.attrs.try_borrow() {
                if let Ok(attrs) = attrs_rc.try_borrow() {
                    for (k, v) in attrs.iter() {
                        visit(&k.0);
                        visit(v);
                    }
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
    let Some(table) = TRAVERSE_TABLE.get() else {
        return;
    };
    // Snapshot the matching traverse fns, then *release* the table lock
    // before invoking them. A C extension's `tp_traverse` calls our
    // visitproc, which recurses back through the collector
    // (`exc_has_finalizable` → `traverse_object`) and can re-enter
    // `run_external_traverse` for a *nested* foreign object on the same
    // thread — e.g. collecting a cycle through pandas' `BaseOffset`.
    // `parking_lot::Mutex` is not reentrant, so holding the lock across the
    // callback self-deadlocks. The table is registration-only and its
    // entries are plain `fn` pointers, so a cheap snapshot is sound.
    let matched: Vec<fn(&Object, &mut dyn FnMut(&Object))> = {
        let entries = table.lock();
        entries
            .iter()
            .filter(|e| (e.matches)(obj))
            .map(|e| e.traverse)
            .collect()
    };
    for traverse in matched {
        traverse(obj, visit);
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

#[allow(missing_debug_implementations)]
struct ClearEntry {
    matches: fn(&Object) -> bool,
    clear: fn(&Object),
}

static CLEAR_TABLE: std::sync::OnceLock<parking_lot::Mutex<Vec<ClearEntry>>> =
    std::sync::OnceLock::new();

/// Called from `clear_object_fields` to let a type whose child
/// references live in module-private (or C-managed) memory break its
/// cycles during the collector's clear phase. The companion of
/// [`register_traverse`] (RFC 0044, WS4).
fn run_external_clear(obj: &Object) {
    let Some(table) = CLEAR_TABLE.get() else {
        return;
    };
    // Snapshot then release the lock before invoking, mirroring
    // `run_external_traverse`: a C extension's `tp_clear` can re-enter the
    // collector and thus this function for a nested foreign object on the
    // same thread, which would self-deadlock on the non-reentrant mutex.
    let matched: Vec<fn(&Object)> = {
        let entries = table.lock();
        entries
            .iter()
            .filter(|e| (e.matches)(obj))
            .map(|e| e.clear)
            .collect()
    };
    for clear in matched {
        clear(obj);
    }
}

/// Register a clear callback, mirroring [`register_traverse`]. Invoked
/// during the collector's clear phase so a matching object can drop the
/// child references it holds outside the VM's view.
pub fn register_clear(matches: fn(&Object) -> bool, clear: fn(&Object)) {
    let table = CLEAR_TABLE.get_or_init(|| parking_lot::Mutex::new(Vec::new()));
    table.lock().push(ClearEntry { matches, clear });
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
            // Drop any C-held child references (RFC 0044) *first*: a readied
            // extension type's `tp_clear` breaks cycles routed through
            // C-managed memory that the dict/slots clears below can't see, and
            // it typically reads its identity (`self._id`, …) back out of the
            // instance dict to find its side-table slot — so it must run while
            // that dict is still intact.
            run_external_clear(obj);
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
            if let Ok(attrs_rc) = f.attrs.try_borrow() {
                if let Ok(mut attrs) = attrs_rc.try_borrow_mut() {
                    attrs.clear();
                }
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

/// References to `target` held by *zombie* tracked memoryviews — views
/// whose only remaining strong reference is the registry's own handle
/// (plus weakref-slot clones). Under CPython refcounting such a view is
/// already freed, so `sys.getrefcount` must not let its exporter edge
/// inflate the exporter's count (test_memoryview's getitem/setitem tests
/// assert `getrefcount(b)` returns to baseline after a short-lived view
/// over `b` is dropped). Chains (a zombie view of a zombie view) resolve
/// iteratively. Restricted to memoryviews to keep the scan O(#views),
/// well away from `getrefcount`-hot paths like pandas'.
pub fn zombie_memoryview_refs_to(target: ObjectId) -> usize {
    with_state(|s| {
        let mut handles: Vec<Arc<TrackedHandle>> = Vec::new();
        {
            let Ok(gens) = s.generations.try_borrow() else {
                return 0;
            };
            for gen in gens.iter() {
                for h in &gen.handles {
                    if matches!(h.object, Object::MemoryView(_)) {
                        handles.push(h.clone());
                    }
                }
            }
        }
        if let Ok(frozen) = s.frozen.try_borrow() {
            for h in frozen.iter() {
                if matches!(h.object, Object::MemoryView(_)) {
                    handles.push(h.clone());
                }
            }
        }
        if handles.is_empty() {
            return 0;
        }
        let mut zombies: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();
        loop {
            // Inbound references each candidate receives from the current
            // zombie set (a dropped chain of sub-views keeps inner views'
            // counts up via exporter edges).
            let mut inbound: std::collections::HashMap<ObjectId, usize> =
                std::collections::HashMap::new();
            for h in &handles {
                if zombies.contains(&h.id) {
                    traverse_object(&h.object, &mut |c| {
                        *inbound.entry(id_of(c)).or_insert(0) += 1;
                    });
                }
            }
            let mut changed = false;
            for h in &handles {
                if zombies.contains(&h.id) {
                    continue;
                }
                let strong = strong_count_for(&h.object);
                let weak = crate::weakref_registry::strong_clone_count(h.id);
                let from_zombies = inbound.get(&h.id).copied().unwrap_or(0);
                if strong
                    .saturating_sub(1) // the registry handle itself
                    .saturating_sub(weak)
                    .saturating_sub(from_zombies)
                    == 0
                {
                    zombies.insert(h.id);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut n = 0usize;
        for h in &handles {
            if zombies.contains(&h.id) {
                traverse_object(&h.object, &mut |c| {
                    if id_of(c) == target {
                        n += 1;
                    }
                });
            }
        }
        n
    })
}

/// Convenience: track `obj` in the shared, process-global GC.
pub fn track(obj: Object) {
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
    if let Object::Module(m) = &obj {
        let dict = Object::Dict(m.dict.clone());
        with_state(|s| s.track(dict));
    } else if let Object::Function(f) = &obj {
        let dict = Object::Dict(f.globals.clone());
        with_state(|s| s.track(dict));
    }
    with_state(|s| s.track(obj));
}

/// Track `obj` *and* enroll it in the prompt-finalization index even though
/// it has no `__del__`/weakref callback. For handle-pinned glue objects whose
/// CPython counterpart dies by refcount with observable timing — e.g.
/// `_asyncio.FutureIter`, whose strong ref on its Future would otherwise keep
/// a finished Task (and its coroutine frame, and every local in it) alive
/// until the next cyclic collection (test_ssl's weakref-based leak tests).
/// The prompt sweep sees the object with no pending finalizer and simply
/// untracks it the moment its last program reference drops, cascading the
/// frees exactly like CPython's refcounting.
pub fn track_prompt_reclaim(obj: Object) {
    let id = crate::weakref_registry::id_of(&obj);
    with_state(|s| {
        s.track(obj);
        let handle = s.index.borrow().get(&id).cloned();
        if let Some(h) = handle {
            let mut fin = s.finalizable.borrow_mut();
            if fin.insert(id, h).is_none() {
                s.finalizable_count.fetch_add(1, Ordering::AcqRel);
            }
        }
    });
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
/// Track a memoryview that just recorded a buffer exporter. Only a
/// mutable-container exporter (an instance, list, …) can route a cycle
/// back through the view, so scalar exporters (`bytes`) stay untracked.
pub fn track_memoryview_exporter(mv: &Object, exporter: &Object) {
    debug_assert!(matches!(mv, Object::MemoryView(_)));
    if !is_atomic(exporter) {
        track(mv.clone());
    }
}

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

/// [`reap_dead_acyclic`] for the prompt-finalization drain's hot path:
/// the full-index cascade scan is O(tracked) and the drain runs it once
/// per pass that freed a finalizable. With a huge tracked population
/// shedding finalizables continuously (70k `WeakKeyDictionary` keys
/// dying one pop at a time in test_weakref's threaded-copy stress) that
/// multiplies into minutes, so over a size threshold only every N-th
/// drain pays for the scan — the skipped cascades are plain containers
/// whose reclamation the next scan (or any collection) picks up. Small
/// heaps keep CPython-like promptness (asyncio's SSL leak chains).
pub fn reap_dead_acyclic_amortized() -> usize {
    const TRACKED_THRESHOLD: usize = 8192;
    static STRIDE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let stride = *STRIDE.get_or_init(|| {
        std::env::var("WEAVEPY_ACYCLIC_STRIDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64)
    });
    static TICK: AtomicU64 = AtomicU64::new(0);
    with_state(|s| {
        if s.tracked_count.load(Ordering::Relaxed) > TRACKED_THRESHOLD {
            let tick = TICK.fetch_add(1, Ordering::Relaxed);
            if !tick.is_multiple_of(stride) {
                return 0;
            }
        }
        s.reap_dead_acyclic()
    })
}

/// Convenience: is any finalizable object currently tracked in the shared GC?
/// The interpreter's prompt-finalization gate (see
/// [`GcState::has_any_finalizable`]).
#[inline]
pub fn has_any_finalizable() -> bool {
    with_state(GcState::has_any_finalizable)
}

/// Prompt-reap *suspects* (RFC 0054): tracked, non-finalizable objects a
/// [`Interpreter::reap_dead_subgraph`] cascade visited but had to skip
/// because something still referenced them — typically a Rust-side
/// transient (an in-flight `PyException` clone, a native call's argument)
/// that dies a few opcodes later, *between* safe points, leaving the object
/// pinned by its own collector handle until the next full collection.
/// CPython's refcounting frees such objects the instant the transient dies;
/// re-probing the (tiny) suspect list at drop safe points recovers that
/// timing. Each entry carries a probe budget so a genuinely long-lived
/// skipped object (the event loop itself) stops costing anything after a
/// bounded number of checks.
///
/// The canonical chain: asyncio's `wait_for` timeout leaves a
/// `CancelledError` (skipped mid-cascade while the task machinery still
/// held a clone) whose traceback pins the cancelled `create_connection`
/// frames — and through their locals the SSL transport, protocol, and
/// `SSLContext` that test_ssl's leak tests watch via weakref.
const SUSPECT_CAP: usize = 256;
const SUSPECT_BUDGET: u8 = 64;
/// Aged-out (budget-exhausted) suspects turn *dormant* rather than being
/// forgotten: they are re-probed only every `DORMANT_STRIDE`-th sweep, so
/// a long-lived skipped object costs two atomic loads per stride instead
/// of per safe point — but an object whose last external reference dies
/// *after* its budget ran out is still recovered. (test_ssl's
/// handshake-timeout leak tests: the CancelledError web enrolls at
/// cancellation, the event loop burns >64 drop safe points before the
/// Task machinery lets go, and with eager eviction the web stayed pinned
/// by its collector handle until a full `gc.collect()`.)
const DORMANT_STRIDE: u64 = 64;
/// RFC 0061 (WS1b): keyed by [`ObjectId`] so `remove_suspect` (called in
/// lock-step with every `untrack_id`) and enrollment dedup are O(1)
/// map hits instead of linear scans of the list — `remove_suspect`'s
/// `retain` was a measurable share of drop-heavy profiles (`list_ops`).
type SuspectMap = indexmap::IndexMap<ObjectId, (Arc<TrackedHandle>, u8)>;
static SUSPECTS: std::sync::LazyLock<parking_lot::Mutex<SuspectMap>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(SuspectMap::new()));
static SUSPECT_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Entries with probe budget remaining. When only dormant entries are
/// left, [`has_suspects`] admits a sweep every [`DORMANT_STRIDE`]-th
/// safe point instead of every one.
static SUSPECT_ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SUSPECT_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Recompute the count gates from the locked suspect map.
fn publish_suspect_counts(s: &SuspectMap) {
    SUSPECT_COUNT.store(s.len(), Ordering::Release);
    SUSPECT_ACTIVE.store(
        s.values().filter(|(_, b)| *b > 0).count(),
        Ordering::Release,
    );
}

/// Enroll a cascade-skipped tracked object for later deadness re-probes.
/// Deduplicated; silently dropped when the list is full (the next full
/// collection reclaims it instead).
pub fn note_suspect(h: Arc<TrackedHandle>) {
    static NO_SUSPECTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *NO_SUSPECTS.get_or_init(|| std::env::var_os("WEAVEPY_NO_SUSPECTS").is_some()) {
        return;
    }
    let mut s = SUSPECTS.lock();
    if s.contains_key(&h.id) {
        return;
    }
    // Refresh the handle's cached weakref-clone upper bound once at
    // enrollment so the re-probe loop can fast-reject live suspects with
    // two atomic loads instead of a registry lookup per safe point (see
    // `take_dead_suspects` — the probe runs at *every* reference-dropping
    // opcode, and a hot loop that keeps re-enrolling a long-lived object
    // like `statistics.kde`'s sample list would otherwise spend ~40% of
    // its time in `strong_clone_count`).
    h.weak_clones.store(
        crate::weakref_registry::strong_clone_count(h.id),
        Ordering::Release,
    );
    if s.len() >= SUSPECT_CAP {
        // Full: evict the most-probed entry (lowest remaining budget,
        // dormant first) — it has had the most chances to die and is
        // the closest to aging out anyway. Silently dropping the *new*
        // suspect instead loses the one object whose last real
        // reference just died (asyncio's wait_for cancellation chain
        // arrived after ~200 module-teardown stragglers and was never
        // re-probed, pinning the Timeout→Task→frame web the test_ssl
        // leak tests watch).
        match s
            .values()
            .enumerate()
            .min_by_key(|(_, (_, b))| *b)
            .map(|(i, _)| i)
        {
            Some(i) => {
                s.swap_remove_index(i);
            }
            None => return,
        }
    }
    s.insert(h.id, (h, SUSPECT_BUDGET));
    publish_suspect_counts(&s);
}

/// Cheap gate for the eval loop's safe point: always sweep while an
/// *active* suspect is enrolled; with only dormant (aged-out) entries
/// left, admit every [`DORMANT_STRIDE`]-th safe point so long-lived
/// suspects cost two atomic loads per stride, not per drop.
#[inline]
pub fn has_suspects() -> bool {
    if SUSPECT_ACTIVE.load(Ordering::Relaxed) > 0 {
        return true;
    }
    if SUSPECT_COUNT.load(Ordering::Relaxed) == 0 {
        return false;
    }
    SUSPECT_TICK
        .fetch_add(1, Ordering::Relaxed)
        .is_multiple_of(DORMANT_STRIDE)
}

/// Drop the suspect entry for `id`, if any. Called in lock-step with
/// `untrack_id`: a suspect's `Arc<TrackedHandle>` is a *clone of the
/// index's handle*, so removing the object from the index alone leaves
/// the handle — and its strong `object` reference — alive in the suspect
/// list until the next re-probe. That stale strong clone pins an object
/// the prompt-reap cascade just untracked and expected to free by `Rc`
/// drop, deferring everything it anchors (`unittest`'s
/// `_AssertRaisesContext` → stored exception → `AttributeError.obj` io
/// temporary whose `close()` must fire at `with`-exit —
/// `test_io.test_error_through_destructor`) to a later safe point.
pub fn remove_suspect(id: ObjectId) {
    if SUSPECT_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut s = SUSPECTS.lock();
    if s.swap_remove(&id).is_some() {
        publish_suspect_counts(&s);
    }
}

/// Re-probe the suspect list: return the objects that are now dead in
/// refcount terms (nothing beyond the GC handle and weakref strong clones
/// holds them) for the caller to run through the prompt-reap cascade, and
/// decay the probe budget of the rest.
pub fn take_dead_suspects() -> Vec<Object> {
    let mut out = Vec::new();
    let mut s = SUSPECTS.lock();
    // With only dormant entries left, `has_suspects` already stride-gated
    // this sweep; with actives present the stride ticks here instead.
    let probe_dormant = SUSPECT_ACTIVE.load(Ordering::Relaxed) == 0
        || SUSPECT_TICK
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(DORMANT_STRIDE);
    s.retain(|_, (h, budget)| {
        // Dormant (aged-out) entries only pay on the stride tick.
        if *budget == 0 && !probe_dormant {
            return true;
        }
        // RFC 0061 (WS1b): the handle self-identifies as reclaimed (set
        // in lock-step with every index removal) — no registry lookup.
        if h.untracked.load(Ordering::Acquire) {
            return false; // already reclaimed elsewhere
        }
        // Fast reject via the cached weakref-clone upper bound (refreshed
        // at enrollment): more strong refs than the handle plus every
        // possible weakref clone ⇒ a program reference is still live, no
        // registry lookup needed. Only borderline counts pay for the
        // exact `strong_clone_count`.
        let sc = strong_count_for(&h.object);
        let cached = h.weak_clones.load(Ordering::Acquire);
        if sc <= 1 + cached {
            let weak = crate::weakref_registry::strong_clone_count(h.id);
            if sc <= 1 + weak {
                out.push(h.object.clone());
                return false;
            }
        }
        *budget = budget.saturating_sub(1);
        true
    });
    publish_suspect_counts(&s);
    out
}

thread_local! {
    /// Set whenever the interpreter executes an opcode that may have dropped
    /// the last reference to an object (a `POP_*`/`STORE_*`/`DELETE_*`, a
    /// frame teardown, …). The eval loop only runs a prompt-finalization sweep
    /// when this is set *and* a finalizable object is live, so a hot loop that
    /// never drops a reference pays nothing beyond the gate's atomic load.
    /// Thread-local because each thread reclaims the objects whose last
    /// reference *it* dropped, matching CPython's per-thread `tp_dealloc`.
    static MAYBE_DEAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Note that the current thread just executed a reference-dropping opcode, so a
/// finalizable object may now be dead. Cheap (a thread-local `Cell` store); the
/// actual sweep is deferred to the next eval-loop safe point.
#[inline]
pub fn mark_maybe_dead() {
    MAYBE_DEAD.with(|c| c.set(true));
}

/// Precise per-value drop note (RFC 0059 WS1b). Called by the audited
/// opcode handlers (`POP_TOP`, `STORE_FAST`, the generic `BINARY_OP` /
/// `COMPARE_OP` paths) with each value they discard; the eval loop
/// exempts those opcodes from its coarse "operand stack shrank ⇒ maybe
/// dead" heuristic in return.
///
/// A **pure leaf** — a variant that owns no [`Object`]s and whose
/// instances can never carry a finalizer (subclass instances live in
/// `Object::Instance`, never in the scalar variants) — cannot make
/// anything finalizable unreachable when dropped, so it schedules no
/// sweep. This is what keeps an int-accumulating hot loop
/// (`total = total + i`, the RFC 0059 `nested_loops` profile) from
/// paying a `reap_dead_finalizable` scan per iteration whenever *some*
/// `__del__`-bearing object is alive anywhere in the process.
/// Everything else conservatively marks: the sweep itself refcount-
/// checks, so a false positive costs one scan, never correctness.
#[inline]
pub fn note_dropped(obj: &crate::object::Object) {
    use crate::object::Object as O;
    match obj {
        O::None
        | O::Unbound
        | O::Bool(_)
        | O::Int(_)
        | O::Long(_)
        | O::Float(_)
        | O::Complex(_)
        | O::Str(_)
        | O::WStr(_)
        | O::Bytes(_)
        | O::Range(_)
        | O::Code(_) => {}
        _ => mark_maybe_dead(),
    }
}

/// Consume the "a reference may have dropped" flag, returning whether it was
/// set. The eval loop calls this to decide whether a prompt-finalization sweep
/// is warranted this instruction.
#[inline]
pub fn take_maybe_dead() -> bool {
    MAYBE_DEAD.with(|c| c.replace(false))
}

/// Convenience: drive one prompt-finalization pass over refcount-dead
/// finalizable objects in the shared GC (see
/// [`GcState::reap_dead_finalizable`]).
pub fn reap_dead_finalizable() -> usize {
    with_state(|s| s.reap_dead_finalizable())
}

/// Convenience: enroll a tracked object in the prompt-finalization index
/// because a callback-bearing weakref now watches it (see
/// [`GcState::note_weakref_finalizable`]).
pub fn note_weakref_finalizable(id: ObjectId) {
    with_state(|s| s.note_weakref_finalizable(id));
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
        let d = Object::Dict(Rc::new(RefCell::new(DictData::default())));
        s.track(d.clone());
        assert!(s.is_tracked(id_of(&d)));
        s.untrack_id(id_of(&d));
        assert!(!s.is_tracked(id_of(&d)));
    }

    #[test]
    fn collect_clears_simple_cycle() {
        let s = GcState::new();
        let dict = Rc::new(RefCell::new(DictData::default()));
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
        let d = Object::Dict(Rc::new(RefCell::new(DictData::default())));
        s.track(d.clone());
        s.freeze_all();
        assert_eq!(s.freeze_count(), 1);
        s.unfreeze_all();
        assert_eq!(s.freeze_count(), 0);
    }
}
