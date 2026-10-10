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
pub struct Bucket {
    pub(crate) hash: i64,
    pub key: DictKey,
    pub value: Object,
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
            (Object::Str(a), Object::Str(b)) if crate::shared_value::SharedStr::known_hash(a).is_none() => {
                Some(a.as_bytes() == b.as_bytes())
            }
            (Object::Str(a), Object::WStr(_)) if crate::shared_value::SharedStr::known_hash(a).is_none() => {
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

/// An ordered hash table from [`DictKey`] to [`Object`] (see the module
/// docs).
pub struct DictMap {
    entries: Vec<Bucket>,
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

impl Default for DictMap {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for DictMap {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            slots: self.slots.clone(),
            shift: self.shift,
        }
    }
}

impl fmt::Debug for DictMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.entries.iter().map(|b| (&b.key, &b.value)))
            .finish()
    }
}

impl DictMap {
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
    pub fn as_entries(&self) -> &[Bucket] {
        &self.entries
    }

    /// Make room for `additional` more entries.
    pub fn reserve(&mut self, additional: usize) {
        self.entries.reserve(additional);
        self.reserve_slots(additional);
    }

    /// [`Self::reserve`], reporting an allocation failure instead of
    /// aborting.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), std::collections::TryReserveError> {
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
            match key.probe_eq_unhashed(&b.key)? {
                true => {
                    found = Some(i);
                    break;
                }
                false => {}
            }
        }
        Some(found)
    }

    /// [`Self::get`], always in line: for the interpreter's subscript
    /// fast paths, where the call would cost a fair share of the probe.
    #[inline(always)]
    pub fn get_hot<Q: Probe + ?Sized>(&self, key: &Q) -> Option<&Object> {
        if self.slots.is_empty() {
            if self.entries.is_empty() {
                return None;
            }
            if let Some(found) = self.scan_unhashed(key) {
                // SAFETY: a found position is in bounds.
                return found.map(|i| unsafe { &self.entries.get_unchecked(i).value });
            }
        }
        let i = self.find_hashed(key.probe_hash(), key).ok()?;
        // SAFETY: a found position is in bounds.
        Some(unsafe { &self.entries.get_unchecked(i).value })
    }

    #[inline]
    pub fn get<Q: Probe + ?Sized>(&self, key: &Q) -> Option<&Object> {
        let i = self.get_index_of(key)?;
        // SAFETY: a found position is in bounds.
        Some(unsafe { &self.entries.get_unchecked(i).value })
    }

    #[inline]
    pub fn get_mut<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<&mut Object> {
        let i = self.get_index_of(key)?;
        // SAFETY: a found position is in bounds.
        Some(unsafe { &mut self.entries.get_unchecked_mut(i).value })
    }

    #[inline]
    pub fn get_full<Q: Probe + ?Sized>(&self, key: &Q) -> Option<(usize, &DictKey, &Object)> {
        let i = self.get_index_of(key)?;
        let b = &self.entries[i];
        Some((i, &b.key, &b.value))
    }

    #[inline]
    pub fn get_full_mut<Q: Probe + ?Sized>(
        &mut self,
        key: &Q,
    ) -> Option<(usize, &DictKey, &mut Object)> {
        let i = self.get_index_of(key)?;
        let b = &mut self.entries[i];
        Some((i, &b.key, &mut b.value))
    }

    #[inline]
    pub fn get_key_value<Q: Probe + ?Sized>(&self, key: &Q) -> Option<(&DictKey, &Object)> {
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
            out.push(k as *const DictKey);
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
    pub fn get_index(&self, i: usize) -> Option<(&DictKey, &Object)> {
        self.entries.get(i).map(|b| (&b.key, &b.value))
    }

    #[inline]
    pub fn get_index_mut(&mut self, i: usize) -> Option<(&DictKey, &mut Object)> {
        self.entries.get_mut(i).map(|b| (&b.key, &mut b.value))
    }

    /// Mutable access to a key in place. The caller must keep its hash
    /// and equality unchanged.
    #[inline]
    pub fn get_index_mut2(&mut self, i: usize) -> Option<(&mut DictKey, &mut Object)> {
        self.entries.get_mut(i).map(|b| (&mut b.key, &mut b.value))
    }

    #[inline]
    pub fn first(&self) -> Option<(&DictKey, &Object)> {
        self.get_index(0)
    }

    #[inline]
    pub fn last(&self) -> Option<(&DictKey, &Object)> {
        self.entries.last().map(|b| (&b.key, &b.value))
    }

    // ----------------------------------------------------------------
    // Insertion.

    /// Append `key` (hash `hash`), known to be absent, at slot `pos` (from
    /// a missed [`Self::find_hashed`]): its position.
    #[inline]
    fn push_at(&mut self, pos: usize, hash: i64, key: DictKey, value: Object) -> usize {
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
    pub fn insert_full(&mut self, key: DictKey, value: Object) -> (usize, Option<Object>) {
        let hash = key.probe_hash();
        self.insert_hashed(hash, key, value)
    }

    /// [`Self::insert_full`] with the key's Python hash precomputed.
    #[inline]
    pub fn insert_hashed(&mut self, hash: i64, key: DictKey, value: Object) -> (usize, Option<Object>) {
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
    pub fn insert(&mut self, key: DictKey, value: Object) -> Option<Object> {
        self.insert_full(key, value).1
    }

    /// Append `key` with Python hash `hash` without looking for an equal
    /// key: the caller knows there is none. Its position.
    pub fn insert_unique_hashed(&mut self, hash: i64, key: DictKey, value: Object) -> usize {
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
    pub fn entry(&mut self, key: DictKey) -> Entry<'_> {
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
    pub fn probe_entry<Q: Probe + ?Sized>(&mut self, key: &Q) -> ProbeEntry<'_> {
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
    pub fn shift_remove_index(&mut self, i: usize) -> Option<(DictKey, Object)> {
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
    pub fn swap_remove_index(&mut self, i: usize) -> Option<(DictKey, Object)> {
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

    /// Remove the last entry.
    pub fn pop(&mut self) -> Option<(DictKey, Object)> {
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

    pub fn shift_remove_full<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(usize, DictKey, Object)> {
        let i = self.get_index_of(key)?;
        let (k, v) = self.shift_remove_index(i)?;
        Some((i, k, v))
    }

    pub fn shift_remove_entry<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(DictKey, Object)> {
        let i = self.get_index_of(key)?;
        self.shift_remove_index(i)
    }

    pub fn shift_remove<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<Object> {
        self.shift_remove_entry(key).map(|(_, v)| v)
    }

    pub fn swap_remove<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<Object> {
        let i = self.get_index_of(key)?;
        self.swap_remove_index(i).map(|(_, v)| v)
    }

    pub fn swap_remove_full<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(usize, DictKey, Object)> {
        let i = self.get_index_of(key)?;
        let (k, v) = self.swap_remove_index(i)?;
        Some((i, k, v))
    }

    /// Keep the entries `keep` accepts, in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&DictKey, &mut Object) -> bool) {
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
    pub fn drain<R: std::ops::RangeBounds<usize>>(&mut self, range: R) -> std::vec::IntoIter<(DictKey, Object)> {
        let out: Vec<(DictKey, Object)> = self.entries.drain(range).map(|b| (b.key, b.value)).collect();
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
    pub fn sort_by(&mut self, mut cmp: impl FnMut(&DictKey, &Object, &DictKey, &Object) -> std::cmp::Ordering) {
        self.entries
            .sort_by(|a, b| cmp(&a.key, &a.value, &b.key, &b.value));
        self.reindex();
    }

    /// Sort the entries by key with `cmp` (stable).
    pub fn sort_by_key_with(&mut self, mut cmp: impl FnMut(&DictKey, &DictKey) -> std::cmp::Ordering) {
        self.entries.sort_by(|a, b| cmp(&a.key, &b.key));
        self.reindex();
    }

    // ----------------------------------------------------------------
    // Iteration.

    #[inline]
    pub fn iter(&self) -> Iter<'_> {
        Iter(self.entries.iter())
    }

    #[inline]
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        IterMut(self.entries.iter_mut())
    }

    /// Mutable keys and values. The caller must keep every key's hash and
    /// equality unchanged.
    #[inline]
    pub fn iter_mut2(&mut self) -> IterMut2<'_> {
        IterMut2(self.entries.iter_mut())
    }

    #[inline]
    pub fn keys(&self) -> Keys<'_> {
        Keys(self.entries.iter())
    }

    #[inline]
    pub fn values(&self) -> Values<'_> {
        Values(self.entries.iter())
    }

    #[inline]
    pub fn values_mut(&mut self) -> ValuesMut<'_> {
        ValuesMut(self.entries.iter_mut())
    }

    pub fn into_keys(self) -> impl DoubleEndedIterator<Item = DictKey> + ExactSizeIterator {
        self.entries.into_iter().map(|b| b.key)
    }

    pub fn into_values(self) -> impl DoubleEndedIterator<Item = Object> + ExactSizeIterator {
        self.entries.into_iter().map(|b| b.value)
    }
}

/// An entry of [`DictMap::entry`].
#[derive(Debug)]
pub enum Entry<'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(VacantEntry<'a>),
}

/// A present entry.
#[derive(Debug)]
pub struct OccupiedEntry<'a> {
    map: &'a mut DictMap,
    index: usize,
}

/// An absent key, with the slot it would take.
#[derive(Debug)]
pub struct VacantEntry<'a> {
    map: &'a mut DictMap,
    key: DictKey,
    hash: i64,
    pos: usize,
}

/// An entry of [`DictMap::probe_entry`].
#[derive(Debug)]
pub enum ProbeEntry<'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(ProbeVacancy<'a>),
}

/// An absent probe key, with its hash and the slot it would take.
#[derive(Debug)]
pub struct ProbeVacancy<'a> {
    map: &'a mut DictMap,
    hash: i64,
    pos: usize,
}

impl<'a> Entry<'a> {
    pub fn or_insert(self, default: Object) -> &'a mut Object {
        match self {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(default),
        }
    }

    pub fn or_insert_with(self, f: impl FnOnce() -> Object) -> &'a mut Object {
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

impl<'a> OccupiedEntry<'a> {
    #[inline]
    pub fn index(&self) -> usize {
        self.index
    }

    #[inline]
    pub fn key(&self) -> &DictKey {
        &self.map.entries[self.index].key
    }

    #[inline]
    pub fn get(&self) -> &Object {
        &self.map.entries[self.index].value
    }

    #[inline]
    pub fn get_mut(&mut self) -> &mut Object {
        &mut self.map.entries[self.index].value
    }

    #[inline]
    pub fn into_mut(self) -> &'a mut Object {
        &mut self.map.entries[self.index].value
    }

    #[inline]
    pub fn insert(&mut self, value: Object) -> Object {
        std::mem::replace(self.get_mut(), value)
    }

    pub fn shift_remove_entry(self) -> (DictKey, Object) {
        self.map
            .shift_remove_index(self.index)
            .expect("occupied entry is present")
    }

    pub fn shift_remove(self) -> Object {
        self.shift_remove_entry().1
    }
}

impl<'a> VacantEntry<'a> {
    #[inline]
    pub fn key(&self) -> &DictKey {
        &self.key
    }

    pub fn insert(self, value: Object) -> &'a mut Object {
        let i = self.map.push_at(self.pos, self.hash, self.key, value);
        &mut self.map.entries[i].value
    }
}

impl<'a> ProbeVacancy<'a> {
    /// The probe's Python hash.
    #[inline]
    pub fn hash(&self) -> i64 {
        self.hash
    }

    /// Insert `key`, which must equal the probe (same hash, same key):
    /// its position.
    #[inline]
    pub fn insert(self, key: DictKey, value: Object) -> usize {
        self.map.push_at(self.pos, self.hash, key, value)
    }
}

macro_rules! entry_iter {
    ($name:ident, $inner:ty, $item:ty, |$b:ident| $map:expr) => {
        pub struct $name<'a>($inner);

        impl<'a> Iterator for $name<'a> {
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

        impl DoubleEndedIterator for $name<'_> {
            #[inline]
            fn next_back(&mut self) -> Option<Self::Item> {
                self.0.next_back().map(|$b| $map)
            }

            #[inline]
            fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
                self.0.nth_back(n).map(|$b| $map)
            }
        }

        impl ExactSizeIterator for $name<'_> {
            #[inline]
            fn len(&self) -> usize {
                self.0.len()
            }
        }

        impl std::iter::FusedIterator for $name<'_> {}

        impl fmt::Debug for $name<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }
    };
}

entry_iter!(Iter, std::slice::Iter<'a, Bucket>, (&'a DictKey, &'a Object), |b| (&b.key, &b.value));
entry_iter!(IterMut, std::slice::IterMut<'a, Bucket>, (&'a DictKey, &'a mut Object), |b| (&b.key, &mut b.value));
entry_iter!(IterMut2, std::slice::IterMut<'a, Bucket>, (&'a mut DictKey, &'a mut Object), |b| (&mut b.key, &mut b.value));
entry_iter!(Keys, std::slice::Iter<'a, Bucket>, &'a DictKey, |b| &b.key);
entry_iter!(Values, std::slice::Iter<'a, Bucket>, &'a Object, |b| &b.value);
entry_iter!(ValuesMut, std::slice::IterMut<'a, Bucket>, &'a mut Object, |b| &mut b.value);

impl Clone for Iter<'_> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Clone for Keys<'_> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Clone for Values<'_> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// The owning iterator of a [`DictMap`].
pub struct IntoIter(std::vec::IntoIter<Bucket>);

impl Iterator for IntoIter {
    type Item = (DictKey, Object);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|b| (b.key, b.value))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl DoubleEndedIterator for IntoIter {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(|b| (b.key, b.value))
    }
}

impl ExactSizeIterator for IntoIter {}

impl fmt::Debug for IntoIter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntoIter").finish_non_exhaustive()
    }
}

impl IntoIterator for DictMap {
    type Item = (DictKey, Object);
    type IntoIter = IntoIter;

    fn into_iter(self) -> IntoIter {
        IntoIter(self.entries.into_iter())
    }
}

impl<'a> IntoIterator for &'a DictMap {
    type Item = (&'a DictKey, &'a Object);
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut DictMap {
    type Item = (&'a DictKey, &'a mut Object);
    type IntoIter = IterMut<'a>;

    fn into_iter(self) -> IterMut<'a> {
        self.iter_mut()
    }
}

impl Extend<(DictKey, Object)> for DictMap {
    fn extend<I: IntoIterator<Item = (DictKey, Object)>>(&mut self, iter: I) {
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

impl<'a> Extend<(&'a DictKey, &'a Object)> for DictMap {
    fn extend<I: IntoIterator<Item = (&'a DictKey, &'a Object)>>(&mut self, iter: I) {
        self.extend(iter.into_iter().map(|(k, v)| (k.clone(), v.clone())));
    }
}

impl FromIterator<(DictKey, Object)> for DictMap {
    fn from_iter<I: IntoIterator<Item = (DictKey, Object)>>(iter: I) -> Self {
        let mut m = Self::new();
        m.extend(iter);
        m
    }
}

impl<const N: usize> From<[(DictKey, Object); N]> for DictMap {
    fn from(arr: [(DictKey, Object); N]) -> Self {
        Self::from_iter(arr)
    }
}

impl<Q: Probe + ?Sized> std::ops::Index<&Q> for DictMap {
    type Output = Object;

    fn index(&self, key: &Q) -> &Object {
        self.get(key).expect("key not found")
    }
}

impl std::ops::Index<usize> for DictMap {
    type Output = Object;

    fn index(&self, i: usize) -> &Object {
        &self.entries[i].value
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
        assert_eq!(c.get(&k(5)).map(|o| matches!(o, Object::Int(5))), Some(true));
        assert!(c.get(&k(50)).is_none());
    }
}
