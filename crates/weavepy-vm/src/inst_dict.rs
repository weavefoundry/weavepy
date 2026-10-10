//! Split instance dictionaries: CPython's shared keys and inline values.
//!
//! An ordinary instance stores its attributes as a plain vector of values
//! whose names live once per class in a [`SharedKeys`] table, as long as
//! it assigns them in the order the class's first instances did (the
//! usual `__init__` shape). Constructing such an instance allocates no
//! hash table, and an attribute's position is the same in every instance,
//! so the indexed caches that address `__dict__` entries by insertion
//! order serve both layouts unchanged.
//!
//! Anything that needs the real `__dict__` (`vars(obj)`, a `del`, an
//! out-of-order or thirty-first attribute, the C API, pickling) goes
//! through [`InstDict::get`] or one of its siblings, which *materializes*
//! it: the values move into an ordinary [`DictData`] published in the
//! instance, and the instance keeps that dictionary from then on, as
//! CPython does. Only the hot paths that know about the split layout
//! avoid that step, so every other path sees exactly the dictionary it
//! always did.

use crate::object::{DictData, DictKey, Object};
use crate::shared_value::SharedStr;
use crate::sync::{LazyArc, Rc, RefCell};
use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// The most attribute names a class shares (CPython's `SHARED_KEYS_MAX_SIZE`).
pub const SHARED_KEYS_CAP: usize = 30;

/// A class's attribute names for its split instance dictionaries, in the
/// order its instances first assigned them.
///
/// Append-only: a published name never moves or changes, and the table
/// never reallocates, so a reader needs only the published length.
/// Appends happen under the GIL (never in free-threaded mode).
pub struct SharedKeys {
    len: AtomicUsize,
    /// One bit per published name's Python hash (`hash & 63`): a clear
    /// bit proves a name absent without comparing any.
    filter: AtomicU64,
    keys: [UnsafeCell<MaybeUninit<DictKey>>; SHARED_KEYS_CAP],
    /// Each published name's Python hash.
    hashes: [UnsafeCell<i64>; SHARED_KEYS_CAP],
    /// Tables made from this one by a deletion (see
    /// [`Self::without`]): `(n, i, table)` for the first `n` names less
    /// the `i`th. Touched only under the GIL.
    derived: UnsafeCell<Vec<(u32, u32, Rc<SharedKeys>)>>,
}

// SAFETY: names are published with release/acquire ordering and are
// immutable afterwards; the single writer is serialized by the GIL.
unsafe impl Send for SharedKeys {}
// SAFETY: as above.
unsafe impl Sync for SharedKeys {}

impl std::fmt::Debug for SharedKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries((0..self.len()).filter_map(|i| self.get(i)))
            .finish()
    }
}

impl Default for SharedKeys {
    fn default() -> Self {
        Self {
            len: AtomicUsize::new(0),
            filter: AtomicU64::new(0),
            keys: [const { UnsafeCell::new(MaybeUninit::uninit()) }; SHARED_KEYS_CAP],
            hashes: [const { UnsafeCell::new(0) }; SHARED_KEYS_CAP],
            derived: UnsafeCell::new(Vec::new()),
        }
    }
}

impl SharedKeys {
    /// How many names are published.
    #[inline]
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    /// Whether no name is published yet.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `i`th name.
    #[inline]
    pub fn get(&self, i: usize) -> Option<&DictKey> {
        if i < self.len() {
            // SAFETY: slots below the published length are initialized
            // and never written again.
            Some(unsafe { (*self.keys[i].get()).assume_init_ref() })
        } else {
            None
        }
    }

    /// The first of the first `n` names equal to `name`, whose Python
    /// hash is `hash`.
    #[inline(always)]
    fn position_hashed(&self, n: usize, name: &str, hash: i64) -> Option<usize> {
        if self.filter.load(Ordering::Relaxed) & (1 << (hash & 63)) == 0 {
            return None;
        }
        (0..n.min(self.len())).find(|&i| {
            // SAFETY: slot `i` is published (below the length).
            let h = unsafe { *self.hashes[i].get() };
            h == hash && self.get(i).is_some_and(|k| key_names_str(k, name))
        })
    }

    /// How many leading names are not `name` (Python hash `hash`): its
    /// position, or all of them when none is. An instance holding no more
    /// values than this has no attribute `name`.
    pub(crate) fn names_before(&self, name: &str, hash: i64) -> usize {
        let n = self.len();
        self.position_hashed(n, name, hash).unwrap_or(n)
    }

    /// Publish the `str` name `name` as the next name; `None` when the
    /// table is full. The caller holds the GIL outside free-threaded mode.
    fn push(&self, name: &SharedStr) -> Option<usize> {
        self.push_key(
            DictKey(Object::Str(name.clone())),
            crate::object::py_str_hash(name),
        )
    }

    /// Remove the `i`th name, closing up the rest: only for a table no
    /// one else holds (an instance's private names, see
    /// [`SplitValues::remove_str`]).
    fn remove_at(&mut self, i: usize) {
        let n = *self.len.get_mut();
        if i >= n {
            return;
        }
        // SAFETY: `&mut self` is exclusive; the first `n` names are
        // initialized, the `i`th drops once, and the later ones move down
        // one slot each.
        unsafe {
            self.keys[i].get_mut().assume_init_drop();
            for k in i + 1..n {
                let key = self.keys[k].get_mut().assume_init_read();
                self.keys[k - 1].get_mut().write(key);
                *self.hashes[k - 1].get_mut() = *self.hashes[k].get_mut();
            }
        }
        *self.len.get_mut() = n - 1;
        let mut filter = 0u64;
        for h in &mut self.hashes[..n - 1] {
            filter |= 1 << (*h.get_mut() & 63);
        }
        *self.filter.get_mut() = filter;
        // The tables derived from the old names no longer derive from these.
        self.derived.get_mut().clear();
    }

    /// The first `n` names less the `i`th (`i + 1 < n <= len`), as a table
    /// of their own: the one an earlier deletion of the same shape made,
    /// when this table remembers it, so instances that delete the same
    /// attributes in the same order share their tables, as they share
    /// their class's (deletions in `__enter__`, say). A remembered table
    /// may have grown since (appends of later stores), but its first
    /// `n - 1` names stay these. The caller holds the GIL.
    fn without(&self, n: usize, i: usize) -> Rc<SharedKeys> {
        const DERIVED_CAP: usize = 8;
        // SAFETY: GIL-serialized; nothing below reaches this list again
        // (building a table runs no code).
        let derived = unsafe { &mut *self.derived.get() };
        if let Some((_, _, t)) = derived
            .iter()
            .find(|&&(dn, di, _)| dn as usize == n && di as usize == i)
        {
            return t.clone();
        }
        let fresh = SharedKeys::default();
        for k in (0..n).filter(|&k| k != i) {
            let key = self.get(k).expect("a set value's name").clone();
            // SAFETY: slot `k` is published (below the length).
            fresh.push_key(key, unsafe { *self.hashes[k].get() });
        }
        let fresh = Rc::new(fresh);
        if derived.len() < DERIVED_CAP {
            derived.push((n as u32, i as u32, fresh.clone()));
        }
        fresh
    }

    /// [`Self::push`] of a name key whose Python hash is `hash`.
    fn push_key(&self, key: DictKey, hash: i64) -> Option<usize> {
        let n = self.len.load(Ordering::Relaxed);
        if n >= SHARED_KEYS_CAP {
            return None;
        }
        // SAFETY: slot `n` is unpublished, so no reader can see it, and
        // the GIL serializes writers.
        unsafe {
            (*self.keys[n].get()).write(key);
            *self.hashes[n].get() = hash;
        }
        self.filter.fetch_or(1 << (hash & 63), Ordering::Relaxed);
        self.len.store(n + 1, Ordering::Release);
        Some(n)
    }
}

impl Drop for SharedKeys {
    fn drop(&mut self) {
        let n = *self.len.get_mut();
        for slot in &mut self.keys[..n] {
            // SAFETY: the first `n` slots are initialized, and each is
            // dropped once here.
            unsafe { slot.get_mut().assume_init_drop() };
        }
    }
}

/// `a == b` for attribute names without a call into `memcmp`: they are
/// short and almost always differ in length or the first byte.
#[inline(always)]
fn name_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    if a.len() > 16 {
        return a == b;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether the `str` key `key` names `name`: identity first (names are
/// interned on both sides), then contents.
#[inline]
fn key_names(key: &DictKey, name: &SharedStr) -> bool {
    matches!(&key.0, Object::Str(s) if SharedStr::ptr_eq(s, name) || name_eq(s, name))
}

/// [`key_names`] for a borrowed name.
#[inline]
fn key_names_str(key: &DictKey, name: &str) -> bool {
    matches!(&key.0, Object::Str(s) if name_eq(s, name))
}

/// A name's last position among a class's shared names (see
/// [`SplitValues::holds_memo`]): the names table's address and the
/// position. Touched only under the GIL.
#[derive(Default, Debug)]
pub struct NameMemo(std::cell::UnsafeCell<(usize, u32)>);

// SAFETY: read and written only under the GIL (never in free-threaded
// mode, whose paths don't consult it).
unsafe impl Sync for NameMemo {}
// SAFETY: as above.
unsafe impl Send for NameMemo {}

/// A name's position in the names table it was last found in (see
/// [`SplitValues::get_memo`]). A hit is verified against the table, so a
/// stale or torn pair only costs a search.
#[derive(Default, Debug)]
pub struct PosMemo {
    keys: AtomicUsize,
    pos: AtomicUsize,
    /// The address of the name's text in the table (a hit whose name
    /// sits at the same address, with the same hash, needs no compare).
    text: AtomicUsize,
}

impl PosMemo {
    pub const fn new() -> Self {
        Self {
            keys: AtomicUsize::new(0),
            pos: AtomicUsize::new(0),
            text: AtomicUsize::new(0),
        }
    }
}

/// The address of a `str` key's text (0 for any other key).
#[inline(always)]
fn key_text(key: &DictKey) -> usize {
    match &key.0 {
        Object::Str(s) => s.as_ptr() as usize,
        _ => 0,
    }
}

/// The header of an instance's split values allocation; the values
/// follow it.
#[repr(C)]
struct SplitHeader {
    /// An owned strong reference to the class's shared names.
    keys: *const SharedKeys,
    len: u32,
    cap: u32,
}

/// An instance's split attribute values: `values[i]` belongs to
/// `keys[i]`, and the instance holds exactly the first `len` shared
/// names. One pointer wide; the names, length and capacity live in the
/// values allocation's header.
#[derive(Default)]
pub struct SplitValues {
    block: Option<std::ptr::NonNull<SplitHeader>>,
}

// SAFETY: the block is owned exclusively; its contents are `Send`/`Sync`
// objects and a shared-keys reference.
unsafe impl Send for SplitValues {}
// SAFETY: as above; shared access is read-only.
unsafe impl Sync for SplitValues {}

impl std::fmt::Debug for SplitValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl Drop for SplitValues {
    fn drop(&mut self) {
        let Some(block) = self.block.take() else {
            return;
        };
        // SAFETY: the block is ours; its first `len` values are
        // initialized and its `keys` holds one strong count (or is null).
        unsafe {
            let h = block.as_ptr();
            std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(
                Self::values_ptr(h),
                (*h).len as usize,
            ));
            if !(*h).keys.is_null() {
                drop(Rc::from_raw((*h).keys));
            }
            std::alloc::dealloc(h.cast(), Self::layout((*h).cap as usize));
        }
    }
}

impl SplitValues {
    /// Where native code finds a split block's parts: the block pointer
    /// (null when empty) within the values, and the shared names' pointer,
    /// the `u32` length and the first value within a block.
    pub(crate) const BLOCK_OFFSET: usize = std::mem::offset_of!(Self, block);
    pub(crate) const KEYS_OFFSET: usize = std::mem::offset_of!(SplitHeader, keys);
    pub(crate) const LEN_OFFSET: usize = std::mem::offset_of!(SplitHeader, len);
    pub(crate) const CAP_OFFSET: usize = std::mem::offset_of!(SplitHeader, cap);
    pub(crate) const VALUES_OFFSET: usize = std::mem::size_of::<SplitHeader>();

    #[inline]
    fn layout(cap: usize) -> std::alloc::Layout {
        std::alloc::Layout::new::<SplitHeader>()
            .extend(std::alloc::Layout::array::<Object>(cap).expect("split capacity"))
            .expect("split layout")
            .0
            .pad_to_align()
    }

    /// The first value slot of the block at `h`.
    #[inline(always)]
    fn values_ptr(h: *mut SplitHeader) -> *mut Object {
        // SAFETY: the values start right after the header (both are
        // word-aligned), inside the block's allocation.
        unsafe { h.add(1).cast::<Object>() }
    }

    /// The shared names, while any value is (or was) set.
    #[inline(always)]
    fn keys(&self) -> Option<&SharedKeys> {
        let h = self.block?.as_ptr();
        // SAFETY: a non-null `keys` is a live strong reference owned by
        // the block, which outlives `&self`.
        unsafe { (*h).keys.as_ref() }
    }

    /// The shared names' address (`0` none) and how many values are set.
    #[inline(always)]
    pub fn keys_and_len(&self) -> (usize, usize) {
        match self.block {
            // SAFETY: the block is live while owned.
            Some(b) => unsafe {
                let h = b.as_ptr();
                ((*h).keys as usize, (*h).len as usize)
            },
            None => (0, 0),
        }
    }

    /// How many attributes are set.
    #[inline(always)]
    pub fn len(&self) -> usize {
        match self.block {
            // SAFETY: the block is live while owned.
            Some(b) => unsafe { (*b.as_ptr()).len as usize },
            None => 0,
        }
    }

    /// Whether no attribute is set.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every value, in assignment order.
    #[inline(always)]
    pub fn values(&self) -> &[Object] {
        match self.block {
            // SAFETY: the first `len` values are initialized.
            Some(b) => unsafe {
                std::slice::from_raw_parts(Self::values_ptr(b.as_ptr()), (*b.as_ptr()).len as usize)
            },
            None => &[],
        }
    }

    /// [`Self::values`], writable.
    #[inline(always)]
    fn values_mut(&mut self) -> &mut [Object] {
        match self.block {
            // SAFETY: as above, and `&mut self` is exclusive.
            Some(b) => unsafe {
                std::slice::from_raw_parts_mut(
                    Self::values_ptr(b.as_ptr()),
                    (*b.as_ptr()).len as usize,
                )
            },
            None => &mut [],
        }
    }

    /// Append `value`, growing the block to at least `want` slots; the
    /// first append adopts `keys`.
    fn push(&mut self, keys: impl FnOnce() -> Rc<SharedKeys>, want: usize, value: Object) {
        let (len, cap) = match self.block {
            // SAFETY: the block is live while owned.
            Some(b) => unsafe { ((*b.as_ptr()).len as usize, (*b.as_ptr()).cap as usize) },
            None => (0, 0),
        };
        if len == cap {
            let new_cap = want.max(len + 1).max(cap * 2).max(2);
            let new_layout = Self::layout(new_cap);
            // SAFETY: a nonzero-size layout; a grown block keeps its
            // header and initialized prefix (realloc copies them).
            // (The block is allocated with the header's alignment.)
            #[allow(clippy::cast_ptr_alignment)]
            let h = unsafe {
                match self.block {
                    Some(b) => {
                        std::alloc::realloc(b.as_ptr().cast(), Self::layout(cap), new_layout.size())
                    }
                    None => std::alloc::alloc(new_layout),
                }
            }
            .cast::<SplitHeader>();
            let Some(h) = std::ptr::NonNull::new(h) else {
                std::alloc::handle_alloc_error(new_layout);
            };
            // SAFETY: `h` is a live block of `new_cap` slots.
            unsafe {
                if self.block.is_none() {
                    h.as_ptr().write(SplitHeader {
                        keys: std::ptr::null(),
                        len: 0,
                        cap: 0,
                    });
                }
                (*h.as_ptr()).cap = new_cap as u32;
            }
            self.block = Some(h);
        }
        let h = self.block.expect("allocated above").as_ptr();
        // SAFETY: slot `len` is inside the capacity and uninitialized.
        unsafe {
            if (*h).keys.is_null() {
                (*h).keys = Rc::into_raw(keys());
            }
            Self::values_ptr(h).add(len).write(value);
            (*h).len = len as u32 + 1;
        }
    }

    /// The `i`th attribute in assignment order (the index a `__dict__`
    /// entry of the same instance would have).
    #[inline(always)]
    pub fn get_index(&self, i: usize) -> Option<(&DictKey, &Object)> {
        let v = self.values().get(i)?;
        let k = self.keys()?.get(i)?;
        Some((k, v))
    }

    /// The `i`th value, when these values are laid out over `keys` (so the
    /// name at `i` is whatever `keys` published there).
    #[inline(always)]
    pub fn get_over(&self, keys: *const SharedKeys, i: usize) -> Option<&Object> {
        let h = self.block?.as_ptr();
        // SAFETY: the block is live while owned, and its first `len` values
        // are initialized.
        unsafe {
            if !std::ptr::eq((*h).keys, keys) || i >= (*h).len as usize {
                return None;
            }
            Some(&*Self::values_ptr(h).add(i))
        }
    }

    /// [`Self::get_over`], writable in place.
    #[inline(always)]
    pub fn get_over_mut(&mut self, keys: *const SharedKeys, i: usize) -> Option<&mut Object> {
        let h = self.block?.as_ptr();
        // SAFETY: as `get_over`, and `&mut self` is exclusive.
        unsafe {
            if !std::ptr::eq((*h).keys, keys) || i >= (*h).len as usize {
                return None;
            }
            Some(&mut *Self::values_ptr(h).add(i))
        }
    }

    /// Append `value` as the value of position `i` of `keys`, when that
    /// is the next unset position of values laid out over `keys` (or the
    /// first value of an instance with none, which adopts `keys` through
    /// `share`); `Err` hands the value back, touching nothing.
    #[inline(always)]
    pub fn append_over(
        &mut self,
        keys: &SharedKeys,
        share: impl FnOnce() -> Rc<SharedKeys>,
        i: usize,
        value: Object,
    ) -> Result<(), Object> {
        if let Some(b) = self.block {
            let h = b.as_ptr();
            // SAFETY: the block is live while owned; slot `len` is inside
            // the capacity (checked) and uninitialized.
            unsafe {
                let len = (*h).len as usize;
                if std::ptr::eq((*h).keys, keys) && i == len && len < (*h).cap as usize {
                    Self::values_ptr(h).add(len).write(value);
                    (*h).len = len as u32 + 1;
                    return Ok(());
                }
                if !(*h).keys.is_null() || len != 0 {
                    return Err(value);
                }
            }
        }
        // No value yet (a fresh or recycled instance): adopt the names.
        if i != 0 || keys.is_empty() {
            return Err(value);
        }
        self.first_push(share, keys.len(), value);
        Ok(())
    }

    /// [`Self::push`] for an instance's first value, out of line.
    #[inline(never)]
    fn first_push(&mut self, keys: impl FnOnce() -> Rc<SharedKeys>, want: usize, value: Object) {
        // A recycled block too small for every shared name goes: the
        // appends after this one then always find room (see
        // `can_append_at`).
        if let Some(b) = self.block {
            // SAFETY: the block is live while owned; with no value and no
            // names (the caller's state) it owns nothing else.
            unsafe {
                let h = b.as_ptr();
                if ((*h).cap as usize) < want {
                    debug_assert!((*h).len == 0 && (*h).keys.is_null());
                    std::alloc::dealloc(h.cast(), Self::layout((*h).cap as usize));
                    self.block = None;
                }
            }
        }
        self.push(keys, want, value);
    }

    /// Make room for `cap` values before the first one arrives, adopting
    /// the class's names (`keys`): a constructor that sets its fields in the
    /// class's order then appends each in place, the first included (see
    /// [`Self::append_over`] and compiled code's new-key stores). Nothing
    /// when a block exists (values, or a recycled one) or `cap` is zero.
    pub(crate) fn reserve_for(&mut self, keys: impl FnOnce() -> Rc<SharedKeys>, cap: usize) {
        if self.block.is_some() || cap == 0 {
            return;
        }
        let layout = Self::layout(cap);
        // SAFETY: a nonzero-size layout (the header alone has a size).
        #[allow(clippy::cast_ptr_alignment)]
        let h = unsafe { std::alloc::alloc(layout) }.cast::<SplitHeader>();
        let Some(h) = std::ptr::NonNull::new(h) else {
            std::alloc::handle_alloc_error(layout);
        };
        // SAFETY: `h` is a fresh block of `cap` slots; the header owns one
        // strong count of the names.
        unsafe {
            h.as_ptr().write(SplitHeader {
                keys: Rc::into_raw(keys()),
                len: 0,
                cap: cap as u32,
            });
        }
        self.block = Some(h);
    }

    /// Lay `n` values down as the first `n` of values over `keys` (a
    /// fresh instance's constructor stores, in its names' order), taking
    /// value `k` from `value(k)` once each; a block without names adopts
    /// them through `share`. `false` when the block can't take them all
    /// at once (no block, values already set, too little room or other
    /// names), touching nothing and calling `value` never.
    pub(crate) fn fill_fresh(
        &mut self,
        keys: &SharedKeys,
        share: impl FnOnce() -> Rc<SharedKeys>,
        n: usize,
        mut value: impl FnMut(usize) -> Object,
    ) -> bool {
        if n > keys.len() {
            return false;
        }
        let Some(b) = self.block else {
            return false;
        };
        let h = b.as_ptr();
        // SAFETY: the block is live while owned; its first `n` slots are
        // inside the capacity (checked) and unset (the length is zero).
        unsafe {
            if (*h).len != 0 || ((*h).cap as usize) < n {
                return false;
            }
            if (*h).keys.is_null() {
                (*h).keys = Rc::into_raw(share());
            } else if !std::ptr::eq((*h).keys, keys) {
                return false;
            }
            let values = Self::values_ptr(h);
            for k in 0..n {
                values.add(k).write(value(k));
            }
            (*h).len = n as u32;
        }
        true
    }

    /// Whether [`Self::append_over`] of position `i` of `keys` succeeds
    /// once the positions from the current length up to `i` have been
    /// appended first (the next of a run of in-order appends).
    #[inline]
    pub fn can_append_at(&self, keys: &SharedKeys, i: usize) -> bool {
        if i >= keys.len() {
            return false;
        }
        match self.block {
            // SAFETY: the block is live while owned.
            Some(b) => unsafe {
                let h = b.as_ptr();
                if std::ptr::eq((*h).keys, keys) {
                    i >= (*h).len as usize && i < (*h).cap as usize
                } else {
                    // No values yet: the first append sizes the block for
                    // every name.
                    (*h).keys.is_null() && (*h).len == 0
                }
            },
            None => true,
        }
    }

    /// [`Self::get_index`] with the value writable in place.
    #[inline(always)]
    pub fn get_index_mut(&mut self, i: usize) -> Option<(&DictKey, &mut Object)> {
        let keys: *const SharedKeys = self.keys()?;
        let v = self.values_mut().get_mut(i)?;
        // SAFETY: the keys outlive `&mut self` (the block owns them), and
        // they are disjoint from the values.
        let k = unsafe { &*keys }.get(i)?;
        Some((k, v))
    }

    /// The position of attribute `name`.
    #[inline]
    pub fn position(&self, name: &SharedStr) -> Option<usize> {
        let keys = self.keys()?;
        let n = self.len();
        // The constructor shape: the class's next name is unset here, and
        // names are unique, so none of the set ones can match.
        if keys.get(n).is_some_and(|k| key_names(k, name)) {
            return None;
        }
        (0..n).find(|&i| keys.get(i).is_some_and(|k| key_names(k, name)))
    }

    /// The position of attribute `name` whose Python hash is `hash`: a
    /// name no instance of the class ever set answers without comparing.
    #[inline(always)]
    pub fn position_hashed(&self, name: &str, hash: i64) -> Option<usize> {
        self.keys()?.position_hashed(self.len(), name, hash)
    }

    /// Whether these values hold `name` (Python hash `hash`), with `memo`
    /// remembering the names table it last searched and the name's
    /// position there (all of them when absent). The table only grows,
    /// so values no longer than that position can't hold the name, and
    /// only longer ones search again.
    #[inline]
    pub fn holds_memo(&self, name: &str, hash: i64, memo: &NameMemo) -> bool {
        let Some(keys) = self.keys() else {
            return false;
        };
        // (A clear filter bit settles it at once.)
        if keys.filter.load(Ordering::Relaxed) & (1 << (hash & 63)) == 0 {
            return false;
        }
        let n = self.len();
        let at = std::ptr::from_ref(keys) as usize;
        // SAFETY: GIL-serialized (see `NameMemo`).
        let (k, before) = unsafe { *memo.0.get() };
        if k == at && n <= before as usize {
            return false;
        }
        let before = keys.names_before(name, hash);
        // SAFETY: as above.
        unsafe { *memo.0.get() = (at, u32::try_from(before).unwrap_or(u32::MAX)) };
        n > before
    }

    /// The value of attribute `name` (Python hash `hash`), with `memo`
    /// remembering the names table it was last found in and its position
    /// there. A name never moves within a table, so a hit checks just
    /// that position (the name there, in case the table's address was
    /// reused); anything else searches and remembers.
    #[inline]
    pub fn get_memo(
        &self,
        name: &str,
        hash: i64,
        memo: &PosMemo,
    ) -> Option<&Object> {
        let keys = self.keys()?;
        let n = self.len();
        let at = std::ptr::from_ref(keys) as usize;
        let (k, i) = (
            memo.keys.load(Ordering::Relaxed),
            memo.pos.load(Ordering::Relaxed),
        );
        if k == at && i < n {
            // SAFETY: slot `i` is published (below the values' length,
            // which never exceeds the table's).
            let h = unsafe { *keys.hashes[i].get() };
            if h == hash
                && keys.get(i).is_some_and(|key| {
                    key_text(key) == memo.text.load(Ordering::Relaxed)
                        || key_names_str(key, name)
                })
            {
                return self.values().get(i);
            }
        }
        let i = keys.position_hashed(n, name, hash)?;
        memo.keys.store(at, Ordering::Relaxed);
        memo.pos.store(i, Ordering::Relaxed);
        memo.text
            .store(keys.get(i).map_or(0, key_text), Ordering::Relaxed);
        self.values().get(i)
    }

    /// [`Self::position`] for a borrowed name (identity of the bytes
    /// first: a name read off an interned string settles without a
    /// comparison).
    #[inline]
    pub fn position_str(&self, name: &str) -> Option<usize> {
        let keys = self.keys()?;
        let n = self.len();
        for i in 0..n {
            if let Some(DictKey(Object::Str(s))) = keys.get(i) {
                if std::ptr::eq(s.as_ptr(), name.as_ptr()) && s.len() == name.len() {
                    return Some(i);
                }
            }
        }
        (0..n).find(|&i| keys.get(i).is_some_and(|k| key_names_str(k, name)))
    }

    /// The value of attribute `name`.
    #[inline]
    pub fn get(&self, name: &SharedStr) -> Option<&Object> {
        self.position(name).map(|i| &self.values()[i])
    }

    /// [`Self::get`] for a borrowed name.
    pub fn get_str(&self, name: &str) -> Option<&Object> {
        self.position_str(name).map(|i| &self.values()[i])
    }

    /// Every attribute, in assignment order.
    pub fn iter(&self) -> impl Iterator<Item = (&DictKey, &Object)> {
        let keys = self.keys();
        self.values()
            .iter()
            .enumerate()
            .filter_map(move |(i, v)| Some((keys?.get(i)?, v)))
    }

    /// A new strong handle on the names, while any value is (or was) set.
    fn keys_rc(&self) -> Option<Rc<SharedKeys>> {
        self.block.and_then(|b| {
            // SAFETY: a non-null `keys` is a live strong reference.
            let k = unsafe { (*b.as_ptr()).keys };
            (!k.is_null()).then(|| unsafe {
                Rc::increment_strong_count(k);
                Rc::from_raw(k)
            })
        })
    }

    /// A copy of the names and values (for a materialization that can't
    /// move them).
    fn snapshot(&self) -> (Option<Rc<SharedKeys>>, Vec<Object>) {
        let keys = self.block.and_then(|b| {
            // SAFETY: a non-null `keys` is a live strong reference.
            let k = unsafe { (*b.as_ptr()).keys };
            (!k.is_null()).then(|| unsafe {
                Rc::increment_strong_count(k);
                Rc::from_raw(k)
            })
        });
        (keys, self.values().to_vec())
    }

    /// Store `value` under the interned name `name` if the split layout
    /// can hold it: an existing attribute is overwritten in place
    /// (returning the old value), and a new one is appended when it is
    /// the next shared name (or becomes it). `Err` hands the value back
    /// when the instance needs a real dictionary instead.
    pub fn store(
        &mut self,
        class_keys: impl FnOnce() -> Rc<SharedKeys>,
        name: &SharedStr,
        value: Object,
    ) -> Result<Option<Object>, Object> {
        let n = self.len();
        let adopted;
        let keys: &SharedKeys = match self.keys() {
            Some(k) => k,
            None => {
                adopted = class_keys();
                &adopted
            }
        };
        // The constructor shape first: the class's next name (names are
        // unique, so it can't also be among the set ones).
        let next = keys.get(n);
        if !next.is_some_and(|k| key_names(k, name)) {
            // An existing attribute.
            if let Some(i) = (0..n).find(|&i| keys.get(i).is_some_and(|k| key_names(k, name))) {
                return Ok(Some(std::mem::replace(&mut self.values_mut()[i], value)));
            }
            // A new name at the end of the table.
            if next.is_some() || keys.len() != n || keys.push(name).is_none() {
                return Err(value);
            }
        }
        let want = keys.len();
        let keys_ptr: *const SharedKeys = keys;
        self.push(
            // SAFETY: `keys_ptr` is either the block's own reference or
            // `adopted`, both live here; the new strong count is the
            // block's.
            || unsafe {
                Rc::increment_strong_count(keys_ptr);
                Rc::from_raw(keys_ptr)
            },
            want,
            value,
        );
        Ok(None)
    }

    /// Remove attribute `name`, returning its value (`None` when it is not
    /// set). The last value simply leaves (the instance then holds one
    /// name fewer of the same table); removing any other lays the rest out
    /// over a table of their names, in order (the one other instances that
    /// deleted the same way share, see [`SharedKeys::without`], or one
    /// private to this instance, which closes up in place on the next
    /// deletion). Compiled code checks
    /// an instance's names against its class's before using a position, so
    /// such an instance takes the general paths from then on, as one with
    /// a real dictionary does, without materializing one (CPython's split
    /// dictionaries also keep their layout across a deletion).
    pub fn remove_str(&mut self, name: &str) -> Option<Object> {
        let i = self.position_str(name)?;
        let h = self.block?.as_ptr();
        // SAFETY: the block is live while owned, `i` is below its length,
        // and every value from `i` on moves down one slot exactly once
        // (the removed one moves out first).
        unsafe {
            let n = (*h).len as usize;
            let values = Self::values_ptr(h);
            let removed = values.add(i).read();
            if i + 1 < n {
                let mut keys = Rc::from_raw((*h).keys);
                match Rc::get_mut(&mut keys) {
                    // Names already private to this instance (an earlier
                    // deletion's) close up in place.
                    Some(own) => own.remove_at(i),
                    // Shared names: the table for the rest of them.
                    None => keys = keys.without(n, i),
                }
                (*h).keys = Rc::into_raw(keys);
                std::ptr::copy(values.add(i + 1), values.add(i), n - i - 1);
            }
            (*h).len = n as u32 - 1;
            Some(removed)
        }
    }

    /// Move every value out (the caller drops them after releasing the
    /// cell) and forget the names.
    pub fn take(&mut self) -> Vec<Object> {
        let Some(b) = self.block else {
            return Vec::new();
        };
        let h = b.as_ptr();
        // SAFETY: the first `len` values move out exactly once (the length
        // is zeroed before anything can observe it), and the names'
        // strong count is released once.
        unsafe {
            let n = (*h).len as usize;
            let mut out = Vec::with_capacity(n);
            std::ptr::copy_nonoverlapping(Self::values_ptr(h), out.as_mut_ptr(), n);
            out.set_len(n);
            (*h).len = 0;
            let keys = std::mem::replace(&mut (*h).keys, std::ptr::null());
            if !keys.is_null() {
                drop(Rc::from_raw(keys));
            }
            out
        }
    }

    /// Drop every value, keeping the allocation, and forget the names.
    ///
    /// The values leave the block before any of them drops (a drop can run
    /// code that reaches this instance), staged on the stack when there
    /// are few rather than in a fresh vector.
    pub fn reset(&mut self) {
        const STAGE: usize = 8;
        let Some(b) = self.block else {
            return;
        };
        let h = b.as_ptr();
        // SAFETY: as `take`: the first `len` values move out exactly once
        // (the length is zeroed before anything can observe it), and the
        // names' strong count is released once.
        unsafe {
            let n = (*h).len as usize;
            if n > STAGE {
                drop(self.take());
                return;
            }
            let mut staged = [const { std::mem::MaybeUninit::<Object>::uninit() }; STAGE];
            std::ptr::copy_nonoverlapping(Self::values_ptr(h), staged.as_mut_ptr().cast(), n);
            (*h).len = 0;
            let keys = std::mem::replace(&mut (*h).keys, std::ptr::null());
            if !keys.is_null() {
                drop(Rc::from_raw(keys));
            }
            std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(
                staged.as_mut_ptr().cast::<Object>(),
                n,
            ));
        }
    }
}

/// An instance's `__dict__`: split values until something needs the
/// dictionary itself, then that dictionary (see the module docs).
///
/// The accessors mirror [`LazyArc`]'s and materialize a split layout
/// first, so a caller that never heard of the split layout sees the same
/// dictionary it always did.
pub struct InstDict {
    lazy: LazyArc<RefCell<DictData>>,
    split: RefCell<SplitValues>,
}

impl Default for InstDict {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Rc<RefCell<DictData>>> for InstDict {
    fn from(d: Rc<RefCell<DictData>>) -> Self {
        Self {
            lazy: LazyArc::from(d),
            split: RefCell::new(SplitValues::default()),
        }
    }
}

impl From<LazyArc<RefCell<DictData>>> for InstDict {
    fn from(lazy: LazyArc<RefCell<DictData>>) -> Self {
        Self {
            lazy,
            split: RefCell::new(SplitValues::default()),
        }
    }
}

impl Clone for InstDict {
    #[track_caller]
    fn clone(&self) -> Self {
        // A shallow instance copy shares the dictionary itself.
        self.get();
        Self::from(self.lazy.clone())
    }
}

impl std::fmt::Debug for InstDict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("InstDict").field(&self.lazy).finish()
    }
}

impl InstDict {
    /// Where native code finds the published dictionary's pointer (null
    /// while the values are split) and the split values' cell.
    pub(crate) const LAZY_OFFSET: usize =
        std::mem::offset_of!(Self, lazy) + LazyArc::<RefCell<DictData>>::POINTER_OFFSET;
    pub(crate) const SPLIT_OFFSET: usize = std::mem::offset_of!(Self, split);

    pub fn new() -> Self {
        Self {
            lazy: LazyArc::new(),
            split: RefCell::new(SplitValues::default()),
        }
    }

    /// The instance that owns this field.
    #[inline]
    fn owner(&self) -> &crate::types::PyInstance {
        let off = std::mem::offset_of!(crate::types::PyInstance, dict);
        // SAFETY: an `InstDict` exists only as `PyInstance::dict`, so the
        // containing instance starts `off` bytes before it and outlives
        // this borrow (and is aligned as a `PyInstance`).
        #[allow(clippy::cast_ptr_alignment)]
        unsafe {
            &*std::ptr::from_ref(self)
                .cast::<u8>()
                .sub(off)
                .cast::<crate::types::PyInstance>()
        }
    }

    /// The published dictionary, if any, without materializing.
    #[inline]
    pub fn published(&self) -> Option<&RefCell<DictData>> {
        self.lazy.get()
    }

    /// The split values, exclusively.
    #[inline]
    pub fn split_mut(&mut self) -> &mut SplitValues {
        self.split.get_mut()
    }

    /// The split values cell (meaningful while nothing is published).
    #[inline]
    pub fn split_cell(&self) -> &RefCell<SplitValues> {
        &self.split
    }

    /// The split values, read without borrow bookkeeping: `None` when a
    /// dictionary is published, the cell is mutably borrowed, or cells are
    /// shared across threads.
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek`]: the view must not outlive
    /// anything that could store an attribute or materialize the dict.
    #[inline]
    pub unsafe fn split_peek(&self) -> Option<&SplitValues> {
        if self.lazy.get().is_some() {
            return None;
        }
        // SAFETY: forwarded contract.
        unsafe { self.split.peek() }
    }

    /// [`Self::split_peek`]'s exclusive form.
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek_mut`].
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn split_peek_mut(&self) -> Option<&mut SplitValues> {
        if self.lazy.get().is_some() {
            return None;
        }
        // SAFETY: forwarded contract.
        unsafe { self.split.peek_mut() }
    }

    /// Whether the split layout holds any attribute.
    #[inline]
    fn has_split(&self) -> bool {
        // SAFETY: a length read, finished before anything else runs.
        match unsafe { self.split.peek() } {
            Some(s) => !s.is_empty(),
            None => self.split.try_borrow().map_or(true, |s| !s.is_empty()),
        }
    }

    /// The dictionary, materializing a split layout; `None` when the
    /// instance has no attributes and never had a dictionary.
    #[inline]
    #[track_caller]
    pub fn get(&self) -> Option<&RefCell<DictData>> {
        if let Some(d) = self.lazy.get() {
            return Some(d);
        }
        if !self.has_split() {
            return None;
        }
        Some(self.materialize())
    }

    /// The dictionary, created by `make` when there is none (after
    /// materializing a split layout).
    #[inline]
    #[track_caller]
    pub fn get_or_init(&self, make: impl FnOnce() -> Rc<RefCell<DictData>>) -> &RefCell<DictData> {
        if let Some(d) = self.get() {
            return d;
        }
        self.lazy.get_or_init(make)
    }

    /// Another owner of the dictionary, if one exists (materializing).
    #[track_caller]
    pub fn get_shared(&self) -> Option<Rc<RefCell<DictData>>> {
        self.get();
        self.lazy.get_shared()
    }

    /// Another owner of the dictionary, created with the owner's
    /// deferred-tracking record when there is none.
    #[track_caller]
    pub fn share(&self) -> Rc<RefCell<DictData>> {
        self.owner().dict_cell();
        self.lazy.share()
    }

    /// The published dictionary's strong count (`0` when none is).
    pub fn strong_count(&self) -> usize {
        self.lazy.strong_count()
    }

    /// Move the split values into a published dictionary.
    #[cold]
    #[inline(never)]
    #[track_caller]
    fn materialize(&self) -> &RefCell<DictData> {
        note_materialize(std::panic::Location::caller());
        let owner = self.owner();
        let (keys, values) = match self.split.try_borrow_mut() {
            Ok(mut s) => {
                let keys = s.keys_rc();
                (keys, s.take())
            }
            // A live view of the values (nothing in the VM holds one across
            // a materializing call): copy them, and leave the stale split
            // to the next exclusive reset. Readers check the dictionary
            // first.
            Err(_) => {
                // SAFETY: only shared borrows can be live here; the copy
                // reads through the cell without creating a `&mut`.
                let s = unsafe { &*self.split.as_ptr() };
                s.snapshot()
            }
        };
        // A deferred owner holds only atomic values, and a tracked one
        // needs no barrier: the table is built without either. The names
        // are distinct `str`s whose hashes the shared table kept, so the
        // entries go down in order and are indexed once, without hashing
        // or comparing.
        let mut values = values;
        let map = match keys {
            Some(keys) => {
                let n = values.len().min(keys.len());
                let src = values.as_ptr();
                let map = crate::dictmap::DictMap::from_unique_hashed(n, |i| {
                    // SAFETY: slot `i` is published (below the length),
                    // and value `i` moves out once (the vector forgets the
                    // first `n` below).
                    unsafe {
                        let k = (*keys.keys[i].get()).assume_init_ref();
                        (*keys.hashes[i].get(), k.clone(), src.add(i).read())
                    }
                });
                // SAFETY: the first `n` values moved into the table; any
                // past the names (none in practice) drop with the vector.
                unsafe {
                    let rest = values.len() - n;
                    std::ptr::copy(src.add(n), values.as_mut_ptr(), rest);
                    values.set_len(rest);
                }
                map
            }
            None => crate::dictmap::DictMap::default(),
        };
        drop(values);
        let deferred = if owner.deferred.get() {
            std::ptr::from_ref(owner) as usize
        } else {
            0
        };
        let d = DictData::from_map_for(deferred, map);
        self.lazy.get_or_init(|| Rc::new(RefCell::new(d)))
    }
}

/// `WEAVEPY_SPLIT_TRACE=1`: count materializations by call site and
/// report them at exit (to find hot paths that should read the split
/// layout directly).
fn note_materialize(at: &'static std::panic::Location<'static>) {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static COUNTS: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);
    if !*ON.get_or_init(|| {
        let on = std::env::var_os("WEAVEPY_SPLIT_TRACE").is_some();
        if on {
            extern "C" fn report() {
                let Ok(g) = COUNTS.lock() else { return };
                let Some(m) = g.as_ref() else { return };
                let mut v: Vec<_> = m.iter().collect();
                v.sort_by(|a, b| b.1.cmp(a.1));
                eprintln!("## split dict materializations");
                for (site, n) in v.into_iter().take(40) {
                    eprintln!("{n:10} {site}");
                }
            }
            // SAFETY: registering a plain `extern "C"` exit handler.
            unsafe { libc::atexit(report) };
        }
        on
    }) {
        return;
    }
    if let Ok(mut g) = COUNTS.lock() {
        *g.get_or_insert_with(HashMap::new)
            .entry(format!("{}:{}", at.file(), at.line()))
            .or_default() += 1;
    }
}

impl crate::types::PyInstance {
    /// The attribute at position `i` of the class's shared names, while
    /// this instance's values are still split over its own class's names:
    /// the name there is fixed for as long as the class lives, so a
    /// caller that proved it once needs no name check again.
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek`].
    #[inline(always)]
    pub unsafe fn split_field(&self, i: usize) -> Option<&Object> {
        let keys: *const SharedKeys = self.cls_raw().shared_keys.get()?;
        // SAFETY: forwarded contract.
        unsafe { self.dict.split_peek() }?.get_over(keys, i)
    }

    /// Set the attribute at position `i` of the class's shared names by
    /// appending `value`, when the instance's split values stop just
    /// before `i` and their block has room (the constructor shape, after
    /// its first store); `Err` hands the value back, touching nothing.
    /// The caller proved the name at `i` and that a plain `__dict__`
    /// store is what the assignment means (see `split_store`).
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek_mut`].
    #[inline(always)]
    pub unsafe fn split_append(&self, i: usize, value: Object) -> Result<(), Object> {
        if self.c_body.get() != 0 || crate::gil::free_threading_enabled() {
            return Err(value);
        }
        let cls = self.cls_raw();
        let Some(keys) = cls.shared_keys.get() else {
            return Err(value);
        };
        if !value.is_gc_atomic() && self.deferred.get() {
            // The write barrier, before the values are borrowed (tracking
            // early is always sound).
            self.ensure_gc_tracked();
        }
        // SAFETY: forwarded contract.
        let Some(split) = (unsafe { self.dict.split_peek_mut() }) else {
            return Err(value);
        };
        split.append_over(keys, || cls.shared_keys.share(), i, value)
    }

    /// Store `value` at position `i` of the class's shared names from one
    /// view of the split values: over the value set there, when
    /// `droppable` accepts it (as [`Self::split_field_mut`]), else as the
    /// next value (as [`Self::split_append`]). The write barrier for the
    /// value runs first. `Err` hands the value back, touching nothing,
    /// with `true` when the old value was refused (the caller declines
    /// the whole store) and `false` when the split layout can't take it.
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek_mut`].
    #[inline(always)]
    pub unsafe fn split_store_at(
        &self,
        i: usize,
        value: Object,
        droppable: impl FnOnce(&Object) -> bool,
    ) -> Result<(), (Object, bool)> {
        let cls = self.cls_raw();
        let Some(keys) = cls.shared_keys.get() else {
            return Err((value, false));
        };
        if !value.is_gc_atomic() && self.deferred.get() {
            self.ensure_gc_tracked();
        }
        // SAFETY: forwarded contract.
        let Some(split) = (unsafe { self.dict.split_peek_mut() }) else {
            return Err((value, false));
        };
        if let Some(slot) = split.get_over_mut(keys, i) {
            if !droppable(slot) {
                return Err((value, true));
            }
            drop(std::mem::replace(slot, value));
            return Ok(());
        }
        if self.c_body.get() != 0 || crate::gil::free_threading_enabled() {
            return Err((value, false));
        }
        split
            .append_over(keys, || cls.shared_keys.share(), i, value)
            .map_err(|v| (v, false))
    }

    /// The split length and whether position `i` of the class's shared
    /// names is ready for a store: an overwrite of a set value (which
    /// `droppable` must accept), or the next of a run of in-order appends
    /// starting at `*cursor` (the split length once the earlier appends
    /// land; `None` starts it at the current length). `None` when the
    /// split layout can't say.
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek`].
    #[inline]
    pub unsafe fn split_store_ready(
        &self,
        i: usize,
        cursor: &mut Option<usize>,
        droppable: impl FnOnce(&Object) -> bool,
    ) -> Option<bool> {
        if self.c_body.get() != 0 || crate::gil::free_threading_enabled() {
            return None;
        }
        let keys = self.cls_raw().shared_keys.get()?;
        // SAFETY: forwarded contract.
        let split = unsafe { self.dict.split_peek() }?;
        let at = cursor.get_or_insert(split.len());
        if let Some(v) = split.get_over(keys, i) {
            return Some(droppable(v));
        }
        if i == *at && split.can_append_at(keys, i) {
            *at += 1;
            return Some(true);
        }
        None
    }

    /// [`Self::split_field`] for an in-place overwrite by a value of
    /// atomicity `atomic`, whose write barrier runs first (a non-atomic
    /// value starts tracking a deferred instance).
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek_mut`].
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn split_field_mut(&self, i: usize, atomic: bool) -> Option<&mut Object> {
        let keys: *const SharedKeys = self.cls_raw().shared_keys.get()?;
        if !atomic && self.deferred.get() {
            self.ensure_gc_tracked();
        }
        // SAFETY: forwarded contract.
        unsafe { self.dict.split_peek_mut() }?.get_over_mut(keys, i)
    }

    /// The `i`th attribute in assignment order, from whichever layout the
    /// instance uses, read without borrow bookkeeping. `None` when it is
    /// absent or the storage is borrowed (take the general path).
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek`]: the view ends before anything
    /// that could store an attribute runs.
    #[inline(always)]
    pub unsafe fn attr_peek_index(&self, i: usize) -> Option<(&DictKey, &Object)> {
        match self.dict.published() {
            // SAFETY: forwarded contract.
            Some(d) => unsafe { d.peek() }?.get_index(i),
            // SAFETY: forwarded contract.
            None => unsafe { self.dict.split_cell().peek() }?.get_index(i),
        }
    }

    /// [`Self::attr_peek_index`] for an in-place value store: the write
    /// barrier for a value of that atomicity runs first (a non-atomic
    /// value starts tracking a deferred instance), and the key layout is
    /// left alone.
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek_mut`].
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn attr_peek_index_mut(
        &self,
        i: usize,
        atomic: bool,
    ) -> Option<(&DictKey, &mut Object)> {
        match self.dict.published() {
            Some(d) => {
                // SAFETY: forwarded contract.
                let d = unsafe { d.peek_mut() }?;
                let map = if atomic {
                    d.map_mut_unstamped()
                } else {
                    d.map_mut_value_store()
                };
                map.get_index_mut(i)
            }
            None => {
                if !atomic && self.deferred.get() {
                    self.ensure_gc_tracked();
                }
                // SAFETY: forwarded contract.
                unsafe { self.dict.split_cell().peek_mut() }?.get_index_mut(i)
            }
        }
    }

    /// `f` of the `i`th attribute (either layout, borrowed safely); `None`
    /// when it is absent or the storage is mutably borrowed.
    #[inline]
    pub fn attr_index_map<R>(&self, i: usize, f: impl FnOnce(&DictKey, &Object) -> R) -> Option<R> {
        match self.dict.published() {
            Some(d) => {
                let d = d.try_borrow().ok()?;
                let (k, v) = d.get_index(i)?;
                Some(f(k, v))
            }
            None => {
                let s = self.dict.split_cell().try_borrow().ok()?;
                let (k, v) = s.get_index(i)?;
                Some(f(k, v))
            }
        }
    }

    /// Replace the value of the `i`th attribute (either layout, borrowed
    /// safely) with `value`, returning the displaced value; `Err` hands
    /// `value` back when there is no such attribute or the storage is
    /// borrowed. The write barrier for `value` runs first.
    pub fn attr_replace_index(&self, i: usize, value: Object) -> Result<Object, Object> {
        let atomic = value.is_gc_atomic();
        match self.dict.published() {
            Some(d) => {
                let Ok(mut d) = d.try_borrow_mut() else {
                    return Err(value);
                };
                let map = if atomic {
                    d.map_mut_unstamped()
                } else {
                    d.map_mut_value_store()
                };
                match map.get_index_mut(i) {
                    Some((_, slot)) => Ok(std::mem::replace(slot, value)),
                    None => Err(value),
                }
            }
            None => {
                if !atomic && self.deferred.get() {
                    self.ensure_gc_tracked();
                }
                let Ok(mut s) = self.dict.split_cell().try_borrow_mut() else {
                    return Err(value);
                };
                match s.get_index_mut(i) {
                    Some((_, slot)) => Ok(std::mem::replace(slot, value)),
                    None => Err(value),
                }
            }
        }
    }

    /// Whether attribute `name` (Python hash `hash`) is set, read without
    /// borrow bookkeeping; `None` when that can't be told natively.
    ///
    /// # Safety
    ///
    /// As [`crate::sync::GilCell::peek`].
    #[inline]
    pub unsafe fn attr_peek_has(&self, name: &str, hash: i64) -> Option<bool> {
        match self.dict.published() {
            Some(d) => {
                // SAFETY: forwarded contract.
                let d = unsafe { d.peek() }?;
                if d.is_empty() || !d.may_hold_str_hash(hash) {
                    return Some(false);
                }
                let probe = crate::object::LeafNameProbe::new(name, hash);
                let hit = d.contains_key(&probe);
                (!probe.saw_exotic()).then_some(hit)
            }
            None => {
                // SAFETY: forwarded contract.
                let s = unsafe { self.dict.split_cell().peek() }?;
                Some(s.position_hashed(name, hash).is_some())
            }
        }
    }

    /// Attribute `name`'s value (either layout), without materializing
    /// a split layout and without running code.
    pub fn attr_get_str(&self, name: &str) -> Option<Object> {
        match self.dict.published() {
            Some(d) => d.borrow().get(&crate::object::StrKey(name)).cloned(),
            None => self.dict.split_cell().borrow().get_str(name).cloned(),
        }
    }

    /// The position of attribute `name` in assignment order (either
    /// layout, no materializing).
    pub fn attr_position_str(&self, name: &str) -> Option<u32> {
        let i = match self.dict.published() {
            Some(d) => {
                // Never runs Python: a stored key only a user `__eq__`
                // could equate leaves the position unknown.
                let probe =
                    crate::object::LeafNameProbe::new(name, crate::object::py_str_hash(name));
                let i = d.try_borrow().ok()?.get_index_of(&probe);
                if probe.saw_exotic() {
                    return None;
                }
                i?
            }
            None => self
                .dict
                .split_cell()
                .try_borrow()
                .ok()?
                .position_str(name)?,
        };
        u32::try_from(i).ok()
    }

    /// How many attributes are set (either layout, no materializing).
    pub fn attr_count(&self) -> usize {
        match self.dict.published() {
            Some(d) => d.try_borrow().map_or(0, |d| d.len()),
            None => self.dict.split_cell().try_borrow().map_or(0, |s| s.len()),
        }
    }

    /// Visit every attribute (either layout, no materializing); a
    /// borrowed storage is skipped.
    pub fn for_each_attr(&self, mut f: impl FnMut(&DictKey, &Object)) {
        match self.dict.published() {
            Some(d) => {
                if let Ok(d) = d.try_borrow() {
                    for (k, v) in d.iter() {
                        f(k, v);
                    }
                }
            }
            None => {
                if let Ok(s) = self.dict.split_cell().try_borrow() {
                    for (k, v) in s.iter() {
                        f(k, v);
                    }
                }
            }
        }
    }

    /// Delete attribute `name` through the split layout (see
    /// [`SplitValues::remove_str`]): `Some` with the removed value, or with
    /// `None` when the instance has no such attribute; `None` when the
    /// instance has (or needs) a real dictionary. Runs no code: the caller
    /// releases the value. The caller has established that a plain
    /// `__dict__` deletion is what this `del` means.
    pub fn split_remove(&self, name: &str) -> Option<Option<Object>> {
        if self.dict.published().is_some()
            || self.c_body.get() != 0
            || crate::gil::free_threading_enabled()
            || self.cls_raw().native_kind.get() != 0
        {
            return None;
        }
        let mut split = self.dict.split_cell().try_borrow_mut().ok()?;
        Some(split.remove_str(name))
    }

    /// Store `value` under the interned name `name` through the split
    /// layout: the displaced value on an overwrite, or `Err` handing the
    /// value back when the instance needs (or has) a real dictionary.
    /// Runs no code. The caller has established that a plain `__dict__`
    /// store is what this assignment means.
    #[inline]
    pub fn split_store(&self, name: &SharedStr, value: Object) -> Result<Option<Object>, Object> {
        if self.dict.published().is_some()
            || self.c_body.get() != 0
            || crate::gil::free_threading_enabled()
        {
            return Err(value);
        }
        let cls = self.cls_raw();
        if cls.native_kind.get() != 0 {
            return Err(value);
        }
        if self.deferred.get() && !value.is_gc_atomic() {
            self.ensure_gc_tracked();
        }
        // SAFETY: nothing below runs code or reaches this cell again.
        let Some(split) = (unsafe { self.dict.split_cell().peek_mut() }) else {
            return Err(value);
        };
        split.store(|| cls.shared_keys.share(), name, value)
    }
}
