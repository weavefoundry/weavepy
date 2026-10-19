//! The insertion-ordered hash table behind every Python `dict`.
//!
//! The layout follows CPython's compact dict: a dense vector of entries in
//! insertion order, each carrying its key's Python hash, plus a sparse
//! table of slots that point into it. A table of at most [`SMALL`] entries
//! has no slots at all: a probe scans the entries, comparing hashes first,
//! so the commonest dicts (keyword arguments, small literals, instance
//! dictionaries) are one allocation.
//!
//! A slot is one word: the entry's index plus one in the low half (zero
//! marks an empty slot) and the high half of the mixed hash (the key's
//! Python hash times a Fibonacci constant) above it. The top bits of the
//! mixed hash pick the home slot, and collisions probe linearly; a probe
//! compares the slot's half-hash before it touches an entry, and an entry
//! only when the full stored hash matches, so a key's `__eq__` runs only
//! against keys of equal hash, as in CPython. Removal shifts the run that
//! follows a freed slot back over it, so there are no tombstones.
//!
//! Positions are dense: removing an entry shifts the later ones down, so
//! `get_index(i)` is always the `i`-th key in order (the contract the
//! iterators and the position caches rely on).
//!
//! A lookup takes any [`Probe`]: a stored key's own type, or one of the
//! borrowed probes in [`crate::object`] (a `&str` name, a native-equality
//! leaf probe) that hash and compare without building an `Object`.

use crate::object::{DictKey, Object};
use std::fmt;

/// The most entries a table keeps without slots.
pub const SMALL: usize = 8;

/// The Fibonacci multiplier that mixes a Python hash into slot bits.
const MIX: u64 = 0x9E37_79B9_7F4A_7C15;

/// The low half of a slot: the entry index plus one.
const IX_MASK: u64 = 0xFFFF_FFFF;

/// A stored entry: the key's Python hash, the key, and its value.
#[derive(Clone, Debug)]
pub struct Bucket<V = Object> {
    pub(crate) hash: i64,
    pub key: DictKey,
    pub value: V,
}

/// A key a table can be probed with: its Python hash and its equality with
/// a stored key of the same hash.
pub trait Probe {
    /// The probe's Python hash.
    fn probe_hash(&self) -> i64;
    /// Whether the probe equals `stored` (whose hash equals the probe's).
    fn probe_eq(&self, stored: &DictKey) -> bool;

    /// Whether the probe equals `stored` whatever its hash, when that is
    /// settled natively and costs less than hashing the probe; `None`
    /// when the hashes must be compared first. A small table is scanned
    /// with this before the probe is hashed (a `str` probe whose hash
    /// isn't known compares bytes instead of hashing them).
    #[inline(always)]
    fn probe_eq_unhashed(&self, _stored: &DictKey) -> Option<bool> {
        None
    }
}

impl Probe for DictKey {
    #[inline]
    fn probe_hash(&self) -> i64 {
        crate::object::dict_key_hash(&self.0)
    }

    #[inline]
    fn probe_eq(&self, stored: &DictKey) -> bool {
        self == stored
    }

    #[inline]
    fn probe_eq_unhashed(&self, stored: &DictKey) -> Option<bool> {
        // A new string (its hash not yet computed) compares with the
        // stored strings instead.
        match (&self.0, &stored.0) {
            (Object::Str(a), Object::Str(b))
                if crate::shared_value::SharedStr::known_hash(a).is_none() =>
            {
                Some(a.as_bytes() == b.as_bytes())
            }
            (Object::Str(a), Object::WStr(_))
                if crate::shared_value::SharedStr::known_hash(a).is_none() =>
            {
                Some(false)
            }
            _ => None,
        }
    }
}

impl<P: Probe + ?Sized> Probe for &P {
    #[inline]
    fn probe_hash(&self) -> i64 {
        (**self).probe_hash()
    }

    #[inline]
    fn probe_eq(&self, stored: &DictKey) -> bool {
        (**self).probe_eq(stored)
    }

    #[inline]
    fn probe_eq_unhashed(&self, stored: &DictKey) -> Option<bool> {
        (**self).probe_eq_unhashed(stored)
    }
}

/// Byte equality for key strings, in line: a probe that meets an equal
/// stored string of another object compares a few words instead of
/// calling `memcmp` (keys are mostly short names).
#[inline(always)]
pub fn key_bytes_eq(a: &[u8], b: &[u8]) -> bool {
    let n = a.len();
    if n != b.len() {
        return false;
    }
    // SAFETY: every read is inside both slices (`n` bytes each): the two
    // loads of a width overlap when `n` is below twice it.
    unsafe {
        let (p, q) = (a.as_ptr(), b.as_ptr());
        if n >= 8 {
            if n > 16 {
                return a == b;
            }
            let x = p.cast::<u64>().read_unaligned() ^ q.cast::<u64>().read_unaligned();
            let y = p.add(n - 8).cast::<u64>().read_unaligned()
                ^ q.add(n - 8).cast::<u64>().read_unaligned();
            return (x | y) == 0;
        }
        if n >= 4 {
            let x = p.cast::<u32>().read_unaligned() ^ q.cast::<u32>().read_unaligned();
            let y = p.add(n - 4).cast::<u32>().read_unaligned()
                ^ q.add(n - 4).cast::<u32>().read_unaligned();
            return (x | y) == 0;
        }
        if n >= 2 {
            let x = p.cast::<u16>().read_unaligned() ^ q.cast::<u16>().read_unaligned();
            let y = p.add(n - 2).cast::<u16>().read_unaligned()
                ^ q.add(n - 2).cast::<u16>().read_unaligned();
            return (x | y) == 0;
        }
        n == 0 || *p == *q
    }
}

/// An ordered hash table from [`DictKey`] to `V` (see the module docs):
/// [`DictMap`] for a dict, [`SetMap`] for a set.
pub struct Table<V> {
    entries: Vec<Bucket<V>>,
    /// The slot table: empty for a small table, else a power of two long.
    slots: Box<[u64]>,
    /// `64 - log2(slots.len())`: the shift that takes a mixed hash to its
    /// home slot.
    shift: u32,
}

#[inline(always)]
fn mix(hash: i64) -> u64 {
    (hash as u64).wrapping_mul(MIX)
}

/// The slot count for `n` entries: at most two thirds full.
fn slots_for(n: usize) -> usize {
    (n.max(SMALL + 1) * 3 / 2 + 1).next_power_of_two()
}

impl<V> Default for Table<V> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Clone> Clone for Table<V> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            slots: self.slots.clone(),
            shift: self.shift,
        }
    }
}

impl<V: fmt::Debug> fmt::Debug for Table<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.entries.iter().map(|b| (&b.key, &b.value)))
            .finish()
    }
}

impl<V> Table<V> {
    /// An empty table.
    #[inline]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            slots: Box::new([]),
            shift: 64,
        }
    }

    /// An empty table with room for `n` entries.
    pub fn with_capacity(n: usize) -> Self {
        let mut m = Self {
            entries: Vec::with_capacity(n),
            slots: Box::new([]),
            shift: 64,
        };
        if n > SMALL {
            m.rebuild_slots(slots_for(n));
        }
        m
    }

    /// A table of the `n` entries `entry(i)` gives in order, whose keys are
    /// distinct and each paired with its Python hash, built in one pass:
    /// the entries are laid down, then indexed once (no probe compares a
    /// key).
    #[inline]
    pub fn from_unique_hashed(n: usize, mut entry: impl FnMut(usize) -> (i64, DictKey, V)) -> Self {
        let mut entries: Vec<Bucket<V>> = Vec::with_capacity(n);
        let dst = entries.as_mut_ptr();
        for i in 0..n {
            let (hash, key, value) = entry(i);
            // SAFETY: `i < n`, inside the capacity; the length is set once
            // every slot below `n` is written.
            unsafe { dst.add(i).write(Bucket { hash, key, value }) };
        }
        // SAFETY: the first `n` buckets were written above.
        unsafe { entries.set_len(n) };
        let mut m = Self {
            entries,
            slots: Box::new([]),
            shift: 64,
        };
        if m.entries.len() > SMALL {
            m.rebuild_slots(slots_for(m.entries.len()));
        }
        m
    }

    /// [`Self::with_capacity`]; the hasher is implied (the table mixes
    /// Python hashes itself).
    #[inline]
    pub fn with_capacity_and_hasher(n: usize, _: crate::fasthash::FxBuildHasher) -> Self {
        Self::with_capacity(n)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many entries fit before the entry vector reallocates.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    /// The entries in order.
    #[inline]
    pub fn as_entries(&self) -> &[Bucket<V>] {
        &self.entries
    }

    /// Make room for `additional` more entries.
    pub fn reserve(&mut self, additional: usize) {
        self.entries.reserve(additional);
        self.reserve_slots(additional);
    }

    /// [`Self::reserve`], reporting an allocation failure instead of
    /// aborting.
    pub fn try_reserve(
        &mut self,
        additional: usize,
    ) -> Result<(), std::collections::TryReserveError> {
        self.entries.try_reserve(additional)?;
        self.reserve_slots(additional);
        Ok(())
    }

    fn reserve_slots(&mut self, additional: usize) {
        let want = self.entries.len().saturating_add(additional);
        if want > SMALL && (want * 3 > self.slots.len() * 2) {
            self.rebuild_slots(slots_for(want));
        }
    }

    /// Release spare capacity.
    pub fn shrink_to_fit(&mut self) {
        self.entries.shrink_to_fit();
        if self.entries.len() <= SMALL {
            self.slots = Box::new([]);
            self.shift = 64;
        } else if slots_for(self.entries.len()) < self.slots.len() {
            self.rebuild_slots(slots_for(self.entries.len()));
        }
    }

    /// Remove every entry, keeping the allocations.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.slots.fill(0);
    }

    /// Rebuild the slot table at `n` slots (a power of two) from the
    /// entries' stored hashes; no key is hashed or compared.
    fn rebuild_slots(&mut self, n: usize) {
        debug_assert!(n.is_power_of_two() && n >= 16);
        assert!(
            n <= 1 << 32 && self.entries.len() < IX_MASK as usize,
            "dict too large"
        );
        self.slots = vec![0u64; n].into_boxed_slice();
        self.shift = 64 - n.trailing_zeros();
        let mask = n - 1;
        for (i, b) in self.entries.iter().enumerate() {
            let m = mix(b.hash);
            let mut pos = (m >> self.shift) as usize;
            // SAFETY: `pos` is masked into the table throughout.
            unsafe {
                while *self.slots.get_unchecked(pos) != 0 {
                    pos = (pos + 1) & mask;
                }
                *self.slots.get_unchecked_mut(pos) = (m & !IX_MASK) | (i as u64 + 1);
            }
        }
    }

    /// Re-derive the slots after the entries were reordered or dropped in
    /// bulk.
    fn reindex(&mut self) {
        if self.slots.is_empty() {
            return;
        }
        self.rebuild_slots(self.slots.len());
    }

    /// The position of the entry equal to `key` with hash `hash`, or, on a
    /// miss, `Err` with the empty slot where it would go (unused for a
    /// small table).
    #[inline(always)]
    fn find_hashed<Q: Probe + ?Sized>(&self, hash: i64, key: &Q) -> Result<usize, usize> {
        if self.slots.is_empty() {
            for (i, b) in self.entries.iter().enumerate() {
                if b.hash == hash && key.probe_eq(&b.key) {
                    return Ok(i);
                }
            }
            return Err(0);
        }
        let m = mix(hash);
        let mask = self.slots.len() - 1;
        let mut pos = (m >> self.shift) as usize;
        loop {
            // SAFETY: `pos` is masked into the table, and a nonzero slot
            // holds a live entry index plus one.
            unsafe {
                let s = *self.slots.get_unchecked(pos);
                if s == 0 {
                    return Err(pos);
                }
                if (s ^ m) <= IX_MASK {
                    let ix = (s & IX_MASK) as usize - 1;
                    let b = self.entries.get_unchecked(ix);
                    if b.hash == hash && key.probe_eq(&b.key) {
                        return Ok(ix);
                    }
                }
            }
            pos = (pos + 1) & mask;
        }
    }

    /// The slot that holds entry `ix` (whose hash is `hash`).
    #[inline]
    fn slot_of(&self, hash: i64, ix: usize) -> usize {
        let m = mix(hash);
        let mask = self.slots.len() - 1;
        let mut pos = (m >> self.shift) as usize;
        let want = ix as u64 + 1;
        loop {
            // SAFETY: masked into the table; entry `ix` is present, so
            // the walk ends at its slot before any empty one.
            let s = unsafe { *self.slots.get_unchecked(pos) };
            debug_assert!(s != 0, "entry {ix} has no slot");
            if s & IX_MASK == want {
                return pos;
            }
            pos = (pos + 1) & mask;
        }
    }

    /// Free slot `pos`, shifting back the run after it (no tombstones).
    fn erase_slot(&mut self, mut pos: usize) {
        let mask = self.slots.len() - 1;
        let bits = 64 - self.shift;
        let mut next = (pos + 1) & mask;
        loop {
            let s = self.slots[next];
            if s == 0 {
                break;
            }
            let home = (s >> (64 - bits)) as usize;
            // The entry at `next` may fill the hole when its home is not
            // strictly between the hole and itself.
            if (next.wrapping_sub(home) & mask) >= (next.wrapping_sub(pos) & mask) {
                self.slots[pos] = s;
                pos = next;
            }
            next = (next + 1) & mask;
        }
        self.slots[pos] = 0;
    }

    /// Point the slot of entry `from` (hash `hash`) at entry `to`.
    #[inline]
    fn repoint(&mut self, hash: i64, from: usize, to: usize) {
        let pos = self.slot_of(hash, from);
        self.slots[pos] = (self.slots[pos] & !IX_MASK) | (to as u64 + 1);
    }

    // ----------------------------------------------------------------
    // Lookups.

    /// The position of the entry equal to `key`.
    #[inline]
    pub fn get_index_of<Q: Probe + ?Sized>(&self, key: &Q) -> Option<usize> {
        if self.slots.is_empty() {
            if self.entries.is_empty() {
                return None;
            }
            if let Some(found) = self.scan_unhashed(key) {
                return found;
            }
        }
        self.find_hashed(key.probe_hash(), key).ok()
    }

    /// A small table's lookup without hashing the probe (see
    /// [`Probe::probe_eq_unhashed`]); `None` when some entry needs the
    /// hash.
    #[inline(always)]
    fn scan_unhashed<Q: Probe + ?Sized>(&self, key: &Q) -> Option<Option<usize>> {
        let mut found = None;
        for (i, b) in self.entries.iter().enumerate() {
            if key.probe_eq_unhashed(&b.key)? {
                found = Some(i);
                break;
            }
        }
        Some(found)
    }

    /// [`Self::get`], always in line: for the interpreter's subscript
    /// fast paths, where the call would cost a fair share of the probe.
    #[inline(always)]
    pub fn get_hot<Q: Probe + ?Sized>(&self, key: &Q) -> Option<&V> {
        let i = self.get_index_hot(key)?;
        // SAFETY: a found position is in bounds.
        Some(unsafe { &self.entries.get_unchecked(i).value })
    }

    /// [`Self::get_index_of`], always in line (see [`Self::get_hot`]).
    #[inline(always)]
    pub fn get_index_hot<Q: Probe + ?Sized>(&self, key: &Q) -> Option<usize> {
        if self.slots.is_empty() {
            if self.entries.is_empty() {
                return None;
            }
            if let Some(found) = self.scan_unhashed(key) {
                return found;
            }
        }
        self.find_hashed(key.probe_hash(), key).ok()
    }

    #[inline]
    pub fn get<Q: Probe + ?Sized>(&self, key: &Q) -> Option<&V> {
        let i = self.get_index_of(key)?;
        // SAFETY: a found position is in bounds.
        Some(unsafe { &self.entries.get_unchecked(i).value })
    }

    #[inline]
    pub fn get_mut<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<&mut V> {
        let i = self.get_index_of(key)?;
        // SAFETY: a found position is in bounds.
        Some(unsafe { &mut self.entries.get_unchecked_mut(i).value })
    }

    #[inline]
    pub fn get_full<Q: Probe + ?Sized>(&self, key: &Q) -> Option<(usize, &DictKey, &V)> {
        let i = self.get_index_of(key)?;
        let b = &self.entries[i];
        Some((i, &b.key, &b.value))
    }

    #[inline]
    pub fn get_full_mut<Q: Probe + ?Sized>(
        &mut self,
        key: &Q,
    ) -> Option<(usize, &DictKey, &mut V)> {
        let i = self.get_index_of(key)?;
        let b = &mut self.entries[i];
        Some((i, &b.key, &mut b.value))
    }

    #[inline]
    pub fn get_key_value<Q: Probe + ?Sized>(&self, key: &Q) -> Option<(&DictKey, &V)> {
        let i = self.get_index_of(key)?;
        let b = &self.entries[i];
        Some((&b.key, &b.value))
    }

    #[inline]
    pub fn contains_key<Q: Probe + ?Sized>(&self, key: &Q) -> bool {
        self.get_index_of(key).is_some()
    }

    /// The position of the first entry with Python hash `hash` that `eq`
    /// accepts; `eq` sees only keys of that exact hash.
    pub fn find_by_hash(&self, hash: i64, mut eq: impl FnMut(&DictKey) -> bool) -> Option<usize> {
        struct Pred<'a, F>(std::cell::RefCell<&'a mut F>);
        impl<F: FnMut(&DictKey) -> bool> Probe for Pred<'_, F> {
            fn probe_hash(&self) -> i64 {
                unreachable!("hashed by the caller")
            }
            fn probe_eq(&self, stored: &DictKey) -> bool {
                (self.0.borrow_mut())(stored)
            }
        }
        self.find_hashed(hash, &Pred(std::cell::RefCell::new(&mut eq)))
            .ok()
    }

    /// Every stored key with Python hash `hash`, in probe order.
    pub fn keys_with_hash(&self, hash: i64) -> Vec<&DictKey> {
        let mut out = Vec::new();
        let _ = self.find_by_hash(hash, |k| {
            out.push(std::ptr::from_ref::<DictKey>(k));
            false
        });
        // SAFETY: the pointers address entries of `self`, borrowed for the
        // returned lifetime.
        out.into_iter().map(|p| unsafe { &*p }).collect()
    }

    /// The stored Python hash of the entry at `i`.
    #[inline]
    pub fn hash_at(&self, i: usize) -> Option<i64> {
        self.entries.get(i).map(|b| b.hash)
    }

    // ----------------------------------------------------------------
    // Positional access.

    #[inline]
    pub fn get_index(&self, i: usize) -> Option<(&DictKey, &V)> {
        self.entries.get(i).map(|b| (&b.key, &b.value))
    }

    #[inline]
    pub fn get_index_mut(&mut self, i: usize) -> Option<(&DictKey, &mut V)> {
        self.entries.get_mut(i).map(|b| (&b.key, &mut b.value))
    }

    /// Mutable access to a key in place. The caller must keep its hash
    /// and equality unchanged.
    #[inline]
    pub fn get_index_mut2(&mut self, i: usize) -> Option<(&mut DictKey, &mut V)> {
        self.entries.get_mut(i).map(|b| (&mut b.key, &mut b.value))
    }

    #[inline]
    pub fn first(&self) -> Option<(&DictKey, &V)> {
        self.get_index(0)
    }

    #[inline]
    pub fn last(&self) -> Option<(&DictKey, &V)> {
        self.entries.last().map(|b| (&b.key, &b.value))
    }

    // ----------------------------------------------------------------
    // Insertion.

    /// Append `key` (hash `hash`), known to be absent, at slot `pos` (from
    /// a missed [`Self::find_hashed`]): its position.
    #[inline]
    fn push_at(&mut self, pos: usize, hash: i64, key: DictKey, value: V) -> usize {
        let i = self.entries.len();
        if self.slots.is_empty() {
            if i < SMALL {
                self.entries.push(Bucket { hash, key, value });
                return i;
            }
            self.entries.push(Bucket { hash, key, value });
            self.rebuild_slots(slots_for(i + 1));
            return i;
        }
        if (i + 1) * 3 > self.slots.len() * 2 {
            self.entries.push(Bucket { hash, key, value });
            self.rebuild_slots(self.slots.len() * 2);
            return i;
        }
        assert!(i < IX_MASK as usize - 1, "dict too large");
        // SAFETY: `pos` is an empty slot inside the table.
        unsafe {
            *self.slots.get_unchecked_mut(pos) = (mix(hash) & !IX_MASK) | (i as u64 + 1);
        }
        self.entries.push(Bucket { hash, key, value });
        i
    }

    /// Insert or replace: the entry's position and the replaced value.
    #[inline]
    pub fn insert_full(&mut self, key: DictKey, value: V) -> (usize, Option<V>) {
        let hash = key.probe_hash();
        self.insert_hashed(hash, key, value)
    }

    /// [`Self::insert_full`] with the key's Python hash precomputed.
    #[inline]
    pub fn insert_hashed(&mut self, hash: i64, key: DictKey, value: V) -> (usize, Option<V>) {
        match self.find_hashed(hash, &key) {
            Ok(i) => {
                // SAFETY: a found position is in bounds.
                let slot = unsafe { &mut self.entries.get_unchecked_mut(i).value };
                (i, Some(std::mem::replace(slot, value)))
            }
            Err(pos) => (self.push_at(pos, hash, key, value), None),
        }
    }

    #[inline]
    pub fn insert(&mut self, key: DictKey, value: V) -> Option<V> {
        self.insert_full(key, value).1
    }

    /// Append `key` with Python hash `hash` without looking for an equal
    /// key: the caller knows there is none. Its position.
    pub fn insert_unique_hashed(&mut self, hash: i64, key: DictKey, value: V) -> usize {
        if self.slots.is_empty() {
            return self.push_at(0, hash, key, value);
        }
        let m = mix(hash);
        let mask = self.slots.len() - 1;
        let mut pos = (m >> self.shift) as usize;
        while self.slots[pos] != 0 {
            pos = (pos + 1) & mask;
        }
        self.push_at(pos, hash, key, value)
    }

    /// The entry for `key`, to read, change, or fill.
    pub fn entry(&mut self, key: DictKey) -> Entry<'_, V> {
        let hash = key.probe_hash();
        match self.find_hashed(hash, &key) {
            Ok(index) => Entry::Occupied(OccupiedEntry { map: self, index }),
            Err(pos) => Entry::Vacant(VacantEntry {
                map: self,
                key,
                hash,
                pos,
            }),
        }
    }

    /// The entry for a probe: its position if present, else a vacancy
    /// that inserts the key the caller builds.
    #[inline(always)]
    pub fn probe_entry<Q: Probe + ?Sized>(&mut self, key: &Q) -> ProbeEntry<'_, V> {
        let hash = key.probe_hash();
        match self.find_hashed(hash, key) {
            Ok(index) => ProbeEntry::Occupied(OccupiedEntry { map: self, index }),
            Err(pos) => ProbeEntry::Vacant(ProbeVacancy {
                map: self,
                hash,
                pos,
            }),
        }
    }

    // ----------------------------------------------------------------
    // Removal.

    /// Remove entry `i`, shifting the later entries down.
    pub fn shift_remove_index(&mut self, i: usize) -> Option<(DictKey, V)> {
        let n = self.entries.len();
        if i >= n {
            return None;
        }
        if self.slots.is_empty() {
            let b = self.entries.remove(i);
            return Some((b.key, b.value));
        }
        let pos = self.slot_of(self.entries[i].hash, i);
        self.erase_slot(pos);
        let b = self.entries.remove(i);
        // Renumber the entries that moved down: one slot walk each when
        // few moved, else one pass over the table.
        let moved = n - 1 - i;
        if moved * 4 <= self.slots.len() {
            for j in i..n - 1 {
                let h = self.entries[j].hash;
                self.repoint(h, j + 1, j);
            }
        } else {
            let above = i as u64 + 1;
            for s in self.slots.iter_mut() {
                if *s & IX_MASK > above {
                    *s -= 1;
                }
            }
        }
        Some((b.key, b.value))
    }

    /// Remove entry `i`, moving the last entry into its place.
    pub fn swap_remove_index(&mut self, i: usize) -> Option<(DictKey, V)> {
        let n = self.entries.len();
        if i >= n {
            return None;
        }
        if !self.slots.is_empty() {
            let pos = self.slot_of(self.entries[i].hash, i);
            self.erase_slot(pos);
            if i != n - 1 {
                let h = self.entries[n - 1].hash;
                self.repoint(h, n - 1, i);
            }
        }
        let b = self.entries.swap_remove(i);
        Some((b.key, b.value))
    }

    /// Put `key` (hash `hash`), known to be absent, in entry `i`'s place,
    /// leaving every other entry where it is: the replaced key and value.
    pub fn replace_index_unique(
        &mut self,
        i: usize,
        hash: i64,
        key: DictKey,
        value: V,
    ) -> Option<(DictKey, V)> {
        let old = self.entries.get(i)?.hash;
        if !self.slots.is_empty() {
            let pos = self.slot_of(old, i);
            self.erase_slot(pos);
            let m = mix(hash);
            let mask = self.slots.len() - 1;
            let mut pos = (m >> self.shift) as usize;
            while self.slots[pos] != 0 {
                pos = (pos + 1) & mask;
            }
            self.slots[pos] = (m & !IX_MASK) | (i as u64 + 1);
        }
        let b = std::mem::replace(&mut self.entries[i], Bucket { hash, key, value });
        Some((b.key, b.value))
    }

    /// Remove the last entry.
    pub fn pop(&mut self) -> Option<(DictKey, V)> {
        let n = self.entries.len();
        if n == 0 {
            return None;
        }
        if !self.slots.is_empty() {
            let pos = self.slot_of(self.entries[n - 1].hash, n - 1);
            self.erase_slot(pos);
        }
        let b = self.entries.pop()?;
        Some((b.key, b.value))
    }

    pub fn shift_remove_full<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(usize, DictKey, V)> {
        let i = self.get_index_of(key)?;
        let (k, v) = self.shift_remove_index(i)?;
        Some((i, k, v))
    }

    pub fn shift_remove_entry<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(DictKey, V)> {
        let i = self.get_index_of(key)?;
        self.shift_remove_index(i)
    }

    pub fn shift_remove<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<V> {
        self.shift_remove_entry(key).map(|(_, v)| v)
    }

    pub fn swap_remove<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<V> {
        let i = self.get_index_of(key)?;
        self.swap_remove_index(i).map(|(_, v)| v)
    }

    pub fn swap_remove_full<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(usize, DictKey, V)> {
        let i = self.get_index_of(key)?;
        let (k, v) = self.swap_remove_index(i)?;
        Some((i, k, v))
    }

    /// Keep the entries `keep` accepts, in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&DictKey, &mut V) -> bool) {
        let n = self.entries.len();
        self.entries.retain_mut(|b| keep(&b.key, &mut b.value));
        if self.entries.len() != n {
            self.reindex();
        }
    }

    /// Keep the first `len` entries.
    pub fn truncate(&mut self, len: usize) {
        if len < self.entries.len() {
            self.entries.truncate(len);
            self.reindex();
        }
    }

    /// Split off the entries from `at` on.
    pub fn split_off(&mut self, at: usize) -> Self {
        let tail = self.entries.split_off(at);
        self.reindex();
        let mut out = Self {
            entries: tail,
            slots: Box::new([]),
            shift: 64,
        };
        if out.entries.len() > SMALL {
            out.rebuild_slots(slots_for(out.entries.len()));
        }
        out
    }

    /// Remove the entries in `range`, yielding them.
    pub fn drain<R: std::ops::RangeBounds<usize>>(
        &mut self,
        range: R,
    ) -> std::vec::IntoIter<(DictKey, V)> {
        let out: Vec<(DictKey, V)> = self
            .entries
            .drain(range)
            .map(|b| (b.key, b.value))
            .collect();
        self.reindex();
        out.into_iter()
    }

    /// Move entry `from` to position `to`, shifting the ones between.
    pub fn move_index(&mut self, from: usize, to: usize) {
        let n = self.entries.len();
        assert!(from < n && to < n, "move_index out of bounds");
        if from == to {
            return;
        }
        if self.slots.is_empty() {
            if from < to {
                self.entries[from..=to].rotate_left(1);
            } else {
                self.entries[to..=from].rotate_right(1);
            }
            return;
        }
        let (lo, hi) = (from.min(to), from.max(to));
        // Locate every affected slot before renumbering any.
        let at: Vec<usize> = (lo..=hi)
            .map(|k| self.slot_of(self.entries[k].hash, k))
            .collect();
        for (k, &pos) in (lo..=hi).zip(&at) {
            let new = if k == from {
                to
            } else if from < to {
                k - 1
            } else {
                k + 1
            };
            self.slots[pos] = (self.slots[pos] & !IX_MASK) | (new as u64 + 1);
        }
        if from < to {
            self.entries[from..=to].rotate_left(1);
        } else {
            self.entries[to..=from].rotate_right(1);
        }
    }

    /// Swap the entries at `a` and `b`.
    pub fn swap_indices(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        if !self.slots.is_empty() {
            let pa = self.slot_of(self.entries[a].hash, a);
            let pb = self.slot_of(self.entries[b].hash, b);
            self.slots[pa] = (self.slots[pa] & !IX_MASK) | (b as u64 + 1);
            self.slots[pb] = (self.slots[pb] & !IX_MASK) | (a as u64 + 1);
        }
        self.entries.swap(a, b);
    }

    /// Reverse the order.
    pub fn reverse(&mut self) {
        self.entries.reverse();
        self.reindex();
    }

    /// Sort the entries with `cmp` (stable).
    pub fn sort_by(
        &mut self,
        mut cmp: impl FnMut(&DictKey, &V, &DictKey, &V) -> std::cmp::Ordering,
    ) {
        self.entries
            .sort_by(|a, b| cmp(&a.key, &a.value, &b.key, &b.value));
        self.reindex();
    }

    /// Sort the entries by key with `cmp` (stable).
    pub fn sort_by_key_with(
        &mut self,
        mut cmp: impl FnMut(&DictKey, &DictKey) -> std::cmp::Ordering,
    ) {
        self.entries.sort_by(|a, b| cmp(&a.key, &b.key));
        self.reindex();
    }

    // ----------------------------------------------------------------
    // Iteration.

    #[inline]
    pub fn iter(&self) -> Iter<'_, V> {
        Iter(self.entries.iter())
    }

    #[inline]
    pub fn iter_mut(&mut self) -> IterMut<'_, V> {
        IterMut(self.entries.iter_mut())
    }

    /// Mutable keys and values. The caller must keep every key's hash and
    /// equality unchanged.
    #[inline]
    pub fn iter_mut2(&mut self) -> IterMut2<'_, V> {
        IterMut2(self.entries.iter_mut())
    }

    #[inline]
    pub fn keys(&self) -> Keys<'_, V> {
        Keys(self.entries.iter())
    }

    #[inline]
    pub fn values(&self) -> Values<'_, V> {
        Values(self.entries.iter())
    }

    #[inline]
    pub fn values_mut(&mut self) -> ValuesMut<'_, V> {
        ValuesMut(self.entries.iter_mut())
    }

    pub fn into_keys(self) -> impl DoubleEndedIterator<Item = DictKey> + ExactSizeIterator {
        self.entries.into_iter().map(|b| b.key)
    }

    pub fn into_values(self) -> impl DoubleEndedIterator<Item = V> + ExactSizeIterator {
        self.entries.into_iter().map(|b| b.value)
    }
}

/// An entry of [`DictMap::entry`].
#[derive(Debug)]
pub enum Entry<'a, V = Object> {
    Occupied(OccupiedEntry<'a, V>),
    Vacant(VacantEntry<'a, V>),
}

/// A present entry.
#[derive(Debug)]
pub struct OccupiedEntry<'a, V = Object> {
    map: &'a mut Table<V>,
    index: usize,
}

/// An absent key, with the slot it would take.
#[derive(Debug)]
pub struct VacantEntry<'a, V = Object> {
    map: &'a mut Table<V>,
    key: DictKey,
    hash: i64,
    pos: usize,
}

/// An entry of [`DictMap::probe_entry`].
#[derive(Debug)]
pub enum ProbeEntry<'a, V = Object> {
    Occupied(OccupiedEntry<'a, V>),
    Vacant(ProbeVacancy<'a, V>),
}

/// An absent probe key, with its hash and the slot it would take.
#[derive(Debug)]
pub struct ProbeVacancy<'a, V = Object> {
    map: &'a mut Table<V>,
    hash: i64,
    pos: usize,
}

impl<'a, V> Entry<'a, V> {
    pub fn or_insert(self, default: V) -> &'a mut V {
        match self {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(default),
        }
    }

    pub fn or_insert_with(self, f: impl FnOnce() -> V) -> &'a mut V {
        match self {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(f()),
        }
    }

    pub fn index(&self) -> usize {
        match self {
            Entry::Occupied(e) => e.index,
            Entry::Vacant(e) => e.map.len(),
        }
    }
}

impl<'a, V> OccupiedEntry<'a, V> {
    #[inline]
    pub fn index(&self) -> usize {
        self.index
    }

    #[inline]
    pub fn key(&self) -> &DictKey {
        &self.map.entries[self.index].key
    }

    #[inline]
    pub fn get(&self) -> &V {
        &self.map.entries[self.index].value
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut V {
        &mut self.map.entries[self.index].value
    }

    #[inline]
    pub fn into_mut(self) -> &'a mut V {
        &mut self.map.entries[self.index].value
    }

    #[inline]
    pub fn insert(&mut self, value: V) -> V {
        std::mem::replace(self.get_mut(), value)
    }

    pub fn shift_remove_entry(self) -> (DictKey, V) {
        self.map
            .shift_remove_index(self.index)
            .expect("occupied entry is present")
    }

    pub fn shift_remove(self) -> V {
        self.shift_remove_entry().1
    }
}

impl<'a, V> VacantEntry<'a, V> {
    #[inline]
    pub fn key(&self) -> &DictKey {
        &self.key
    }

    pub fn insert(self, value: V) -> &'a mut V {
        let i = self.map.push_at(self.pos, self.hash, self.key, value);
        &mut self.map.entries[i].value
    }
}

impl<V> ProbeVacancy<'_, V> {
    /// The probe's Python hash.
    #[inline]
    pub fn hash(&self) -> i64 {
        self.hash
    }

    /// Insert `key`, which must equal the probe (same hash, same key):
    /// its position.
    #[inline]
    pub fn insert(self, key: DictKey, value: V) -> usize {
        self.map.push_at(self.pos, self.hash, key, value)
    }
}

macro_rules! entry_iter {
    ($name:ident, $inner:ty, $item:ty, |$b:ident| $map:expr) => {
        pub struct $name<'a, V = Object>($inner);

        impl<'a, V> Iterator for $name<'a, V> {
            type Item = $item;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                self.0.next().map(|$b| $map)
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                self.0.size_hint()
            }

            #[inline]
            fn nth(&mut self, n: usize) -> Option<Self::Item> {
                self.0.nth(n).map(|$b| $map)
            }

            #[inline]
            fn count(self) -> usize {
                self.0.len()
            }

            #[inline]
            fn last(mut self) -> Option<Self::Item> {
                self.next_back()
            }
        }

        impl<V> DoubleEndedIterator for $name<'_, V> {
            #[inline]
            fn next_back(&mut self) -> Option<Self::Item> {
                self.0.next_back().map(|$b| $map)
            }

            #[inline]
            fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
                self.0.nth_back(n).map(|$b| $map)
            }
        }

        impl<V> ExactSizeIterator for $name<'_, V> {
            #[inline]
            fn len(&self) -> usize {
                self.0.len()
            }
        }

        impl<V> std::iter::FusedIterator for $name<'_, V> {}

        impl<V> fmt::Debug for $name<'_, V> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }
    };
}

entry_iter!(
    Iter,
    std::slice::Iter<'a, Bucket<V>>,
    (&'a DictKey, &'a V),
    |b| (&b.key, &b.value)
);
entry_iter!(
    IterMut,
    std::slice::IterMut<'a, Bucket<V>>,
    (&'a DictKey, &'a mut V),
    |b| (&b.key, &mut b.value)
);
entry_iter!(
    IterMut2,
    std::slice::IterMut<'a, Bucket<V>>,
    (&'a mut DictKey, &'a mut V),
    |b| (&mut b.key, &mut b.value)
);
entry_iter!(Keys, std::slice::Iter<'a, Bucket<V>>, &'a DictKey, |b| &b
    .key);
entry_iter!(Values, std::slice::Iter<'a, Bucket<V>>, &'a V, |b| &b.value);
entry_iter!(
    ValuesMut,
    std::slice::IterMut<'a, Bucket<V>>,
    &'a mut V,
    |b| &mut b.value
);

impl<V> Clone for Iter<'_, V> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<V> Clone for Keys<'_, V> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<V> Clone for Values<'_, V> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// The owning iterator of a [`DictMap`].
pub struct IntoIter<V = Object>(std::vec::IntoIter<Bucket<V>>);

impl<V> Iterator for IntoIter<V> {
    type Item = (DictKey, V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|b| (b.key, b.value))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<V> DoubleEndedIterator for IntoIter<V> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(|b| (b.key, b.value))
    }
}

impl<V> ExactSizeIterator for IntoIter<V> {}

impl<V> fmt::Debug for IntoIter<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntoIter").finish_non_exhaustive()
    }
}

impl<V> IntoIterator for Table<V> {
    type Item = (DictKey, V);
    type IntoIter = IntoIter<V>;

    fn into_iter(self) -> IntoIter<V> {
        IntoIter(self.entries.into_iter())
    }
}

impl<'a, V> IntoIterator for &'a Table<V> {
    type Item = (&'a DictKey, &'a V);
    type IntoIter = Iter<'a, V>;

    fn into_iter(self) -> Iter<'a, V> {
        self.iter()
    }
}

impl<'a, V> IntoIterator for &'a mut Table<V> {
    type Item = (&'a DictKey, &'a mut V);
    type IntoIter = IterMut<'a, V>;

    fn into_iter(self) -> IterMut<'a, V> {
        self.iter_mut()
    }
}

impl<V> Extend<(DictKey, V)> for Table<V> {
    fn extend<I: IntoIterator<Item = (DictKey, V)>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        let (lo, _) = iter.size_hint();
        // (As IndexMap: reserve for all when empty, else half, since some
        // keys may already be present.)
        self.reserve(if self.is_empty() { lo } else { lo.div_ceil(2) });
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

impl<'a, V: Clone + 'a> Extend<(&'a DictKey, &'a V)> for Table<V> {
    fn extend<I: IntoIterator<Item = (&'a DictKey, &'a V)>>(&mut self, iter: I) {
        self.extend(iter.into_iter().map(|(k, v)| (k.clone(), v.clone())));
    }
}

impl<V> FromIterator<(DictKey, V)> for Table<V> {
    fn from_iter<I: IntoIterator<Item = (DictKey, V)>>(iter: I) -> Self {
        let mut m = Self::new();
        m.extend(iter);
        m
    }
}

impl<V, const N: usize> From<[(DictKey, V); N]> for Table<V> {
    fn from(arr: [(DictKey, V); N]) -> Self {
        Self::from_iter(arr)
    }
}

impl<V, Q: Probe + ?Sized> std::ops::Index<&Q> for Table<V> {
    type Output = V;

    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("key not found")
    }
}

impl<V> std::ops::Index<usize> for Table<V> {
    type Output = V;

    fn index(&self, i: usize) -> &V {
        &self.entries[i].value
    }
}

/// The table behind a `dict`.
pub type DictMap = Table<Object>;

/// The table behind a `set` or `frozenset`: [`Table`]'s entries without
/// values (a key and its hash), with the set operations' names. Iteration
/// is in insertion order; a removal moves the last entry into the freed
/// position (`set` order is arbitrary, and a set's iterator reports any
/// change of size).
#[derive(Clone, Default)]
pub struct SetMap(Table<()>);

impl fmt::Debug for SetMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

/// The keys of a [`SetMap`], in order.
pub type SetIter<'a> = Keys<'a, ()>;

impl SetMap {
    #[inline]
    pub fn new() -> Self {
        Self(Table::new())
    }

    #[inline]
    pub fn with_capacity(n: usize) -> Self {
        Self(Table::with_capacity(n))
    }

    /// [`Self::with_capacity`]; the hasher is implied (see [`Table`]).
    #[inline]
    pub fn with_capacity_and_hasher(n: usize, _: crate::fasthash::FxBuildHasher) -> Self {
        Self::with_capacity(n)
    }

    /// [`Self::new`]; the hasher is implied.
    #[inline]
    pub fn with_hasher(_: crate::fasthash::FxBuildHasher) -> Self {
        Self::new()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }

    #[inline]
    pub fn reserve(&mut self, additional: usize) {
        self.0.reserve(additional);
    }

    pub fn shrink_to_fit(&mut self) {
        self.0.shrink_to_fit();
    }

    #[inline]
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// The entries in order (each key with its stored hash).
    #[inline]
    pub fn as_entries(&self) -> &[Bucket<()>] {
        self.0.as_entries()
    }

    /// Add `key`: `false` when an equal key was already present (which
    /// stays).
    #[inline]
    pub fn insert(&mut self, key: DictKey) -> bool {
        self.insert_full(key).1
    }

    /// [`Self::insert`], with the key's position.
    #[inline]
    pub fn insert_full(&mut self, key: DictKey) -> (usize, bool) {
        let hash = key.probe_hash();
        self.insert_hashed(hash, key)
    }

    /// [`Self::insert_full`] with the key's Python hash precomputed.
    #[inline]
    pub fn insert_hashed(&mut self, hash: i64, key: DictKey) -> (usize, bool) {
        match self.0.find_hashed(hash, &key) {
            Ok(i) => (i, false),
            Err(pos) => (self.0.push_at(pos, hash, key, ()), true),
        }
    }

    /// Append `key` with Python hash `hash`, known to be absent: its
    /// position.
    #[inline]
    pub fn insert_unique_hashed(&mut self, hash: i64, key: DictKey) -> usize {
        self.0.insert_unique_hashed(hash, key, ())
    }

    /// Add a probe's key if absent: the key is built (by `make`) only
    /// then. `true` when it was added.
    #[inline]
    pub fn insert_probe<Q: Probe + ?Sized>(
        &mut self,
        key: &Q,
        make: impl FnOnce() -> DictKey,
    ) -> bool {
        match self.0.probe_entry(key) {
            ProbeEntry::Occupied(_) => false,
            ProbeEntry::Vacant(v) => {
                v.insert(make(), ());
                true
            }
        }
    }

    /// The position of the key equal to `key`. The probe is hashed even
    /// when the table is empty (CPython hashes first, so a raising
    /// `__hash__` surfaces from `x in set()`).
    #[inline]
    fn find<Q: Probe + ?Sized>(&self, key: &Q) -> Option<usize> {
        if self.0.is_empty() {
            let _ = key.probe_hash();
            return None;
        }
        self.0.get_index_of(key)
    }

    #[inline]
    pub fn contains<Q: Probe + ?Sized>(&self, key: &Q) -> bool {
        self.find(key).is_some()
    }

    /// [`Self::contains`], always in line, for a probe whose hash runs no
    /// code (the interpreter's membership fast paths).
    #[inline(always)]
    pub fn contains_hot<Q: Probe + ?Sized>(&self, key: &Q) -> bool {
        self.0.get_index_hot(key).is_some()
    }

    #[inline]
    pub fn get_index_of<Q: Probe + ?Sized>(&self, key: &Q) -> Option<usize> {
        self.find(key)
    }

    /// The stored key equal to `key`.
    #[inline]
    pub fn get<Q: Probe + ?Sized>(&self, key: &Q) -> Option<&DictKey> {
        let i = self.find(key)?;
        Some(&self.0.as_entries()[i].key)
    }

    #[inline]
    pub fn get_full<Q: Probe + ?Sized>(&self, key: &Q) -> Option<(usize, &DictKey)> {
        let i = self.find(key)?;
        Some((i, &self.0.as_entries()[i].key))
    }

    /// The position of the first key with Python hash `hash` that `eq`
    /// accepts (see [`Table::find_by_hash`]).
    #[inline]
    pub fn find_by_hash(&self, hash: i64, eq: impl FnMut(&DictKey) -> bool) -> Option<usize> {
        self.0.find_by_hash(hash, eq)
    }

    /// The stored Python hash of the key at `i`.
    #[inline]
    pub fn hash_at(&self, i: usize) -> Option<i64> {
        self.0.hash_at(i)
    }

    #[inline]
    pub fn get_index(&self, i: usize) -> Option<&DictKey> {
        self.0.as_entries().get(i).map(|b| &b.key)
    }

    #[inline]
    pub fn first(&self) -> Option<&DictKey> {
        self.get_index(0)
    }

    #[inline]
    pub fn last(&self) -> Option<&DictKey> {
        self.0.as_entries().last().map(|b| &b.key)
    }

    /// Remove the key equal to `key` (the last key takes its place):
    /// whether it was present.
    #[inline]
    pub fn swap_remove<Q: Probe + ?Sized>(&mut self, key: &Q) -> bool {
        self.swap_take(key).is_some()
    }

    /// [`Self::swap_remove`], returning the removed key.
    #[inline]
    pub fn swap_take<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<DictKey> {
        let i = self.find(key)?;
        self.0.swap_remove_index(i).map(|(k, ())| k)
    }

    /// [`Self::swap_take`], with the key's former position.
    #[inline]
    pub fn swap_remove_full<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(usize, DictKey)> {
        let i = self.find(key)?;
        self.0.swap_remove_index(i).map(|(k, ())| (i, k))
    }

    /// Remove the key equal to `key`, shifting the later keys down.
    #[inline]
    pub fn shift_remove<Q: Probe + ?Sized>(&mut self, key: &Q) -> bool {
        self.shift_take(key).is_some()
    }

    /// [`Self::shift_remove`], returning the removed key.
    #[inline]
    pub fn shift_take<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<DictKey> {
        let i = self.find(key)?;
        self.0.shift_remove_index(i).map(|(k, ())| k)
    }

    #[inline]
    pub fn swap_remove_index(&mut self, i: usize) -> Option<DictKey> {
        self.0.swap_remove_index(i).map(|(k, ())| k)
    }

    #[inline]
    pub fn shift_remove_index(&mut self, i: usize) -> Option<DictKey> {
        self.0.shift_remove_index(i).map(|(k, ())| k)
    }

    /// Remove the last key.
    #[inline]
    pub fn pop(&mut self) -> Option<DictKey> {
        self.0.pop().map(|(k, ())| k)
    }

    /// Keep the keys `keep` accepts, in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&DictKey) -> bool) {
        self.0.retain(|k, ()| keep(k));
    }

    /// Remove the keys in `range`, yielding them.
    pub fn drain<R: std::ops::RangeBounds<usize>>(
        &mut self,
        range: R,
    ) -> impl Iterator<Item = DictKey> {
        self.0.drain(range).map(|(k, ())| k)
    }

    /// Sort the keys with `cmp` (stable).
    pub fn sort_by(&mut self, mut cmp: impl FnMut(&DictKey, &DictKey) -> std::cmp::Ordering) {
        self.0.sort_by_key_with(|a, b| cmp(a, b));
    }

    #[inline]
    pub fn iter(&self) -> SetIter<'_> {
        self.0.keys()
    }

    /// The keys in `self` that `other` lacks, in order.
    pub fn difference<'a>(&'a self, other: &'a SetMap) -> impl Iterator<Item = &'a DictKey> + 'a {
        self.iter().filter(move |k| !other.contains(*k))
    }

    /// The keys in both, in `self`'s order.
    pub fn intersection<'a>(&'a self, other: &'a SetMap) -> impl Iterator<Item = &'a DictKey> + 'a {
        self.iter().filter(move |k| other.contains(*k))
    }

    /// `self`'s keys, then `other`'s that `self` lacks.
    pub fn union<'a>(&'a self, other: &'a SetMap) -> impl Iterator<Item = &'a DictKey> + 'a {
        self.iter().chain(other.difference(self))
    }

    /// The keys in exactly one, `self`'s first.
    pub fn symmetric_difference<'a>(
        &'a self,
        other: &'a SetMap,
    ) -> impl Iterator<Item = &'a DictKey> + 'a {
        self.difference(other).chain(other.difference(self))
    }

    pub fn is_subset(&self, other: &SetMap) -> bool {
        self.len() <= other.len() && self.iter().all(|k| other.contains(k))
    }

    pub fn is_superset(&self, other: &SetMap) -> bool {
        other.is_subset(self)
    }

    pub fn is_disjoint(&self, other: &SetMap) -> bool {
        let (small, big) = if self.len() <= other.len() {
            (self, other)
        } else {
            (other, self)
        };
        small.iter().all(|k| !big.contains(k))
    }
}

impl IntoIterator for SetMap {
    type Item = DictKey;
    type IntoIter = std::iter::Map<IntoIter<()>, fn((DictKey, ())) -> DictKey>;

    fn into_iter(self) -> Self::IntoIter {
        fn key((k, ()): (DictKey, ())) -> DictKey {
            k
        }
        self.0.into_iter().map(key as fn((DictKey, ())) -> DictKey)
    }
}

impl<'a> IntoIterator for &'a SetMap {
    type Item = &'a DictKey;
    type IntoIter = SetIter<'a>;

    fn into_iter(self) -> SetIter<'a> {
        self.iter()
    }
}

impl Extend<DictKey> for SetMap {
    fn extend<I: IntoIterator<Item = DictKey>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        let (lo, _) = iter.size_hint();
        self.reserve(if self.is_empty() { lo } else { lo.div_ceil(2) });
        for k in iter {
            self.insert(k);
        }
    }
}

impl<'a> Extend<&'a DictKey> for SetMap {
    fn extend<I: IntoIterator<Item = &'a DictKey>>(&mut self, iter: I) {
        self.extend(iter.into_iter().cloned());
    }
}

impl FromIterator<DictKey> for SetMap {
    fn from_iter<I: IntoIterator<Item = DictKey>>(iter: I) -> Self {
        let mut s = Self::new();
        s.extend(iter);
        s
    }
}

impl<const N: usize> From<[DictKey; N]> for SetMap {
    fn from(arr: [DictKey; N]) -> Self {
        Self::from_iter(arr)
    }
}

/// Equal sizes, and every key of one in the other (order aside).
impl PartialEq for SetMap {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|k| other.contains(k))
    }
}

impl std::ops::Index<usize> for SetMap {
    type Output = DictKey;

    fn index(&self, i: usize) -> &DictKey {
        &self.0.as_entries()[i].key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(i: i64) -> DictKey {
        DictKey(Object::Int(i))
    }

    fn check(m: &DictMap) {
        for (i, b) in m.entries.iter().enumerate() {
            assert_eq!(m.get_index_of(&b.key), Some(i));
        }
        if !m.slots.is_empty() {
            let used = m.slots.iter().filter(|s| **s != 0).count();
            assert_eq!(used, m.len());
        }
    }

    #[test]
    fn insert_remove_order() {
        let mut m = DictMap::new();
        for i in 0..200 {
            assert!(m.insert(k(i * 1024), Object::Int(i)).is_none());
            check(&m);
        }
        for i in (0..200).step_by(3) {
            assert!(m.shift_remove(&k(i * 1024)).is_some());
            check(&m);
        }
        let keys: Vec<i64> = m
            .keys()
            .map(|k| match k.0 {
                Object::Int(i) => i / 1024,
                _ => unreachable!(),
            })
            .collect();
        let want: Vec<i64> = (0..200).filter(|i| i % 3 != 0).collect();
        assert_eq!(keys, want);
        m.move_index(0, 50);
        check(&m);
        m.move_index(60, 3);
        check(&m);
        m.swap_remove_index(7);
        check(&m);
        while m.pop().is_some() {
            check(&m);
        }
        assert!(m.is_empty());
    }

    #[test]
    fn small_and_clone() {
        let mut m = DictMap::new();
        for i in 0..8 {
            m.insert(k(i), Object::Int(i));
        }
        assert!(m.slots.is_empty());
        m.insert(k(8), Object::Int(8));
        assert!(!m.slots.is_empty());
        let c = m.clone();
        check(&c);
        assert_eq!(
            c.get(&k(5)).map(|o| matches!(o, Object::Int(5))),
            Some(true)
        );
        assert!(c.get(&k(50)).is_none());
    }

    #[test]
    fn replace_in_place() {
        for n in [5_i64, 40] {
            let mut m = DictMap::new();
            for i in 0..n {
                m.insert(k(i * 1024), Object::Int(i));
            }
            for (step, i) in (0..n).rev().enumerate() {
                let new = 100_000 + step as i64;
                let key = k(new);
                let hash = key.probe_hash();
                let at = (i as usize * 7) % m.len();
                let (old, _) = m
                    .replace_index_unique(at, hash, key, Object::Int(new))
                    .expect("in range");
                assert!(m.get(&old).is_none());
                assert_eq!(m.get_index_of(&k(new)), Some(at));
                assert_eq!(m.len(), n as usize);
                check(&m);
            }
            assert!(m
                .replace_index_unique(n as usize, 0, k(-1), Object::None)
                .is_none());
        }
    }
}
