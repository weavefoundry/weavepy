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
//! follows a freed slot back over it, so the slot table has no tombstones.
//!
//! Positions are dense: `get_index(i)` is always the `i`-th key in order
//! (the contract the iterators and the position caches rely on), and
//! removing a key moves the later ones down by one. Moving them in memory
//! costs time in proportion to the entries behind the key, so the removals
//! a program makes in bulk (`del d[k]`, `d.pop(k)`, an `OrderedDict`'s)
//! leave a tombstone in the entry list instead
//! ([`Table::lazy_remove_index`]). Positions stay dense all the same: a
//! rank index over the tombstones ([`Holes`]) turns a position into its
//! entry and back, and the table closes the gaps in one pass when the
//! entry list fills or the tombstones outnumber the keys. A table that has
//! no tombstone (nearly every table) pays one test per positional access.
//!
//! A lookup takes any [`Probe`]: a stored key's own type, or one of the
//! borrowed probes in [`crate::object`] (a `&str` name, a native-equality
//! leaf probe) that hash and compare without building an `Object`.

use crate::object::{DictKey, Object};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Whether `b` is a tombstone: a removed key's place in the entry list,
/// kept until the table closes its gaps (its key is the unbound marker,
/// which no dict stores, and nothing points at it).
#[inline(always)]
fn is_tomb<V>(b: &Bucket<V>) -> bool {
    matches!(b.key.0, Object::Unbound)
}

/// A value a tombstone can hold: one whose drop does nothing.
pub trait Vacant {
    fn vacant() -> Self;
}

impl Vacant for Object {
    #[inline(always)]
    fn vacant() -> Self {
        Object::None
    }
}

impl Vacant for () {
    #[inline(always)]
    fn vacant() -> Self {}
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

/// A probe whose equality with a stored key of equal hash settles
/// natively (see [`Table::find_native`]).
pub trait NativeEq {
    /// Whether the probe equals `stored`; `None` when that needs more
    /// than a native compare (another kind of key, a long string).
    fn native_eq(&self, stored: &DictKey) -> Option<bool>;
}

/// A `str` probe: equal to a stored `str` of the same bytes.
struct StrEq<'a>(&'a crate::shared_value::SharedStr);

impl NativeEq for StrEq<'_> {
    #[inline(always)]
    fn native_eq(&self, stored: &DictKey) -> Option<bool> {
        match &stored.0 {
            Object::Str(b) if crate::shared_value::SharedStr::ptr_eq(self.0, b) => Some(true),
            Object::Str(b) => key_bytes_eq_native(self.0.as_bytes(), b.as_bytes()),
            _ => None,
        }
    }
}

/// A machine `int` probe: equal to a stored machine `int` of its value.
struct IntEq(i64);

impl NativeEq for IntEq {
    #[inline(always)]
    fn native_eq(&self, stored: &DictKey) -> Option<bool> {
        match stored.0 {
            Object::Int(j) => Some(j == self.0),
            _ => None,
        }
    }
}

/// A finite `float` probe: equal to a stored `float` of its value (`0.0`
/// and `-0.0` are equal and hash alike). A stored `int` of equal value has
/// the same hash too, and leaves the lookup undecided.
struct FloatEq(f64);

impl NativeEq for FloatEq {
    #[inline(always)]
    fn native_eq(&self, stored: &DictKey) -> Option<bool> {
        match stored.0 {
            Object::Float(g) => Some(g == self.0),
            _ => None,
        }
    }
}

/// A class probe hashed by identity: equal to itself (a stored class of
/// equal hash but another identity might compare through its metaclass's
/// `__eq__`, so that leaves the lookup undecided).
struct TypeEq<'a>(&'a crate::Rc<crate::types::TypeObject>);

impl NativeEq for TypeEq<'_> {
    #[inline(always)]
    fn native_eq(&self, stored: &DictKey) -> Option<bool> {
        match &stored.0 {
            Object::Type(u) if crate::Rc::ptr_eq(self.0, u) => Some(true),
            _ => None,
        }
    }
}

/// [`key_bytes_eq`] for a native lookup: in line (calling nothing) up to
/// 16 bytes, the commonest keys; a longer pair out of line.
#[inline(always)]
fn key_bytes_eq_native(a: &[u8], b: &[u8]) -> Option<bool> {
    let n = a.len();
    if n != b.len() {
        return Some(false);
    }
    if n <= 16 {
        return Some(key_bytes_eq(a, b));
    }
    Some(long_key_bytes_eq(a, b))
}

/// [`key_bytes_eq_native`] past 16 bytes.
#[cold]
#[inline(never)]
fn long_key_bytes_eq(a: &[u8], b: &[u8]) -> bool {
    a == b
}

/// An ordered hash table from [`DictKey`] to `V` (see the module docs):
/// [`DictMap`] for a dict, [`SetMap`] for a set.
pub struct Table<V> {
    /// The keys in order, with a tombstone wherever a lazy removal took
    /// one; the last entry is always a key.
    entries: Vec<Bucket<V>>,
    /// The slot table: `1 << (64 - shift)` words this table owns, none
    /// (a dangling pointer) for a small table. (One word, not a boxed
    /// slice's two: the count is in `shift`.)
    slots: std::ptr::NonNull<u64>,
    /// `64 - log2(the slot count)`: the shift that takes a mixed hash to
    /// its home slot; 64 for a small table.
    shift: u32,
    /// How many of `entries` are tombstones.
    dead: u32,
    /// Where they are, once the table has had one.
    holes: Option<Box<Holes>>,
}

// SAFETY: the slot words are owned by the table (allocated in
// `alloc_slots`, freed in `free_slots` and on drop) and reached only
// through it, as a boxed slice's would be.
unsafe impl<V: Send> Send for Table<V> {}
// SAFETY: as above.
unsafe impl<V: Sync> Sync for Table<V> {}

impl<V> Drop for Table<V> {
    fn drop(&mut self) {
        self.free_slots();
    }
}

/// `n` zeroed slot words (`n` a power of two).
fn alloc_slots(n: usize) -> std::ptr::NonNull<u64> {
    let words: Box<[u64]> = vec![0u64; n].into_boxed_slice();
    // SAFETY: a box's pointer is never null.
    unsafe { std::ptr::NonNull::new_unchecked(Box::into_raw(words).cast::<u64>()) }
}

/// No translation remembered (see [`Holes::last`]).
const NO_PAIR: u64 = u64::MAX;

/// The most entries a lazy removal moves down, in a table without
/// tombstones, before it leaves a tombstone instead.
const SHIFT_MAX: usize = 16;

/// Where a table's tombstones are: what turns a key's position (its rank
/// among the keys) into its entry's index and back.
struct Holes {
    /// One bit per entry, set for a tombstone. A power of two of words;
    /// entries past them are keys.
    bits: Vec<u64>,
    /// A Fenwick tree over the words' tombstone counts: `tree[i]` counts
    /// the `i & -i` words that end with word `i - 1` (`tree[0]` is unused).
    tree: Vec<u32>,
    /// Whether `tree` is kept: from the first tombstone past the front
    /// run on (until then no position needs a count).
    counted: bool,
    /// How many entries at the front are tombstones: a table that loses
    /// its oldest keys (a queue, a cache that evicts in order) keeps all
    /// its tombstones there.
    head: usize,
    /// The first tombstone past those (or one before it), `usize::MAX`
    /// for none: a key below it has its entry `head` past its position.
    first: usize,
    /// The last translation made, `position << 32 | entry`, or
    /// [`NO_PAIR`]: a walk by position continues from it, and a position
    /// just found by key is its entry again without a search.
    last: AtomicU64,
}

impl Holes {
    fn new() -> Self {
        Self {
            bits: Vec::new(),
            tree: Vec::new(),
            counted: false,
            head: 0,
            first: usize::MAX,
            last: AtomicU64::new(NO_PAIR),
        }
    }

    /// Forget every tombstone (the entries were closed up).
    fn reset(&mut self) {
        self.bits.fill(0);
        self.counted = false;
        self.head = 0;
        self.first = usize::MAX;
        *self.last.get_mut() = NO_PAIR;
    }

    /// Entry `p` (not the last) becomes a tombstone, one of `total` now,
    /// in an entry list with room for `cap`.
    fn mark(&mut self, p: usize, cap: usize, total: usize) {
        let w = p / 64;
        if w >= self.bits.len() {
            let n = cap.max(p + 1).div_ceil(64).next_power_of_two();
            self.bits.resize(n, 0);
            if self.counted {
                self.count();
            }
        }
        self.bits[w] |= 1 << (p % 64);
        if self.counted {
            let mut i = w + 1;
            while i < self.tree.len() {
                self.tree[i] += 1;
                i += i & i.wrapping_neg();
            }
        }
        if p == self.head {
            // The front run grows, through any tombstones that follow it.
            self.head = self.next_live(p);
            if self.first < self.head {
                self.first = if total == self.head {
                    usize::MAX
                } else {
                    self.next_dead(self.head)
                };
            }
        } else {
            self.first = self.first.min(p);
        }
        if self.first != usize::MAX && !self.counted {
            self.count();
        }
        *self.last.get_mut() = NO_PAIR;
    }

    /// Build `tree` from `bits`, and keep it from here on.
    #[cold]
    fn count(&mut self) {
        let n = self.bits.len();
        self.tree.clear();
        self.tree.resize(n + 1, 0);
        for i in 1..=n {
            let c = self.tree[i] + self.bits[i - 1].count_ones();
            self.tree[i] = c;
            let up = i + (i & i.wrapping_neg());
            if up <= n {
                self.tree[up] += c;
            }
        }
        self.counted = true;
    }

    /// The first tombstone at or after entry `p` (`usize::MAX` for none).
    /// (Each call from `mark` starts past where the last one ended, so
    /// between two resets they read the bits once over.)
    fn next_dead(&self, p: usize) -> usize {
        let mut w = p / 64;
        if w >= self.bits.len() {
            return usize::MAX;
        }
        let mut word = self.bits[w] & (u64::MAX << (p % 64));
        loop {
            if word != 0 {
                return w * 64 + word.trailing_zeros() as usize;
            }
            w += 1;
            if w >= self.bits.len() {
                return usize::MAX;
            }
            word = self.bits[w];
        }
    }

    /// Tombstone `p` is gone (popped off the end of the entries).
    fn unmark(&mut self, p: usize) {
        let w = p / 64;
        self.bits[w] &= !(1 << (p % 64));
        if self.counted {
            let mut i = w + 1;
            while i < self.tree.len() {
                self.tree[i] -= 1;
                i += i & i.wrapping_neg();
            }
        }
        *self.last.get_mut() = NO_PAIR;
    }

    /// How many of the `total` tombstones precede entry `p`, one at or
    /// past [`Self::first`] (so the tree is kept).
    fn rank(&self, p: usize, total: usize) -> usize {
        debug_assert!(self.counted);
        let w = p / 64;
        if w >= self.bits.len() {
            return total;
        }
        let mut n = (self.bits[w] & ((1u64 << (p % 64)) - 1)).count_ones() as usize;
        let mut i = w;
        while i > 0 {
            n += self.tree[i] as usize;
            i &= i - 1;
        }
        n
    }

    /// The entry of the `l`-th key, one at or past [`Self::first`] (so the
    /// tree is kept).
    fn select(&self, l: usize) -> usize {
        debug_assert!(self.counted);
        let n = self.bits.len();
        // The last word with at most `l` keys before it, by descent.
        let (mut word, mut rem, mut k) = (0, l, n);
        while k > 0 {
            let next = word + k;
            if next <= n {
                let keys = 64 * k - self.tree[next] as usize;
                if keys <= rem {
                    word = next;
                    rem -= keys;
                }
            }
            k >>= 1;
        }
        if word == n {
            return 64 * n + rem;
        }
        // (That word holds more than `rem` keys.)
        let mut live = !self.bits[word];
        let mut base = 0;
        loop {
            let c = (live as u16).count_ones() as usize;
            if rem < c {
                break;
            }
            rem -= c;
            live >>= 16;
            base += 16;
        }
        let mut low = live as u16;
        for _ in 0..rem {
            low &= low - 1;
        }
        64 * word + base + low.trailing_zeros() as usize
    }

    /// The first key's entry after entry `p` (there is one).
    fn next_live(&self, p: usize) -> usize {
        let mut q = p + 1;
        loop {
            let w = q / 64;
            if w >= self.bits.len() {
                return q;
            }
            let live = !self.bits[w] >> (q % 64);
            if live != 0 {
                return q + live.trailing_zeros() as usize;
            }
            q = (w + 1) * 64;
        }
    }

    /// The last key's entry before entry `p` (there is one).
    fn prev_live(&self, p: usize) -> usize {
        let mut q = p;
        loop {
            let w = (q - 1) / 64;
            if w >= self.bits.len() {
                return q - 1;
            }
            let upto = (q - 1) % 64 + 1;
            let live = !self.bits[w] & (u64::MAX >> (64 - upto));
            if live != 0 {
                return w * 64 + 63 - live.leading_zeros() as usize;
            }
            q = w * 64;
        }
    }
}

#[inline(always)]
fn mix(hash: i64) -> u64 {
    (hash as u64).wrapping_mul(MIX)
}

/// Where a dict table's parts sit in it, and an entry's in an entry (byte
/// offsets), with the slot mixing constants: compiled code's in-line
/// first probe (see `frame_jit::DictLayout`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct TableLayout {
    pub(crate) entries_ptr: usize,
    pub(crate) slots_ptr: usize,
    pub(crate) shift: usize,
    pub(crate) bucket_size: usize,
    pub(crate) bucket_hash: usize,
    pub(crate) bucket_key: usize,
    pub(crate) bucket_value: usize,
    pub(crate) mix: u64,
    pub(crate) ix_mask: u64,
}

/// [`TableLayout`], read off a sample table (`None` if it isn't the shape
/// the code assumes: a vector's words told apart by their values, a
/// one-word slot pointer, a 32-bit shift that reads 64 for a table
/// without slots).
pub(crate) fn table_layout() -> Option<TableLayout> {
    let n = SMALL + 4;
    let mut t: Table<Object> = Table::with_capacity(n);
    for i in 0..n as i64 {
        t.insert_hashed(i, DictKey(Object::Int(i)), Object::Int(i));
    }
    if t.is_small()
        || t.entries.capacity() == t.entries.len()
        || Table::<Object>::new().shift != 64
        || std::mem::size_of_val(&t.slots) != std::mem::size_of::<usize>()
    {
        return None;
    }
    let base = std::ptr::from_ref(&t) as usize;
    // SAFETY: words inside the live sample.
    let word = |off: usize| unsafe { *((base + off) as *const usize) };
    let vec_at = std::mem::offset_of!(Table<Object>, entries);
    let entries_ptr = (0..3)
        .map(|k| vec_at + 8 * k)
        .find(|&off| word(off) == t.entries.as_ptr() as usize)?;
    let slots_ptr = std::mem::offset_of!(Table<Object>, slots);
    (word(slots_ptr) == t.slots.as_ptr() as usize).then_some(())?;
    (std::mem::size_of::<u32>() == std::mem::size_of_val(&t.shift)).then_some(())?;
    Some(TableLayout {
        entries_ptr,
        slots_ptr,
        shift: std::mem::offset_of!(Table<Object>, shift),
        bucket_size: std::mem::size_of::<Bucket<Object>>(),
        bucket_hash: std::mem::offset_of!(Bucket<Object>, hash),
        bucket_key: std::mem::offset_of!(Bucket<Object>, key),
        bucket_value: std::mem::offset_of!(Bucket<Object>, value),
        mix: MIX,
        ix_mask: IX_MASK,
    })
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
        if self.dead != 0 {
            return self.clone_keys(0);
        }
        Self {
            entries: self.entries.clone(),
            slots: self.clone_slots(),
            shift: self.shift,
            dead: 0,
            holes: None,
        }
    }
}

impl<V: Clone> Table<V> {
    /// A copy whose entry list has room for `extra` more entries (a
    /// union's), allocated once.
    pub fn clone_with_room(&self, extra: usize) -> Self {
        if self.dead != 0 {
            return self.clone_keys(extra);
        }
        let mut entries = Vec::with_capacity(self.entries.len() + extra);
        entries.extend_from_slice(&self.entries);
        Self {
            entries,
            slots: self.clone_slots(),
            shift: self.shift,
            dead: 0,
            holes: None,
        }
    }

    /// A copy of a table with tombstones, without them.
    #[inline(never)]
    fn clone_keys(&self, extra: usize) -> Self {
        let mut entries = Vec::with_capacity(self.len() + extra);
        entries.extend(self.entries.iter().filter(|b| !is_tomb(b)).cloned());
        let mut m = Self {
            entries,
            slots: std::ptr::NonNull::dangling(),
            shift: 64,
            dead: 0,
            holes: None,
        };
        if m.entries.len() > SMALL {
            m.rebuild_slots(slots_for(m.entries.len()));
        }
        m
    }
}

impl<V: fmt::Debug> fmt::Debug for Table<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<V> Table<V> {
    /// An empty table.
    #[inline]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            slots: std::ptr::NonNull::dangling(),
            shift: 64,
            dead: 0,
            holes: None,
        }
    }

    /// An empty table with room for `n` entries.
    pub fn with_capacity(n: usize) -> Self {
        let mut m = Self {
            entries: Vec::with_capacity(n),
            slots: std::ptr::NonNull::dangling(),
            shift: 64,
            dead: 0,
            holes: None,
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
            slots: std::ptr::NonNull::dangling(),
            shift: 64,
            dead: 0,
            holes: None,
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
        self.entries.len() - self.dead as usize
    }

    /// (The last entry is a key, so a table without keys has no entry.)
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the table has no slots (a probe scans its entries).
    #[inline(always)]
    fn is_small(&self) -> bool {
        self.shift == 64
    }

    /// How many slots the table has.
    #[inline(always)]
    fn nslots(&self) -> usize {
        if self.shift == 64 {
            0
        } else {
            1 << (64 - self.shift)
        }
    }

    /// The slot count less one, of a table with slots.
    #[inline(always)]
    fn slot_mask(&self) -> usize {
        debug_assert!(!self.is_small());
        (u64::MAX >> (self.shift & 63)) as usize
    }

    /// The slot words.
    #[cfg(test)]
    fn slot_words(&self) -> &[u64] {
        // SAFETY: `nslots` words the table owns (none: a dangling
        // pointer, which an empty slice may hold).
        unsafe { std::slice::from_raw_parts(self.slots.as_ptr(), self.nslots()) }
    }

    /// [`Self::slot_words`], to write.
    #[inline(always)]
    fn slot_words_mut(&mut self) -> &mut [u64] {
        // SAFETY: as `slot_words`, and the table is borrowed mutably.
        unsafe { std::slice::from_raw_parts_mut(self.slots.as_ptr(), self.nslots()) }
    }

    /// Slot `pos` (below the slot count).
    #[inline(always)]
    fn slot(&self, pos: usize) -> u64 {
        debug_assert!(pos < self.nslots());
        // SAFETY: inside the table's slot words, per the caller.
        unsafe { *self.slots.as_ptr().add(pos) }
    }

    /// Set slot `pos` (below the slot count).
    #[inline(always)]
    fn set_slot(&mut self, pos: usize, s: u64) {
        debug_assert!(pos < self.nslots());
        // SAFETY: as `slot`, and the table is borrowed mutably.
        unsafe { *self.slots.as_ptr().add(pos) = s }
    }

    /// Give the slot words back: the table is small again.
    fn free_slots(&mut self) {
        let n = self.nslots();
        if n != 0 {
            // SAFETY: the boxed slice `alloc_slots` made, of `n` words,
            // which nothing else refers to.
            drop(unsafe {
                Box::from_raw(std::ptr::slice_from_raw_parts_mut(self.slots.as_ptr(), n))
            });
            self.slots = std::ptr::NonNull::dangling();
            self.shift = 64;
        }
    }

    /// A copy of the slot words (to go with a copy of `shift`).
    fn clone_slots(&self) -> std::ptr::NonNull<u64> {
        let n = self.nslots();
        if n == 0 {
            return std::ptr::NonNull::dangling();
        }
        let copy = alloc_slots(n);
        // SAFETY: two allocations of `n` words each.
        unsafe { std::ptr::copy_nonoverlapping(self.slots.as_ptr(), copy.as_ptr(), n) };
        copy
    }

    /// The entry index of position `i` (past the entries for a position
    /// past the keys).
    #[inline(always)]
    fn entry_of(&self, i: usize) -> usize {
        if self.dead == 0 {
            i
        } else {
            self.entry_of_holed(i)
        }
    }

    #[inline(never)]
    fn entry_of_holed(&self, i: usize) -> usize {
        let Some(h) = self.holes.as_deref() else {
            return i;
        };
        if i >= self.len() {
            return usize::MAX;
        }
        if i + h.head < h.first {
            return i + h.head;
        }
        let pair = h.last.load(Ordering::Relaxed);
        let (at, entry) = ((pair >> 32) as usize, (pair & IX_MASK) as usize);
        let p = if pair == NO_PAIR {
            h.select(i)
        } else if i == at {
            return entry;
        } else if i == at + 1 {
            h.next_live(entry)
        } else if i + 1 == at {
            h.prev_live(entry)
        } else {
            h.select(i)
        };
        h.last
            .store(((i as u64) << 32) | p as u64, Ordering::Relaxed);
        p
    }

    /// The position of the key in entry `p`.
    #[inline(always)]
    fn position_of(&self, p: usize) -> usize {
        if self.dead == 0 {
            p
        } else {
            self.position_of_holed(p)
        }
    }

    #[inline(never)]
    fn position_of_holed(&self, p: usize) -> usize {
        let Some(h) = self.holes.as_deref() else {
            return p;
        };
        if p < h.first {
            return p - h.head;
        }
        let i = p - h.rank(p, self.dead as usize);
        h.last
            .store(((i as u64) << 32) | p as u64, Ordering::Relaxed);
        i
    }

    /// The entries without their tombstones (the slots are stale until
    /// rebuilt).
    fn drop_tombstones(&mut self) {
        if self.dead == 0 {
            return;
        }
        self.entries.retain(|b| !is_tomb(b));
        self.dead = 0;
        if let Some(h) = self.holes.as_deref_mut() {
            h.reset();
        }
    }

    /// Close the tombstones' gaps: the keys move down in order, and the
    /// slot table is rebuilt at `n` slots.
    #[inline(never)]
    fn compact(&mut self, n: usize) {
        self.drop_tombstones();
        self.rebuild_slots(n);
    }

    /// [`Self::compact`] for an operation that works on the entry list as
    /// a whole (a sort, a reversal, a cut).
    #[inline(always)]
    fn make_dense(&mut self) {
        if self.dead != 0 {
            self.compact(self.nslots());
        }
    }

    /// Pop the tombstones a removal left at the end of the entries.
    #[inline(always)]
    fn trim(&mut self) {
        if self.dead != 0 {
            self.trim_holed();
        }
    }

    #[inline(never)]
    fn trim_holed(&mut self) {
        let Some(h) = self.holes.as_deref_mut() else {
            return;
        };
        while self.entries.last().is_some_and(is_tomb) {
            self.entries.pop();
            h.unmark(self.entries.len());
            self.dead -= 1;
        }
        *h.last.get_mut() = NO_PAIR;
        if self.dead == 0 {
            h.head = 0;
        }
        if self.dead as usize == h.head {
            h.first = usize::MAX;
        }
    }

    /// How many entries fit before the entry vector reallocates.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    /// The entries in order, of a table that takes no lazy removal (a
    /// set's).
    #[inline]
    pub fn as_entries(&self) -> &[Bucket<V>] {
        debug_assert_eq!(self.dead, 0, "entries with tombstones");
        &self.entries
    }

    /// Each key with its stored Python hash, in order.
    #[inline]
    pub fn hashed_keys(&self) -> impl Iterator<Item = (i64, &DictKey)> {
        self.entries
            .iter()
            .filter(|b| !is_tomb(b))
            .map(|b| (b.hash, &b.key))
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
        if want > SMALL && (want * 3 > self.nslots() * 2) {
            if self.dead != 0 {
                // Tombstones took the room: close them up (see
                // `push_growing`).
                let keys = self.len().saturating_add(additional);
                self.compact(slots_for(keys + keys / 2).max(self.nslots()));
                return;
            }
            self.rebuild_slots(slots_for(want));
        }
    }

    /// Release spare capacity.
    pub fn shrink_to_fit(&mut self) {
        self.make_dense();
        self.entries.shrink_to_fit();
        if self.entries.len() <= SMALL {
            self.free_slots();
        } else if slots_for(self.entries.len()) < self.nslots() {
            self.rebuild_slots(slots_for(self.entries.len()));
        }
    }

    /// Remove every entry, keeping the allocations.
    #[inline]
    pub fn clear(&mut self) {
        self.entries.clear();
        // A small table has no slots, so skip the `memset` call. A recycled
        // keyword dict otherwise made one per call, of zero bytes at the
        // empty slice's dangling address, and on AMD Zen 4 and Zen 5 that
        // cost about 100 ns (glibc's AVX-512 `memset` stores through a mask
        // there, apparently taking a microcode assist for the unmapped
        // page).
        if !self.is_small() {
            self.slot_words_mut().fill(0);
        }
        self.forget_tombstones();
    }

    /// The entry list was emptied: so is the tombstones' index.
    #[inline(always)]
    fn forget_tombstones(&mut self) {
        if self.dead != 0 {
            self.dead = 0;
            if let Some(h) = self.holes.as_deref_mut() {
                h.reset();
            }
        }
    }

    /// Rebuild the slot table at `n` slots (a power of two) from the
    /// entries' stored hashes; no key is hashed or compared.
    fn rebuild_slots(&mut self, n: usize) {
        debug_assert!(n.is_power_of_two() && n >= 16);
        assert!(
            n <= 1 << 32 && self.entries.len() < IX_MASK as usize,
            "dict too large"
        );
        self.free_slots();
        self.slots = alloc_slots(n);
        self.shift = 64 - n.trailing_zeros();
        let mask = n - 1;
        let holed = self.dead != 0;
        for (i, b) in self.entries.iter().enumerate() {
            if holed && is_tomb(b) {
                continue;
            }
            let m = mix(b.hash);
            let mut pos = (m >> self.shift) as usize;
            // SAFETY: `pos` is masked into the table throughout.
            unsafe {
                while *self.slots.as_ptr().add(pos) != 0 {
                    pos = (pos + 1) & mask;
                }
                *self.slots.as_ptr().add(pos) = (m & !IX_MASK) | (i as u64 + 1);
            }
        }
    }

    /// Re-derive the slots after the entries were reordered or dropped in
    /// bulk.
    fn reindex(&mut self) {
        if self.is_small() {
            return;
        }
        self.rebuild_slots(self.nslots());
    }

    /// The entry equal to `key` with hash `hash`, or, on a miss, `Err` with
    /// the empty slot where it would go (unused for a small table).
    #[inline(always)]
    fn find_hashed<Q: Probe + ?Sized>(&self, hash: i64, key: &Q) -> Result<usize, usize> {
        if self.is_small() {
            for (i, b) in self.entries.iter().enumerate() {
                if b.hash == hash && key.probe_eq(&b.key) {
                    return Ok(i);
                }
            }
            return Err(0);
        }
        let m = mix(hash);
        let mask = self.slot_mask();
        let mut pos = (m >> self.shift) as usize;
        loop {
            // SAFETY: `pos` is masked into the table, and a nonzero slot
            // holds a live entry index plus one.
            unsafe {
                let s = *self.slots.as_ptr().add(pos);
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
        let mask = self.slot_mask();
        let mut pos = (m >> self.shift) as usize;
        let want = ix as u64 + 1;
        loop {
            // SAFETY: masked into the table; entry `ix` is present, so
            // the walk ends at its slot before any empty one.
            let s = unsafe { *self.slots.as_ptr().add(pos) };
            debug_assert!(s != 0, "entry {ix} has no slot");
            if s & IX_MASK == want {
                return pos;
            }
            pos = (pos + 1) & mask;
        }
    }

    /// Free slot `pos`, shifting back the run after it (no tombstones).
    fn erase_slot(&mut self, mut pos: usize) {
        let mask = self.slot_mask();
        let bits = 64 - self.shift;
        let mut next = (pos + 1) & mask;
        loop {
            let s = self.slot(next);
            if s == 0 {
                break;
            }
            let home = (s >> (64 - bits)) as usize;
            // The entry at `next` may fill the hole when its home is not
            // strictly between the hole and itself.
            if (next.wrapping_sub(home) & mask) >= (next.wrapping_sub(pos) & mask) {
                self.set_slot(pos, s);
                pos = next;
            }
            next = (next + 1) & mask;
        }
        self.set_slot(pos, 0);
    }

    /// Point the slot of entry `from` (hash `hash`) at entry `to`.
    #[inline]
    fn repoint(&mut self, hash: i64, from: usize, to: usize) {
        let pos = self.slot_of(hash, from);
        self.set_slot(pos, (self.slot(pos) & !IX_MASK) | (to as u64 + 1));
    }

    // ----------------------------------------------------------------
    // Lookups.

    /// The position of the key equal to `key`.
    #[inline]
    pub fn get_index_of<Q: Probe + ?Sized>(&self, key: &Q) -> Option<usize> {
        let p = self.find(key)?;
        Some(self.position_of(p))
    }

    /// The entry of the key equal to `key`.
    #[inline]
    fn find<Q: Probe + ?Sized>(&self, key: &Q) -> Option<usize> {
        if self.is_small() {
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
        let i = self.find_hot(key)?;
        // SAFETY: a found position is in bounds.
        Some(unsafe { &self.entries.get_unchecked(i).value })
    }

    /// A lookup that settles natively, always in line and calling nothing
    /// (the subscript fast paths that use it need no stack frame): `eq`
    /// compares the probe with a stored key of equal hash. `Ok(i)` found
    /// in entry `i` (for [`Self::value_at_unchecked`]; not a position),
    /// `Err(pos)` exactly absent (`pos` as [`Self::find_hashed`] gives
    /// it), `None` undecided.
    #[inline(always)]
    pub fn find_native<E: NativeEq>(&self, hash: i64, eq: &E) -> Option<Result<usize, usize>> {
        if self.is_small() {
            for (i, b) in self.entries.iter().enumerate() {
                if b.hash == hash && eq.native_eq(&b.key)? {
                    return Some(Ok(i));
                }
            }
            return Some(Err(0));
        }
        let m = mix(hash);
        let mask = self.slot_mask();
        let mut pos = (m >> self.shift) as usize;
        loop {
            // SAFETY: as in `find_hashed`.
            unsafe {
                let s = *self.slots.as_ptr().add(pos);
                if s == 0 {
                    return Some(Err(pos));
                }
                if (s ^ m) <= IX_MASK {
                    let ix = (s & IX_MASK) as usize - 1;
                    let b = self.entries.get_unchecked(ix);
                    if b.hash == hash && eq.native_eq(&b.key)? {
                        return Some(Ok(ix));
                    }
                }
            }
            pos = (pos + 1) & mask;
        }
    }

    /// [`Self::find_native`] for `key`, with its hash: a `str` whose hash
    /// is known, a machine `int` hashed by itself, or a finite `float`,
    /// met only by stored keys of its own kind (any other key, or a stored
    /// key of another kind with an equal hash, leaves it undecided).
    #[inline(always)]
    pub fn find_object_native(&self, key: &Object) -> Option<(i64, Result<usize, usize>)> {
        use crate::shared_value::SharedStr;
        match key {
            Object::Str(s) => {
                let hash = SharedStr::known_hash(s)?;
                Some((hash, self.find_native(hash, &StrEq(s))?))
            }
            // (A machine int below the hash modulus is its own hash.)
            Object::Int(i) if (*i as u64) < (1 << 61) - 1 => {
                Some((*i, self.find_native(*i, &IntEq(*i))?))
            }
            // (A NaN is found by identity, which the general lookup has.)
            Object::Float(f) if f.is_finite() => {
                let hash = crate::object::py_hash_double(*f);
                Some((hash, self.find_native(hash, &FloatEq(*f))?))
            }
            _ => None,
        }
    }

    /// [`Self::find_native`] for class `t`, which the caller found hashed
    /// and compared by identity (see `type_hash_is_identity`).
    #[inline(always)]
    pub fn find_type_native(
        &self,
        t: &crate::Rc<crate::types::TypeObject>,
    ) -> Option<Result<usize, usize>> {
        // (As `identity_hash` hashes a class.)
        let hash = crate::object::pointer_hash(crate::Rc::as_ptr(t).cast());
        self.find_native(hash, &TypeEq(t))
    }

    /// [`Self::find_object_native`], hashing a `str` key first (as an
    /// insert would; the hash is cached in the string).
    #[inline]
    pub fn find_object_hashing(&self, key: &Object) -> Option<(i64, Result<usize, usize>)> {
        if let Object::Str(s) = key {
            crate::shared_value::SharedStr::hash_cached(s);
        }
        self.find_object_native(key)
    }

    /// Append `key()` (Python hash `hash`), exactly absent, with
    /// `value()`, at the empty slot `pos` a [`Self::find_native`] miss
    /// gave, when that needs no growth: `false`, changing nothing (and
    /// building nothing), otherwise. Calls nothing itself.
    #[inline(always)]
    pub fn push_native(
        &mut self,
        pos: usize,
        hash: i64,
        key: impl FnOnce() -> DictKey,
        value: impl FnOnce() -> V,
    ) -> bool {
        let i = self.entries.len();
        if i == self.entries.capacity() {
            return false;
        }
        if self.is_small() {
            if i >= SMALL {
                return false;
            }
        } else {
            if (i + 1) * 3 > self.nslots() * 2 || i >= IX_MASK as usize - 1 {
                return false;
            }
            // SAFETY: `pos` is an empty slot inside the table.
            unsafe {
                *self.slots.as_ptr().add(pos) = (mix(hash) & !IX_MASK) | (i as u64 + 1);
            }
        }
        // SAFETY: `i` is below the capacity (checked); the length covers
        // the bucket once it's written.
        unsafe {
            self.entries.as_mut_ptr().add(i).write(Bucket {
                hash,
                key: key(),
                value: value(),
            });
            self.entries.set_len(i + 1);
        }
        true
    }

    /// The value of entry `i`, which a [`Self::find_native`] of the
    /// caller's found.
    ///
    /// # Safety
    ///
    /// `i` is an entry that lookup gave, with no change to the table
    /// since.
    #[inline(always)]
    pub unsafe fn value_at_unchecked(&self, i: usize) -> &V {
        // SAFETY: per the caller.
        unsafe { &self.entries.get_unchecked(i).value }
    }

    /// [`Self::value_at_unchecked`], mutably.
    ///
    /// # Safety
    ///
    /// As for [`Self::value_at_unchecked`].
    #[inline(always)]
    pub unsafe fn value_at_unchecked_mut(&mut self, i: usize) -> &mut V {
        // SAFETY: per the caller.
        unsafe { &mut self.entries.get_unchecked_mut(i).value }
    }

    /// [`Self::get_index_of`], always in line (see [`Self::get_hot`]).
    #[inline(always)]
    pub fn get_index_hot<Q: Probe + ?Sized>(&self, key: &Q) -> Option<usize> {
        let p = self.find_hot(key)?;
        Some(self.position_of(p))
    }

    /// [`Self::find`], always in line.
    #[inline(always)]
    fn find_hot<Q: Probe + ?Sized>(&self, key: &Q) -> Option<usize> {
        if self.is_small() {
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
        let i = self.find(key)?;
        // SAFETY: a found position is in bounds.
        Some(unsafe { &self.entries.get_unchecked(i).value })
    }

    #[inline]
    pub fn get_mut<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<&mut V> {
        let i = self.find(key)?;
        // SAFETY: a found position is in bounds.
        Some(unsafe { &mut self.entries.get_unchecked_mut(i).value })
    }

    #[inline]
    pub fn get_full<Q: Probe + ?Sized>(&self, key: &Q) -> Option<(usize, &DictKey, &V)> {
        let p = self.find(key)?;
        let b = &self.entries[p];
        Some((self.position_of(p), &b.key, &b.value))
    }

    #[inline]
    pub fn get_full_mut<Q: Probe + ?Sized>(
        &mut self,
        key: &Q,
    ) -> Option<(usize, &DictKey, &mut V)> {
        let p = self.find(key)?;
        let i = self.position_of(p);
        let b = &mut self.entries[p];
        Some((i, &b.key, &mut b.value))
    }

    #[inline]
    pub fn get_key_value<Q: Probe + ?Sized>(&self, key: &Q) -> Option<(&DictKey, &V)> {
        let i = self.find(key)?;
        let b = &self.entries[i];
        Some((&b.key, &b.value))
    }

    #[inline]
    pub fn contains_key<Q: Probe + ?Sized>(&self, key: &Q) -> bool {
        self.find(key).is_some()
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
            .map(|p| self.position_of(p))
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
        self.entries.get(self.entry_of(i)).map(|b| b.hash)
    }

    // ----------------------------------------------------------------
    // Positional access.

    #[inline]
    pub fn get_index(&self, i: usize) -> Option<(&DictKey, &V)> {
        self.entries
            .get(self.entry_of(i))
            .map(|b| (&b.key, &b.value))
    }

    #[inline]
    pub fn get_index_mut(&mut self, i: usize) -> Option<(&DictKey, &mut V)> {
        let p = self.entry_of(i);
        self.entries.get_mut(p).map(|b| (&b.key, &mut b.value))
    }

    /// Mutable access to a key in place. The caller must keep its hash
    /// and equality unchanged.
    #[inline]
    pub fn get_index_mut2(&mut self, i: usize) -> Option<(&mut DictKey, &mut V)> {
        let p = self.entry_of(i);
        self.entries.get_mut(p).map(|b| (&mut b.key, &mut b.value))
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
    /// a missed [`Self::find_hashed`]): its entry.
    #[inline(always)]
    fn push_at(&mut self, pos: usize, hash: i64, key: DictKey, value: V) -> usize {
        let i = self.entries.len();
        if self.is_small() {
            if i < SMALL {
                self.entries.push(Bucket { hash, key, value });
                return i;
            }
        } else if (i + 1) * 3 <= self.nslots() * 2 && i < IX_MASK as usize - 1 {
            // SAFETY: `pos` is an empty slot inside the table.
            unsafe {
                *self.slots.as_ptr().add(pos) = (mix(hash) & !IX_MASK) | (i as u64 + 1);
            }
            self.entries.push(Bucket { hash, key, value });
            return i;
        }
        self.push_growing(Bucket { hash, key, value })
    }

    /// [`Self::push_at`] for an entry that outgrows the slot table: the
    /// table is rebuilt larger with it.
    #[inline(never)]
    fn push_growing(&mut self, bucket: Bucket<V>) -> usize {
        if self.dead != 0 {
            // Tombstones took the room: close them up instead, in a table
            // with room for half as many keys again (so a table that loses
            // a key for each it gains makes this pass once per that many).
            let keys = self.len() + 1;
            self.drop_tombstones();
            let i = self.entries.len();
            self.entries.push(bucket);
            self.rebuild_slots(slots_for(keys + keys / 2).max(self.nslots()));
            return i;
        }
        let i = self.entries.len();
        assert!(i < IX_MASK as usize - 1, "dict too large");
        let n = if self.is_small() {
            slots_for(i + 1)
        } else {
            self.nslots() * 2
        };
        self.entries.push(bucket);
        self.rebuild_slots(n);
        i
    }

    /// Insert or replace: the entry's position and the replaced value. A
    /// key that compares natively (see [`Self::find_object_native`]) is
    /// probed without the general equality.
    #[inline]
    pub fn insert_full(&mut self, key: DictKey, value: V) -> (usize, Option<V>) {
        if let Some((hash, r)) = self.find_object_hashing(&key.0) {
            return match r {
                Ok(p) => {
                    // SAFETY: a found entry.
                    let slot = unsafe { &mut self.entries.get_unchecked_mut(p).value };
                    let old = std::mem::replace(slot, value);
                    (self.position_of(p), Some(old))
                }
                Err(pos) => (self.push_last(pos, hash, key, value), None),
            };
        }
        let hash = key.probe_hash();
        self.insert_hashed(hash, key, value)
    }

    /// [`Self::insert_full`] with the key's Python hash precomputed.
    #[inline]
    pub fn insert_hashed(&mut self, hash: i64, key: DictKey, value: V) -> (usize, Option<V>) {
        match self.find_hashed(hash, &key) {
            Ok(p) => {
                // SAFETY: a found entry is in bounds.
                let slot = unsafe { &mut self.entries.get_unchecked_mut(p).value };
                let old = std::mem::replace(slot, value);
                (self.position_of(p), Some(old))
            }
            Err(pos) => (self.push_last(pos, hash, key, value), None),
        }
    }

    /// [`Self::push_at`], giving the new key's position (the last).
    #[inline(always)]
    fn push_last(&mut self, pos: usize, hash: i64, key: DictKey, value: V) -> usize {
        let p = self.push_at(pos, hash, key, value);
        p - self.dead as usize
    }

    #[inline]
    pub fn insert(&mut self, key: DictKey, value: V) -> Option<V> {
        self.insert_full(key, value).1
    }

    /// Append `key` with Python hash `hash` without looking for an equal
    /// key: the caller knows there is none. Its position.
    pub fn insert_unique_hashed(&mut self, hash: i64, key: DictKey, value: V) -> usize {
        if self.is_small() {
            return self.push_at(0, hash, key, value);
        }
        let m = mix(hash);
        let mask = self.slot_mask();
        let mut pos = (m >> self.shift) as usize;
        while self.slot(pos) != 0 {
            pos = (pos + 1) & mask;
        }
        self.push_last(pos, hash, key, value)
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

    /// Remove entry `p`, moving the last entry into its place.
    fn swap_remove_entry(&mut self, p: usize) -> (DictKey, V) {
        let n = self.entries.len();
        if !self.is_small() {
            let pos = self.slot_of(self.entries[p].hash, p);
            self.erase_slot(pos);
            if p != n - 1 {
                let h = self.entries[n - 1].hash;
                self.repoint(h, n - 1, p);
            }
        }
        let b = self.entries.swap_remove(p);
        self.trim();
        (b.key, b.value)
    }

    /// Remove the key at position `i`, moving the last key into its place.
    pub fn swap_remove_index(&mut self, i: usize) -> Option<(DictKey, V)> {
        let p = self.entry_of(i);
        if p >= self.entries.len() {
            return None;
        }
        Some(self.swap_remove_entry(p))
    }

    /// Put `key` (hash `hash`), known to be absent, in the place of the
    /// key at position `i`, leaving every other key where it is: the
    /// replaced key and value.
    pub fn replace_index_unique(
        &mut self,
        i: usize,
        hash: i64,
        key: DictKey,
        value: V,
    ) -> Option<(DictKey, V)> {
        let i = self.entry_of(i);
        let old = self.entries.get(i)?.hash;
        if !self.is_small() {
            let pos = self.slot_of(old, i);
            self.erase_slot(pos);
            let m = mix(hash);
            let mask = self.slot_mask();
            let mut pos = (m >> self.shift) as usize;
            while self.slot(pos) != 0 {
                pos = (pos + 1) & mask;
            }
            self.set_slot(pos, (m & !IX_MASK) | (i as u64 + 1));
        }
        let b = std::mem::replace(&mut self.entries[i], Bucket { hash, key, value });
        Some((b.key, b.value))
    }

    /// Remove the last key.
    pub fn pop(&mut self) -> Option<(DictKey, V)> {
        let n = self.entries.len();
        if n == 0 {
            return None;
        }
        if !self.is_small() {
            let pos = self.slot_of(self.entries[n - 1].hash, n - 1);
            self.erase_slot(pos);
        }
        let b = self.entries.pop()?;
        self.trim();
        Some((b.key, b.value))
    }

    pub fn swap_remove<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<V> {
        let p = self.find(key)?;
        Some(self.swap_remove_entry(p).1)
    }

    pub fn swap_remove_full<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(usize, DictKey, V)> {
        let p = self.find(key)?;
        let i = self.position_of(p);
        let (k, v) = self.swap_remove_entry(p);
        Some((i, k, v))
    }

    /// Keep the entries `keep` accepts, in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&DictKey, &mut V) -> bool) {
        let n = self.entries.len();
        self.entries
            .retain_mut(|b| !is_tomb(b) && keep(&b.key, &mut b.value));
        if self.entries.len() != n {
            self.forget_tombstones();
            self.reindex();
        }
    }

    /// Keep the first `len` entries.
    pub fn truncate(&mut self, len: usize) {
        self.make_dense();
        if len < self.entries.len() {
            self.entries.truncate(len);
            self.reindex();
        }
    }

    /// Split off the entries from `at` on.
    pub fn split_off(&mut self, at: usize) -> Self {
        self.make_dense();
        let tail = self.entries.split_off(at);
        self.reindex();
        let mut out = Self {
            entries: tail,
            slots: std::ptr::NonNull::dangling(),
            shift: 64,
            dead: 0,
            holes: None,
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
        self.make_dense();
        let out: Vec<(DictKey, V)> = self
            .entries
            .drain(range)
            .map(|b| (b.key, b.value))
            .collect();
        self.reindex();
        out.into_iter()
    }

    /// Move entry `from` to position `to`, shifting the ones between, in
    /// a table without tombstones.
    fn rotate_index(&mut self, from: usize, to: usize) {
        if self.is_small() {
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
            self.set_slot(pos, (self.slot(pos) & !IX_MASK) | (new as u64 + 1));
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
        let (a, b) = (self.entry_of(a), self.entry_of(b));
        if !self.is_small() {
            let pa = self.slot_of(self.entries[a].hash, a);
            let pb = self.slot_of(self.entries[b].hash, b);
            self.set_slot(pa, (self.slot(pa) & !IX_MASK) | (b as u64 + 1));
            self.set_slot(pb, (self.slot(pb) & !IX_MASK) | (a as u64 + 1));
        }
        self.entries.swap(a, b);
    }

    /// Reverse the order.
    pub fn reverse(&mut self) {
        self.make_dense();
        self.entries.reverse();
        self.reindex();
    }

    /// Sort the entries with `cmp` (stable).
    pub fn sort_by(
        &mut self,
        mut cmp: impl FnMut(&DictKey, &V, &DictKey, &V) -> std::cmp::Ordering,
    ) {
        self.make_dense();
        self.entries
            .sort_by(|a, b| cmp(&a.key, &a.value, &b.key, &b.value));
        self.reindex();
    }

    /// Sort the entries by key with `cmp` (stable).
    pub fn sort_by_key_with(
        &mut self,
        mut cmp: impl FnMut(&DictKey, &DictKey) -> std::cmp::Ordering,
    ) {
        self.make_dense();
        self.entries.sort_by(|a, b| cmp(&a.key, &b.key));
        self.reindex();
    }

    // ----------------------------------------------------------------
    // Iteration.

    #[inline]
    pub fn iter(&self) -> Iter<'_, V> {
        Iter {
            left: self.len(),
            inner: self.entries.iter(),
        }
    }

    #[inline]
    pub fn iter_mut(&mut self) -> IterMut<'_, V> {
        IterMut {
            left: self.len(),
            inner: self.entries.iter_mut(),
        }
    }

    /// Mutable keys and values. The caller must keep every key's hash and
    /// equality unchanged.
    #[inline]
    pub fn iter_mut2(&mut self) -> IterMut2<'_, V> {
        IterMut2 {
            left: self.len(),
            inner: self.entries.iter_mut(),
        }
    }

    #[inline]
    pub fn keys(&self) -> Keys<'_, V> {
        Keys {
            left: self.len(),
            inner: self.entries.iter(),
        }
    }

    #[inline]
    pub fn values(&self) -> Values<'_, V> {
        Values {
            left: self.len(),
            inner: self.entries.iter(),
        }
    }

    #[inline]
    pub fn values_mut(&mut self) -> ValuesMut<'_, V> {
        ValuesMut {
            left: self.len(),
            inner: self.entries.iter_mut(),
        }
    }

    pub fn into_keys(mut self) -> impl DoubleEndedIterator<Item = DictKey> + ExactSizeIterator {
        self.drop_tombstones();
        std::mem::take(&mut self.entries).into_iter().map(|b| b.key)
    }

    pub fn into_values(mut self) -> impl DoubleEndedIterator<Item = V> + ExactSizeIterator {
        self.drop_tombstones();
        std::mem::take(&mut self.entries)
            .into_iter()
            .map(|b| b.value)
    }
}

impl<V: Vacant> Table<V> {
    /// Remove entry `p` of a table with slots. The entries behind it move
    /// down, unless the table has tombstones already, or `lazy` and more
    /// than [`SHIFT_MAX`] would move: then a tombstone takes its place.
    fn remove_entry(&mut self, p: usize, lazy: bool) -> (DictKey, V) {
        let n = self.entries.len();
        let pos = self.slot_of(self.entries[p].hash, p);
        self.erase_slot(pos);
        if p == n - 1 {
            let b = self.entries.pop().expect("entry `p`");
            self.trim();
            return (b.key, b.value);
        }
        let moved = n - 1 - p;
        if self.dead == 0 && (!lazy || moved <= SHIFT_MAX) {
            let b = self.entries.remove(p);
            // Renumber the entries that moved down: one slot walk each
            // when few moved, else one pass over the table.
            if moved * 4 <= self.nslots() {
                for j in p..n - 1 {
                    let h = self.entries[j].hash;
                    self.repoint(h, j + 1, j);
                }
            } else {
                let above = p as u64 + 1;
                for s in self.slot_words_mut() {
                    if *s & IX_MASK > above {
                        *s -= 1;
                    }
                }
            }
            return (b.key, b.value);
        }
        let tomb = Bucket {
            hash: 0,
            key: DictKey(Object::Unbound),
            value: V::vacant(),
        };
        let b = std::mem::replace(&mut self.entries[p], tomb);
        self.dead += 1;
        let cap = self.entries.capacity();
        self.holes
            .get_or_insert_with(|| Box::new(Holes::new()))
            .mark(p, cap, self.dead as usize);
        // More tombstones than keys: close them up (a pass over the keys,
        // paid for by the removals that made the tombstones).
        if self.dead as usize > self.len() + SHIFT_MAX {
            self.compact(self.nslots());
        }
        (b.key, b.value)
    }

    /// Remove entry `p` (see [`Self::remove_entry`]).
    #[inline]
    fn take(&mut self, p: usize, lazy: bool) -> (DictKey, V) {
        if self.is_small() {
            let b = self.entries.remove(p);
            return (b.key, b.value);
        }
        self.remove_entry(p, lazy)
    }

    /// Remove the key at position `i`, shifting the later entries down.
    pub fn shift_remove_index(&mut self, i: usize) -> Option<(DictKey, V)> {
        let p = self.entry_of(i);
        if p >= self.entries.len() {
            return None;
        }
        Some(self.take(p, false))
    }

    /// Remove the key at position `i`; the later keys' positions go down
    /// by one, but their entries may stay where they are, a tombstone in
    /// the removed one's place (see the module docs). For a table that
    /// loses keys anywhere, many times over: the cost doesn't grow with
    /// the keys behind the removed one.
    pub fn lazy_remove_index(&mut self, i: usize) -> Option<(DictKey, V)> {
        let p = self.entry_of(i);
        if p >= self.entries.len() {
            return None;
        }
        Some(self.take(p, true))
    }

    pub fn shift_remove_full<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(usize, DictKey, V)> {
        let p = self.find(key)?;
        let i = self.position_of(p);
        let (k, v) = self.take(p, false);
        Some((i, k, v))
    }

    pub fn shift_remove_entry<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(DictKey, V)> {
        let p = self.find(key)?;
        Some(self.take(p, false))
    }

    pub fn shift_remove<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<V> {
        self.shift_remove_entry(key).map(|(_, v)| v)
    }

    /// [`Self::lazy_remove_index`] of the key equal to `key`.
    pub fn lazy_remove_entry<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<(DictKey, V)> {
        let p = self.find(key)?;
        Some(self.take(p, true))
    }

    /// [`Self::lazy_remove_entry`] if `ok` accepts the key's value (one
    /// probe for the test and the removal).
    pub fn lazy_remove_entry_if<Q: Probe + ?Sized>(
        &mut self,
        key: &Q,
        ok: impl FnOnce(&V) -> bool,
    ) -> Option<(DictKey, V)> {
        let p = self.find(key)?;
        if !ok(&self.entries[p].value) {
            return None;
        }
        Some(self.take(p, true))
    }

    /// [`Self::lazy_remove_entry`], giving the value alone.
    pub fn lazy_remove<Q: Probe + ?Sized>(&mut self, key: &Q) -> Option<V> {
        self.lazy_remove_entry(key).map(|(_, v)| v)
    }

    /// Move the key at position `from` to position `to`, shifting the ones
    /// between. A move to the end is a lazy removal and an append.
    pub fn move_index(&mut self, from: usize, to: usize) {
        let n = self.len();
        assert!(from < n && to < n, "move_index out of bounds");
        if from == to {
            return;
        }
        if to == n - 1 && !self.is_small() {
            let p = self.entry_of(from);
            let hash = self.entries[p].hash;
            let (key, value) = self.remove_entry(p, true);
            self.insert_unique_hashed(hash, key, value);
            return;
        }
        self.make_dense();
        self.rotate_index(from, to);
    }
}

/// An entry of [`DictMap::entry`].
#[derive(Debug)]
pub enum Entry<'a, V = Object> {
    Occupied(OccupiedEntry<'a, V>),
    Vacant(VacantEntry<'a, V>),
}

/// A present entry (`index` is its entry, not its position).
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
            Entry::Occupied(e) => e.index(),
            Entry::Vacant(e) => e.map.len(),
        }
    }
}

impl<'a, V> OccupiedEntry<'a, V> {
    /// The key's position.
    #[inline]
    pub fn index(&self) -> usize {
        self.map.position_of(self.index)
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
}

impl<V: Vacant> OccupiedEntry<'_, V> {
    pub fn shift_remove_entry(self) -> (DictKey, V) {
        self.map.take(self.index, false)
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
        self.map.push_last(self.pos, self.hash, key, value)
    }
}

macro_rules! entry_iter {
    ($name:ident, $inner:ty, $item:ty, |$b:ident| $map:expr) => {
        /// (`left` counts the keys ahead: the entries less their
        /// tombstones.)
        pub struct $name<'a, V = Object> {
            inner: $inner,
            left: usize,
        }

        impl<'a, V> Iterator for $name<'a, V> {
            type Item = $item;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                loop {
                    let $b = self.inner.next()?;
                    if !is_tomb(&*$b) {
                        self.left -= 1;
                        return Some($map);
                    }
                }
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                (self.left, Some(self.left))
            }

            #[inline]
            fn nth(&mut self, n: usize) -> Option<Self::Item> {
                if self.left == self.inner.len() {
                    // No tombstone ahead.
                    let found = self.inner.nth(n);
                    self.left = self.inner.len();
                    let $b = found?;
                    return Some($map);
                }
                for _ in 0..n {
                    self.next()?;
                }
                self.next()
            }

            #[inline]
            fn count(self) -> usize {
                self.left
            }

            #[inline]
            fn last(mut self) -> Option<Self::Item> {
                self.next_back()
            }
        }

        impl<V> DoubleEndedIterator for $name<'_, V> {
            #[inline]
            fn next_back(&mut self) -> Option<Self::Item> {
                loop {
                    let $b = self.inner.next_back()?;
                    if !is_tomb(&*$b) {
                        self.left -= 1;
                        return Some($map);
                    }
                }
            }

            #[inline]
            fn nth_back(&mut self, n: usize) -> Option<Self::Item> {
                if self.left == self.inner.len() {
                    let found = self.inner.nth_back(n);
                    self.left = self.inner.len();
                    let $b = found?;
                    return Some($map);
                }
                for _ in 0..n {
                    self.next_back()?;
                }
                self.next_back()
            }
        }

        impl<V> ExactSizeIterator for $name<'_, V> {
            #[inline]
            fn len(&self) -> usize {
                self.left
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
        Self {
            inner: self.inner.clone(),
            left: self.left,
        }
    }
}

impl<V> Clone for Keys<'_, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            left: self.left,
        }
    }
}

impl<V> Clone for Values<'_, V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            left: self.left,
        }
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

    fn into_iter(mut self) -> IntoIter<V> {
        self.drop_tombstones();
        IntoIter(std::mem::take(&mut self.entries).into_iter())
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
        &self.entries[self.entry_of(i)].value
    }
}

/// The table behind a `dict`.
pub type DictMap = Table<Object>;

impl Table<Object> {
    /// Drop every entry, in order, without the out-of-line drop of a
    /// scalar key or value (an `int` key, a `None` value): a dying dict's
    /// release. The entry list keeps its allocation, now empty.
    #[inline]
    pub(crate) fn clear_entries_inline(&mut self) {
        let n = self.entries.len();
        let base = self.entries.as_mut_ptr();
        /// Release `o` in place: nothing for a scalar, a plain decrement
        /// for a shared value that isn't its last owner, else its drop.
        #[inline(always)]
        unsafe fn release(o: *mut Object) {
            // SAFETY: per the caller, `o` is a live object dropped once.
            unsafe {
                if let Some(word) = (*o).strong_word() {
                    if !crate::rc::try_release_word(word) {
                        std::ptr::drop_in_place(o);
                    }
                }
            }
        }
        // SAFETY: the length goes to zero first, so a drop that panics
        // leaks the rest rather than dropping one twice; each entry is
        // released once.
        // (A tombstone's key and value are scalars: nothing to release.)
        unsafe {
            self.entries.set_len(0);
            for i in 0..n {
                let b = base.add(i);
                release(&raw mut (*b).key.0);
                release(&raw mut (*b).value);
            }
        }
        self.forget_tombstones();
    }
}

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

    /// [`Self::insert`], with the key's position. A key that compares
    /// natively (see [`Table::find_object_native`]) is probed without the
    /// general equality.
    #[inline]
    pub fn insert_full(&mut self, key: DictKey) -> (usize, bool) {
        if let Some((hash, r)) = self.0.find_object_hashing(&key.0) {
            return match r {
                Ok(i) => (i, false),
                Err(pos) => (self.0.push_at(pos, hash, key, ()), true),
            };
        }
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

    /// [`Self::insert_probe`] that adds the key only when `exact` says the
    /// probe's miss is final (asked after the probe ran): `Some(true)` it was
    /// added, `Some(false)` an equal key was present, `None` the miss can't
    /// be trusted and nothing changed.
    #[inline]
    pub fn insert_probe_if<Q: Probe + ?Sized>(
        &mut self,
        key: &Q,
        exact: impl FnOnce(&Q) -> bool,
        make: impl FnOnce() -> DictKey,
    ) -> Option<bool> {
        match self.0.probe_entry(key) {
            ProbeEntry::Occupied(_) => Some(false),
            ProbeEntry::Vacant(v) if exact(key) => {
                v.insert(make(), ());
                Some(true)
            }
            ProbeEntry::Vacant(_) => None,
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

    /// [`Table::find_object_native`] over the set's keys.
    #[inline(always)]
    pub fn find_object_native(&self, key: &Object) -> Option<(i64, Result<usize, usize>)> {
        self.0.find_object_native(key)
    }

    /// [`Table::find_type_native`] over the set's keys.
    #[inline(always)]
    pub fn find_type_native(
        &self,
        t: &crate::Rc<crate::types::TypeObject>,
    ) -> Option<Result<usize, usize>> {
        self.0.find_type_native(t)
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
        let tombs = m.entries.iter().filter(|b| is_tomb(b)).count();
        assert_eq!(tombs, m.dead as usize);
        assert!(!m.entries.last().is_some_and(is_tomb));
        assert_eq!(m.iter().len(), m.len());
        for (i, (key, _)) in m.iter().enumerate() {
            assert_eq!(m.get_index_of(key), Some(i));
            assert!(m.get_index(i).is_some_and(|(k, _)| k == key));
        }
        assert!(m.get_index(m.len()).is_none());
        if !m.is_small() {
            let used = m.slot_words().iter().filter(|s| **s != 0).count();
            assert_eq!(used, m.len());
        }
    }

    fn int(o: &Object) -> i64 {
        match o {
            Object::Int(i) => *i,
            _ => unreachable!(),
        }
    }

    /// Every operation, in a random order, against a plain vector of
    /// pairs: the same keys at the same positions throughout, whatever
    /// tombstones the table holds.
    #[test]
    fn matches_a_vector_through_lazy_removals() {
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut rnd = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as usize % n.max(1)
        };
        for round in 0..6 {
            let mut m = DictMap::new();
            let mut model: Vec<(i64, i64)> = Vec::new();
            let mut fresh = 0_i64;
            let steps = [400_i64, 3000, 3000, 6000, 12_000, 20_000][round];
            // (The first third of a round mostly adds keys, more of them
            // in the later rounds; the rest loses more than it adds.)
            let grow = [2, 3, 3, 4, 6, 8][round];
            let mut most = 0;
            for step in 0..steps {
                let n = model.len();
                let op = if step < steps / 3 && rnd(grow) != 0 {
                    0
                } else {
                    rnd(22)
                };
                most = most.max(m.dead);
                match op {
                    // A new key (several of every draw, so tables grow).
                    0..=2 | 22.. => {
                        fresh += 1;
                        let key = fresh * 1024 + (fresh % 3);
                        let (at, old) = m.insert_full(k(key), Object::Int(step));
                        assert!(old.is_none());
                        assert_eq!(at, n);
                        model.push((key, step));
                    }
                    // A present key's value.
                    3 if n > 0 => {
                        let i = rnd(n);
                        let (at, old) = m.insert_full(k(model[i].0), Object::Int(-1));
                        assert_eq!(at, i);
                        assert_eq!(old.as_ref().map(int), Some(model[i].1));
                        model[i].1 = -1;
                    }
                    // Lazy removals: by key, by position, the first key.
                    4..=6 if n > 0 => {
                        let i = rnd(n);
                        let (key, v) = m.lazy_remove_entry(&k(model[i].0)).expect("present");
                        assert_eq!((int(&key.0), int(&v)), model.remove(i));
                    }
                    7 | 8 if n > 0 => {
                        let i = if op == 7 { 0 } else { rnd(n) };
                        let (key, v) = m.lazy_remove_index(i).expect("in range");
                        assert_eq!((int(&key.0), int(&v)), model.remove(i));
                    }
                    9 if n > 0 => {
                        let i = rnd(n);
                        let (at, key, v) = m.shift_remove_full(&k(model[i].0)).expect("present");
                        assert_eq!(at, i);
                        assert_eq!((int(&key.0), int(&v)), model.remove(i));
                    }
                    10 if n > 0 => {
                        let i = rnd(n);
                        let (key, v) = m.swap_remove_index(i).expect("in range");
                        assert_eq!((int(&key.0), int(&v)), model.swap_remove(i));
                    }
                    11 if n > 0 => {
                        let (key, v) = m.pop().expect("a key");
                        assert_eq!(Some((int(&key.0), int(&v))), model.pop());
                    }
                    // Moves: to the end (a lazy removal and an append),
                    // to the front, anywhere.
                    12 | 13 if n > 0 => {
                        let from = rnd(n);
                        // (A move that isn't to the end closes the gaps,
                        // so it is the rare one.)
                        let to = match rnd(60) {
                            0 => 0,
                            1 => rnd(n),
                            _ => n - 1,
                        };
                        m.move_index(from, to);
                        let e = model.remove(from);
                        model.insert(to, e);
                    }
                    14 if n > 1 => {
                        let (a, b) = (rnd(n), rnd(n));
                        m.swap_indices(a, b);
                        model.swap(a, b);
                    }
                    15 if n > 0 => {
                        let i = rnd(n);
                        fresh += 1;
                        let key = k(fresh * 1024);
                        let hash = key.probe_hash();
                        let (old, _) = m
                            .replace_index_unique(i, hash, key, Object::Int(7))
                            .expect("in range");
                        assert_eq!(int(&old.0), model[i].0);
                        model[i] = (fresh * 1024, 7);
                    }
                    // Reads by position and by key.
                    16 if n > 0 => {
                        let i = rnd(n);
                        let (key, v) = m.get_index(i).expect("in range");
                        assert_eq!((int(&key.0), int(v)), model[i]);
                        assert_eq!(m.get_index_of(&k(model[i].0)), Some(i));
                        assert_eq!(m.get_full(&k(model[i].0)).map(|f| f.0), Some(i));
                        assert_eq!(m.hash_at(i), Some(k(model[i].0).probe_hash()));
                        assert_eq!(int(&m[i]), model[i].1);
                        assert_eq!(m.first().map(|f| int(&f.0 .0)), Some(model[0].0));
                        assert_eq!(m.last().map(|f| int(&f.0 .0)), Some(model[n - 1].0));
                        // A walk by position, forward and back.
                        let from = rnd(n);
                        for (j, e) in model.iter().enumerate().skip(from).take(40) {
                            assert_eq!(m.get_index(j).map(|f| int(&f.0 .0)), Some(e.0));
                        }
                        for j in (from.saturating_sub(40)..from).rev() {
                            assert_eq!(m.get_index(j).map(|f| int(&f.0 .0)), Some(model[j].0));
                        }
                        if let Entry::Occupied(e) = m.entry(k(model[i].0)) {
                            assert_eq!(e.index(), i);
                        } else {
                            panic!("present");
                        }
                    }
                    // Whole-table operations (rare: each closes the gaps).
                    17 if rnd(40) == 0 => match rnd(7) {
                        0 => {
                            m.reverse();
                            model.reverse();
                        }
                        1 => {
                            m.sort_by_key_with(|a, b| int(&a.0).cmp(&int(&b.0)));
                            model.sort_by_key(|e| e.0);
                        }
                        2 => {
                            let keep = rnd(n + 1);
                            m.truncate(keep);
                            model.truncate(keep);
                        }
                        3 => {
                            m.retain(|key, _| int(&key.0) % 5 != 0);
                            model.retain(|e| e.0 % 5 != 0);
                        }
                        4 => {
                            let (a, b) = (rnd(n + 1), rnd(n + 1));
                            let (a, b) = (a.min(b), a.max(b));
                            let got: Vec<i64> = m.drain(a..b).map(|e| int(&e.0 .0)).collect();
                            let want: Vec<i64> = model.drain(a..b).map(|e| e.0).collect();
                            assert_eq!(got, want);
                        }
                        5 => {
                            let at = rnd(n + 1);
                            let tail = m.split_off(at);
                            let want = model.split_off(at);
                            check(&tail);
                            let got: Vec<i64> = tail.keys().map(|key| int(&key.0)).collect();
                            assert_eq!(got, want.iter().map(|e| e.0).collect::<Vec<_>>());
                        }
                        _ => {
                            m.shrink_to_fit();
                        }
                    },
                    18 if rnd(20) == 0 => {
                        let c = m.clone();
                        check(&c);
                        assert_eq!(c.dead, 0);
                        let got: Vec<(i64, i64)> = c
                            .into_iter()
                            .map(|(key, v)| (int(&key.0), int(&v)))
                            .collect();
                        assert_eq!(got, model);
                    }
                    19 if n > 0 => {
                        let i = rnd(n);
                        if let Entry::Occupied(e) = m.entry(k(model[i].0)) {
                            let (key, v) = e.shift_remove_entry();
                            assert_eq!((int(&key.0), int(&v)), model.remove(i));
                        }
                    }
                    20 => {
                        if rnd(25) == 0 {
                            m.reserve(rnd(64));
                        }
                        let absent = k(-5);
                        assert!(m.get(&absent).is_none());
                        assert!(m.lazy_remove(&absent).is_none());
                        assert!(m.get_index_of(&absent).is_none());
                    }
                    _ => {}
                }
                assert_eq!(m.len(), model.len());
                assert_eq!(m.is_empty(), model.is_empty());
                if step % 97 == 0 || m.len() < 40 {
                    check(&m);
                    let got: Vec<(i64, i64)> =
                        m.iter().map(|(key, v)| (int(&key.0), int(v))).collect();
                    assert_eq!(got, model);
                    let back: Vec<i64> = m.keys().rev().map(|key| int(&key.0)).collect();
                    let want: Vec<i64> = model.iter().rev().map(|e| e.0).collect();
                    assert_eq!(back, want);
                    assert_eq!(m.values().len(), model.len());
                    let skip = rnd(model.len() + 2);
                    assert_eq!(
                        m.keys().nth(skip).map(|key| int(&key.0)),
                        model.get(skip).map(|e| e.0)
                    );
                    assert_eq!(
                        m.keys().nth_back(skip).map(|key| int(&key.0)),
                        model.iter().rev().nth(skip).map(|e| e.0)
                    );
                    for e in &model {
                        assert_eq!(m.get(&k(e.0)).map(int), Some(e.1));
                    }
                }
            }
            // Drained from the front: the first key each time.
            while let Some((key, _)) = m.first() {
                let key = int(&key.0);
                assert_eq!(key, model[0].0);
                assert!(m.lazy_remove(&k(key)).is_some());
                model.remove(0);
                if model.len().is_multiple_of(64) {
                    check(&m);
                }
            }
            assert!(m.is_empty() && model.is_empty());
            assert_eq!(m.dead, 0);
            assert!(round < 3 || most > 200, "round {round}: {most} tombstones");
        }
    }

    /// A table that loses its oldest key for each new one keeps its
    /// tombstones bounded by its keys, and a set never has one.
    #[test]
    fn tombstones_stay_bounded() {
        let mut m = DictMap::new();
        for i in 0..1000 {
            m.insert(k(i), Object::Int(i));
        }
        let mut most = 0;
        for i in 1000..60_000_i64 {
            m.insert(k(i), Object::Int(i));
            assert!(m.lazy_remove(&k(i - 1000)).is_some());
            most = most.max(m.dead as usize);
            assert!(m.entries.len() <= 3 * m.len() + SHIFT_MAX + 1);
        }
        assert!(most > 0);
        assert_eq!(m.len(), 1000);
        assert_eq!(m.first().map(|f| int(&f.0 .0)), Some(59_000));
        check(&m);
        let mut s = SetMap::new();
        for i in 0..200 {
            s.insert(k(i));
        }
        for i in 0..150 {
            assert!(s.shift_remove(&k(i)));
        }
        assert_eq!(s.0.dead, 0);
        assert_eq!(s.as_entries().len(), 50);
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
        assert!(m.is_small());
        m.insert(k(8), Object::Int(8));
        assert!(!m.is_small());
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
