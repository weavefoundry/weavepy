//! Set algebra on the tables themselves.
//!
//! A `set` operation compares one operand's keys with the other's. Every
//! stored key carries its Python hash, so nothing here hashes a key again
//! (no `__hash__` runs), and a pair of keys with equal hashes compares
//! natively or by identity; only a pair that needs a Python `__eq__` is
//! left over, and inside [`pure`] that comparison is deferred: it answers
//! "not equal" and marks the result undecided. So each function below
//! runs no Python code under [`pure`], reads its operands in place (no
//! snapshot of either), and walks only the side the operation has to
//! walk: the smaller one for an intersection, a subset test or a
//! disjointness test, the right one for an in-place difference. A caller
//! takes the answer when [`pure`] decides it and runs its general path,
//! which snapshots the operands and lets comparisons run Python,
//! otherwise.
//!
//! Which elements a result holds, and in what order, follows CPython's
//! `setobject.c`: an intersection keeps the walked (smaller) operand's
//! elements, the right one's on a tie; a union and a difference keep the
//! left operand's.

use crate::dictmap::{Bucket, SetMap};
use crate::object::{DictKey, Object};
use weavepy_compiler::{BinOpKind, CompareKind};

/// Run `f` with Python key comparisons deferred: its result, or `None`
/// when some comparison needed Python (the result is then meaningless,
/// and `f` must have changed nothing).
#[inline]
pub(crate) fn pure<R>(f: impl FnOnce() -> R) -> Option<R> {
    let (out, deferred) = crate::object::with_key_eq_deferred(f);
    (!deferred).then_some(out)
}

/// `f` on the table of an exact `set` (if nothing is changing it) or
/// `frozenset`; `None` for anything else.
#[inline]
pub(crate) fn with_set<R>(o: &Object, f: impl FnOnce(&SetMap) -> R) -> Option<R> {
    match o {
        Object::Set(s) => Some(f(&*s.try_borrow().ok()?)),
        Object::FrozenSet(s) => Some(f(s)),
        _ => None,
    }
}

/// [`with_set`] for two operands at once (the same set twice is fine).
#[inline]
pub(crate) fn with_sets<R>(
    a: &Object,
    b: &Object,
    f: impl FnOnce(&SetMap, &SetMap) -> R,
) -> Option<R> {
    with_set(a, |x| with_set(b, |y| f(x, y)))?
}

#[inline(always)]
fn has(s: &SetMap, e: &Bucket<()>) -> bool {
    s.contains_hashed(e.hash, &e.key)
}

/// Whether every key of `a` is in `b`.
pub(crate) fn subset(a: &SetMap, b: &SetMap) -> bool {
    a.len() <= b.len() && a.as_entries().iter().all(|e| has(b, e))
}

/// Whether the two hold the same keys.
pub(crate) fn equal(a: &SetMap, b: &SetMap) -> bool {
    a.len() == b.len() && subset(a, b)
}

/// Whether no key is in both.
pub(crate) fn disjoint(a: &SetMap, b: &SetMap) -> bool {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    !small.as_entries().iter().any(|e| has(big, e))
}

/// `a <op> b` as CPython's `set_richcompare` answers it.
pub(crate) fn compare(a: &SetMap, b: &SetMap, op: CompareKind) -> bool {
    match op {
        CompareKind::Eq => equal(a, b),
        CompareKind::NotEq => !equal(a, b),
        CompareKind::LtE => subset(a, b),
        CompareKind::Lt => a.len() < b.len() && subset(a, b),
        CompareKind::GtE => subset(b, a),
        CompareKind::Gt => a.len() > b.len() && subset(b, a),
    }
}

/// `a | b`: `a`'s keys, then `b`'s that `a` lacks.
pub(crate) fn union(a: &SetMap, b: &SetMap) -> SetMap {
    let mut out = a.clone();
    if !b.is_empty() {
        out.reserve(b.len().div_ceil(2));
        for e in b.as_entries() {
            out.insert_hashed(e.hash, e.key.clone());
        }
    }
    out
}

/// `a & b`: the smaller operand's keys that the other holds (`b`'s on a
/// tie), in the smaller one's order.
pub(crate) fn intersection(a: &SetMap, b: &SetMap) -> SetMap {
    let (small, big) = if b.len() > a.len() { (a, b) } else { (b, a) };
    let mut out = SetMap::new();
    for e in small.as_entries() {
        if has(big, e) {
            out.insert_unique_hashed(e.hash, e.key.clone());
        }
    }
    out
}

/// `a - b`: `a`'s keys that `b` lacks, in order. A `b` much smaller than
/// `a` is looked up in a copy of `a` (whose table is copied whole, with no
/// key probed) and its hits taken out, as CPython's
/// `set_copy_and_difference` does.
pub(crate) fn difference(a: &SetMap, b: &SetMap) -> SetMap {
    if b.len() * 4 < a.len() {
        let mut hits: Vec<usize> = b
            .as_entries()
            .iter()
            .filter_map(|e| a.index_hashed(e.hash, &e.key))
            .collect();
        let mut out = a.clone();
        if hits.len() <= 8 {
            hits.sort_unstable_by(|x, y| y.cmp(x));
            for at in hits {
                out.shift_remove_index(at);
            }
        } else {
            hits.sort_unstable();
            let (mut at, mut next) = (0, 0);
            out.retain(|_| {
                let gone = hits.get(next) == Some(&at);
                at += 1;
                next += usize::from(gone);
                !gone
            });
        }
        return out;
    }
    let mut out = SetMap::new();
    for e in a.as_entries() {
        if !has(b, e) {
            out.insert_unique_hashed(e.hash, e.key.clone());
        }
    }
    out
}

/// `a ^ b`: `a`'s keys that `b` lacks, then `b`'s that `a` lacks.
pub(crate) fn symmetric_difference(a: &SetMap, b: &SetMap) -> SetMap {
    let mut out = difference(a, b);
    for e in b.as_entries() {
        if !has(a, e) {
            out.insert_unique_hashed(e.hash, e.key.clone());
        }
    }
    out
}

/// `a <op> b` for the four set operators; `None` for any other operator.
pub(crate) fn algebra(a: &SetMap, b: &SetMap, op: BinOpKind) -> Option<SetMap> {
    Some(match op {
        BinOpKind::BitOr => union(a, b),
        BinOpKind::BitAnd => intersection(a, b),
        BinOpKind::Sub => difference(a, b),
        BinOpKind::BitXor => symmetric_difference(a, b),
        _ => return None,
    })
}

/// What an in-place operation will do to its left operand, decided with
/// both operands only read (see [`plan`]) and applied by [`Plan::apply`]
/// once that is known to have run no Python.
pub(crate) enum Plan {
    /// Nothing changes.
    Keep,
    /// Every key goes.
    Clear,
    /// The keys at these positions go.
    Remove(Vec<usize>),
    /// The keys at these positions go and these arrive.
    Toggle(Vec<usize>, Vec<(i64, DictKey)>),
    /// These keys arrive (none is present).
    Add(Vec<(i64, DictKey)>),
    /// The table becomes this one.
    Replace(SetMap),
}

/// The change `a <op>= b` makes to `a`: `None` for an operator that isn't
/// a set's. Walks `b`, except where `a` is the smaller side of a
/// difference; an intersection is the one `a & b` makes, elements and
/// all, as CPython's `set_intersection_update` swaps it in.
pub(crate) fn plan(a: &SetMap, b: &SetMap, op: BinOpKind) -> Option<Plan> {
    Some(match op {
        BinOpKind::BitOr => {
            let add: Vec<(i64, DictKey)> = b
                .as_entries()
                .iter()
                .filter(|e| !has(a, e))
                .map(|e| (e.hash, e.key.clone()))
                .collect();
            if add.is_empty() {
                Plan::Keep
            } else {
                Plan::Add(add)
            }
        }
        BinOpKind::Sub => {
            let gone: Vec<usize> = if b.len() <= a.len() {
                b.as_entries()
                    .iter()
                    .filter_map(|e| a.index_hashed(e.hash, &e.key))
                    .collect()
            } else {
                a.as_entries()
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| has(b, e))
                    .map(|(i, _)| i)
                    .collect()
            };
            if gone.is_empty() {
                Plan::Keep
            } else if gone.len() == a.len() {
                Plan::Clear
            } else {
                Plan::Remove(gone)
            }
        }
        BinOpKind::BitAnd => {
            let both = intersection(a, b);
            if both.is_empty() {
                Plan::Clear
            } else {
                Plan::Replace(both)
            }
        }
        BinOpKind::BitXor => {
            let mut gone = Vec::new();
            let mut add = Vec::new();
            for e in b.as_entries() {
                match a.index_hashed(e.hash, &e.key) {
                    Some(at) => gone.push(at),
                    None => add.push((e.hash, e.key.clone())),
                }
            }
            if gone.is_empty() && add.is_empty() {
                Plan::Keep
            } else {
                Plan::Toggle(gone, add)
            }
        }
        _ => return None,
    })
}

impl Plan {
    /// Make the planned change to `a`, the table the plan was made for
    /// and unchanged since. Compares no keys: positions name what goes,
    /// and what arrives is known to be absent. Returns the keys taken
    /// out, for the caller to release once `a` is no longer borrowed.
    pub(crate) fn apply(self, a: &mut SetMap) -> Vec<DictKey> {
        let take = |a: &mut SetMap, mut gone: Vec<usize>| -> Vec<DictKey> {
            // (From the highest position down: each removal moves the
            // last key into the freed place, never one still to go.)
            gone.sort_unstable_by(|x, y| y.cmp(x));
            // (An iterable may name one key twice.)
            gone.dedup();
            gone.into_iter()
                .filter_map(|at| a.swap_remove_index(at))
                .collect()
        };
        match self {
            Plan::Keep => Vec::new(),
            Plan::Clear => a.drain(..).collect(),
            Plan::Remove(gone) => take(a, gone),
            Plan::Toggle(gone, add) => {
                let out = take(a, gone);
                a.reserve(add.len());
                for (hash, key) in add {
                    a.insert_unique_hashed(hash, key);
                }
                out
            }
            Plan::Add(add) => {
                a.reserve(add.len());
                for (hash, key) in add {
                    a.insert_unique_hashed(hash, key);
                }
                Vec::new()
            }
            Plan::Replace(new) => std::mem::replace(a, new).into_iter().collect(),
        }
    }
}

/// `dst <op>= src` in place, for a `set` and a `set` or `frozenset` whose
/// keys compare without Python: `true` when done, `false` (nothing
/// touched) for the caller's general path. An operand with itself is
/// settled by the operator alone.
pub(crate) fn update(dst: &Object, src: &Object, op: BinOpKind) -> bool {
    let Object::Set(cell) = dst else {
        return false;
    };
    if crate::builtins::set_ops_pending() {
        return false;
    }
    let same = matches!(src, Object::Set(other) if crate::sync::Rc::ptr_eq(cell, other));
    let planned = if same {
        match op {
            // (`a &= a` keeps `a`'s own elements, as `a & a` does.)
            BinOpKind::BitOr | BinOpKind::BitAnd => Some(Plan::Keep),
            BinOpKind::Sub | BinOpKind::BitXor => Some(Plan::Clear),
            _ => None,
        }
    } else {
        with_sets(dst, src, |a, b| pure(|| plan(a, b, op)))
            .flatten()
            .flatten()
    };
    let Some(planned) = planned else {
        return false;
    };
    if matches!(planned, Plan::Keep) {
        return true;
    }
    let Ok(mut a) = cell.try_borrow_mut() else {
        return false;
    };
    let gone = planned.apply(&mut a);
    drop(a);
    drop(gone);
    true
}
