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
//! interpreter model.
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

use crate::sync::RefCell;
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};

use crate::object::Object;
use crate::weak_object::WeakObject;
use crate::weakref_registry::{id_of, ObjectId};

/// A set of object ids (addresses), hashed with the address mixer.
type IdSet = std::collections::HashSet<ObjectId, BuildHasherDefault<ObjectIdHasher>>;

/// The registry's id index: a tracked object's slot by its payload
/// address, for the kinds that don't record their slot themselves.
type GcIndex = std::collections::HashMap<ObjectId, u32, BuildHasherDefault<ObjectIdHasher>>;

/// The standard CPython generation count (3) and default
/// thresholds (CPython 3.14's): gen 0 collects when 2000 net tracked allocations
/// have happened; gen 1 every 10 gen 0 collections; gen 2
/// every 10 gen 1 collections.
pub const N_GENERATIONS: usize = 3;
// Color and generation are bounded states; reference counts remain full-width.
const _: () = assert!(N_GENERATIONS > 0 && N_GENERATIONS <= u8::MAX as usize + 1);
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

/// No registry slot: the [`GcSlot`] of an object the collector doesn't
/// hold, and the end of the free list.
pub const NO_SLOT: u32 = u32::MAX;

/// The registry slot an object of a kind that records its own (see
/// [`Registry`]) carries: [`NO_SLOT`] while it isn't registered. A copy of
/// the object is another object, so a clone starts unregistered.
#[derive(Debug)]
pub struct GcSlot(crate::sync::Cell<u32>);

impl GcSlot {
    pub fn new() -> Self {
        Self(crate::sync::Cell::new(NO_SLOT))
    }

    #[inline]
    pub fn get(&self) -> u32 {
        self.0.get()
    }

    #[inline]
    fn set(&self, slot: u32) {
        self.0.set(slot);
    }

    /// Whether the object has a registry entry.
    #[inline]
    pub fn is_registered(&self) -> bool {
        self.get() != NO_SLOT
    }
}

impl Default for GcSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for GcSlot {
    fn clone(&self) -> Self {
        Self::new()
    }
}

/// [`Entry::gen`] of an entry `gc.freeze()` holds (it sits in the frozen
/// list).
const GEN_FROZEN: u8 = 0xfe;
/// [`Entry::gen`] of a free slot.
const GEN_FREE: u8 = 0xff;

/// [`Entry::flags`]: the object's `__del__` has run to completion
/// (CPython guarantees a finalizer runs at most once).
const F_FINALIZED: u8 = 1;
/// The object's `__del__` was queued by a collection and hasn't run yet.
/// While set, the object is excluded from the `collected` count: its
/// finalizer (drained after `gc.collect()` returns) may resurrect it, and
/// CPython only counts objects that are actually reclaimed.
const F_QUEUED: u8 = 2;
/// The object is a candidate of the collection in progress; the entry's
/// `gc_refs` holds its marking count.
const F_CAND: u8 = 4;
/// The collection in progress found the candidate reachable from outside
/// the candidates (its marking color is grey or black).
const F_REACHED: u8 = 8;

/// One registered object: a non-owning handle on it plus the bookkeeping
/// that outlives a collection. The collector never keeps an object alive.
/// An entry whose object died stays in its generation until its removal
/// (an instance removes its own from its `Drop`) or the next prune or
/// collection of that generation; its weak handle keeps the allocation
/// reserved until then, so the address the index keys it by can't be
/// reused by another object meanwhile.
struct Entry {
    /// The object, held weakly; `None` for a free slot.
    obj: Option<WeakObject>,
    /// Position in the owning list (`gens[gen]`, or `frozen`), or the next
    /// free slot for a free one.
    pos: u32,
    /// While [`F_CAND`] is set: the object's references from outside the
    /// candidates, as the collection counts them (CPython's `gc_refs`).
    gc_refs: std::cell::Cell<i32>,
    /// While [`F_CAND`] is set: the candidate's node number in the
    /// collection's edge lists.
    node: std::cell::Cell<u32>,
    /// Generation (0..N_GENERATIONS), [`GEN_FROZEN`] or [`GEN_FREE`].
    gen: u8,
    /// [`F_FINALIZED`], [`F_QUEUED`] and [`F_CAND`].
    flags: std::cell::Cell<u8>,
}

impl Entry {
    #[inline]
    fn has(&self, flag: u8) -> bool {
        self.flags.get() & flag != 0
    }

    #[inline]
    fn set(&self, flag: u8, on: bool) {
        let f = self.flags.get();
        self.flags.set(if on { f | flag } else { f & !flag });
    }
}

/// Whether the registry finds `w`'s object through the object's own
/// [`GcSlot`] (instances, classes, functions and the generator family)
/// rather than the id index.
#[inline]
fn slot_is_intrusive(w: &WeakObject) -> bool {
    matches!(
        w,
        WeakObject::Instance(_)
            | WeakObject::Type(_)
            | WeakObject::Function(_)
            | WeakObject::Generator(_)
            | WeakObject::Coroutine(_)
            | WeakObject::AsyncGenerator(_)
    )
}

/// The [`GcSlot`] of a kind that records its own registry slot.
#[inline]
fn intrusive_slot(obj: &Object) -> Option<&GcSlot> {
    match obj {
        Object::Instance(i) => Some(&i.gc_slot),
        Object::Type(t) => Some(&t.gc_slot),
        Object::Function(f) => Some(&f.gc_slot),
        Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g) => Some(&g.gc_slot),
        _ => None,
    }
}

/// The [`GcSlot`] of the object `w` names, of a kind that records its own.
///
/// # Safety
///
/// The object must be alive.
#[inline]
unsafe fn weak_intrusive_slot(w: &WeakObject) -> Option<&GcSlot> {
    // SAFETY: alive, per the caller; the handle keeps the allocation.
    unsafe {
        match w {
            WeakObject::Instance(w) => Some(&(*w.as_ptr()).gc_slot),
            WeakObject::Type(w) => Some(&(*w.as_ptr()).gc_slot),
            WeakObject::Function(w) => Some(&(*w.as_ptr()).gc_slot),
            WeakObject::Generator(w) | WeakObject::Coroutine(w) | WeakObject::AsyncGenerator(w) => {
                Some(&(*w.as_ptr()).gc_slot)
            }
            _ => None,
        }
    }
}

/// Forget the registry slot a live object records (its entry is going).
/// A dead one's field is gone with it.
#[inline]
fn clear_intrusive_slot(w: &WeakObject) {
    fn clear<T: 'static>(w: &crate::sync::Weak<T>, slot: impl Fn(&T) -> &GcSlot) {
        if w.strong_count() > 0 {
            // SAFETY: the object is alive (the handle keeps the
            // allocation, the count says the payload is live), and the
            // field is an atomic cell.
            slot(unsafe { &*w.as_ptr() }).set(NO_SLOT);
        }
    }
    match w {
        WeakObject::Instance(w) => clear(w, |i| &i.gc_slot),
        WeakObject::Type(w) => clear(w, |t| &t.gc_slot),
        WeakObject::Function(w) => clear(w, |f| &f.gc_slot),
        WeakObject::Generator(w) | WeakObject::Coroutine(w) | WeakObject::AsyncGenerator(w) => {
            clear(w, |g| &g.gc_slot);
        }
        _ => {}
    }
}

/// The registry of tracked objects: a slab of entries addressed by slot,
/// one list of slots per generation (and one for the frozen set), and an
/// id index for the kinds that don't record their own slot.
///
/// Every list keeps each member's position in its entry, so a removal is
/// a swap with the list's last member, and a collection walks a
/// generation without hashing. An instance records its slot itself
/// ([`crate::types::PyInstance::gc_slot`]), so registering one, finding
/// it from an edge, and its death's removal never touch the index.
struct Registry {
    slab: Vec<Entry>,
    /// Head of the free-slot list (threaded through `Entry::pos`).
    free: u32,
    gens: [Vec<u32>; N_GENERATIONS],
    frozen: Vec<u32>,
    /// Payload address -> slot, for the kinds without an intrusive slot.
    index: GcIndex,
}

impl Registry {
    fn new() -> Self {
        Self {
            slab: Vec::new(),
            free: NO_SLOT,
            gens: Default::default(),
            frozen: Vec::new(),
            index: GcIndex::default(),
        }
    }

    #[inline]
    fn entry(&self, slot: u32) -> &Entry {
        &self.slab[slot as usize]
    }

    #[inline]
    fn list_mut(&mut self, gen: u8) -> &mut Vec<u32> {
        if gen == GEN_FROZEN {
            &mut self.frozen
        } else {
            &mut self.gens[usize::from(gen)]
        }
    }

    /// The slot of the registered object `obj`, if any. `filter` is the
    /// index's miss-filter.
    #[inline]
    fn slot_of(&self, obj: &Object, filter: &crate::hot_filter::RebuildableBloom) -> Option<u32> {
        if let Some(cell) = intrusive_slot(obj) {
            let s = cell.get();
            return (s != NO_SLOT).then_some(s);
        }
        let key = obj.payload_addr()? as ObjectId;
        if !filter.may_contain(key) {
            return None;
        }
        self.index.get(&key).copied()
    }

    /// Add `obj` to list `gen` in a fresh slot.
    fn insert(&mut self, obj: WeakObject, gen: u8, flags: u8) -> u32 {
        let pos = self.list_mut(gen).len() as u32;
        let entry = Entry {
            obj: Some(obj),
            pos,
            gc_refs: std::cell::Cell::new(0),
            node: std::cell::Cell::new(0),
            gen,
            flags: std::cell::Cell::new(flags),
        };
        let slot = if self.free != NO_SLOT {
            let slot = self.free;
            let e = &mut self.slab[slot as usize];
            self.free = e.pos;
            *e = entry;
            slot
        } else {
            let slot = u32::try_from(self.slab.len()).expect("collector registry overflow");
            // (A collection's grey stack tags temporaries with the top bit.)
            assert!(slot < TEMP_BIT, "collector registry overflow");
            self.slab.push(entry);
            slot
        };
        self.list_mut(gen).push(slot);
        slot
    }

    /// Take `slot` out of its list (a swap with the list's last member).
    fn unlink(&mut self, slot: u32) {
        let (gen, pos) = {
            let e = self.entry(slot);
            (e.gen, e.pos as usize)
        };
        let list = self.list_mut(gen);
        debug_assert_eq!(list.get(pos), Some(&slot));
        list.swap_remove(pos);
        if let Some(&moved) = list.get(pos) {
            self.slab[moved as usize].pos = pos as u32;
        }
    }

    /// Return an unlinked slot to the free list, handing back its handle
    /// (whose drop only releases memory).
    fn release(&mut self, slot: u32) -> Option<WeakObject> {
        let free = self.free;
        let e = &mut self.slab[slot as usize];
        let w = e.obj.take();
        e.gen = GEN_FREE;
        e.flags.set(0);
        e.pos = free;
        self.free = slot;
        w
    }

    /// Drop `slot`'s entry altogether: from its list, the index, and a live
    /// instance's own record.
    fn remove(&mut self, slot: u32) -> Option<WeakObject> {
        self.unlink(slot);
        self.forget_key(slot);
        self.release(slot)
    }

    /// Drop the index entry (or a live instance's record) naming `slot`.
    fn forget_key(&mut self, slot: u32) {
        let Some(w) = self.slab[slot as usize].obj.as_ref() else {
            return;
        };
        if slot_is_intrusive(w) {
            clear_intrusive_slot(w);
        } else {
            let key = w.addr() as ObjectId;
            if self.index.get(&key) == Some(&slot) {
                self.index.remove(&key);
            }
        }
    }

    /// Every live member of every list, frozen ones included.
    fn len(&self) -> usize {
        self.gens.iter().map(Vec::len).sum::<usize>() + self.frozen.len()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GcStats {
    pub collections: u64,
    pub collected: u64,
    pub uncollectable: u64,
}

/// The top bit of a grey-stack item: the rest is a temporary candidate's
/// position rather than a registry slot.
const TEMP_BIT: u32 = 1 << 31;

/// A temporary candidate: an untracked object a collection found a cycle
/// could route through (see `collect_generation`'s phase 2), held for the
/// collection's duration, with its marking state.
struct Temp {
    obj: Object,
    gc_refs: std::cell::Cell<i64>,
    reached: std::cell::Cell<bool>,
}

/// A collection's view of one candidate (see [`Finder::find`]).
#[derive(Clone, Copy)]
enum Hit {
    /// A registered candidate, by slot.
    Real(u32),
    /// A temporary, by position.
    Temp(u32),
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
    /// Every tracked object, by generation (see [`Registry`]).
    reg: RefCell<Registry>,
    /// Insert-only miss-filter over the registry's id index, maintained at
    /// [`Self::register`] and consulted by the usually-miss lookups.
    /// Rebuilt from the index once most of its bits name objects long
    /// gone.
    tracked_filter: crate::hot_filter::RebuildableBloom,
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
    /// The deferred containers that outlived a sweep: an older generation
    /// of them, re-examined only by collections of the middle generation
    /// and up (a young collection leaves old objects alone, as CPython's
    /// does), so each young collection costs what was deferred since the
    /// last, not every scalar container still alive.
    deferred_old: RefCell<Vec<DeferredContainer>>,
    /// Length at which [`GcState::sweep_deferred`] compacts `deferred`.
    deferred_limit: AtomicUsize,
    /// Dead entries of the older generations pruned since the last
    /// collection: deallocations the young-generation count owes (see
    /// [`GcState::maybe_auto_collect`]).
    old_deaths: AtomicUsize,
    /// Consecutive automatic young collections that reclaimed nothing
    /// (see [`GcState::maybe_auto_collect`]).
    idle_collections: AtomicUsize,
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
    /// Generators, coroutines and async generators born since the last
    /// collection, held weakly as [`Self::young`] holds instances: an
    /// `await` of a coroutine function makes one per call, and most
    /// finish (holding no frame) and die before any collection.
    young_gens: RefCell<Vec<crate::sync::Weak<crate::object::PyGenerator>>>,
    /// Lists and dicts born since the last collection that could close a
    /// cycle, held weakly as [`Self::young`] holds instances: a function
    /// that builds and returns `[a, b]` or `{k: obj}` makes one per call,
    /// and most die before any collection.
    young_lists: RefCell<Vec<crate::sync::Weak<RefCell<Vec<Object>>>>>,
    young_dicts: RefCell<Vec<crate::sync::Weak<RefCell<crate::object::DictData>>>>,
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
            reg: RefCell::new(Registry::new()),
            tracked_filter: crate::hot_filter::RebuildableBloom::new(),
            collecting: AtomicBool::new(false),
            thresholds: RefCell::new(DEFAULT_THRESHOLDS),
            counts: RefCell::new([0; N_GENERATIONS]),
            gen0_gauge: AtomicU64::new(DEFAULT_THRESHOLDS[0] as u64),
            deferred: RefCell::new(Vec::new()),
            deferred_old: RefCell::new(Vec::new()),
            deferred_limit: AtomicUsize::new(DEFERRED_FLOOR),
            old_deaths: AtomicUsize::new(0),
            idle_collections: AtomicUsize::new(0),
            young: RefCell::new(Vec::new()),
            young_fns: RefCell::new(Vec::new()),
            young_gens: RefCell::new(Vec::new()),
            young_lists: RefCell::new(Vec::new()),
            young_dicts: RefCell::new(Vec::new()),
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
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).reg));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).thresholds));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).counts));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).deferred));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).deferred_old));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).young));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).young_fns));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).young_gens));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).young_lists));
            RefCell::reinit_lock_after_fork(std::ptr::addr_of_mut!((*this).young_dicts));
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
    pub fn complete_finalizer(&self, obj: &Object) {
        self.note_finalized(id_of(obj));
        let reg = self.reg.borrow();
        if let Some(slot) = reg.slot_of(obj, &self.tracked_filter) {
            let e = reg.entry(slot);
            e.set(F_FINALIZED, true);
            e.set(F_QUEUED, false);
        }
    }

    /// Claim `obj`'s finalizer for the caller: mark it run, returning
    /// whether it had been claimed before (`None`: `obj` isn't tracked).
    pub fn claim_finalizer(&self, obj: &Object) -> Option<bool> {
        let reg = self.reg.borrow();
        let slot = reg.slot_of(obj, &self.tracked_filter)?;
        let e = reg.entry(slot);
        let was = e.has(F_FINALIZED);
        e.set(F_FINALIZED, true);
        Some(was)
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
        match obj {
            Object::Instance(inst) => {
                if self.nurse(&self.young, inst) {
                    return;
                }
            }
            Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g) => {
                if self.nurse(&self.young_gens, g) {
                    return;
                }
            }
            Object::List(l) => {
                if self.nurse(&self.young_lists, l) {
                    return;
                }
            }
            Object::Dict(d) => {
                if self.nurse(&self.young_dicts, d) {
                    return;
                }
            }
            _ => {}
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

    /// Register the young objects still alive with the collector.
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
        if let Ok(mut gens) = self.young_gens.try_borrow_mut() {
            if !gens.is_empty() {
                let gens = std::mem::take(&mut *gens);
                for w in gens {
                    if let Some(g) = w.upgrade() {
                        // It outlived its young set: trim its frame.
                        crate::compact_generator_frame(&g);
                        let obj = match g.kind {
                            crate::object::CoroutineKind::Generator => Object::Generator(g),
                            crate::object::CoroutineKind::Coroutine => Object::Coroutine(g),
                            crate::object::CoroutineKind::AsyncGenerator => {
                                Object::AsyncGenerator(g)
                            }
                        };
                        self.register(&obj, false);
                    }
                }
            }
        }
        self.flush_set(&self.young_lists, WeakObject::List);
        self.flush_set(&self.young_dicts, WeakObject::Dict);
        self.flush_set(&self.young, WeakObject::Instance);
    }

    /// The young instances and functions still alive (the dead ones are
    /// dropped).
    fn young_live(&self) -> usize {
        let fns = self.young_fns.try_borrow_mut().map_or(0, |mut fns| {
            fns.retain(|w| w.strong_count() > 0);
            fns.len()
        }) + self.young_gens.try_borrow_mut().map_or(0, |mut gens| {
            gens.retain(|w| w.strong_count() > 0);
            gens.len()
        }) + self.young_lists.try_borrow_mut().map_or(0, |mut lists| {
            lists.retain(|w| w.strong_count() > 0);
            lists.len()
        }) + self.young_dicts.try_borrow_mut().map_or(0, |mut dicts| {
            dicts.retain(|w| w.strong_count() > 0);
            dicts.len()
        });
        let Ok(mut young) = self.young.try_borrow_mut() else {
            return fns;
        };
        young.retain(|w| w.strong_count() > 0);
        young.len() + fns
    }

    /// Whether the object `id` is in a young set.
    fn is_young(&self, id: ObjectId) -> bool {
        fn holds<T: 'static>(set: &RefCell<Vec<crate::sync::Weak<T>>>, id: ObjectId) -> bool {
            set.try_borrow().is_ok_and(|young| {
                young
                    .iter()
                    .any(|w| w.as_ptr() as usize as ObjectId == id && w.strong_count() > 0)
            })
        }
        holds(&self.young, id)
            || holds(&self.young_fns, id)
            || holds(&self.young_gens, id)
            || holds(&self.young_lists, id)
            || holds(&self.young_dicts, id)
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
        self.sweep_deferred_from(promote_all, promote_all);
    }

    /// [`Self::sweep_deferred`] of the young deferrals, and of the old ones
    /// too when `old` (see [`Self::deferred_old`]); the young survivors
    /// join the old.
    fn sweep_deferred_from(&self, promote_all: bool, old: bool) {
        self.flush_young();
        let mut promote: Vec<Object> = Vec::new();
        {
            let (Ok(mut deferred), Ok(mut aged)) = (
                self.deferred.try_borrow_mut(),
                self.deferred_old.try_borrow_mut(),
            ) else {
                return;
            };
            let mut keep = |entry: &DeferredContainer| {
                let Some(obj) = entry.upgrade() else {
                    return false;
                };
                if promote_all || container_can_cycle(&obj) {
                    promote.push(obj);
                    return false;
                }
                true
            };
            deferred.retain(&mut keep);
            if old {
                aged.retain(&mut keep);
            }
            aged.append(&mut deferred);
            if aged.len() >= DEFERRED_CAP && !old {
                // (Dead entries first: the old ones were left unswept.)
                aged.retain(|e| !e.is_dead());
            }
            if aged.len() >= DEFERRED_CAP {
                // The set has stopped being a churn buffer: hand it all
                // over, so these allocations resume pacing collections.
                promote.extend(aged.drain(..).filter_map(|e| e.upgrade()));
            }
            self.deferred_limit.store(DEFERRED_FLOOR, Ordering::Relaxed);
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

    /// Enter `obj` in the registry's generation 0 (`false`: it was already
    /// there, or has no heap allocation of its own). A `fresh` object's id
    /// may be a finalized one's, recycled; a young instance's own
    /// finalizer may already have run, and its entry says so.
    fn register(&self, obj: &Object, fresh: bool) -> bool {
        {
            let mut reg = self.reg.borrow_mut();
            // An entry whose object died can't sit at this address: its
            // weak handle keeps the allocation reserved until the entry is
            // pruned.
            if reg.slot_of(obj, &self.tracked_filter).is_some() {
                return false;
            }
            let Some(weak) = WeakObject::new(obj) else {
                return false;
            };
            // SAFETY: `obj` is alive.
            unsafe { self.enter(&mut reg, weak, fresh) };
        }
        serial_add(&self.tracked_count, 1);
        serial_add(&self.tracked_version, 1);
        true
    }

    /// Enter the object `weak` names, which is alive and unregistered, in
    /// generation 0.
    ///
    /// # Safety
    ///
    /// The object must be alive.
    unsafe fn enter(&self, reg: &mut Registry, weak: WeakObject, fresh: bool) -> u32 {
        let key = weak.addr() as ObjectId;
        // `finalized_ids` is keyed by object id (a pointer), which the
        // allocator recycles. A freshly tracked object at a recycled
        // address must start *un*-finalized (`test_is_finalized`). Almost
        // always empty (only `__del__`-bearing objects ever land there).
        let mut flags = 0;
        // SAFETY: a read that runs no code.
        if unsafe { self.finalized_ids.peek() }.is_none_or(|f| !f.is_empty()) {
            if fresh {
                self.finalized_ids.borrow_mut().remove(&key);
            } else if self.finalized_ids.borrow().contains(&key) {
                flags = F_FINALIZED;
            }
        }
        // (A pointer: the field lives in the object, not the handle, which
        // moves into the registry below.)
        // SAFETY: alive, per the caller.
        let own = unsafe { weak_intrusive_slot(&weak) }.map(std::ptr::from_ref);
        if own.is_none() {
            // Publish to the miss-filter *before* the insert becomes
            // observable (the caller holds the registry borrow).
            self.tracked_filter.insert(key);
        }
        // (Registered other than through `track`, which retires the
        // deferral itself.)
        if let WeakObject::Instance(w) = &weak {
            // SAFETY: alive, per the caller.
            let i = unsafe { &*w.as_ptr() };
            if i.is_gc_deferred() {
                i.clear_deferred_tracking();
            }
        }
        let slot = reg.insert(weak, 0, flags);
        match own {
            // SAFETY: the object is alive, per the caller.
            Some(cell) => unsafe { (*cell).set(slot) },
            None => {
                reg.index.insert(key, slot);
            }
        }
        slot
    }

    /// Register the young objects of `set` still alive (see
    /// [`Self::young`]) under one registry borrow, keeping the set's
    /// allocation for the next generation of young.
    fn flush_set<T: 'static>(
        &self,
        set: &RefCell<Vec<crate::sync::Weak<T>>>,
        wrap: fn(crate::sync::Weak<T>) -> WeakObject,
    ) {
        let Ok(mut young) = set.try_borrow_mut() else {
            return;
        };
        if young.is_empty() {
            return;
        }
        let mut added = 0;
        {
            let mut reg = self.reg.borrow_mut();
            for w in young.drain(..) {
                if w.strong_count() == 0 {
                    continue;
                }
                let weak = wrap(w);
                // SAFETY (both): the object is alive (counted above, and
                // nothing has run since).
                let registered = match unsafe { weak_intrusive_slot(&weak) } {
                    Some(own) => own.is_registered(),
                    None => {
                        let key = weak.addr() as ObjectId;
                        self.tracked_filter.may_contain(key) && reg.index.contains_key(&key)
                    }
                };
                if !registered {
                    unsafe { self.enter(&mut reg, weak, false) };
                    added += 1;
                }
            }
        }
        if added > 0 {
            serial_add(&self.tracked_count, added);
            serial_add(&self.tracked_version, 1);
        }
    }

    /// Drop the registry entry of an instance that is dying (from its
    /// `Drop`, with its `gc_slot` and address), if the registry is free to
    /// change: its weak handle otherwise keeps the object's allocation
    /// alive until a collection of its generation prunes it (CPython
    /// unlinks a dying object from its GC list at once). A busy registry
    /// (a collection is releasing garbage) is left as it is; that
    /// collection prunes the entry.
    fn forget_instance(&self, slot: u32, addr: usize) {
        let (Ok(mut reg), Ok(mut counts)) =
            (self.reg.try_borrow_mut(), self.counts.try_borrow_mut())
        else {
            return;
        };
        let Some(e) = reg.slab.get(slot as usize) else {
            return;
        };
        match &e.obj {
            Some(WeakObject::Instance(w)) if w.as_ptr() as usize == addr => {}
            _ => return,
        }
        if e.gen == GEN_FROZEN {
            return;
        }
        let weak = reg.remove(slot);
        // (As `untrack_slot` accounts it.)
        counts[0] = counts[0].saturating_sub(1);
        self.sync_gen0_gauge(counts[0], None);
        drop((reg, counts));
        serial_add(&self.tracked_count, usize::MAX);
        serial_add(&self.tracked_version, 1);
        drop(weak);
    }

    /// Stop tracking `obj`. Backs the explicit `gc._untrack(obj)` extension
    /// and the C-API `PyObject_GC_UnTrack`.
    pub fn untrack(&self, obj: &Object) {
        let slot = self.reg.borrow().slot_of(obj, &self.tracked_filter);
        match slot {
            Some(slot) => self.untrack_slot(slot, id_of(obj), true),
            None => self.untrack_young(id_of(obj)),
        }
    }

    /// [`Self::untrack`] by id, for an object that isn't an instance.
    pub fn untrack_id(&self, id: ObjectId) {
        self.untrack_id_in(id, true);
    }

    /// Stop tracking the object of id `id`, which isn't an instance;
    /// with `young` false, an object in a young set is left there (see
    /// [`untrack_registered_id`]).
    fn untrack_id_in(&self, id: ObjectId, young: bool) {
        // A registered object is never in a young set, so the index (a
        // hash probe) answers first; the young sets are scanned only for
        // an object it doesn't hold. (Finished coroutines are untracked
        // here by the thousand, and the young sets hold up to
        // `YOUNG_CAP` entries.) An index entry left by a dead object
        // whose address a young one now reuses is dropped, and the young
        // sets still searched.
        let slot = if self.tracked_filter.may_contain(id) {
            self.reg.borrow().index.get(&id).copied()
        } else {
            None
        };
        match slot {
            Some(slot) => self.untrack_slot(slot, id, young),
            None if young => self.untrack_young(id),
            None => {}
        }
    }

    /// Remove the entry at `slot` (an object of id `id`), accounting it as
    /// a deallocation; with `young`, a dead entry's id is also searched in
    /// the young sets.
    fn untrack_slot(&self, slot: u32, id: ObjectId, young: bool) {
        let (weak, dead) = {
            let mut reg = self.reg.borrow_mut();
            let dead = reg.entry(slot).obj.as_ref().is_none_or(WeakObject::is_dead);
            (reg.remove(slot), dead)
        };
        if young && dead {
            self.untrack_young(id);
        }
        {
            let mut counts = self.counts.borrow_mut();
            counts[0] = counts[0].saturating_sub(1);
            self.sync_gen0_gauge(counts[0], None);
        }
        drop(weak);
        serial_add(&self.tracked_count, usize::MAX);
        serial_add(&self.tracked_version, 1);
    }

    /// [`Self::untrack_id`] for an object in a young set (searched from
    /// the newest: an untrack usually follows its birth closely).
    fn untrack_young(&self, id: ObjectId) {
        fn remove<T: 'static>(set: &RefCell<Vec<crate::sync::Weak<T>>>, id: ObjectId) -> bool {
            let Ok(mut young) = set.try_borrow_mut() else {
                return false;
            };
            match young
                .iter()
                .rposition(|w| w.as_ptr() as usize as ObjectId == id)
            {
                Some(i) => {
                    young.swap_remove(i);
                    true
                }
                None => false,
            }
        }
        if remove(&self.young_fns, id)
            || remove(&self.young, id)
            || remove(&self.young_gens, id)
            || remove(&self.young_lists, id)
            || remove(&self.young_dicts, id)
        {
            let mut counts = self.counts.borrow_mut();
            counts[0] = counts[0].saturating_sub(1);
            self.sync_gen0_gauge(counts[0], None);
        }
    }

    /// Drop the entries of objects that have died from generation `gen`;
    /// `budget` caps how many entries are examined, starting at `start`
    /// (wrapping). Returns how many were dropped and where the
    /// examination stopped.
    fn prune_dead_in(
        reg: &mut Registry,
        gen: usize,
        start: usize,
        budget: usize,
    ) -> (usize, usize) {
        let mut removed = 0;
        let mut i = if start < reg.gens[gen].len() {
            start
        } else {
            0
        };
        let mut examined = 0;
        while examined < budget && i < reg.gens[gen].len() {
            examined += 1;
            let slot = reg.gens[gen][i];
            if reg.entry(slot).obj.as_ref().is_none_or(WeakObject::is_dead) {
                // (The swap brings the list's last member to `i`.)
                drop(reg.remove(slot));
                removed += 1;
            } else {
                i += 1;
            }
        }
        (removed, i)
    }

    /// Drop the dead young entries, plus a slice of `old_budget` entries
    /// of the older generations' (whose dead are added to `old_deaths`).
    /// Returns the young generation's live population.
    fn prune(&self, old_budget: usize) -> usize {
        let (removed, live) = {
            let mut reg = self.reg.borrow_mut();
            let (mut removed, _) = Self::prune_dead_in(&mut reg, 0, 0, usize::MAX);
            // The older generations, a slice at a time: the cursor runs
            // over gen 1 then gen 2 as one sequence.
            let cursor = self.old_prune_cursor.load(Ordering::Relaxed);
            let n1 = reg.gens[1].len();
            let (g, start) = if cursor < n1 {
                (1, cursor)
            } else {
                (2, cursor - n1)
            };
            let (r, stop) = Self::prune_dead_in(&mut reg, g, start, old_budget);
            removed += r;
            self.old_deaths.fetch_add(r, Ordering::Relaxed);
            let next = if stop >= reg.gens[g].len() {
                if g == 1 {
                    reg.gens[1].len()
                } else {
                    0
                }
            } else if g == 1 {
                stop
            } else {
                reg.gens[1].len() + stop
            };
            self.old_prune_cursor.store(next, Ordering::Relaxed);
            (removed, reg.gens[0].len())
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

    /// Whether the object of id `id` is registered. Only for a kind the id
    /// index holds (not an instance: see [`Self::is_tracked_obj`]).
    pub fn is_tracked(&self, id: ObjectId) -> bool {
        // Usually-miss probe: two relaxed loads instead of the registry
        // borrow. A stale filter bit just takes the precise path.
        if !self.tracked_filter.may_contain(id) {
            return false;
        }
        self.reg.borrow().index.contains_key(&id)
    }

    /// Whether `obj` is registered.
    pub fn is_tracked_obj(&self, obj: &Object) -> bool {
        self.reg
            .borrow()
            .slot_of(obj, &self.tracked_filter)
            .is_some()
    }

    /// [`Self::is_tracked_obj`] for an object that may be young (an
    /// instance, function, generator, list or dict).
    pub fn is_tracked_instance(&self, obj: &Object) -> bool {
        self.is_tracked_obj(obj) || self.is_young(id_of(obj))
    }

    /// Snapshot every live tracked object that still carries an unrun
    /// `__del__`. The interpreter's shutdown pass walks this list to
    /// finalize objects that are still alive at exit; it claims each one
    /// through [`Self::claim_finalizer`], so each `__del__` runs at most
    /// once.
    pub fn finalization_candidates(&self) -> Vec<Object> {
        self.flush_young();
        let reg = self.reg.borrow();
        let mut out = Vec::new();
        let lists = reg.gens.iter().chain(std::iter::once(&reg.frozen));
        for &slot in lists.flatten() {
            let e = reg.entry(slot);
            // A finalizer already queued by a collection (but not yet
            // drained) must not be listed again.
            if e.has(F_FINALIZED) || e.has(F_QUEUED) {
                continue;
            }
            if let Some(obj) = e.obj.as_ref().and_then(WeakObject::upgrade) {
                if has_finalizer(&obj) {
                    out.push(obj);
                }
            }
        }
        out
    }

    /// Number of tracked objects in each generation.
    pub fn counts(&self) -> [usize; N_GENERATIONS] {
        // A deferred container is an allocation an eagerly-tracking build
        // would have counted, so report it as one: drop the ones that have
        // died, then add those still live. `gc.get_count()` and
        // `_testinternalcapi.get_tracked_heap_size()` then read exactly as
        // they would have (`test_gc.test_heap_size`).
        self.sweep_deferred_from(false, true);
        let mut counts = *self.counts.borrow();
        counts[0] = counts[0]
            .saturating_add(self.deferred.borrow().len() + self.deferred_old.borrow().len());
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
    /// is corrected when it trips: the dead young entries are pruned, a
    /// slice of the older generations is pruned and its dead credited, and
    /// the count restarts from the survivors less those deaths, unless
    /// that alone makes up half the threshold. (A loop that builds a
    /// structure and drops the previous one thus seldom collects, as in
    /// CPython; the pruning costs a few steps per allocation.)
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
        // The older objects found dead (a slice of the older generations
        // sized to the threshold, so a program that frees what it
        // allocates is seen to) offset the young survivors. Deaths beyond
        // the survivors are dropped, as CPython's count never goes below
        // zero.
        let young = self.prune(OLD_PRUNE_BUDGET.max(threshold0.saturating_mul(2)));
        let survivors = young + self.young_live();
        let deaths = self.old_deaths.load(Ordering::Relaxed).min(survivors);
        self.old_deaths.store(deaths, Ordering::Relaxed);
        let live = survivors - deaths;
        // A program whose young collections keep finding no cycles (its
        // objects die by reference count, or live on) collects less often:
        // each idle one doubles the survivors the next waits for, up to
        // eight thresholds' worth, and any that reclaims something resets
        // the pace. (A young collection costs far more per object than
        // CPython's, whose candidates sit on intrusive lists.)
        let idle = self.idle_collections.load(Ordering::Relaxed).min(3);
        let wanted = if eligible == 0 {
            (threshold0 / 2) << idle
        } else {
            threshold0 / 2
        };
        if live < wanted {
            // The count trips again once the survivors could reach `wanted`
            // (it must restart below the threshold to trip at all).
            let restart = if live < threshold0 {
                live
            } else {
                threshold0.saturating_sub(wanted - live)
            };
            let mut counts = self.counts.borrow_mut();
            counts[0] = restart;
            self.sync_gen0_gauge(restart, None);
            return false;
        }
        // Automatic young collection: a single pass (see `collect_impl`'s
        // `exact` discussion).
        let collected = self.collect_impl(eligible, false);
        if collected == 0 {
            self.idle_collections.fetch_add(1, Ordering::Relaxed);
        } else {
            self.idle_collections.store(0, Ordering::Relaxed);
        }
        true
    }

    /// Total population (across all generations + frozen).
    pub fn population(&self) -> usize {
        self.flush_young();
        self.reg.borrow().len()
    }

    /// Snapshot all live tracked objects. Used by
    /// `gc.get_objects(generation=...)`.
    pub fn snapshot(&self, generation: Option<usize>) -> Vec<Object> {
        // `gc.get_objects()` enumerates every container CPython tracks,
        // including the all-scalar ones whose tracking we defer.
        self.promote_all_deferred();
        let reg = self.reg.borrow();
        let mut out = Vec::new();
        let mut push = |s: &u32| {
            out.extend(reg.entry(*s).obj.as_ref().and_then(WeakObject::upgrade));
        };
        match generation {
            Some(g) if g < N_GENERATIONS => reg.gens[g].iter().for_each(&mut push),
            _ => reg.gens.iter().for_each(|g| g.iter().for_each(&mut push)),
        }
        if generation.is_none() {
            reg.frozen.iter().for_each(&mut push);
        }
        out
    }

    /// `gc.freeze()` — mark every currently-tracked object as
    /// frozen so it is ignored by future collections.
    pub fn freeze_all(&self) {
        self.flush_young();
        let mut reg = self.reg.borrow_mut();
        for g in 0..N_GENERATIONS {
            for slot in std::mem::take(&mut reg.gens[g]) {
                let pos = reg.frozen.len() as u32;
                let e = &mut reg.slab[slot as usize];
                e.gen = GEN_FROZEN;
                e.pos = pos;
                reg.frozen.push(slot);
            }
        }
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
    }

    /// `gc.unfreeze()` — move every frozen object back to
    /// generation 0.
    pub fn unfreeze_all(&self) {
        let mut reg = self.reg.borrow_mut();
        for slot in std::mem::take(&mut reg.frozen) {
            let pos = reg.gens[0].len() as u32;
            let e = &mut reg.slab[slot as usize];
            e.gen = 0;
            e.pos = pos;
            reg.gens[0].push(slot);
        }
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
    }

    pub fn freeze_count(&self) -> usize {
        self.reg.borrow().frozen.len()
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
        self.sweep_deferred_from(false, true);
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
        self.old_deaths.store(0, Ordering::Relaxed);
        // Promote any deferred container that can now anchor a cycle, so
        // the mark phase sees the whole candidate population (the old
        // deferrals only for the older generations). Before the
        // re-entrancy claim: promotion calls `track_now`.
        self.sweep_deferred_from(false, upto >= 1);
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
        // name objects long gone, rebuild it from the live index (sooner
        // after a full collection, which finds the most dead).
        let live = self.reg.borrow().index.len();
        let stale = self.tracked_filter.inserts_since_rebuild();
        let bound = if gen == N_GENERATIONS - 1 {
            live
        } else {
            live.saturating_mul(4)
        };
        if stale > 4096.max(bound) {
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
        let reg = self.reg.borrow();
        self.tracked_filter.rebuild(reg.index.keys().copied());
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
        let gen = gen.min(N_GENERATIONS - 1);
        // Phase 1: mark the live entries of this generation and the
        // younger ones as candidates. Entries whose object died are dropped
        // on the way. The candidates are the generation lists themselves,
        // and each one's marking state lives in its entry (as CPython's
        // lives in the object's header): the collection takes no reference
        // and makes no copy per candidate.
        if self.mark_candidates(gen) == 0 {
            return 0;
        }
        // Temporary candidates, and their positions by id.
        let mut temps: Vec<Temp> = Vec::new();

        // The registry can't change while the mark phase reads it: no code
        // runs, and a dying object's own removal finds it busy.
        let reg = self.reg.borrow();
        let finder = Finder::new(&reg, &self.tracked_filter);
        let lists = &reg.gens[..=gen];

        // Phase 2: one walk over the candidates promotes the untracked nodes
        // reachable from them to temporary candidates for this pass only,
        // and tallies every candidate's internal references (self-references
        // included) into its `gc_refs`, negated. CPython GC-tracks
        // iterators, tuples, frames, tracebacks, cells, bound methods and
        // descriptor wrappers; we keep them off the books for speed and
        // discover the ones a cycle actually routes through here, so their
        // internal edges are accounted. A temporary is a candidate from its
        // first edge on, so each of its edges is tallied too. Temporaries
        // take part in the mark walk but are never reclaimed or tracked.
        //
        // The walk also records each candidate's edges to candidates (a
        // node per registered candidate, in list order, then one per
        // temporary), so marking what is reachable follows those instead of
        // walking every object a second time.
        let mut found: Vec<Object> = Vec::new();
        let mut edges: Vec<u32> = Vec::new();
        let mut starts: Vec<u32> = Vec::new();
        let tally =
            |obj: &Object, temps: &mut Vec<Temp>, found: &mut Vec<Object>, edges: &mut Vec<u32>| {
                let parent_is_iter = matches!(obj, Object::Iter(_));
                let parent_is_frame = matches!(obj, Object::Frame(_));
                traverse_collected(obj, &mut |child| {
                    if child.never_gc_candidate() {
                        return;
                    }
                    match finder.find(child) {
                        Some(Hit::Real(s)) => {
                            let e = reg.entry(s);
                            e.gc_refs.set(e.gc_refs.get().wrapping_sub(1));
                            edges.push(s);
                        }
                        Some(Hit::Temp(i)) => {
                            let t = &temps[i as usize];
                            t.gc_refs.set(t.gc_refs.get() - 1);
                            edges.push(i | TEMP_BIT);
                        }
                        None => {
                            if promotes_temporarily(child, parent_is_iter, parent_is_frame) {
                                found.push(child.clone());
                            }
                        }
                    }
                });
                // (One entry per edge: a node reached twice is promoted once and
                // tallied twice.)
                for child in found.drain(..) {
                    let i = match finder.find(&child) {
                        Some(Hit::Temp(i)) => i as usize,
                        Some(Hit::Real(s)) => {
                            let e = reg.entry(s);
                            e.gc_refs.set(e.gc_refs.get().wrapping_sub(1));
                            edges.push(s);
                            continue;
                        }
                        None => {
                            finder.add_temp(&child, temps.len());
                            temps.push(Temp {
                                obj: child,
                                gc_refs: std::cell::Cell::new(0),
                                reached: std::cell::Cell::new(false),
                            });
                            temps.len() - 1
                        }
                    };
                    let t = &temps[i];
                    t.gc_refs.set(t.gc_refs.get() - 1);
                    edges.push(i as u32 | TEMP_BIT);
                }
            };
        for &slot in lists.iter().flatten() {
            let e = reg.entry(slot);
            e.node.set(starts.len() as u32);
            starts.push(edges.len() as u32);
            // SAFETY: the mark walk runs no code that could release the
            // object (see `traverse_collected`).
            unsafe {
                e.obj.as_ref().and_then(|w| {
                    w.with_borrowed(|obj| tally(obj, &mut temps, &mut found, &mut edges))
                });
            }
        }
        let n_real = starts.len();
        let mut scanned = 0;
        while scanned < temps.len() {
            starts.push(edges.len() as u32);
            let obj = temps[scanned].obj.clone();
            tally(&obj, &mut temps, &mut found, &mut edges);
            scanned += 1;
        }
        starts.push(edges.len() as u32);
        let graph = Graph {
            reg: &reg,
            temps: &temps,
            edges: &edges,
            starts: &starts,
            n_real,
        };

        // Phase 3: add the outer refcount, read after the walk (an iterator
        // synthesises a fresh wrapper for its buffer on each traverse, alive
        // only during the visit). A temporary holds one reference of its
        // own.
        let mut grey: Vec<u32> = Vec::new();
        for &slot in lists.iter().flatten() {
            let e = reg.entry(slot);
            let strong = e.obj.as_ref().map_or(0, WeakObject::strong_count);
            let refs = i64::from(e.gc_refs.get()) + strong as i64;
            e.gc_refs
                .set(refs.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
            // Phase 4: anything referenced from outside is reachable; mark
            // it and propagate.
            if refs > 0 {
                e.set(F_REACHED, true);
                grey.push(slot);
            }
        }
        for (i, t) in temps.iter().enumerate() {
            t.gc_refs
                .set(strong_count_for(&t.obj) as i64 - 1 + t.gc_refs.get());
            if t.gc_refs.get() > 0 {
                t.reached.set(true);
                grey.push(i as u32 | TEMP_BIT);
            }
        }
        if root_debug() {
            for &g in &grey {
                if g & TEMP_BIT == 0 {
                    let e = reg.entry(g);
                    eprintln!(
                        "[root] slot={} gc_refs={} sc={}",
                        g,
                        e.gc_refs.get(),
                        e.obj.as_ref().map_or(0, WeakObject::strong_count)
                    );
                } else {
                    let t = &temps[(g & !TEMP_BIT) as usize];
                    eprintln!(
                        "[root] temp gc_refs={} {}",
                        t.gc_refs.get(),
                        t.obj.type_name_owned()
                    );
                }
            }
        }
        if stats_debug() {
            let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
            for t in &temps {
                *kinds.entry(t.obj.type_name_owned()).or_default() += 1;
            }
            eprintln!(
                "[gc] gen={} cands={} roots={} temps={} {:?}",
                gen,
                lists.iter().map(Vec::len).sum::<usize>(),
                grey.len(),
                temps.len(),
                kinds
            );
        }
        graph.propagate(&mut grey);

        // Phase 5: the candidates still unreached are unreachable cyclic
        // garbage; hold each for the rest of the collection.
        let mut unreachable: Vec<(u32, Object)> = Vec::new();
        for &slot in lists.iter().flatten() {
            let e = reg.entry(slot);
            if !e.has(F_REACHED) {
                if let Some(obj) = e.obj.as_ref().and_then(WeakObject::upgrade) {
                    unreachable.push((slot, obj));
                }
            }
        }

        // CPython's `handle_weakrefs`: a weakref that is *itself* part of the
        // cyclic trash has its callback cleared without invocation — only
        // weakrefs rooted outside the dying subgraph observe the deaths
        // (test_callbacks_on_callback).
        // (Built only for a collection whose garbage was weakly referenced.)
        let trash_ids: std::cell::OnceCell<IdSet> = std::cell::OnceCell::new();
        let is_trash = |id: ObjectId| {
            trash_ids
                .get_or_init(|| unreachable.iter().map(|(_, o)| id_of(o)).collect())
                .contains(&id)
        };
        let wrapper_is_trash = |slot: &crate::sync::Rc<crate::weakref_registry::WeakRefSlot>| {
            slot.py_ref
                .borrow()
                .as_ref()
                .and_then(crate::sync::Weak::upgrade)
                .is_none_or(|inst| is_trash(crate::sync::Rc::as_ptr(&inst) as usize as u64))
        };
        // The weakrefs to `obj`, cleared (a lock-free probe answers for an
        // object never weakly referenced).
        let notify_clear = |obj: &Object| {
            let id = id_of(obj);
            if crate::weakref_registry::may_have_weakrefs(id) {
                crate::weakref_registry::notify_clear(id)
            } else {
                Vec::new()
            }
        };
        let pending_finalizer =
            |slot: u32, obj: &Object| has_finalizer(obj) && !reg.entry(slot).has(F_FINALIZED);

        if weakref_only {
            let mut weakref_callbacks = Vec::new();
            for (slot, obj) in &unreachable {
                if pending_finalizer(*slot, obj) {
                    continue;
                }
                for (slot, cb) in notify_clear(obj) {
                    if let Some(cb) = cb {
                        if !wrapper_is_trash(&slot) {
                            weakref_callbacks.push((slot, cb));
                        }
                    }
                }
            }
            for &slot in lists.iter().flatten() {
                let e = reg.entry(slot);
                e.set(F_CAND | F_REACHED, false);
            }
            drop(reg);
            queue_weakref_callbacks(weakref_callbacks, |_| false);
            drop(unreachable);
            return 0;
        }

        // CPython clears weakrefs to the *entire* unreachable set
        // (`handle_weakrefs`) BEFORE running any finalizer, so a weakref
        // watching an object a finalizer later revives stays cleared
        // (`test_io.test_garbage_collection`).
        let mut weakref_callbacks = Vec::new();
        for (_, obj) in &unreachable {
            for (slot, cb) in notify_clear(obj) {
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
        // CPython runs `finalize_garbage` before `delete_garbage`, so a
        // pending finalizer always sees its own class, closure cells, and
        // referents intact: protect the deferred objects' whole subgraphs.
        let mut protect: Vec<u32> = Vec::new();
        for (slot, obj) in &unreachable {
            if pending_finalizer(*slot, obj) {
                let e = reg.entry(*slot);
                if !e.has(F_QUEUED) {
                    e.set(F_QUEUED, true);
                    run_finalizer(obj);
                }
                e.set(F_REACHED, true);
                protect.push(*slot);
            }
        }
        graph.propagate(&mut protect);

        let dead: Vec<Object> = unreachable
            .iter()
            .filter(|(slot, _)| !reg.entry(*slot).has(F_REACHED))
            .map(|(_, obj)| obj.clone())
            .collect();
        let collected = dead.len();
        // Temporarily promoted iterators and immutable containers that ended
        // up unreached are cyclic garbage too, freed by refcount once the
        // cycle's mutable anchor is cleared below. CPython counts each
        // (`test_tuple` asserts the closing tuple is counted alongside its
        // list).
        let reported = collected
            + temps
                .iter()
                .filter(|t| {
                    !t.reached.get()
                        && matches!(
                            t.obj,
                            Object::Iter(_) | Object::Tuple(_) | Object::FrozenSet(_)
                        )
                })
                .count();
        // The clears below release objects, whose removal from the registry
        // must find it free.
        drop(reg);

        // Break the cycles by clearing the reclaimed objects' fields — or,
        // under `gc.DEBUG_SAVEALL`, park them in `gc.garbage` intact.
        if self.debug.load(Ordering::Acquire) & DEBUG_SAVEALL != 0 {
            let mut garbage = self.garbage.borrow_mut();
            for obj in &dead {
                garbage.push(obj.clone());
            }
        } else {
            // Instances whose `__dict__` is shared are deferred; once every
            // other dead object has released its references, a dict held
            // only by dead holders is down to one owner and the retry
            // clears it (a live holder keeps it intact).
            let mut shared_dict_holders: Vec<&Object> = Vec::new();
            for obj in &dead {
                if !clear_object_fields(obj) {
                    shared_dict_holders.push(obj);
                }
            }
            for obj in shared_dict_holders {
                clear_object_fields(obj);
            }
        }

        // Queue the weakref callbacks (after finalisers and cyclic clears,
        // matching CPython's order) for the interpreter's next safe point.
        queue_weakref_callbacks(weakref_callbacks, wrapper_is_trash);

        // Phase 6: rebuild the generation lists. Survivors of generation
        // `g` move to generation min(g+1, N_GENERATIONS-1); the dead leave
        // the registry. Dropping the held objects then frees the dead by
        // refcount.
        self.rebuild_generations(gen);
        self.tracked_count.fetch_sub(
            collected.min(self.tracked_count.load(Ordering::Acquire)),
            Ordering::AcqRel,
        );
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
        drop(dead);
        drop(unreachable);
        drop(temps);
        reported
    }

    /// Mark the live entries of generations `0..=upto` as candidates of a
    /// collection, dropping the entries whose object died. Returns how
    /// many there are.
    fn mark_candidates(&self, upto: usize) -> usize {
        let mut reg = self.reg.borrow_mut();
        let mut dead = Vec::new();
        let mut n = 0;
        for g in 0..=upto {
            let mut i = 0;
            while i < reg.gens[g].len() {
                let slot = reg.gens[g][i];
                let e = reg.entry(slot);
                if e.obj.as_ref().is_none_or(WeakObject::is_dead) {
                    // (The swap brings the list's last member to `i`.)
                    dead.push(reg.remove(slot));
                    continue;
                }
                e.gc_refs.set(0);
                e.set(F_REACHED, false);
                e.set(F_CAND, true);
                i += 1;
            }
            n += reg.gens[g].len();
        }
        drop(reg);
        if !dead.is_empty() {
            self.tracked_count.fetch_sub(
                dead.len().min(self.tracked_count.load(Ordering::Acquire)),
                Ordering::AcqRel,
            );
        }
        n
    }

    /// Move the surviving candidates of generations `0..=upto` up a
    /// generation and drop the dead (unreached) ones from the registry. A
    /// member that joined during the collection stays where it is.
    fn rebuild_generations(&self, upto: usize) {
        let mut reg = self.reg.borrow_mut();
        let mut lists: [Vec<u32>; N_GENERATIONS] = Default::default();
        let mut dead = Vec::new();
        for g in 0..=upto {
            let target = (g + 1).min(N_GENERATIONS - 1);
            for slot in std::mem::take(&mut reg.gens[g]) {
                let e = reg.entry(slot);
                if !e.has(F_CAND) {
                    lists[g].push(slot);
                    continue;
                }
                let reached = e.has(F_REACHED);
                e.set(F_CAND | F_REACHED, false);
                if reached {
                    reg.slab[slot as usize].gen = target as u8;
                    lists[target].push(slot);
                } else {
                    // (Already out of its list.)
                    reg.forget_key(slot);
                    dead.push(reg.release(slot));
                }
            }
        }
        for (g, list) in lists.into_iter().enumerate() {
            if g > upto && list.is_empty() {
                continue;
            }
            let start = reg.gens[g].len();
            for (i, &slot) in list.iter().enumerate() {
                reg.slab[slot as usize].pos = (start + i) as u32;
            }
            reg.gens[g].extend(list);
        }
        drop(reg);
        drop(dead);
    }
}

/// A collection's candidates and their edges to one another, as the mark
/// walk recorded them: node `k`'s edges are `edges[starts[k]..starts[k +
/// 1]]`, each a registered candidate's slot or a temporary's position
/// tagged [`TEMP_BIT`]; a registered candidate's node is in its entry,
/// and temporary `i`'s is `n_real + i`.
struct Graph<'a> {
    reg: &'a Registry,
    temps: &'a [Temp],
    edges: &'a [u32],
    starts: &'a [u32],
    n_real: usize,
}

impl Graph<'_> {
    /// Mark everything reachable from the candidates on the `grey` stack
    /// (registry slots, or temporaries tagged [`TEMP_BIT`]), which are
    /// marked already.
    fn propagate(&self, grey: &mut Vec<u32>) {
        while let Some(item) = grey.pop() {
            let node = if item & TEMP_BIT == 0 {
                self.reg.entry(item).node.get() as usize
            } else {
                self.n_real + (item & !TEMP_BIT) as usize
            };
            let range = self.starts[node] as usize..self.starts[node + 1] as usize;
            for &edge in &self.edges[range] {
                if edge & TEMP_BIT == 0 {
                    let e = self.reg.entry(edge);
                    if !e.has(F_REACHED) {
                        e.set(F_REACHED, true);
                        grey.push(edge);
                    }
                } else {
                    let t = &self.temps[(edge & !TEMP_BIT) as usize];
                    if !t.reached.get() {
                        t.reached.set(true);
                        grey.push(edge);
                    }
                }
            }
        }
    }
}

/// How a collection's mark walk finds a child's candidate: through the
/// child's own registry slot, or else a cache of answers by address in
/// front of the registry's id index and the temporaries (a child is
/// usually reached many times: a module's namespace from every frame, a
/// class's method from every bound method). The registry can't change
/// while the walk runs, and a new temporary updates its own cache line.
struct Finder<'a> {
    reg: &'a Registry,
    filter: &'a crate::hot_filter::RebuildableBloom,
    /// Temporaries' positions by id.
    temp_ids: std::cell::RefCell<GcIdMap>,
    /// Some temporary is also a registered object (outside the
    /// candidates): a registered child may then still be a temporary.
    registered_temp: std::cell::Cell<bool>,
    /// Payload address -> answer (see [`Finder::encode`]), direct-mapped.
    cache_keys: [std::cell::Cell<ObjectId>; FINDER_CACHE],
    cache_vals: [std::cell::Cell<u32>; FINDER_CACHE],
}

/// [`Finder`]'s cache size.
const FINDER_CACHE: usize = 512;

impl<'a> Finder<'a> {
    fn new(reg: &'a Registry, filter: &'a crate::hot_filter::RebuildableBloom) -> Self {
        Self {
            reg,
            filter,
            temp_ids: std::cell::RefCell::new(GcIdMap::default()),
            registered_temp: std::cell::Cell::new(false),
            cache_keys: std::array::from_fn(|_| std::cell::Cell::new(0)),
            cache_vals: std::array::from_fn(|_| std::cell::Cell::new(NO_SLOT)),
        }
    }

    #[inline]
    fn line(key: ObjectId) -> usize {
        ((key >> 4) ^ (key >> 13)) as usize & (FINDER_CACHE - 1)
    }

    /// An answer as a cache value: [`NO_SLOT`] for none, a registered
    /// candidate's slot, or a temporary's position tagged [`TEMP_BIT`].
    #[inline]
    fn encode(hit: Option<Hit>) -> u32 {
        match hit {
            None => NO_SLOT,
            Some(Hit::Real(s)) => s,
            Some(Hit::Temp(i)) => i | TEMP_BIT,
        }
    }

    #[inline]
    fn decode(v: u32) -> Option<Hit> {
        if v == NO_SLOT {
            None
        } else if v & TEMP_BIT != 0 {
            Some(Hit::Temp(v & !TEMP_BIT))
        } else {
            Some(Hit::Real(v))
        }
    }

    /// `child`'s candidate, if it is one.
    #[inline]
    fn find(&self, child: &Object) -> Option<Hit> {
        if let Some(cell) = intrusive_slot(child) {
            let s = cell.get();
            if s != NO_SLOT {
                if self.reg.entry(s).has(F_CAND) {
                    return Some(Hit::Real(s));
                }
                if !self.registered_temp.get() {
                    return None;
                }
            }
            // (Of these kinds, only an instance can be a temporary.)
            if !matches!(child, Object::Instance(_)) {
                return None;
            }
        }
        let key = child.payload_addr()? as ObjectId;
        let line = Self::line(key);
        if self.cache_keys[line].get() == key {
            return Self::decode(self.cache_vals[line].get());
        }
        let hit = self.find_uncached(child, key);
        self.cache_keys[line].set(key);
        self.cache_vals[line].set(Self::encode(hit));
        hit
    }

    /// [`Self::find`] past the intrusive slot and the cache.
    fn find_uncached(&self, child: &Object, key: ObjectId) -> Option<Hit> {
        if intrusive_slot(child).is_none() && self.filter.may_contain(key) {
            if let Some(&s) = self.reg.index.get(&key) {
                if self.reg.entry(s).has(F_CAND) {
                    return Some(Hit::Real(s));
                }
                if !self.registered_temp.get() {
                    return None;
                }
            }
        }
        if !may_be_temporary(child) {
            return None;
        }
        let ids = self.temp_ids.borrow();
        if ids.is_empty() {
            return None;
        }
        ids.get(&id_of(child)).map(|&i| Hit::Temp(i as u32))
    }

    /// Whether `obj` has a registry entry.
    fn registered(&self, obj: &Object) -> bool {
        self.reg.slot_of(obj, self.filter).is_some()
    }

    /// Record `child` as the temporary at position `i`.
    fn add_temp(&self, child: &Object, i: usize) {
        if !self.registered_temp.get() && self.registered(child) {
            // The cached "not a candidate" answers for registered
            // objects no longer hold.
            self.registered_temp.set(true);
            for k in &self.cache_keys {
                k.set(0);
            }
        }
        self.temp_ids.borrow_mut().insert(id_of(child), i);
        if let Some(key) = child.payload_addr() {
            let key = key as ObjectId;
            let line = Self::line(key);
            self.cache_keys[line].set(key);
            self.cache_vals[line].set(Self::encode(Some(Hit::Temp(i as u32))));
        }
    }
}

/// Whether [`promotes_temporarily`] can accept `child` (from some parent).
#[inline]
fn may_be_temporary(child: &Object) -> bool {
    matches!(
        child,
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
            | Object::Property(_)
            | Object::List(_)
            | Object::Instance(_)
            | Object::Dict(_)
    )
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
        // A tuple of atomic values can't take part in a cycle (CPython
        // untracks it at creation), and a list of edges or a dict of
        // coordinates holds thousands.
        Object::Tuple(t) => t.iter().any(|x| !is_atomic(x)),
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
    traverse_impl::<false>(obj, visit);
}

/// [`traverse_object`] for a collection's mark walk, whose visitor runs no
/// code and borrows no container: the common containers' cells are read
/// without borrow bookkeeping while nothing holds them mutably, and an
/// instance's class is visited through a handle that takes no reference.
fn traverse_collected(obj: &Object, visit: &mut dyn FnMut(&Object)) {
    traverse_impl::<true>(obj, visit);
}

/// Run `f` on `cell`'s value: a guard-free view when `PEEK` allows one,
/// else a shared borrow (`None` while the cell is borrowed mutably).
#[inline(always)]
fn with_read<const PEEK: bool, T, R>(cell: &RefCell<T>, f: impl FnOnce(&T) -> R) -> Option<R> {
    if PEEK {
        // SAFETY: a `PEEK` traversal's visitor runs no code and borrows no
        // container (see `traverse_collected`), so nothing can borrow the
        // cell mutably while the view lives.
        if let Some(v) = unsafe { cell.peek() } {
            return Some(f(v));
        }
    }
    cell.try_borrow().ok().map(|g| f(&g))
}

#[inline(always)]
fn traverse_impl<const PEEK: bool>(obj: &Object, visit: &mut dyn FnMut(&Object)) {
    match obj {
        Object::List(l) => {
            with_read::<PEEK, _, _>(l, |v| {
                for item in v.iter() {
                    visit(item);
                }
            });
        }
        Object::Tuple(t) => {
            for item in t.iter() {
                visit(item);
            }
        }
        Object::Dict(d) | Object::MappingProxy(d) | Object::SimpleNamespace(d) => {
            with_read::<PEEK, _, _>(d, |m| {
                for (k, v) in m.iter() {
                    visit(&k.0);
                    visit(v);
                }
            });
        }
        Object::MappingProxyObj(inner) => visit(inner),
        Object::Set(s) => {
            with_read::<PEEK, _, _>(s, |m| {
                for k in m.iter() {
                    visit(&k.0);
                }
            });
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
            // SAFETY: as `with_read`'s; the instance keeps its class alive,
            // and the view, which owns no reference, is never dropped.
            match PEEK.then(|| unsafe { i.class.peek() }).flatten() {
                Some(cls) => {
                    if !cls.flags.is_builtin {
                        let view = std::mem::ManuallyDrop::new(Object::Type(unsafe {
                            crate::sync::Rc::from_raw(crate::sync::Rc::as_ptr(cls))
                        }));
                        visit(&view);
                    }
                }
                None => {
                    let cls = i.cls();
                    if !cls.flags.is_builtin {
                        visit(&Object::Type(cls));
                    }
                }
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
                // Split values: the instance's own children. Their names
                // are the class's shared strings, which hold nothing (as
                // CPython's inline values visit only the values).
                with_read::<PEEK, _, _>(i.dict.split_cell(), |split| {
                    for v in split.values() {
                        visit(v);
                    }
                });
            } else if let Some(dict) = i.dict.get_shared() {
                let dict_obj = Object::Dict(dict);
                if is_tracked(&dict_obj) {
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
            with_read::<PEEK, _, _>(&i.slots, |slots| {
                slots.for_each_entry(|k, v| {
                    visit(&k.0);
                    visit(v);
                });
            });
            // A built-in *container* subclass (`class C(list)`, `D(dict)`,
            // `S(set)`, …) keeps its payload in `native`; that container is
            // an internal, separately-untracked detail of the instance, so
            // its elements are the instance's real children. Walk them so
            // the collector sees cycles routed through subclass storage and
            // prompt reclamation can follow such a chain (a leaf `native`
            // like an `int`/`str` subclass simply has no children).
            if let Some(native) = i.native.get() {
                traverse_impl::<PEEK>(native, visit);
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
            if let Some(Ok(slots)) = f.slots_raw.get().map(RefCell::try_borrow) {
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
    for (matches, traverse) in TRAVERSE_TABLE.iter(hook_kind(obj)) {
        if matches(obj) {
            traverse(obj, visit);
        }
    }
}

/// [`HookTable`] kinds: a hook names the kinds of object its `matches` can
/// accept, and only an object of one of them is offered to it.
pub mod hook_kind {
    pub const INSTANCE: u8 = 1;
    pub const GENERATOR: u8 = 2;
    pub const ITER: u8 = 4;
    pub const FRAME: u8 = 8;
    pub const TRACEBACK: u8 = 16;
    pub const OTHER: u8 = 32;
    pub const ANY: u8 = u8::MAX;
}

/// `obj`'s [`hook_kind`].
#[inline]
fn hook_kind(obj: &Object) -> u8 {
    match obj {
        Object::Instance(_) => hook_kind::INSTANCE,
        Object::Generator(_) | Object::Coroutine(_) | Object::AsyncGenerator(_) => {
            hook_kind::GENERATOR
        }
        Object::Iter(_) => hook_kind::ITER,
        Object::Frame(_) => hook_kind::FRAME,
        Object::Traceback(_) => hook_kind::TRACEBACK,
        _ => hook_kind::OTHER,
    }
}

/// A small append-only registry of `(matches, hook)` function-pointer
/// pairs with lock-free reads: hooks are registered at interpreter
/// init and polled on every collection walk, so readers must not take a
/// lock (nor allocate) per object.
const HOOK_TABLE_CAP: usize = 16;

struct HookTable<H: Copy> {
    len: std::sync::atomic::AtomicUsize,
    slots: [std::sync::OnceLock<(fn(&Object) -> bool, H, u8)>; HOOK_TABLE_CAP],
}

impl<H: Copy> HookTable<H> {
    const CAP: usize = HOOK_TABLE_CAP;

    const fn new() -> Self {
        Self {
            len: std::sync::atomic::AtomicUsize::new(0),
            slots: [const { std::sync::OnceLock::new() }; HOOK_TABLE_CAP],
        }
    }

    fn push(&self, kinds: u8, matches: fn(&Object) -> bool, hook: H) {
        let i = self.len.fetch_add(1, Ordering::AcqRel);
        assert!(i < Self::CAP, "too many GC hook registrations");
        let _ = self.slots[i].set((matches, hook, kinds));
    }

    /// The hooks offered an object of [`hook_kind`] `kind`.
    #[inline]
    fn iter(&self, kind: u8) -> impl Iterator<Item = (fn(&Object) -> bool, H)> + '_ {
        let n = self.len.load(Ordering::Acquire).min(Self::CAP);
        // A slot past a registration in flight is still unset; skip it.
        self.slots[..n].iter().filter_map(move |s| {
            s.get()
                .filter(|(_, _, kinds)| kinds & kind != 0)
                .map(|&(m, h, _)| (m, h))
        })
    }
}

static TRAVERSE_TABLE: HookTable<fn(&Object, &mut dyn FnMut(&Object))> = HookTable::new();

/// Register a traverse callback. Called once per Object variant
/// whose fields are not directly visible to `traverse_object`.
pub fn register_traverse(
    matches: fn(&Object) -> bool,
    traverse: fn(&Object, &mut dyn FnMut(&Object)),
) {
    TRAVERSE_TABLE.push(hook_kind::ANY, matches, traverse);
}

/// [`register_traverse`] for a hook whose `matches` only accepts objects
/// of the [`hook_kind`]s in `kinds`.
pub fn register_traverse_for(
    kinds: u8,
    matches: fn(&Object) -> bool,
    traverse: fn(&Object, &mut dyn FnMut(&Object)),
) {
    TRAVERSE_TABLE.push(kinds, matches, traverse);
}

static CLEAR_TABLE: HookTable<fn(&Object)> = HookTable::new();

/// Called from `clear_object_fields` to let a type whose child
/// references live in module-private (or C-managed) memory break its
/// cycles during the collector's clear phase. The companion of
/// [`register_traverse`] (RFC 0044, WS4).
fn run_external_clear(obj: &Object) {
    for (matches, clear) in CLEAR_TABLE.iter(hook_kind(obj)) {
        if matches(obj) {
            clear(obj);
        }
    }
}

/// Register a clear callback, mirroring [`register_traverse`]. Invoked
/// during the collector's clear phase so a matching object can drop the
/// child references it holds outside the VM's view.
pub fn register_clear(matches: fn(&Object) -> bool, clear: fn(&Object)) {
    CLEAR_TABLE.push(hook_kind::ANY, matches, clear);
}

/// [`register_clear`] for a hook whose `matches` only accepts objects of
/// the [`hook_kind`]s in `kinds`.
pub fn register_clear_for(kinds: u8, matches: fn(&Object) -> bool, clear: fn(&Object)) {
    CLEAR_TABLE.push(kinds, matches, clear);
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
                if !slots.is_empty_default() {
                    *slots = crate::types::SlotStorage::default();
                }
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
            if let Some(Ok(mut slots)) = f.slots_raw.get().map(RefCell::try_borrow_mut) {
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

/// [`track`] for a fresh generator, coroutine or async generator: it joins
/// its young set (see [`GcState::nurse`]), or is registered at once.
pub fn track_generator(obj: &Object) {
    let (Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g)) = obj else {
        track(obj);
        return;
    };
    with_state(|s| {
        if !s.nurse(&s.young_gens, g) {
            s.track_now(obj);
        }
    });
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
    with_state(|s| s.untrack(obj));
}

/// Drop the registry entry of the instance at `addr`, which is dying and
/// recorded registry slot `slot` (see [`GcState::forget_instance`]).
#[inline]
pub fn forget_instance(slot: u32, addr: usize) {
    with_state(|s| s.forget_instance(slot, addr));
}

/// [`untrack`] for a registered generator only: one still in a young set
/// stays there. For a finished generator, which holds no frame (and so no
/// references a cycle could run through): finding it among thousands of
/// young entries would cost more than leaving it, and a dead entry is
/// dropped at the next flush.
pub fn untrack_generator(g: &crate::Rc<crate::object::PyGenerator>) {
    let slot = g.gc_slot.get();
    if slot != NO_SLOT {
        let id = crate::Rc::as_ptr(g) as usize as ObjectId;
        with_state(|s| s.untrack_slot(slot, id, false));
    }
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
/// `WP_GC_STATS`: print each collection's population (read once).
fn stats_debug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WP_GC_STATS").is_some())
}

/// `WP_ROOT_DBG`: print each collection's roots (read once).
fn root_debug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WP_ROOT_DBG").is_some())
}

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

/// Convenience: is `obj` currently tracked by the cycle GC?
pub fn is_tracked(obj: &Object) -> bool {
    with_state(|s| s.is_tracked_obj(obj))
}

/// Convenience: claim `obj`'s finalizer (so a later collection
/// won't double-run `__del__`). Returns false if it was already
/// claimed (a tracked object that isn't claims it).
pub fn claim_finalizer(obj: &Object) -> bool {
    with_state(|s| s.claim_finalizer(obj)) != Some(true)
}

/// Convenience: has `id`'s finalizer already run on the current thread?
/// Backs `gc.is_finalized`.
pub fn was_finalized(id: ObjectId) -> bool {
    with_state(|s| s.was_finalized(id))
}

/// Convenience: mark `id`'s finalizer as finished on the current thread's GC
/// (see [`GcState::complete_finalizer`]).
pub fn complete_finalizer(obj: &Object) {
    with_state(|s| s.complete_finalizer(obj));
}

/// Convenience: the live tracked objects with an unrun `__del__` (see
/// [`GcState::finalization_candidates`]).
pub fn finalization_candidates() -> Vec<Object> {
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
    fn registry_entries_stay_compact_and_slots_are_reused() {
        #[cfg(target_pointer_width = "64")]
        assert_eq!(std::mem::size_of::<Entry>(), 32);
        let state = GcState::new();
        let roots: Vec<_> = (0..3)
            .map(|_| Object::Dict(Rc::new(RefCell::new(DictData::default()))))
            .collect();
        for root in &roots {
            state.track_now(root);
        }
        let slab_len = state.reg.borrow().slab.len();
        state.untrack_id(id_of(&roots[1]));
        assert!(!state.is_tracked(id_of(&roots[1])));
        let extra = Object::Dict(Rc::new(RefCell::new(DictData::default())));
        state.track_now(&extra);
        // The freed slot is the new entry's.
        assert_eq!(state.reg.borrow().slab.len(), slab_len);
        assert!(state.is_tracked(id_of(&extra)));
        for obj in [&roots[0], &roots[2], &extra] {
            assert_eq!(placement(&state, obj).map(|p| p.0), Some(0));
        }
    }

    /// `obj`'s generation (or [`GEN_FROZEN`]) and position in its list,
    /// checking that the list holds it there.
    fn placement(state: &GcState, obj: &Object) -> Option<(u8, u32)> {
        let reg = state.reg.borrow();
        let slot = reg.slot_of(obj, &state.tracked_filter)?;
        let e = reg.entry(slot);
        let list = if e.gen == GEN_FROZEN {
            &reg.frozen
        } else {
            &reg.gens[usize::from(e.gen)]
        };
        assert_eq!(list[e.pos as usize], slot);
        Some((e.gen, e.pos))
    }

    #[test]
    fn removals_keep_generation_and_frozen_positions() {
        for frozen in [false, true] {
            let state = GcState::new();
            let roots: Vec<_> = (0..4)
                .map(|_| Object::Dict(Rc::new(RefCell::new(DictData::default()))))
                .collect();
            for root in &roots {
                state.track_now(root);
            }
            if frozen {
                state.freeze_all();
            }
            state.untrack_id(id_of(&roots[0]));
            assert!(!state.is_tracked(id_of(&roots[0])));
            // The last member moved into the vacated position.
            assert_eq!(placement(&state, &roots[3]).unwrap().1, 0);
            state.untrack_id(id_of(&roots[2]));
            for root in [&roots[1], &roots[3]] {
                let (gen, _) = placement(&state, root).unwrap();
                assert_eq!(gen == GEN_FROZEN, frozen);
            }
            assert_eq!(state.freeze_count(), if frozen { 2 } else { 0 });
            state.untrack_id(id_of(&roots[1]));
            state.untrack_id(id_of(&roots[3]));
            assert_eq!(state.freeze_count(), 0);
            assert_eq!(state.population(), 0);
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
                assert_eq!(placement(&state, root), Some((expected, slot as u32)));
            }
        }
        state.freeze_all();
        assert_eq!(state.freeze_count(), roots.len());
        assert_eq!(state.collect(2), 0);
        for root in &roots {
            assert_eq!(placement(&state, root).unwrap().0, GEN_FROZEN);
        }
        state.untrack_id(id_of(&roots[1]));
        assert!(!state.is_tracked(id_of(&roots[1])));
        assert_eq!(state.freeze_count(), 3);
        assert_eq!(placement(&state, &roots[3]).unwrap().1, 1);
        state.unfreeze_all();
        assert_eq!(state.freeze_count(), 0);
        for (slot, index) in [0, 3, 2].into_iter().enumerate() {
            assert_eq!(placement(&state, &roots[index]), Some((0, slot as u32)));
        }
        state.untrack_id(id_of(&roots[3]));
        assert!(!state.is_tracked(id_of(&roots[3])));
        assert_eq!(placement(&state, &roots[2]).unwrap().1, 1);
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
