//! Per-object weak reference registry — RFC 0024.
//!
//! See [`crate::gc_trace`] for the `Arc<…>` rationale: handles are
//! shared with the cycle collector, both of which live in the same
//! `thread_local!`, so the lack of `Send + Sync` on the inner
//! `Object` is intentional.

#![allow(clippy::arc_with_non_send_sync)]
//!
//! Real Python weak references have a couple of contracts CPython
//! programs rely on:
//!
//! - `weakref.ref(obj)()` returns `obj` while it's reachable, and
//!   `None` once `obj` has been collected.
//! - The associated callback runs *after* the object dies but
//!   *before* memory is reclaimed.
//! - `WeakValueDictionary` / `WeakKeyDictionary` / `WeakSet`
//!   self-clean when their referents die.
//!
//! WeavePy's object model is `Rc`-rooted, so cycles aren't
//! collected by `Drop` — we have a separate cycle GC for that
//! (see [`crate::gc_trace`]). To make weak references work
//! correctly we maintain a per-object weakref list keyed by
//! object identity. The list is populated by
//! `weakref.ref(obj)`/`weakref.proxy(obj)`/etc. and walked
//! during the cycle GC's clear phase.
//!
//! A complementary "drop-driven" path uses a `Drop` impl on a
//! sentinel type embedded inside the object's `Rc`'d payload:
//! when the last strong reference dies, the sentinel's drop
//! signals the registry to clear and notify its weakrefs. The
//! `Object` enum doesn't natively support this (existing
//! variants are flat enums), so the registry exposes an
//! explicit `notify_clear(id)` method that callers (the GC,
//! `gc.collect`, finaliser code) can invoke.

use crate::sync::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use crate::object::Object;

/// Identity of a referent. We use the address of an
/// `Rc::as_ptr`'d allocation as the key; the Rc keeps the
/// allocation alive, so the address is stable.
pub type ObjectId = u64;

/// A live weak reference. The Rc'd `target` is gradually nulled
/// out when [`WeakRefRegistry::notify_clear`] runs for the
/// referent's id.
///
/// In the sub-interpreter-per-thread model these slots are
/// per-interpreter (i.e. effectively per-OS-thread) so we use
/// `RefCell` rather than `Mutex` — the registry lives in a
/// thread-local and is never shared across threads.
#[allow(missing_debug_implementations)]
pub struct WeakRefSlot {
    /// Identity of the referent. Used to look up the registry
    /// list when the referent dies.
    pub target_id: ObjectId,
    /// `Some(strong_clone_of_target)` while the referent is
    /// alive. Set to `None` by `notify_clear`.
    pub target: RefCell<Option<Object>>,
    /// `__callback__`. Stored as an `Object` so the user's
    /// callable can be invoked through the normal call path.
    /// `None` if no callback was passed to `weakref.ref`.
    pub callback: RefCell<Option<Object>>,
    /// Cached `id(referent)` so the weakref's `__hash__`
    /// remains stable across the referent's life.
    pub identity_hash: i64,
    /// Has the referent been cleared?
    pub dead: AtomicBool,
    /// Type tag used for `isinstance(w, weakref.ProxyType)`
    /// distinction: 0 = ref, 1 = proxy, 2 = callable proxy.
    pub kind: u8,
    /// Back-pointer to the user-visible weakref object built around
    /// this slot. Weak so the slot doesn't keep the Python wrapper
    /// alive; lets `obj.__weakref__` / `weakref.getweakrefs` return
    /// the *same* object the user holds.
    pub py_ref: RefCell<Option<crate::sync::Weak<crate::types::PyInstance>>>,
}

/// Weakref kinds as exposed to Python. Numeric so the field
/// fits in a `u8`.
pub mod kind {
    pub const REF: u8 = 0;
    pub const PROXY: u8 = 1;
    pub const CALLABLE_PROXY: u8 = 2;
}

impl WeakRefSlot {
    pub fn new(target_id: ObjectId, target: Object, callback: Option<Object>, kind: u8) -> Self {
        Self {
            target_id,
            target: RefCell::new(Some(target.clone())),
            callback: RefCell::new(callback),
            identity_hash: target_id as i64,
            dead: AtomicBool::new(false),
            kind,
            py_ref: RefCell::new(None),
        }
    }

    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }

    pub fn upgrade(&self) -> Option<Object> {
        if self.is_dead() {
            return None;
        }
        self.target.borrow().clone()
    }

    /// Clear the slot. Returns the callback (if any) so the
    /// caller can invoke it on the calling thread.
    pub fn clear(&self) -> Option<Object> {
        if self.dead.swap(true, Ordering::AcqRel) {
            return None;
        }
        *self.target.borrow_mut() = None;
        self.callback.borrow_mut().take()
    }

    pub fn callback(&self) -> Option<Object> {
        self.callback.borrow().clone()
    }
}

/// Per-id registry. Stored as `BTreeMap<ObjectId, Vec<Weak<WeakRefSlot>>>`
/// so the GC can iterate efficiently and dead slots can be
/// pruned in place.
///
/// **Process-global** (see [`REGISTRY`]), mirroring the cycle
/// collector ([`crate::gc_trace`]). RFC 0025 made the whole VM heap
/// `Arc`-rooted: an `Object` allocated on one OS thread can be
/// referenced — and weakly referenced — from another (a
/// `ThreadPoolExecutor` shared with its worker threads is the
/// canonical case). A per-thread registry could therefore never
/// account for, or clear, a weakref whose referent dies on a thread
/// other than the one that created the weakref, so cross-thread
/// finalizers (e.g. `weakref.ref(executor, cb)` → `cb` posting a
/// shutdown sentinel) would never fire. Every mutation happens under
/// the GIL, so the shared map's `GilCell` is effectively uncontended.
#[derive(Default)]
#[allow(missing_debug_implementations)]
pub struct WeakRefRegistry {
    inner: RefCell<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    /// Map from referent id -> list of weakref slots that
    /// observe it. Outer Vec is dense (we shrink-to-fit on
    /// notify); the inner `Weak` is so the slot itself is
    /// freed when no Python reference points at the weakref.
    slots: std::collections::BTreeMap<ObjectId, Vec<Weak<WeakRefSlot>>>,
    /// Bumped on every register call. Used as a cheap
    /// "version" counter so cache-invalidation paths can know
    /// when to re-scan.
    version: u64,
}

impl WeakRefRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh slot. The caller stores the returned
    /// `Arc<WeakRefSlot>` inside the user-visible `Object::Weak`
    /// variant; the registry only retains a `Weak<…>`.
    pub fn register(&self, slot: Arc<WeakRefSlot>) {
        let mut g = self.inner.borrow_mut();
        g.version = g.version.wrapping_add(1);
        let entry = g.slots.entry(slot.target_id).or_default();
        entry.retain(|w| w.strong_count() > 0);
        entry.push(Arc::downgrade(&slot));
    }

    /// Walk all slots watching `id`, clear them, and return the
    /// list of `(weakref_object, callback_object)` pairs the
    /// caller should invoke. Removes the entry for `id`.
    pub fn notify_clear(&self, id: ObjectId) -> Vec<(Arc<WeakRefSlot>, Option<Object>)> {
        let entries = {
            let mut g = self.inner.borrow_mut();
            g.slots.remove(&id).unwrap_or_default()
        };
        let mut out = Vec::with_capacity(entries.len());
        // CPython fires a referent's weakref callbacks newest-first (its
        // weakref list inserts at the head), so a chain of
        // `weakref.finalize(obj, …)` runs in reverse order of creation
        // (`test_weakref` `FinalizeTestCase.test_order`). The registry keeps
        // them in registration order, so walk it in reverse here.
        for w in entries.into_iter().rev() {
            if let Some(slot) = w.upgrade() {
                let cb = slot.clear();
                out.push((slot, cb));
            }
        }
        out
    }

    /// Number of live weakrefs targeting `id`.
    pub fn count(&self, id: ObjectId) -> usize {
        let g = self.inner.borrow();
        g.slots
            .get(&id)
            .map(|v| v.iter().filter(|w| w.strong_count() > 0).count())
            .unwrap_or(0)
    }

    /// How many *strong clones* of the referent the registry is
    /// currently holding for `id`. Every live, un-cleared slot keeps
    /// one `Object` clone of its target alive (see
    /// [`WeakRefSlot::target`]). The cycle collector subtracts this
    /// from an object's outer refcount so a weakref does **not** keep
    /// its referent reachable — otherwise `weakref.ref(obj)()` would
    /// stay live forever and `WeakKeyDictionary`/`WeakValueDictionary`
    /// would never self-clean after `del obj; gc.collect()`.
    pub fn strong_clone_count(&self, id: ObjectId) -> usize {
        let g = self.inner.borrow();
        g.slots
            .get(&id)
            .map(|v| {
                v.iter()
                    .filter_map(Weak::upgrade)
                    .filter(|s| !s.is_dead() && s.target.borrow().is_some())
                    .count()
            })
            .unwrap_or(0)
    }

    /// Snapshot the live weakrefs targeting `id` as
    /// `Arc<WeakRefSlot>` values. Used by
    /// `_weakref.getweakrefs(obj)`.
    pub fn collect_strong(&self, id: ObjectId) -> Vec<Arc<WeakRefSlot>> {
        let g = self.inner.borrow();
        match g.slots.get(&id) {
            Some(list) => list.iter().filter_map(|w| w.upgrade()).collect(),
            None => Vec::new(),
        }
    }

    /// Total number of registered weakrefs across all ids.
    pub fn total_alive(&self) -> usize {
        let g = self.inner.borrow();
        g.slots
            .values()
            .map(|v| v.iter().filter(|w| w.strong_count() > 0).count())
            .sum()
    }

    /// Drop empty entries. Useful as a maintenance tick after
    /// a GC pass.
    pub fn shrink(&self) {
        let mut g = self.inner.borrow_mut();
        g.slots.retain(|_, v| {
            v.retain(|w| w.strong_count() > 0);
            !v.is_empty()
        });
    }

    /// Snapshot one live target `Object` per watched id. Feeds the
    /// collector's weakref-only sweep for referents that aren't in the
    /// tracked set (functions, methods, …): if the only remaining
    /// strong references to a target are the slots' own clones, the
    /// object is unreachable from Python and its weakrefs must clear.
    pub fn targets(&self) -> Vec<(ObjectId, Object)> {
        let g = self.inner.borrow();
        g.slots
            .iter()
            .filter_map(|(id, v)| {
                v.iter()
                    .filter_map(Weak::upgrade)
                    .find_map(|s| s.target.borrow().clone())
                    .map(|t| (*id, t))
            })
            .collect()
    }

    pub fn version(&self) -> u64 {
        self.inner.borrow().version
    }
}

/// The process-global weakref registry. Shared across every OS
/// thread, exactly like [`crate::gc_trace`]'s `GC_STATE`: weak
/// references must observe and clear referents regardless of which
/// thread allocated the weakref or which thread drops the referent's
/// last strong reference (RFC 0025's `Arc`-rooted, thread-shared
/// heap). The interior `GilCell` makes each borrow memory-safe; the
/// GIL serializes mutations so the lock is effectively uncontended.
/// Never dropped (statics have no drop glue).
static REGISTRY: std::sync::LazyLock<WeakRefRegistry> =
    std::sync::LazyLock::new(WeakRefRegistry::new);

/// Run a closure with the process-global weakref registry. Used by
/// helper free functions in this module.
pub fn with_registry<R>(f: impl FnOnce(&WeakRefRegistry) -> R) -> R {
    f(&REGISTRY)
}

/// Convenience: register a slot in the current thread's
/// registry.
pub fn register(slot: Arc<WeakRefSlot>) {
    with_registry(|r| r.register(slot));
}

/// Convenience: notify the current thread's registry that `id`
/// has died.
pub fn notify_clear(id: ObjectId) -> Vec<(Arc<WeakRefSlot>, Option<Object>)> {
    with_registry(|r| r.notify_clear(id))
}

/// Queue every callback from a `notify_clear` result for invocation at
/// the next interpreter safe point. The callback argument is the
/// user-visible weakref object (recovered through the slot's `py_ref`
/// back-pointer; `None` if the wrapper itself is already gone — in that
/// case the callback is dropped, matching CPython, which clears a dead
/// ref's callback without calling it).
pub fn queue_callbacks(cleared: Vec<(Arc<WeakRefSlot>, Option<Object>)>) {
    for (slot, cb) in cleared {
        let Some(cb) = cb else { continue };
        let wr = slot
            .py_ref
            .borrow()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .map(Object::Instance);
        if let Some(wr) = wr {
            crate::vm_singletons::push_pending_weakref_callback(cb, wr);
        }
    }
}

/// Convenience: weakref count for `id` in the current thread.
pub fn count_for(id: ObjectId) -> usize {
    with_registry(|r| r.count(id))
}

/// Convenience: count of registry-held strong clones of `id` in the
/// current thread. Used by the cycle collector's refcount accounting.
pub fn strong_clone_count(id: ObjectId) -> usize {
    with_registry(|r| r.strong_clone_count(id))
}

/// Convenience: collect every live weakref for `id` in the
/// current thread.
pub fn collect_for(id: ObjectId) -> Vec<Arc<WeakRefSlot>> {
    with_registry(|r| r.collect_strong(id))
}

/// Allocate a fresh synthetic object id. Used for objects that
/// don't have a stable address (synthetic Rust-side
/// constructions like `Object::Weak` itself).
pub fn next_synthetic_id() -> ObjectId {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 32);
    NEXT.fetch_add(1, Ordering::AcqRel)
}

/// Compute a stable identity for an `Object`. Mirrors
/// `id(obj)` semantics from Python — two `Object` clones of
/// the same `Rc` produce the same id; `Object::Int(5)` and a
/// freshly-constructed `Object::Int(5)` *also* produce the
/// same id because small ints are interned.
pub fn id_of(obj: &Object) -> ObjectId {
    use crate::sync::Rc;
    match obj {
        Object::None => 1,
        Object::Unbound => 1,
        Object::Bool(false) => 2,
        Object::Bool(true) => 3,
        Object::Int(n) => 0x1000_0000_0000_0000u64 ^ (*n as u64),
        Object::Float(f) => 0x2000_0000_0000_0000u64 ^ f.to_bits(),
        Object::Str(s) => Rc::as_ptr(s).cast::<()>() as usize as u64,
        Object::WStr(cps) => Rc::as_ptr(cps).cast::<()>() as usize as u64,
        Object::Bytes(b) => Rc::as_ptr(b).cast::<()>() as usize as u64,
        Object::Tuple(t) => Rc::as_ptr(t).cast::<()>() as usize as u64,
        Object::List(l) => Rc::as_ptr(l) as usize as u64,
        Object::Dict(d) => Rc::as_ptr(d) as usize as u64,
        Object::Set(s) => Rc::as_ptr(s) as usize as u64,
        Object::FrozenSet(s) => Rc::as_ptr(s) as usize as u64,
        Object::ByteArray(b) => Rc::as_ptr(b) as usize as u64,
        Object::Function(f) => Rc::as_ptr(f) as usize as u64,
        Object::Builtin(b) => Rc::as_ptr(b) as usize as u64,
        Object::BoundMethod(b) => Rc::as_ptr(b) as usize as u64,
        Object::Code(c) => Rc::as_ptr(c) as usize as u64,
        Object::Type(t) => Rc::as_ptr(t) as usize as u64,
        Object::Instance(i) => Rc::as_ptr(i) as usize as u64,
        Object::Module(m) => Rc::as_ptr(m) as usize as u64,
        Object::Generator(g) => Rc::as_ptr(g) as usize as u64,
        Object::Coroutine(g) => Rc::as_ptr(g) as usize as u64,
        Object::AsyncGenerator(g) => Rc::as_ptr(g) as usize as u64,
        Object::AsyncGenAwait(a) => Rc::as_ptr(a) as usize as u64,
        Object::Iter(i) => Rc::as_ptr(i) as usize as u64,
        Object::Range(r) => Rc::as_ptr(r) as usize as u64,
        Object::Cell(c) => Rc::as_ptr(c) as usize as u64,
        Object::Slice(s) => Rc::as_ptr(s) as usize as u64,
        Object::File(f) => Rc::as_ptr(f) as usize as u64,
        Object::Property(p) => Rc::as_ptr(p) as usize as u64,
        Object::StaticMethod(s) => Rc::as_ptr(s) as usize as u64,
        Object::ClassMethod(c) => Rc::as_ptr(c) as usize as u64,
        Object::SlotDescriptor(s) => Rc::as_ptr(s) as usize as u64,
        Object::Frame(f) => Rc::as_ptr(f) as usize as u64,
        Object::Traceback(t) => Rc::as_ptr(t) as usize as u64,
        Object::MemoryView(m) => Rc::as_ptr(m) as usize as u64,
        Object::MappingProxy(d) => Rc::as_ptr(d) as usize as u64,
        Object::DictView(v) => Rc::as_ptr(v) as usize as u64,
        Object::SimpleNamespace(d) => Rc::as_ptr(d) as usize as u64,
        Object::LazyIter(l) => Rc::as_ptr(l) as usize as u64,
        Object::Capsule(c) => Rc::as_ptr(c) as usize as u64,
        Object::Long(b) => Rc::as_ptr(b) as usize as u64,
        Object::Complex(c) => Rc::as_ptr(c) as usize as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_register_and_clear() {
        let reg = WeakRefRegistry::new();
        let slot = Arc::new(WeakRefSlot::new(42, Object::Int(7), None, kind::REF));
        reg.register(slot.clone());
        assert_eq!(reg.count(42), 1);
        let cleared = reg.notify_clear(42);
        assert_eq!(cleared.len(), 1);
        assert!(slot.is_dead());
        assert_eq!(reg.count(42), 0);
    }

    #[test]
    fn shrink_drops_dead_slots() {
        let reg = WeakRefRegistry::new();
        {
            let slot = Arc::new(WeakRefSlot::new(99, Object::Int(0), None, kind::REF));
            reg.register(slot);
        }
        reg.shrink();
        assert_eq!(reg.count(99), 0);
    }

    #[test]
    fn thread_local_registry_works() {
        let slot = Arc::new(WeakRefSlot::new(1, Object::Int(0), None, kind::REF));
        register(slot.clone());
        assert_eq!(count_for(1), 1);
        let cleared = notify_clear(1);
        assert_eq!(cleared.len(), 1);
        assert!(slot.is_dead());
    }
}
