//! Shared immutable values whose slice metadata lives in their allocation.
//!
//! Reference counts and weak-reference synchronization remain with `Arc`.
//! Only strong handles omit metadata; weak handles retain their original DST
//! pointers, so they never read a payload after its last strong owner dies.
use std::alloc::Layout;
use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem::{align_of, size_of, ManuallyDrop};
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::{
    atomic::{AtomicI64, AtomicU64, Ordering},
    Arc, Weak,
};

mod sealed {
    pub trait Sealed {}
}

/// A private-layout payload with immutable slice length in its first word.
///
/// # Safety
///
/// Implementations must have an initialized, immutable `usize` length at
/// offset zero. `pointer` must reconstruct exactly the original Arc pointee,
/// including slice metadata, size, alignment, and element destruction.
/// Constructors must preserve this invariant for every live strong reference.
pub unsafe trait ThinPayload: sealed::Sealed {
    type View: ?Sized;
    fn pointer(data: *const usize, len: usize) -> *const Self;
    fn view(&self) -> &Self::View;
}

/// A single-word strong owner of a length-prefixed Arc payload.
pub struct ThinArc<T: ?Sized + ThinPayload> {
    data: NonNull<usize>,
    ownership: PhantomData<Arc<T>>,
}

// SAFETY: ownership and synchronization stay with Arc, and these are Arc's
// Send/Sync requirements. The stored pointer refers to an immutable payload.
unsafe impl<T: ?Sized + ThinPayload + Send + Sync> Send for ThinArc<T> {}
unsafe impl<T: ?Sized + ThinPayload + Send + Sync> Sync for ThinArc<T> {}

/// A weak owner retaining its full metadata after the payload is destroyed.
pub struct ThinWeak<T: ?Sized + ThinPayload>(Weak<T>);

impl<T: ?Sized + ThinPayload> fmt::Debug for ThinWeak<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl<T: ?Sized + ThinPayload> ThinArc<T> {
    pub(crate) fn from_arc(arc: Arc<T>) -> Self {
        let data = Arc::into_raw(arc).cast::<usize>().cast_mut();
        Self {
            // SAFETY: Arc::into_raw returns a non-null live allocation pointer.
            data: unsafe { NonNull::new_unchecked(data) },
            ownership: PhantomData,
        }
    }

    #[inline]
    fn raw(&self) -> *const T {
        // SAFETY: this owner retains a strong reference. ThinPayload guarantees
        // an initialized, immutable length word throughout that lifetime.
        let len = unsafe { self.data.as_ptr().read() };
        T::pointer(self.data.as_ptr(), len)
    }

    #[inline]
    fn arc_view(&self) -> ManuallyDrop<Arc<T>> {
        // SAFETY: borrow our accounted strong reference without dropping it.
        ManuallyDrop::new(unsafe { Arc::from_raw(self.raw()) })
    }

    #[inline]
    pub fn as_ptr(this: &Self) -> *const T::View {
        ptr::from_ref(&**this)
    }

    #[inline]
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        this.data == other.data
    }

    pub fn strong_count(this: &Self) -> usize {
        Arc::strong_count(&this.arc_view())
    }

    pub fn weak_count(this: &Self) -> usize {
        Arc::weak_count(&this.arc_view())
    }

    pub fn downgrade(this: &Self) -> ThinWeak<T> {
        ThinWeak(Arc::downgrade(&this.arc_view()))
    }

    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        let mut view = this.arc_view();
        let raw = ptr::from_mut(Arc::get_mut(&mut view)?);
        // SAFETY: Arc checked uniqueness of both strong and weak ownership.
        // The borrow is tied to &mut this. The temporary view neither adds nor
        // releases a reference, and no other owner can access the payload.
        unsafe { Some(&mut *raw) }
    }
}

impl<T: ?Sized + ThinPayload> Clone for ThinArc<T> {
    #[inline]
    fn clone(&self) -> Self {
        Self::from_arc(Arc::clone(&self.arc_view()))
    }
}

impl<T: ?Sized + ThinPayload> Drop for ThinArc<T> {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: release this owner's one strong reference exactly once.
        // Arc drops the payload and handles remaining weak references normally.
        unsafe { drop(Arc::from_raw(self.raw())) }
    }
}

impl<T: ?Sized + ThinPayload> Deref for ThinArc<T> {
    type Target = T::View;
    #[inline]
    fn deref(&self) -> &Self::Target {
        // SAFETY: raw reconstructs a live initialized payload, borrowed from self.
        unsafe { (&*self.raw()).view() }
    }
}

impl<T: ?Sized + ThinPayload> AsRef<T::View> for ThinArc<T> {
    fn as_ref(&self) -> &T::View {
        self
    }
}
impl<T: ?Sized + ThinPayload> fmt::Debug for ThinArc<T>
where
    T::View: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}
impl<T: ?Sized + ThinPayload> PartialEq for ThinArc<T>
where
    T::View: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl<T: ?Sized + ThinPayload> Eq for ThinArc<T> where T::View: Eq {}
impl<T: ?Sized + ThinPayload> PartialOrd for ThinArc<T>
where
    T::View: PartialOrd,
{
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}
impl<T: ?Sized + ThinPayload> Ord for ThinArc<T>
where
    T::View: Ord,
{
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}
impl<T: ?Sized + ThinPayload> Hash for ThinArc<T>
where
    T::View: Hash,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}
impl<T: ?Sized + ThinPayload> ThinWeak<T> {
    pub fn upgrade(&self) -> Option<ThinArc<T>> {
        self.0.upgrade().map(ThinArc::from_arc)
    }
    pub fn strong_count(&self) -> usize {
        self.0.strong_count()
    }
    pub fn weak_count(&self) -> usize {
        self.0.weak_count()
    }
}
impl<T: ?Sized + ThinPayload> Clone for ThinWeak<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// Immutable slice payload; its length cannot be changed through safe methods.
/// Carries a memoised Python hash (`-1` until computed), the way CPython's
/// `str`/`bytes` objects cache theirs: a dict key hashes once per object,
/// not once per lookup.
#[repr(C)]
#[derive(Debug)]
pub struct SliceStorage<T> {
    len: usize,
    hash: AtomicI64,
    items: [T],
}
impl<T> sealed::Sealed for SliceStorage<T> {}
// SAFETY: repr(C) places the immutable usize first; the last slice carries len.
// The constructors initialize exactly that many elements in the original Arc.
unsafe impl<T> ThinPayload for SliceStorage<T> {
    type View = [T];
    fn pointer(data: *const usize, len: usize) -> *const Self {
        ptr::slice_from_raw_parts(data.cast::<T>(), len) as *const Self
    }
    fn view(&self) -> &[T] {
        &self.items
    }
}

pub type SharedSlice<T> = ThinArc<SliceStorage<T>>;
pub type WeakSlice<T> = ThinWeak<SliceStorage<T>>;
impl<T> Borrow<[T]> for SharedSlice<T> {
    fn borrow(&self) -> &[T] {
        self
    }
}

impl<T> SharedSlice<T> {
    fn layout(len: usize) -> Layout {
        Layout::new::<usize>()
            .extend(Layout::new::<AtomicI64>())
            .expect("slice header exceeds layout limit")
            .0
            .extend(Layout::array::<T>(len).expect("slice elements exceed layout limit"))
            .expect("slice header exceeds layout limit")
            .0
            .pad_to_align()
    }

    /// The memoised Python hash of the contents, computing and recording
    /// it with `compute` on first use.
    #[inline]
    pub fn hash_cached(this: &Self, compute: impl FnOnce(&[T]) -> i64) -> i64 {
        // SAFETY: as in `deref` — a live payload borrowed from `this`.
        let storage = unsafe { &*this.raw() };
        let h = storage.hash.load(Ordering::Relaxed);
        if h != -1 {
            return h;
        }
        let h = compute(&storage.items);
        storage.hash.store(h, Ordering::Relaxed);
        h
    }

    fn from_vec_owned(items: Vec<T>) -> Self {
        let len = items.len();
        let layout = Self::layout(len);
        macro_rules! allocate {
            ($($word:ty),*) => { $(if layout.align() == align_of::<$word>() && layout.size() % size_of::<$word>() == 0 {
                return Self::from_vec_aligned::<$word>(items, layout);
            })* };
        }
        allocate!(u8, u16, u32, usize, u64, AtomicU64, u128);
        panic!("unsupported shared slice alignment: {}", layout.align());
    }

    fn from_vec_aligned<Word>(items: Vec<T>, layout: Layout) -> Self {
        let len = items.len();
        let storage = Arc::<[Word]>::new_uninit_slice(layout.size() / size_of::<Word>());
        let data = Arc::into_raw(storage).cast::<T>().cast_mut();
        let raw = ptr::slice_from_raw_parts_mut(data, len) as *mut SliceStorage<T>;
        // SAFETY: the original word slice and target have identical padded
        // payload size/alignment. All fields are initialized before any Arc
        // can read them. Vec's iterator moves exactly len elements without
        // callbacks, allocation, or fallible work in this raw-ownership interval.
        unsafe {
            ptr::addr_of_mut!((*raw).len).write(len);
            ptr::addr_of_mut!((*raw).hash).write(AtomicI64::new(-1));
            let dst = ptr::addr_of_mut!((*raw).items).cast::<T>();
            for (i, value) in items.into_iter().enumerate() {
                dst.add(i).write(value);
            }
            Self::from_arc(Arc::from_raw(raw))
        }
    }

    fn from_copy_slice(items: &[T]) -> Self
    where
        T: Copy,
    {
        let layout = Self::layout(items.len());
        macro_rules! allocate {
            ($($word:ty),*) => { $(if layout.align() == align_of::<$word>() && layout.size() % size_of::<$word>() == 0 {
                return Self::from_copy_aligned::<$word>(items, layout);
            })* };
        }
        allocate!(u8, u16, u32, usize, u64, AtomicU64, u128);
        panic!("unsupported shared slice alignment: {}", layout.align());
    }

    fn from_copy_aligned<Word>(items: &[T], layout: Layout) -> Self
    where
        T: Copy,
    {
        let len = items.len();
        let storage = Arc::<[Word]>::new_uninit_slice(layout.size() / size_of::<Word>());
        let data = Arc::into_raw(storage).cast::<T>().cast_mut();
        let raw = ptr::slice_from_raw_parts_mut(data, len) as *mut SliceStorage<T>;
        // SAFETY: equal size/alignment conversion as in from_vec_aligned. The
        // initialized Copy elements are copied once, with no temporary Vec.
        unsafe {
            ptr::addr_of_mut!((*raw).len).write(len);
            ptr::addr_of_mut!((*raw).hash).write(AtomicI64::new(-1));
            let dst = ptr::addr_of_mut!((*raw).items).cast::<T>();
            ptr::copy_nonoverlapping(items.as_ptr(), dst, len);
            Self::from_arc(Arc::from_raw(raw))
        }
    }
}
impl SharedSlice<u8> {
    /// A byte slice of `len` bytes written by `fill` (which receives a
    /// zeroed buffer of exactly that length): one allocation, no
    /// intermediate `Vec`.
    // The backing allocation comes from `Arc::<[usize]>::new_zeroed_slice`,
    // so it carries `usize` alignment by construction; the casts below
    // reinterpret it as the `usize`-headed DST it was sized for.
    #[allow(clippy::cast_ptr_alignment)]
    pub fn build(len: usize, fill: impl FnOnce(&mut [u8])) -> Self {
        let layout = Self::layout(len);
        let words = layout.size() / size_of::<usize>();
        let storage = Arc::<[usize]>::new_zeroed_slice(words);
        // SAFETY: an all-zero word slice is a valid `[usize]`; the header
        // and bytes are (re)written below before any reader exists.
        let storage = unsafe { storage.assume_init() };
        let data = Arc::into_raw(storage).cast::<u8>().cast_mut();
        let raw = ptr::slice_from_raw_parts_mut(data, len) as *mut SliceStorage<u8>;
        unsafe {
            ptr::addr_of_mut!((*raw).len).write(len);
            ptr::addr_of_mut!((*raw).hash).write(AtomicI64::new(-1));
            let dst = ptr::addr_of_mut!((*raw).items).cast::<u8>();
            fill(std::slice::from_raw_parts_mut(dst, len));
            Self::from_arc(Arc::from_raw(raw))
        }
    }
}

impl SharedSlice<u8> {
    /// Append `extra` in place when this handle is the slice's only owner
    /// (no other strong or weak reference): the allocation is resized
    /// rather than copied, so repeated appends cost what `realloc` does
    /// (CPython's `PyUnicode_Append` on a lone reference). `false` leaves
    /// `this` untouched.
    // The backing allocation comes from `Arc::<[usize]>::new_zeroed_slice`,
    // so it carries `usize` alignment by construction; the casts below
    // reinterpret it as the `usize`-headed DST it was sized for.
    #[allow(clippy::cast_ptr_alignment)]
    fn try_extend_unique(this: &mut Self, extra: &[u8]) -> bool {
        if ThinArc::get_mut(this).is_none() {
            return false;
        }
        // SAFETY: a live strong owner's length word.
        let old_len = unsafe { this.data.as_ptr().read() };
        let Some(new_len) = old_len.checked_add(extra.len()) else {
            return false;
        };
        // The `Arc` allocation is its two counters followed by the payload
        // (`ArcInner` is `repr(C)`, and std sizes it from the value layout
        // exactly this way), so the payload's offset and both sizes follow.
        let counters = Layout::new::<[usize; 2]>();
        let (Ok((old_inner, offset)), Ok((new_inner, _))) = (
            counters.extend(Self::layout(old_len)),
            counters.extend(Self::layout(new_len)),
        ) else {
            return false;
        };
        let (old_inner, new_inner) = (old_inner.pad_to_align(), new_inner.pad_to_align());
        // SAFETY: the sole owner (checked above) moves the allocation it owns
        // through the global allocator with the layout it was allocated
        // with; the new length word and hash are written before any reader
        // can exist, and the handle then points at the moved payload, whose
        // eventual `Arc` drop computes the new layout from the new length.
        unsafe {
            let base = this.data.as_ptr().cast::<u8>().sub(offset);
            let grown = std::alloc::realloc(base, old_inner, new_inner.size());
            if grown.is_null() {
                return false;
            }
            let data = grown.add(offset).cast::<usize>();
            let raw =
                ptr::slice_from_raw_parts_mut(data.cast::<u8>(), new_len) as *mut SliceStorage<u8>;
            ptr::addr_of_mut!((*raw).len).write(new_len);
            (*raw).hash.store(-1, Ordering::Relaxed);
            let dst = ptr::addr_of_mut!((*raw).items).cast::<u8>();
            ptr::copy_nonoverlapping(extra.as_ptr(), dst.add(old_len), extra.len());
            this.data = NonNull::new_unchecked(data);
        }
        true
    }
}

impl SharedStr {
    /// Append `suffix` in place when this handle is the string's only
    /// owner (see `SharedSlice::try_extend_unique`); `false` leaves it
    /// untouched.
    pub fn try_append(this: &mut Self, suffix: &str) -> bool {
        SharedSlice::try_extend_unique(&mut this.0, suffix.as_bytes())
    }

    /// The concatenation of `parts`, in one allocation.
    pub fn concat(parts: &[&str]) -> Self {
        let len = parts.iter().map(|p| p.len()).sum();
        Self(SharedSlice::build(len, |buf| {
            let mut at = 0;
            for p in parts {
                buf[at..at + p.len()].copy_from_slice(p.as_bytes());
                at += p.len();
            }
        }))
    }

    /// `s` repeated `times` times, in one allocation.
    pub fn repeat(s: &str, times: usize) -> Self {
        let len = s.len() * times;
        Self(SharedSlice::build(len, |buf| {
            for chunk in buf.chunks_exact_mut(s.len().max(1)) {
                chunk.copy_from_slice(s.as_bytes());
            }
        }))
    }
}

impl<T> From<Vec<T>> for SharedSlice<T> {
    fn from(items: Vec<T>) -> Self {
        Self::from_vec_owned(items)
    }
}
impl<T> From<Box<[T]>> for SharedSlice<T> {
    fn from(items: Box<[T]>) -> Self {
        Self::from_vec_owned(items.into_vec())
    }
}
impl<T: Copy> From<&[T]> for SharedSlice<T> {
    fn from(items: &[T]) -> Self {
        Self::from_copy_slice(items)
    }
}
impl<T: Copy, const N: usize> From<&[T; N]> for SharedSlice<T> {
    fn from(items: &[T; N]) -> Self {
        Self::from_copy_slice(items)
    }
}

/// Shared UTF-8 text. Its private byte owner can only be constructed from str.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct SharedStr(SharedSlice<u8>);
#[derive(Debug)]
pub struct WeakStr(WeakSlice<u8>);
impl SharedStr {
    pub fn as_ptr(this: &Self) -> *const str {
        ptr::from_ref(&**this)
    }
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        ThinArc::ptr_eq(&this.0, &other.0)
    }
    /// The memoised `hash(s)` (see [`SharedSlice::hash_cached`]).
    #[inline]
    pub fn hash_cached(this: &Self) -> i64 {
        SharedSlice::hash_cached(&this.0, |bytes| {
            // SAFETY: the private owner is only ever built from `str`.
            crate::object::py_str_hash(unsafe { std::str::from_utf8_unchecked(bytes) })
        })
    }
    pub fn strong_count(this: &Self) -> usize {
        ThinArc::strong_count(&this.0)
    }
    pub fn weak_count(this: &Self) -> usize {
        ThinArc::weak_count(&this.0)
    }
    pub fn downgrade(this: &Self) -> WeakStr {
        WeakStr(ThinArc::downgrade(&this.0))
    }
}
impl Deref for SharedStr {
    type Target = str;
    fn deref(&self) -> &str {
        // SAFETY: only From<str/String> can create the private byte owner.
        // No mutable byte view is exposed; clones and weak upgrades preserve it.
        unsafe { std::str::from_utf8_unchecked(&self.0) }
    }
}
impl From<&str> for SharedStr {
    fn from(value: &str) -> Self {
        Self(SharedSlice::from(value.as_bytes()))
    }
}
impl From<String> for SharedStr {
    fn from(value: String) -> Self {
        Self(SharedSlice::from(value.into_bytes()))
    }
}
impl From<&String> for SharedStr {
    fn from(value: &String) -> Self {
        Self::from(value.as_str())
    }
}
impl From<Box<str>> for SharedStr {
    fn from(value: Box<str>) -> Self {
        Self::from(String::from(value))
    }
}
impl AsRef<str> for SharedStr {
    fn as_ref(&self) -> &str {
        self
    }
}
impl Borrow<str> for SharedStr {
    fn borrow(&self) -> &str {
        self
    }
}
impl Hash for SharedStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}
impl fmt::Debug for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}
impl fmt::Display for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}
impl WeakStr {
    pub fn upgrade(&self) -> Option<SharedStr> {
        self.0.upgrade().map(SharedStr)
    }
    pub fn strong_count(&self) -> usize {
        self.0.strong_count()
    }
}
impl Clone for WeakStr {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl sealed::Sealed for crate::tuple_storage::TupleStorage {}
// SAFETY: TupleStorage places its immutable usize length first and stores an
// initialized [Object] tail. Only its constructors create these Arc payloads;
// unique mutation changes elements/hash, never slice length or metadata.
unsafe impl ThinPayload for crate::tuple_storage::TupleStorage {
    type View = Self;
    fn pointer(data: *const usize, len: usize) -> *const Self {
        ptr::slice_from_raw_parts(data.cast::<crate::object::Object>(), len) as *const Self
    }
    fn view(&self) -> &Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn strings_preserve_utf8_hash_lookup_and_data_identity() {
        for text in ["", "a", "x\0y", "mañana", "日本語", "🧶🙂", "e\u{301}"] {
            let value = SharedStr::from(text);
            assert_eq!(&*value, text);
            assert_eq!(SharedStr::as_ptr(&value).cast::<u8>(), value.as_ptr());
            assert_eq!(
                value.chars().collect::<Vec<_>>(),
                text.chars().collect::<Vec<_>>()
            );
            let mut set = HashSet::new();
            set.insert(value.clone());
            assert!(SharedStr::ptr_eq(set.get(text).unwrap(), &value));
            assert_eq!(SharedStr::from(text.to_owned()), value);
            assert_eq!(SharedStr::from(text.to_owned().into_boxed_str()), value);
            let weak = SharedStr::downgrade(&value);
            assert_eq!(SharedStr::weak_count(&value), 1);
            drop(set);
            assert_eq!(weak.strong_count(), 1);
            assert!(SharedStr::ptr_eq(&weak.upgrade().unwrap(), &value));
            drop(value);
            assert!(weak.upgrade().is_none());
            assert_eq!(weak.strong_count(), 0);
        }
    }

    #[test]
    fn copy_slices_preserve_empty_boundaries_alignment_and_identity() {
        for n in [0, 1, 2, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 257] {
            let input: Vec<u32> = (0..n).map(|i| 0xd800 + i).collect();
            let copy = SharedSlice::from(input.as_slice());
            let moved = SharedSlice::from(input.clone());
            let boxed = SharedSlice::from(input.clone().into_boxed_slice());
            assert_eq!(&*copy, input.as_slice());
            assert_eq!(copy, moved);
            assert_eq!(copy, boxed);
            assert_eq!(ThinArc::as_ptr(&copy).cast::<u32>(), copy.as_ptr());
            let clone = copy.clone();
            assert!(ThinArc::ptr_eq(&copy, &clone));
            assert!(!ThinArc::ptr_eq(&copy, &moved));
            assert_eq!(ThinArc::strong_count(&copy), 2);
        }
        assert_eq!(&*SharedSlice::from(&[1_u128, u128::MAX]), &[1, u128::MAX]);
        assert_eq!(SharedSlice::from(vec![(); 5]).len(), 5);
        assert_eq!(SharedSlice::from(&[(); 7]).len(), 7);
    }

    #[test]
    fn element_destructors_wait_for_last_strong_but_not_weak_owner() {
        struct Item(Arc<AtomicUsize>);
        impl Drop for Item {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        for n in [0, 1, 2, 15, 33, 64] {
            let drops = Arc::new(AtomicUsize::new(0));
            let value = SharedSlice::from((0..n).map(|_| Item(drops.clone())).collect::<Vec<_>>());
            let clone = value.clone();
            let weak = ThinArc::downgrade(&value);
            let second_weak = weak.clone();
            assert_eq!(weak.weak_count(), 2);
            drop(value);
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            assert_eq!(weak.upgrade().unwrap().len(), n);
            drop(clone);
            assert_eq!(drops.load(Ordering::SeqCst), n);
            assert!(weak.upgrade().is_none());
            assert!(second_weak.upgrade().is_none());
            drop(weak);
            drop(second_weak);
            assert_eq!(drops.load(Ordering::SeqCst), n);
        }
    }

    #[test]
    fn unique_access_rejects_strong_and_weak_aliases() {
        let mut value = SharedSlice::from(&[11_u8, 22, 33]);
        let clone = value.clone();
        assert!(ThinArc::get_mut(&mut value).is_none());
        drop(clone);
        let weak = ThinArc::downgrade(&value);
        assert!(ThinArc::get_mut(&mut value).is_none());
        drop(weak);
        assert_eq!(ThinArc::get_mut(&mut value).unwrap().view(), &[11, 22, 33]);
    }

    #[test]
    fn weak_upgrades_and_shared_text_survive_thread_handoffs() {
        let value = SharedStr::from("shared 🧶 text");
        let weak = SharedStr::downgrade(&value);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let weak = weak.clone();
                scope.spawn(move || {
                    for _ in 0..20 {
                        let local = weak.upgrade().unwrap();
                        assert_eq!(&*local, "shared 🧶 text");
                        assert_eq!(local.chars().count(), 13);
                    }
                });
            }
        });
        drop(value);
        assert!(weak.upgrade().is_none());
    }
}
