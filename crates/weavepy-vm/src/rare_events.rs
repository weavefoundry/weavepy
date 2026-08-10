//! RFC 0060 — CPython's per-interpreter "rare event" counters.
//!
//! CPython's specializing interpreter tracks a handful of events that are
//! assumed rare enough to justify global de-optimization when they happen
//! (`pycore_interp.h` `_rare_events`, surfaced through
//! `_testinternalcapi.get_rare_event_counters()` and asserted by
//! `test_optimizer.TestRareEventCounters`). WeavePy counts the same five
//! events at its own equivalent sites:
//!
//! - `set_class` — `obj.__class__ = C` re-points an instance's type.
//! - `set_bases` — `cls.__bases__ = (…)` recomputes an MRO in place.
//! - `set_eval_frame_func` — `_PyInterpreterState_SetEvalFrameFunc`;
//!   WeavePy has no pluggable frame evaluator, so only the
//!   `_testinternalcapi.set_eval_frame_record`/`set_eval_frame_default`
//!   probes themselves count (exactly the calls the test makes).
//! - `builtin_dict` — a mutation of *the* `builtins` module namespace
//!   (CPython installs a dict watcher on `interp->builtins`).
//! - `func_modification` — assignment to a function's `__code__`,
//!   `__defaults__` or `__kwdefaults__`.
//!
//! Counters are process-global atomics: CPython's are per-interpreter, but
//! WeavePy runs one interpreter per process and the counters must be
//! visible across OS threads sharing it.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::object::DictData;
use crate::sync::{Rc, RefCell};

pub const SET_CLASS: usize = 0;
pub const SET_BASES: usize = 1;
pub const SET_EVAL_FRAME_FUNC: usize = 2;
pub const BUILTIN_DICT: usize = 3;
pub const FUNC_MODIFICATION: usize = 4;

/// Dict keys `get_rare_event_counters()` returns, index-aligned with the
/// constants above (CPython's `RARE_EVENT_INTERP_INC` names).
pub const NAMES: [&str; 5] = [
    "set_class",
    "set_bases",
    "set_eval_frame_func",
    "builtin_dict",
    "func_modification",
];

static COUNTERS: [AtomicU64; 5] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Heap address of the interpreter's `builtins` namespace dict, registered
/// at interpreter construction so [`note_dict_mutation`] can identity-test
/// mutation targets with one relaxed load.
static BUILTINS_DICT_ADDR: AtomicUsize = AtomicUsize::new(0);

#[inline]
pub fn bump(event: usize) {
    COUNTERS[event].fetch_add(1, Ordering::Relaxed);
}

pub fn snapshot() -> [u64; 5] {
    std::array::from_fn(|i| COUNTERS[i].load(Ordering::Relaxed))
}

pub fn reset() {
    for c in &COUNTERS {
        c.store(0, Ordering::Relaxed);
    }
}

pub fn register_builtins_dict(d: &Rc<RefCell<DictData>>) {
    BUILTINS_DICT_ADDR.store(Rc::as_ptr(d) as usize, Ordering::Relaxed);
}

/// Called from the centralized dict mutators for every *effective* change
/// (key added/removed/replaced-with-a-different-object). Counts the
/// `builtin_dict` rare event when the mutated dict is the interpreter's
/// builtins namespace — the WeavePy analogue of CPython's
/// `builtins_dict_watcher`.
#[inline]
pub fn note_dict_mutation(d: &RefCell<DictData>) {
    let addr = std::ptr::from_ref::<RefCell<DictData>>(d) as usize;
    if addr == BUILTINS_DICT_ADDR.load(Ordering::Relaxed) {
        bump(BUILTIN_DICT);
    }
}
