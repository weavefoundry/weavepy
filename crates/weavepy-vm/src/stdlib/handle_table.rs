//! Tables of native state that Python objects refer to by an integer
//! `_handle` attribute (compressors, decompressors, hashers).
//!
//! An instance of a class registered with [`watch_owner_class`] lets its
//! entry go as it dies, as CPython frees such an object's state. Each entry
//! also watches the instance that owns it, and the states of instances that
//! died without the hook (a subclass's, one that died on another thread)
//! go at the next insertion once the table has doubled since it was last
//! swept, so a table holds about as many states as its owners have live
//! instances.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::fasthash::FxBuildHasher;
use crate::object::{DictKey, Object};
use crate::sync::{Rc, RefCell, Weak};
use crate::types::{PyInstance, TypeObject};

/// The smallest table that is swept.
const FIRST_SWEEP: usize = 64;

pub(crate) struct HandleTable<S: 'static> {
    states: HashMap<i64, (Weak<PyInstance>, Rc<RefCell<S>>), FxBuildHasher>,
    sweep_at: usize,
}

impl<S: 'static> Default for HandleTable<S> {
    fn default() -> Self {
        HandleTable {
            states: HashMap::with_hasher(FxBuildHasher),
            sweep_at: FIRST_SWEEP,
        }
    }
}

impl<S: 'static> HandleTable<S> {
    /// `state` under `id`, owned by `owner`.
    pub(crate) fn insert(&mut self, id: i64, owner: &Rc<PyInstance>, state: S) {
        if self.states.len() >= self.sweep_at {
            // (A state's own drop runs no code.)
            self.states.retain(|_, (owner, _)| owner.strong_count() > 0);
            self.sweep_at = (self.states.len() * 2).max(FIRST_SWEEP);
        }
        self.states
            .insert(id, (Rc::downgrade(owner), Rc::new(RefCell::new(state))));
    }

    /// The state under `id`.
    pub(crate) fn get(&self, id: i64) -> Option<Rc<RefCell<S>>> {
        self.states.get(&id).map(|(_, state)| state.clone())
    }

    /// Let the state under `id` go (a holder of a clone keeps it until it
    /// lets go too).
    pub(crate) fn remove(&mut self, id: i64) -> Option<Rc<RefCell<S>>> {
        self.states.remove(&id).map(|(_, state)| state)
    }
}

/// The `_handle` attribute of `inst`, read without waiting on its dict.
pub(crate) fn handle_of(inst: &PyInstance) -> Option<i64> {
    match inst
        .dict_cell()
        .try_borrow()
        .ok()?
        .get(&DictKey(Object::from_static("_handle")))?
    {
        Object::Int(id) => Some(*id),
        _ => None,
    }
}

/// How many classes [`watch_owner_class`] takes (others' instances leave
/// their entries to the sweep).
const OWNER_SLOTS: usize = 16;

static OWNERS: AtomicUsize = AtomicUsize::new(0);
static OWNER_CLASS: [AtomicUsize; OWNER_SLOTS] = [const { AtomicUsize::new(0) }; OWNER_SLOTS];
static OWNER_HOOK: [AtomicUsize; OWNER_SLOTS] = [const { AtomicUsize::new(0) }; OWNER_SLOTS];

/// One bit per watched class (see [`owner_bit`]): a dying instance whose
/// class's bit is clear skips the slot scan.
static OWNER_BITS: AtomicU64 = AtomicU64::new(0);

/// A class address's bit in [`OWNER_BITS`].
#[inline]
fn owner_bit(addr: usize) -> u64 {
    1 << ((addr as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58)
}

/// Run `hook` on each dying instance of exactly `cls` (not a subclass's),
/// which lets the instance's table entry go. The class is kept alive from
/// here on, so no other class ever takes its address.
pub(crate) fn watch_owner_class(cls: &Rc<TypeObject>, hook: fn(&PyInstance)) {
    static PINNED: std::sync::Mutex<Vec<Rc<TypeObject>>> = std::sync::Mutex::new(Vec::new());
    let Ok(mut pinned) = PINNED.lock() else {
        return;
    };
    let addr = Rc::as_ptr(cls) as usize;
    let n = OWNERS.load(Ordering::Acquire);
    if n == OWNER_SLOTS || (0..n).any(|i| OWNER_CLASS[i].load(Ordering::Relaxed) == addr) {
        return;
    }
    pinned.push(cls.clone());
    OWNER_HOOK[n].store(hook as usize, Ordering::Relaxed);
    OWNER_CLASS[n].store(addr, Ordering::Relaxed);
    OWNERS.store(n + 1, Ordering::Release);
    OWNER_BITS.fetch_or(owner_bit(addr), Ordering::Release);
}

/// The hook [`watch_owner_class`] registered for the class at `cls_addr`
/// (the dying `inst`'s), if any: one load and a bit test for most classes.
#[inline]
pub(crate) fn on_instance_death(inst: &PyInstance, cls_addr: usize) {
    if OWNER_BITS.load(Ordering::Acquire) & owner_bit(cls_addr) != 0 {
        on_instance_death_slow(inst, cls_addr);
    }
}

#[cold]
#[inline(never)]
fn on_instance_death_slow(inst: &PyInstance, addr: usize) {
    let n = OWNERS.load(Ordering::Acquire);
    for i in 0..n {
        if OWNER_CLASS[i].load(Ordering::Relaxed) == addr {
            let hook = OWNER_HOOK[i].load(Ordering::Relaxed);
            // SAFETY: the slot holds the `fn(&PyInstance)` stored with it.
            let hook: fn(&PyInstance) = unsafe { std::mem::transmute(hook) };
            hook(inst);
            return;
        }
    }
}
