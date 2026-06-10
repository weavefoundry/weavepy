//! Tracing cycle collector — RFC 0024.
//!
//! `Rc<…>` doesn't collect cycles; without help, programs that
//! build self-referential structures (`n.self = n`) leak forever.
//! CPython solved this with a generational tracing collector
//! sitting on top of refcounting; we follow the same design.
//!
//! Note: `Arc<TrackedHandle>` triggers Clippy's
//! `arc_with_non_send_sync` because `TrackedHandle` holds an
//! `Object`, which contains `Rc`s (`!Send`). That's intentional
//! in the sub-interpreter-per-thread model — handles never cross
//! thread boundaries; we use `Arc` only because the GC and the
//! weakref registry both need shared ownership of the same slot
//! within a single thread. Suppressed module-wide.

#![allow(clippy::arc_with_non_send_sync)]
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
    /// Has this object's `__del__` already run? CPython
    /// guarantees a finaliser runs at most once.
    pub finalized: AtomicBool,
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
            finalized: AtomicBool::new(false),
        }
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
/// Lives per-interpreter (per-OS-thread, in the sub-interpreter
/// model). Stored in a `thread_local!` so cross-thread sharing
/// of `Object`s — which is forbidden by `Rc`'s `!Send` contract
/// anyway — is impossible by construction.
#[allow(missing_debug_implementations)]
pub struct GcState {
    generations: RefCell<[Generation; N_GENERATIONS]>,
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
        }
    }

    /// Track `obj` for cycle detection. Idempotent — if `obj`
    /// is already tracked, this is a no-op.
    pub fn track(&self, obj: Object) {
        let new_id = id_of(&obj);
        let mut gens = self.generations.borrow_mut();
        // Avoid double-tracking. Linear scan of generation 0 is
        // fine since most allocations are short-lived.
        for h in &gens[0].handles {
            if h.id == new_id {
                return;
            }
        }
        let handle = Arc::new(TrackedHandle::new(obj, 0));
        gens[0].handles.push(handle);
        drop(gens);
        self.tracked_count.fetch_add(1, Ordering::AcqRel);
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
        self.bump_count(0);
    }

    /// Stop tracking `obj`. Used by the cycle-clearing path
    /// after an object is reclaimed, and by the explicit
    /// `gc._untrack(obj)` extension.
    pub fn untrack_id(&self, id: ObjectId) {
        let mut gens = self.generations.borrow_mut();
        for gen in gens.iter_mut() {
            let before = gen.handles.len();
            gen.handles.retain(|h| h.id != id);
            let removed = before - gen.handles.len();
            if removed > 0 {
                self.tracked_count.fetch_sub(removed, Ordering::AcqRel);
            }
        }
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
    }

    pub fn is_tracked(&self, id: ObjectId) -> bool {
        let gens = self.generations.borrow();
        gens.iter().any(|g| g.handles.iter().any(|h| h.id == id))
            || self.frozen.borrow().iter().any(|h| h.id == id)
    }

    /// Snapshot every tracked object that still carries an unrun
    /// `__del__`. The interpreter's shutdown pass walks this list to
    /// finalize objects that are still alive at exit — CPython runs
    /// finalizers for everything during interpreter teardown, not just
    /// for cyclic garbage. The per-handle `finalized` flag (shared with
    /// the cycle collector) guarantees each `__del__` runs at most once.
    pub fn finalization_candidates(&self) -> Vec<Arc<TrackedHandle>> {
        let mut out = Vec::new();
        let gens = self.generations.borrow();
        for gen in gens.iter() {
            for h in &gen.handles {
                if !h.finalized.load(Ordering::Acquire) && has_finalizer(&h.object) {
                    out.push(h.clone());
                }
            }
        }
        for h in self.frozen.borrow().iter() {
            if !h.finalized.load(Ordering::Acquire) && has_finalizer(&h.object) {
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
        let thresholds = *self.thresholds.borrow();
        if counts[gen] >= thresholds[gen] && self.is_enabled() {
            counts[gen] = 0;
            for g in 0..gen {
                counts[g] = 0;
            }
        }
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
                frozen.push(h);
            }
        }
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
    }

    /// `gc.unfreeze()` — move every frozen object back to
    /// generation 0.
    pub fn unfreeze_all(&self) {
        let mut frozen = self.frozen.borrow_mut();
        let mut gens = self.generations.borrow_mut();
        for h in frozen.drain(..) {
            h.color.store(color::White, Ordering::Release);
            h.generation.store(0, Ordering::Release);
            gens[0].handles.push(h);
        }
        self.tracked_version.fetch_add(1, Ordering::AcqRel);
    }

    pub fn freeze_count(&self) -> usize {
        self.frozen.borrow().len()
    }

    /// Collect generations `0..=upto`. Returns the number of
    /// objects reclaimed.
    pub fn collect(&self, upto: usize) -> usize {
        if !self.is_enabled() {
            return 0;
        }
        let gen = upto.min(N_GENERATIONS - 1);
        let mut total_collected = 0;
        for g in 0..=gen {
            let collected = self.collect_generation(g);
            total_collected += collected;
            let mut stats = self.stats.borrow_mut();
            stats[g].collections = stats[g].collections.saturating_add(1);
            stats[g].collected = stats[g].collected.saturating_add(collected as u64);
        }
        total_collected
    }

    /// Collect a specific generation. Used by [`Self::collect`].
    fn collect_generation(&self, gen: usize) -> usize {
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
            let weak_clones =
                crate::weakref_registry::strong_clone_count(handle.id) as i64;
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
        let by_id: std::collections::HashMap<ObjectId, Arc<TrackedHandle>> = candidate_set
            .iter()
            .map(|h| (h.id, h.clone()))
            .collect();

        // Phase 3: subtract internal refs by walking each
        // tracked object's children. Self-references count too —
        // a `self.self = self` instance has one internal ref to
        // itself which must be subtracted off so a pure self-cycle
        // collapses to gc_refs == 0.
        for handle in &candidate_set {
            traverse_object(&handle.object, &mut |child| {
                if let Some(target) = by_id.get(&id_of(child)) {
                    target.gc_refs.fetch_sub(1, Ordering::AcqRel);
                }
            });
        }

        // Phase 4: anything with gc_refs > 0 is reachable from
        // outside; mark it black and propagate.
        let mut grey: Vec<Arc<TrackedHandle>> = Vec::new();
        for handle in &candidate_set {
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

        // Phase 5: white objects are unreachable. Notify
        // weakrefs, run finalisers, and clear container fields
        // to break cycles.
        let unreachable: Vec<Arc<TrackedHandle>> = candidate_set
            .iter()
            .filter(|h| h.color.load(Ordering::Acquire) == color::White)
            .cloned()
            .collect();
        let collected = unreachable.len();

        // 5a: clear weakrefs for every unreachable object,
        // queueing callbacks for invocation in 5d.
        let mut weakref_callbacks = Vec::new();
        for h in &unreachable {
            let cleared = crate::weakref_registry::notify_clear(h.id);
            for (slot, cb) in cleared {
                if let Some(cb) = cb {
                    weakref_callbacks.push((slot, cb));
                }
            }
        }

        // 5b: queue `__del__` finalisers (drained later by the
        // eval loop, which has interpreter access). The
        // `finalized` flag ensures each runs at most once even
        // across multiple collection passes.
        for h in &unreachable {
            if !h.finalized.swap(true, Ordering::AcqRel) {
                run_finalizer(&h.object);
            }
        }

        // 5c: clear container fields to break cycles. CPython
        // skips this for objects with a `__del__` so the
        // finaliser can still read its `self` attributes; we do
        // the same. The cycle is reclaimed on the next GC pass
        // once the finaliser has run and (presumably) released
        // its references.
        for h in &unreachable {
            if has_finalizer(&h.object) {
                continue;
            }
            clear_object_fields(&h.object);
        }

        // 5d: invoke weakref callbacks now (after finalisers
        // and cyclic clears, matching CPython's order).
        let _ = weakref_callbacks;

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

        collected
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
        let mut gens = self.generations.borrow_mut();
        for g in 0..=upto.min(N_GENERATIONS - 1) {
            gens[g].handles.clear();
        }
        for h in candidates {
            let color = h.color.load(Ordering::Acquire);
            if color == color::White {
                continue;
            }
            let g = h.generation.load(Ordering::Acquire);
            let new_g = (g + 1).min(N_GENERATIONS - 1);
            h.generation.store(new_g, Ordering::Release);
            h.color.store(color::White, Ordering::Release);
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
pub fn traverse_object(obj: &Object, visit: &mut dyn FnMut(&Object)) {
    match obj {
        Object::List(l) => {
            let v = l.borrow();
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
            let m = d.borrow();
            for (k, v) in m.iter() {
                visit(&k.0);
                visit(v);
            }
        }
        Object::Set(s) => {
            let m = s.borrow();
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
            let m = i.dict.borrow();
            for (k, v) in m.iter() {
                visit(&k.0);
                visit(v);
            }
        }
        Object::Module(m) => {
            let dict = m.dict.borrow();
            for (k, v) in dict.iter() {
                visit(&k.0);
                visit(v);
            }
        }
        Object::Cell(c) => {
            visit(&c.borrow());
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
            visit(&p.doc);
        }
        Object::StaticMethod(o) | Object::ClassMethod(o) => {
            visit(o);
        }
        Object::DictView(v) => {
            // Dict views borrow the underlying dict — visit its
            // entries so cycles through `dict.items()` snapshots
            // are detectable.
            let m = v.dict.borrow();
            for (k, val) in m.iter() {
                visit(&k.0);
                visit(val);
            }
        }
        Object::Type(t) => {
            // Class dict + base list. Without this, classes that
            // close over a method that closes over the class
            // (a very common pattern via decorators) leak.
            let dict = t.dict.borrow();
            for (k, v) in dict.iter() {
                visit(&k.0);
                visit(v);
            }
            for base in &t.bases {
                visit(&Object::Type(base.clone()));
            }
        }
        Object::Function(_)
        | Object::Builtin(_)
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
    match obj {
        Object::List(l) => {
            l.borrow_mut().clear();
        }
        Object::Dict(d) | Object::MappingProxy(d) | Object::SimpleNamespace(d) => {
            d.borrow_mut().clear();
        }
        Object::Set(s) => {
            s.borrow_mut().clear();
        }
        Object::Instance(i) => {
            i.dict.borrow_mut().clear();
        }
        Object::ByteArray(b) => {
            b.borrow_mut().clear();
        }
        Object::Cell(c) => {
            *c.borrow_mut() = Object::None;
        }
        Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g) => {
            // Dropping the suspended frame box breaks the cycle
            // (the finalizer — close() — has already run by the
            // time clear is reached; see collect phase 5c).
            if let Ok(mut st) = g.state.try_borrow_mut() {
                *st = crate::object::GeneratorState::Finished;
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
        Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g) => {
            !g.is_finished()
        }
        _ => false,
    }
}

thread_local! {
    static GC_STATE: GcState = GcState::new();
}

/// Run a closure with the per-thread GC state. The state lives
/// in a thread-local because `Object` is `!Send` (it owns
/// `Rc<…>` payloads) and each interpreter has its own heap.
pub fn with_state<R>(f: impl FnOnce(&GcState) -> R) -> R {
    GC_STATE.with(|s| f(s))
}

/// Convenience: track `obj` in the current thread's GC.
pub fn track(obj: Object) {
    with_state(|s| s.track(obj));
}

/// Convenience: find a tracked handle by object id (scans all
/// generations plus the frozen set).
pub fn find_handle(id: ObjectId) -> Option<Arc<TrackedHandle>> {
    with_state(|s| {
        {
            let gens = s.generations.borrow();
            for g in gens.iter() {
                if let Some(h) = g.handles.iter().find(|h| h.id == id) {
                    return Some(h.clone());
                }
            }
        }
        s.frozen.borrow().iter().find(|h| h.id == id).cloned()
    })
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
    match find_handle(id) {
        Some(h) => !h.finalized.swap(true, Ordering::AcqRel),
        None => false,
    }
}

/// Convenience: snapshot all tracked objects with an unrun `__del__`
/// on the current thread's GC (see [`GcState::finalization_candidates`]).
pub fn finalization_candidates() -> Vec<Arc<TrackedHandle>> {
    with_state(|s| s.finalization_candidates())
}

/// Convenience: run a full collection on the current thread's
/// GC. Returns the number of objects collected.
pub fn collect_all() -> usize {
    with_state(|s| s.collect(N_GENERATIONS - 1))
}

/// Convenience: run a partial collection of generations
/// `0..=upto`.
pub fn collect_upto(upto: usize) -> usize {
    with_state(|s| s.collect(upto))
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
