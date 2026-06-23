//! RFC 0021 — adaptive specialization for the bytecode dispatcher.
//!
//! ## Overview
//!
//! Every instruction in a [`weavepy_compiler::CodeObject`] gets a
//! sibling [`weavepy_compiler::InlineCache`] slot. Before entering
//! the generic handler for a hot opcode, the dispatcher consults
//! the slot:
//!
//! - On a known specialized state, the dispatcher takes a
//!   type-specific fast path that skips the dunder-method search,
//!   skips the dict-keyed lookups, and lifts the operands out of
//!   the stack with as little [`Object::clone`] traffic as the
//!   borrow-checker tolerates.
//! - On `Empty`, the dispatcher runs the generic handler, then
//!   inspects the operand types and — if the shape matches a known
//!   specialization — installs that specialization into the cache.
//!   Subsequent dispatches go through the fast path.
//! - On `Cooldown(n)`, the dispatcher runs the generic handler and
//!   decrements `n`. When `n` reaches `0`, the cache returns to
//!   `Empty` and re-attempts specialization on the next dispatch.
//!
//! ## Layout
//!
//! Helpers in this file split into two groups:
//!
//! 1. **`attempt_specialize_*`** — called *after* a generic
//!    handler has run. They inspect the operand types and return
//!    the [`InlineCache`] state to install.
//!
//! 2. **`fast_*` execution helpers** — called when the cache is
//!    in a known specialized state. They perform the guard check
//!    and the fast path. On guard miss they return `false` so the
//!    dispatcher can deopt and run the generic handler.
//!
//! The dispatcher (`Interpreter::step`) wires the two together
//! per opcode.
//!
//! ## Fingerprints
//!
//! For [`InlineCache::LoadAttrInstance`] et al., the cached
//! `type_id` / `module_id` / `globals_id` / `builtins_id` is
//! `Rc::as_ptr(&value) as u64` — a cheap monotonic identity that
//! changes when the underlying allocation does. Address reuse
//! after drop is harmless: the next guard miss deopts and the
//! cache cools down before re-attempting.
//!
//! ## Stats
//!
//! When `WEAVEPY_VM_STATS=1` is set in the environment, the
//! per-opcode counters in [`Stats`] are incremented on every
//! dispatch / hit / miss / specialization event. The counters are
//! a no-op when the env var is unset.

use crate::sync::Rc;
use crate::sync::RefCell;

use weavepy_compiler::{BinOpKind, CompareKind, InlineCache, COOLDOWN};

use crate::object::{DictData, Object, PyIterator};
use crate::types::TypeObject;

// ---------- specialization decisions: BINARY_OP ----------

/// Inspect the operands of a `BINARY_OP` whose generic handler
/// just succeeded and decide whether to install a specialization.
///
/// Returns the [`InlineCache`] to install. Callers should set the
/// cache slot to that value unconditionally; if the inputs don't
/// match any specialization shape this returns
/// [`InlineCache::Cooldown`] so the dispatcher waits before
/// trying again.
pub fn attempt_specialize_binary_op(a: &Object, b: &Object, op: BinOpKind) -> InlineCache {
    use BinOpKind as B;
    use Object as O;
    match (a, b, op) {
        (O::Int(_), O::Int(_), B::Add) => InlineCache::BinOpAddInt,
        (O::Int(_), O::Int(_), B::Sub) => InlineCache::BinOpSubInt,
        (O::Int(_), O::Int(_), B::Mult) => InlineCache::BinOpMulInt,
        (O::Float(_), O::Float(_), B::Add) => InlineCache::BinOpAddFloat,
        (O::Float(_), O::Float(_), B::Sub) => InlineCache::BinOpSubFloat,
        (O::Float(_), O::Float(_), B::Mult) => InlineCache::BinOpMulFloat,
        (O::Str(_), O::Str(_), B::Add) => InlineCache::BinOpAddStr,
        _ => InlineCache::Cooldown(COOLDOWN),
    }
}

// ---------- specialization decisions: COMPARE_OP ----------

/// Decide on a [`CompareOp`] specialization. Same shape as
/// [`attempt_specialize_binary_op`].
///
/// All comparison operators (`<`, `<=`, `==`, `!=`, `>`, `>=`)
/// share the same fast path because the comparison kind already
/// rides in the instruction's `arg` field; the cache only needs
/// to know the operand type.
pub fn attempt_specialize_compare_op(a: &Object, b: &Object, _op: CompareKind) -> InlineCache {
    use Object as O;
    match (a, b) {
        (O::Int(_), O::Int(_)) => InlineCache::CompareOpInt,
        (O::Float(_), O::Float(_)) => InlineCache::CompareOpFloat,
        (O::Str(_), O::Str(_)) => InlineCache::CompareOpStr,
        _ => InlineCache::Cooldown(COOLDOWN),
    }
}

// ---------- specialization decisions: LOAD_ATTR ----------

/// Decide on a `LOAD_ATTR` specialization. The `key_idx` argument
/// is the index of `name` in the receiver's attribute dict; the
/// fast path uses it to skip the string-keyed hash lookup that the
/// generic handler runs.
///
/// Returns `Empty` (i.e., "don't specialize") for receiver shapes
/// that have a `__getattr__` / `__getattribute__` override or an
/// MRO that we don't yet know how to fingerprint cheaply — those
/// have to keep running through the generic path.
pub fn attempt_specialize_load_attr(obj: &Object, name: &str) -> InlineCache {
    match obj {
        Object::Module(m) => {
            let dict = m.dict.borrow();
            if let Some(idx) = dict.index_of_key_str(name) {
                return InlineCache::LoadAttrModule {
                    module_id: rc_id(&m.dict),
                    key_idx: idx,
                };
            }
            InlineCache::Cooldown(COOLDOWN)
        }
        Object::Instance(inst) => {
            // Only cache when the type doesn't customize lookup.
            // If the class has __getattr__ / __getattribute__ /
            // descriptors, the slow path is mandatory.
            if type_has_attr_override(&inst.cls()) {
                return InlineCache::Cooldown(COOLDOWN);
            }
            // First check the instance dict — that's the
            // `LoadAttrInstance` shape.
            let dict = inst.dict.borrow();
            if let Some(idx) = dict.index_of_key_str(name) {
                return InlineCache::LoadAttrInstance {
                    type_id: rc_id(&inst.cls()),
                    key_idx: idx,
                };
            }
            drop(dict);
            // Otherwise look in the type's dict — the
            // `LoadAttrType` shape (descriptor or class attribute).
            let cls = inst.cls();
            let class_dict = cls.dict.borrow();
            if let Some(idx) = class_dict.index_of_key_str(name) {
                return InlineCache::LoadAttrType {
                    type_id: rc_id(&cls),
                    key_idx: idx,
                };
            }
            InlineCache::Cooldown(COOLDOWN)
        }
        _ => InlineCache::Cooldown(COOLDOWN),
    }
}

// ---------- specialization decisions: LOAD_GLOBAL ----------

/// Decide on a `LOAD_GLOBAL` specialization.
///
/// The fast path takes advantage of two facts:
///
/// 1. The `IndexMap` underneath `DictData` exposes O(1) lookup
///    by integer index once we know the slot. So caching the
///    slot index lets us skip the hash lookup.
/// 2. Builtins and globals are stable across dispatches in steady
///    state. The guard checks the `Rc::as_ptr` of the dict, so
///    if user code clobbers `globals` or rebinds the symbol the
///    next dispatch deopts.
///
/// For `LoadGlobalBuiltin`, we additionally verify that the same
/// name *isn't* shadowed in globals before taking the fast path.
pub fn attempt_specialize_load_global(
    globals: &Rc<RefCell<DictData>>,
    builtins: &Rc<RefCell<DictData>>,
    name: &str,
) -> InlineCache {
    let g = globals.borrow();
    if let Some(idx) = g.index_of_key_str(name) {
        return InlineCache::LoadGlobalModule {
            globals_id: rc_id(globals),
            key_idx: idx,
        };
    }
    drop(g);
    let b = builtins.borrow();
    if let Some(idx) = b.index_of_key_str(name) {
        return InlineCache::LoadGlobalBuiltin {
            builtins_id: rc_id(builtins),
            key_idx: idx,
        };
    }
    InlineCache::Cooldown(COOLDOWN)
}

// ---------- specialization decisions: STORE_ATTR ----------

/// Decide on a `STORE_ATTR` specialization.
///
/// Mirrors [`attempt_specialize_load_attr`] but for the write
/// side. We only specialize when the attribute already exists in
/// the instance dict — i.e., we're updating an existing slot, not
/// creating a new one. (CPython's specialization scheme does the
/// same thing.)
pub fn attempt_specialize_store_attr(obj: &Object, name: &str) -> InlineCache {
    match obj {
        Object::Instance(inst) => {
            let cls = inst.cls();
            if type_has_attr_override(&cls) {
                return InlineCache::Cooldown(COOLDOWN);
            }
            // Never specialize a name that resolves to a class-level data
            // descriptor — a `__slots__` member, a `property`, or any
            // object exposing `__set__`/`__delete__`. Those writes must
            // route through the generic `STORE_ATTR` so the slot side
            // table / property setter fires; the dict-index fast path can
            // only service *plain* instance-dict attributes. (CPython does
            // the same: it uses a distinct `STORE_ATTR_SLOT` for slots and
            // refuses `STORE_ATTR_INSTANCE_VALUE` when a data descriptor
            // shadows the name.) Without this guard a `__slots__` class
            // whose attribute happens to share an index with the dict
            // layout (e.g. `operator.itemgetter` built during interpreter
            // bootstrap) silently writes slot values into `__dict__`,
            // stranding them where the slot descriptor can't read them.
            if name_is_data_descriptor(&cls, name) {
                return InlineCache::Cooldown(COOLDOWN);
            }
            let dict = inst.dict.borrow();
            if let Some(idx) = dict.index_of_key_str(name) {
                return InlineCache::StoreAttrInstance {
                    type_id: rc_id(&cls),
                    key_idx: idx,
                };
            }
            InlineCache::Cooldown(COOLDOWN)
        }
        _ => InlineCache::Cooldown(COOLDOWN),
    }
}

/// True when `name` resolves to a class-level *data descriptor* on
/// `ty`'s MRO: a `__slots__` member, a `property`, or any user
/// descriptor exposing `__set__`/`__delete__`. Such names own their
/// storage (slot side table / setter) and must not be serviced by the
/// instance-dict-index `STORE_ATTR` fast path.
fn name_is_data_descriptor(ty: &Rc<TypeObject>, name: &str) -> bool {
    match ty.lookup(name) {
        Some(Object::SlotDescriptor(_) | Object::Property(_)) => true,
        Some(Object::Instance(desc)) => {
            let dcls = desc.cls();
            dcls.lookup("__set__").is_some() || dcls.lookup("__delete__").is_some()
        }
        _ => false,
    }
}

// ---------- specialization decisions: FOR_ITER ----------

/// Decide on a `FOR_ITER` specialization. The cache stores no
/// fingerprint — the iterator's *kind* is the fingerprint, and
/// it's checked at the start of the fast path against the
/// concrete enum variant.
pub fn attempt_specialize_for_iter(it: &Object) -> InlineCache {
    if let Object::Iter(it) = it {
        match &*it.borrow() {
            PyIterator::List { .. } => InlineCache::ForIterList,
            PyIterator::Tuple { .. } => InlineCache::ForIterTuple,
            PyIterator::Range { .. } => InlineCache::ForIterRange,
            _ => InlineCache::Cooldown(COOLDOWN),
        }
    } else {
        InlineCache::Cooldown(COOLDOWN)
    }
}

// ---------- specialization decisions: UNPACK_SEQUENCE ----------

/// Decide on an `UNPACK_SEQUENCE` specialization.
///
/// Special-cases a two-tuple (`a, b = pair`) because that's by
/// far the most common shape — the inlined two-element push is
/// measurably faster than the general path on benchmark fixtures
/// dominated by tuple destructuring.
pub fn attempt_specialize_unpack_sequence(seq: &Object, n: usize) -> InlineCache {
    match seq {
        Object::Tuple(items) if items.len() == n && n == 2 => InlineCache::UnpackSequenceTwoTuple,
        Object::Tuple(items) if items.len() == n => InlineCache::UnpackSequenceTuple,
        Object::List(xs) if xs.borrow().len() == n => InlineCache::UnpackSequenceList,
        _ => InlineCache::Cooldown(COOLDOWN),
    }
}

// ---------- specialization decisions: CALL ----------

/// Decide on a `CALL` specialization (RFC 0032).
///
/// We only specialize the *exact positional arity, no keywords* shape —
/// the call site supplies precisely `arg_count` positionals and the
/// function declares no `*args`/`**kwargs`/keyword-only parameters. That
/// lets the fast path skip the entire argument-binding pass in
/// `call_python`. Generators/coroutines are excluded (their call returns
/// a suspended object, not a frame result). Functions with cells take
/// the `CallPyExact` shape (still skips binding, but builds cells via
/// `make_frame`); cell-free functions take the leaner `CallPyExactNoFree`.
pub fn attempt_specialize_call(callable: &Object, argc: usize) -> InlineCache {
    match callable {
        Object::Function(f) => {
            let code = f.code();
            if code.is_generator || code.is_coroutine || code.is_async_generator {
                return InlineCache::Cooldown(COOLDOWN);
            }
            if code.has_varargs || code.has_varkeywords || code.kwonly_count != 0 {
                return InlineCache::Cooldown(COOLDOWN);
            }
            // Only the exact-arity shape: anything needing defaults (too
            // few) or *args overflow (too many) keeps the generic path.
            if code.arg_count as usize != argc {
                return InlineCache::Cooldown(COOLDOWN);
            }
            let func_id = rc_id(f);
            let argc = u32::try_from(argc).unwrap_or(u32::MAX);
            if code.cellvars.is_empty() && code.freevars.is_empty() && f.closure.is_empty() {
                InlineCache::CallPyExactNoFree { func_id, argc }
            } else {
                InlineCache::CallPyExact { func_id, argc }
            }
        }
        _ => InlineCache::Cooldown(COOLDOWN),
    }
}

// ---------- shared helpers ----------

/// Cheap fingerprint for an `Rc<T>`. Two clones of the same
/// allocation produce the same value; allocations dropped and
/// later reused at the same address can collide, but the deopt
/// path catches that on the next guard miss.
#[inline]
pub fn rc_id<T>(rc: &Rc<T>) -> u64 {
    Rc::as_ptr(rc) as usize as u64
}

/// Whether a type's MRO defines an attribute-access override that
/// would invalidate the simple "dict slot" fast path. We bail out
/// of LOAD_ATTR / STORE_ATTR specialization for these.
fn type_has_attr_override(ty: &Rc<TypeObject>) -> bool {
    if ty.lookup("__getattr__").is_some() {
        return true;
    }
    // `object.__getattribute__` lives in `object`'s dict as a sentinel, so a
    // bare `is_some()` would match *every* class. Only a genuine user override
    // (anything other than that sentinel) should disable the dict-slot fast
    // path — the default lookup is exactly what the fast path reproduces.
    match ty.lookup("__getattribute__") {
        Some(Object::Builtin(b)) if b.name == ".object_getattribute" => {}
        Some(_) => return true,
        None => {}
    }
    if ty.lookup("__setattr__").is_some() {
        return true;
    }
    false
}

// ---------- per-opcode dispatch stats (`WEAVEPY_VM_STATS=1`) ----------

/// Per-opcode dispatch counters. Updated by the VM hot path when
/// stats are enabled.
#[derive(Debug)]
pub struct Stats {
    /// Total dispatches across all opcodes.
    pub total_dispatches: u64,
    /// Per opcode (indexed by `OpCode as usize`):
    pub specialized_hit: [u64; OPCODE_TABLE_LEN],
    pub specialized_miss: [u64; OPCODE_TABLE_LEN],
    pub specialization_attempts: [u64; OPCODE_TABLE_LEN],
    pub specialization_success: [u64; OPCODE_TABLE_LEN],
    pub specialization_skip: [u64; OPCODE_TABLE_LEN],
}

impl Default for Stats {
    fn default() -> Self {
        // `[u64; N]: Default` only fires for `N <= 32`; we have 256
        // bins (one per `OpCode`), so spell the zero-filled arrays
        // explicitly here.
        Self {
            total_dispatches: 0,
            specialized_hit: [0; OPCODE_TABLE_LEN],
            specialized_miss: [0; OPCODE_TABLE_LEN],
            specialization_attempts: [0; OPCODE_TABLE_LEN],
            specialization_success: [0; OPCODE_TABLE_LEN],
            specialization_skip: [0; OPCODE_TABLE_LEN],
        }
    }
}

/// Plenty for any future opcode set. `OpCode` is `repr(u8)` so
/// 256 covers the address space.
pub const OPCODE_TABLE_LEN: usize = 256;

thread_local! {
    static STATS: RefCell<Stats> = RefCell::new(Stats::default());
    static STATS_ENABLED: bool = std::env::var("WEAVEPY_VM_STATS").is_ok();
}

/// Whether stats collection is enabled for this thread (cached
/// from the env var on first read).
#[inline]
pub fn stats_enabled() -> bool {
    STATS_ENABLED.with(|e| *e)
}

/// Increment the `total_dispatches` counter. No-op when stats
/// are disabled.
#[inline]
pub fn record_dispatch() {
    if !stats_enabled() {
        return;
    }
    STATS.with(|s| s.borrow_mut().total_dispatches += 1);
}

/// Record a successful specialized fast path for an opcode.
#[inline]
pub fn record_hit(op: u8) {
    if !stats_enabled() {
        return;
    }
    STATS.with(|s| s.borrow_mut().specialized_hit[op as usize] += 1);
}

/// Record a guard miss: the cache thought it knew the operand
/// types, but the guard failed and we deopted.
#[inline]
pub fn record_miss(op: u8) {
    if !stats_enabled() {
        return;
    }
    STATS.with(|s| s.borrow_mut().specialized_miss[op as usize] += 1);
}

/// Record an attempt to specialize (the generic path ran and
/// we're considering installing a fast path).
#[inline]
pub fn record_specialize_attempt(op: u8) {
    if !stats_enabled() {
        return;
    }
    STATS.with(|s| s.borrow_mut().specialization_attempts[op as usize] += 1);
}

/// Record that a specialization decision installed a fast-path
/// cache entry.
#[inline]
pub fn record_specialize_success(op: u8) {
    if !stats_enabled() {
        return;
    }
    STATS.with(|s| s.borrow_mut().specialization_success[op as usize] += 1);
}

/// Record that a specialization decision declined to install a
/// fast path (cooldown).
#[inline]
pub fn record_specialize_skip(op: u8) {
    if !stats_enabled() {
        return;
    }
    STATS.with(|s| s.borrow_mut().specialization_skip[op as usize] += 1);
}

/// Snapshot the current stats for the calling thread. Returns a
/// fresh [`Stats`] with the counts at the time of call; the
/// thread-local accumulator is *not* reset.
pub fn snapshot() -> Stats {
    STATS.with(|s| {
        let s = s.borrow();
        Stats {
            total_dispatches: s.total_dispatches,
            specialized_hit: s.specialized_hit,
            specialized_miss: s.specialized_miss,
            specialization_attempts: s.specialization_attempts,
            specialization_success: s.specialization_success,
            specialization_skip: s.specialization_skip,
        }
    })
}

/// Reset the calling thread's stats accumulator. Used by tests
/// that want a clean baseline.
pub fn reset() {
    STATS.with(|s| *s.borrow_mut() = Stats::default());
}

/// Format the snapshot as a markdown table — handy for CI logs
/// and the `WEAVEPY_VM_STATS=1` shutdown print.
pub fn format_stats_markdown(snap: &Stats) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "## VM dispatch stats");
    let _ = writeln!(out);
    let _ = writeln!(out, "Total dispatches: **{}**", snap.total_dispatches);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| op | hits | misses | spec attempts | spec ok | spec skip |"
    );
    let _ = writeln!(
        out,
        "|----|------|--------|---------------|---------|-----------|"
    );
    for op in 0..OPCODE_TABLE_LEN {
        let h = snap.specialized_hit[op];
        let m = snap.specialized_miss[op];
        let a = snap.specialization_attempts[op];
        let ok = snap.specialization_success[op];
        let sk = snap.specialization_skip[op];
        if h == 0 && m == 0 && a == 0 && ok == 0 && sk == 0 {
            continue;
        }
        let _ = writeln!(out, "| {op:#04x} | {h} | {m} | {a} | {ok} | {sk} |");
    }
    out
}

// ---------- dict helpers used by the specializer ----------

trait DictDataExt {
    /// Lookup the integer slot index of `key_str` in the dict.
    /// Returns `None` if the key isn't present.
    fn index_of_key_str(&self, key_str: &str) -> Option<u32>;
}

impl DictDataExt for DictData {
    fn index_of_key_str(&self, key_str: &str) -> Option<u32> {
        let key = crate::object::DictKey(Object::from_str(key_str));
        self.get_full(&key)
            .map(|(idx, _, _)| u32::try_from(idx).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binop_ints_specialize_to_add_int() {
        let a = Object::Int(1);
        let b = Object::Int(2);
        assert_eq!(
            attempt_specialize_binary_op(&a, &b, BinOpKind::Add),
            InlineCache::BinOpAddInt
        );
    }

    #[test]
    fn binop_int_float_does_not_specialize() {
        let a = Object::Int(1);
        let b = Object::Float(2.0);
        assert!(matches!(
            attempt_specialize_binary_op(&a, &b, BinOpKind::Add),
            InlineCache::Cooldown(_)
        ));
    }

    #[test]
    fn compare_op_floats_specialize() {
        let a = Object::Float(1.0);
        let b = Object::Float(2.0);
        assert_eq!(
            attempt_specialize_compare_op(&a, &b, CompareKind::Lt),
            InlineCache::CompareOpFloat
        );
    }

    #[test]
    fn unpack_two_tuple_special_cases() {
        let t = Object::new_tuple(vec![Object::Int(1), Object::Int(2)]);
        assert_eq!(
            attempt_specialize_unpack_sequence(&t, 2),
            InlineCache::UnpackSequenceTwoTuple
        );
    }

    #[test]
    fn unpack_three_tuple_uses_general_tuple_path() {
        let t = Object::new_tuple(vec![Object::Int(1), Object::Int(2), Object::Int(3)]);
        assert_eq!(
            attempt_specialize_unpack_sequence(&t, 3),
            InlineCache::UnpackSequenceTuple
        );
    }
}
