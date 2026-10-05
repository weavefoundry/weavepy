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
use std::mem::{align_of, size_of, ManuallyDrop, MaybeUninit};
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::{
    atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
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
    /// Release the last reference to a payload. A payload whose release
    /// can release more containers goes through `rc::release_nested`.
    fn release_last(last: Arc<Self>) {
        drop(last);
    }
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
        // SAFETY: this owner keeps the payload alive; the copy accounts for
        // the added strong reference.
        unsafe { crate::rc::increment_strong(self.raw()) };
        Self {
            data: self.data,
            ownership: PhantomData,
        }
    }
}

impl<T: ?Sized + ThinPayload> Drop for ThinArc<T> {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: release this owner's one strong reference exactly once.
        // Arc drops the payload and handles remaining weak references normally.
        unsafe {
            if crate::rc::try_release_shared(self.raw()) {
                return;
            }
            // Text and byte payloads have no destructor: their last owner
            // can free them without `Arc`'s locked decrements.
            if !std::mem::needs_drop::<T>() && crate::rc::try_free_last(self.raw()) {
                return;
            }
            T::release_last(Arc::from_raw(self.raw()))
        }
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

/// The code-point count of a [`StrStorage`] not yet counted.
const CHARS_UNKNOWN: usize = usize::MAX;

/// UTF-8 text payload: [`SliceStorage`]'s length and memoised hash, plus
/// the memoised code-point count (`CHARS_UNKNOWN` until first needed).
/// CPython's PEP 393 strings know their length in code points, and with
/// it whether they are ASCII; this word gives `len(s)`, index and slice
/// bounds, and every ASCII fast path the same O(1) answer, and lets a
/// string derived from ASCII text (a split field, a case mapping, a join
/// of ASCII parts) be born knowing it.
#[repr(C)]
#[derive(Debug)]
pub struct StrStorage {
    len: usize,
    hash: AtomicI64,
    chars: AtomicUsize,
    bytes: [u8],
}
impl sealed::Sealed for StrStorage {}
// SAFETY: repr(C) places the immutable usize first; the byte tail carries
// len. The constructors below initialize the header and exactly len bytes.
unsafe impl ThinPayload for StrStorage {
    type View = [u8];
    // `data` is the word-aligned header the byte view was cast from.
    #[allow(clippy::cast_ptr_alignment)]
    fn pointer(data: *const usize, len: usize) -> *const Self {
        ptr::slice_from_raw_parts(data.cast::<u8>(), len) as *const Self
    }
    fn view(&self) -> &[u8] {
        &self.bytes
    }
}

/// Copy `len` bytes from `src` to `dst`. Most strings are a few words
/// long (a split field, a dict key), and for them two overlapping word
/// moves beat a call to the platform `memcpy` through its stub.
///
/// # Safety
///
/// As [`ptr::copy_nonoverlapping`].
#[inline(always)]
unsafe fn copy_text(src: *const u8, dst: *mut u8, len: usize) {
    // SAFETY: every access below lies within the first `len` bytes of
    // both buffers (the caller's contract).
    unsafe {
        if len >= 16 {
            ptr::copy_nonoverlapping(src, dst, len);
        } else if len >= 8 {
            let head = src.cast::<u64>().read_unaligned();
            let tail = src.add(len - 8).cast::<u64>().read_unaligned();
            dst.cast::<u64>().write_unaligned(head);
            dst.add(len - 8).cast::<u64>().write_unaligned(tail);
        } else if len >= 4 {
            let head = src.cast::<u32>().read_unaligned();
            let tail = src.add(len - 4).cast::<u32>().read_unaligned();
            dst.cast::<u32>().write_unaligned(head);
            dst.add(len - 4).cast::<u32>().write_unaligned(tail);
        } else if len > 0 {
            *dst = *src;
            *dst.add(len / 2) = *src.add(len / 2);
            *dst.add(len - 1) = *src.add(len - 1);
        }
    }
}

impl StrStorage {
    /// The padded payload layout for `len` bytes of text: three header
    /// words, then the bytes, rounded up to whole words.
    fn layout(len: usize) -> Layout {
        Layout::new::<[usize; 3]>()
            .extend(Layout::array::<u8>(len).expect("string exceeds layout limit"))
            .expect("string exceeds layout limit")
            .0
            .pad_to_align()
    }

    /// A payload of `len` bytes whose contents `init` writes, given the
    /// (uninitialized) destination. One allocation, no intermediate `Vec`.
    ///
    /// # Safety
    ///
    /// `init` must initialize all `len` bytes with UTF-8 text, and
    /// `chars`, when not `CHARS_UNKNOWN`, must be its code-point count.
    // The backing allocation comes from `Arc::<[usize]>::new_uninit_slice`,
    // so it carries `usize` alignment by construction; the casts below
    // reinterpret it as the `usize`-headed DST it was sized for.
    #[allow(clippy::cast_ptr_alignment)]
    #[inline]
    unsafe fn alloc(len: usize, chars: usize, init: impl FnOnce(*mut u8)) -> ThinArc<Self> {
        let words = Self::layout(len).size() / size_of::<usize>();
        let storage = Arc::<[usize]>::new_uninit_slice(words);
        let data = Arc::into_raw(storage).cast::<u8>().cast_mut();
        let raw = ptr::slice_from_raw_parts_mut(data, len) as *mut Self;
        // SAFETY: the word slice and the payload have identical padded
        // size and alignment. The header is written here and the bytes by
        // `init` (the caller's contract) before any reader can exist.
        unsafe {
            ptr::addr_of_mut!((*raw).len).write(len);
            ptr::addr_of_mut!((*raw).hash).write(AtomicI64::new(-1));
            ptr::addr_of_mut!((*raw).chars).write(AtomicUsize::new(chars));
            init(ptr::addr_of_mut!((*raw).bytes).cast::<u8>());
            ThinArc::from_arc(Arc::from_raw(raw))
        }
    }
}

impl ThinArc<StrStorage> {
    #[inline]
    fn storage(&self) -> &StrStorage {
        // SAFETY: a live payload borrowed from `self`.
        unsafe { &*self.raw() }
    }

    /// Append `extra` (of `extra_chars` code points) in place when this
    /// handle is the string's only owner (no other strong or weak
    /// reference): the allocation is resized rather than copied, so
    /// repeated appends cost what `realloc` does (CPython's
    /// `PyUnicode_Append` on a lone reference). `false` leaves `this`
    /// untouched.
    #[allow(clippy::cast_ptr_alignment)]
    fn try_extend_unique(this: &mut Self, extra: &[u8], extra_chars: usize) -> bool {
        if ThinArc::get_mut(this).is_none() {
            return false;
        }
        // SAFETY: a live strong owner's length word.
        let old_len = unsafe { this.data.as_ptr().read() };
        let Some(new_len) = old_len.checked_add(extra.len()) else {
            return false;
        };
        let old_chars = this.storage().chars.load(Ordering::Relaxed);
        let new_chars = if old_chars == CHARS_UNKNOWN {
            CHARS_UNKNOWN
        } else {
            old_chars + extra_chars
        };
        // The `Arc` allocation is its two counters followed by the payload
        // (`ArcInner` is `repr(C)`, and std sizes it from the value layout
        // exactly this way), so the payload's offset and both sizes follow.
        let counters = Layout::new::<[usize; 2]>();
        let (Ok((old_inner, offset)), Ok((new_inner, _))) = (
            counters.extend(StrStorage::layout(old_len)),
            counters.extend(StrStorage::layout(new_len)),
        ) else {
            return false;
        };
        let (old_inner, new_inner) = (old_inner.pad_to_align(), new_inner.pad_to_align());
        // SAFETY: the sole owner (checked above) moves the allocation it owns
        // through the global allocator with the layout it was allocated
        // with; the new length, hash, and count are written before any
        // reader can exist, and the handle then points at the moved
        // payload, whose eventual `Arc` drop computes the new layout from
        // the new length.
        unsafe {
            let base = this.data.as_ptr().cast::<u8>().sub(offset);
            let grown = std::alloc::realloc(base, old_inner, new_inner.size());
            if grown.is_null() {
                return false;
            }
            let data = grown.add(offset).cast::<usize>();
            let raw = ptr::slice_from_raw_parts_mut(data.cast::<u8>(), new_len) as *mut StrStorage;
            ptr::addr_of_mut!((*raw).len).write(new_len);
            (*raw).hash.store(-1, Ordering::Relaxed);
            (*raw).chars.store(new_chars, Ordering::Relaxed);
            let dst = ptr::addr_of_mut!((*raw).bytes).cast::<u8>();
            ptr::copy_nonoverlapping(extra.as_ptr(), dst.add(old_len), extra.len());
            this.data = NonNull::new_unchecked(data);
        }
        true
    }
}

/// Shared UTF-8 text. Its private byte owner can only be constructed from str.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct SharedStr(ThinArc<StrStorage>);
#[derive(Debug)]
pub struct WeakStr(ThinWeak<StrStorage>);
impl SharedStr {
    pub fn as_ptr(this: &Self) -> *const str {
        ptr::from_ref(&**this)
    }
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        ThinArc::ptr_eq(&this.0, &other.0)
    }
    /// The memoised `hash(s)`, computed on first use the way CPython's
    /// `str` caches its hash: a dict key hashes once per object, not once
    /// per lookup.
    #[inline]
    pub fn hash_cached(this: &Self) -> i64 {
        let storage = this.0.storage();
        let h = storage.hash.load(Ordering::Relaxed);
        if h != -1 {
            return h;
        }
        let h = crate::object::py_str_hash(this);
        storage.hash.store(h, Ordering::Relaxed);
        h
    }
    /// `len(s)` in code points, memoised in the string itself.
    #[inline]
    pub fn char_count(this: &Self) -> usize {
        let storage = this.0.storage();
        let n = storage.chars.load(Ordering::Relaxed);
        if n != CHARS_UNKNOWN {
            return n;
        }
        let n = this.chars().count();
        storage.chars.store(n, Ordering::Relaxed);
        n
    }
    /// The code-point count if it is already known, without counting.
    #[inline]
    pub fn known_char_count(this: &Self) -> Option<usize> {
        let n = this.0.storage().chars.load(Ordering::Relaxed);
        (n != CHARS_UNKNOWN).then_some(n)
    }
    /// Whether the text is pure ASCII (one byte per code point), from the
    /// memoised count.
    #[inline]
    pub fn is_ascii(this: &Self) -> bool {
        Self::char_count(this) == this.len()
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

    /// A copy of `text`, known to be pure ASCII (`debug_assert`ed): it is
    /// born with its code-point count.
    #[inline]
    pub fn from_ascii(text: &str) -> Self {
        debug_assert!(text.is_ascii());
        Self::with_count(text, text.len())
    }

    /// A copy of `text`, which the caller knows to hold `chars` code points.
    #[inline]
    pub fn with_count(text: &str, chars: usize) -> Self {
        debug_assert_eq!(text.chars().count(), chars);
        let len = text.len();
        // SAFETY: exactly `len` bytes of UTF-8 are copied, and `chars`
        // counts them (the caller's word, checked in debug builds).
        Self(unsafe {
            StrStorage::alloc(len, chars, |dst| {
                copy_text(text.as_ptr(), dst, len);
            })
        })
    }

    /// A string of `len` bytes that `fill` writes, in order, through a
    /// [`TextWriter`] over its final allocation: one allocation, no
    /// intermediate `String`, no zeroing. `chars` is its code-point count
    /// when the caller knows it. Panics unless `fill` writes exactly `len`
    /// bytes.
    ///
    /// # Safety
    ///
    /// The bytes `fill` writes must be valid UTF-8, of `chars` code points
    /// when `chars` is given.
    #[inline]
    pub unsafe fn build_unchecked(
        len: usize,
        chars: Option<usize>,
        fill: impl FnOnce(&mut TextWriter<'_>),
    ) -> Self {
        // SAFETY: the writer initializes every byte it accounts for, and
        // the assertion checks that it accounted for all `len`; the
        // caller vouches for the text and its count.
        let out = Self(unsafe {
            StrStorage::alloc(len, chars.unwrap_or(CHARS_UNKNOWN), |dst| {
                let mut writer = TextWriter {
                    buf: std::slice::from_raw_parts_mut(dst.cast::<MaybeUninit<u8>>(), len),
                    at: 0,
                };
                fill(&mut writer);
                assert_eq!(writer.at, len, "string builder wrote the wrong length");
            })
        });
        debug_assert!(std::str::from_utf8(out.0.as_ref()).is_ok());
        debug_assert!(chars.is_none_or(|n| n == out.chars().count()));
        out
    }

    /// Append `suffix` in place when this handle is the string's only
    /// owner (see `ThinArc::<StrStorage>::try_extend_unique`); `false`
    /// leaves it untouched.
    pub fn try_append(this: &mut Self, suffix: &str) -> bool {
        // Keep a known count known: `len(s)` after `s += t` in a loop would
        // otherwise recount the whole string each time.
        let extra_chars = if this.0.storage().chars.load(Ordering::Relaxed) == CHARS_UNKNOWN {
            0
        } else {
            suffix.chars().count()
        };
        ThinArc::<StrStorage>::try_extend_unique(&mut this.0, suffix.as_bytes(), extra_chars)
    }

    /// The concatenation of `parts`, in one allocation.
    pub fn concat(parts: &[&str]) -> Self {
        let len = parts.iter().map(|p| p.len()).sum();
        // SAFETY: a concatenation of `str`s is UTF-8.
        unsafe {
            Self::build_unchecked(len, None, |out| {
                for p in parts {
                    out.push(p.as_bytes());
                }
            })
        }
    }

    /// `s` repeated `times` times, in one allocation.
    pub fn repeat(s: &str, times: usize) -> Self {
        let len = s.len() * times;
        // SAFETY: a repetition of a `str` is UTF-8.
        unsafe {
            Self::build_unchecked(len, None, |out| {
                if !s.is_empty() {
                    for _ in 0..times {
                        out.push(s.as_bytes());
                    }
                }
            })
        }
    }
}

/// The sequential writer [`SharedStr::build_unchecked`] hands its `fill`:
/// each write lands after the last, and the builder checks that the
/// writes covered the whole string.
#[derive(Debug)]
pub struct TextWriter<'a> {
    buf: &'a mut [MaybeUninit<u8>],
    at: usize,
}
impl TextWriter<'_> {
    /// Append `bytes`. Panics past the end of the string.
    #[inline]
    pub fn push(&mut self, bytes: &[u8]) {
        let end = self.at + bytes.len();
        assert!(end <= self.buf.len(), "string builder overflow");
        // SAFETY: `at..end` lies within the buffer (checked above), which
        // can't overlap the borrowed `bytes`.
        unsafe {
            copy_text(
                bytes.as_ptr(),
                self.buf.as_mut_ptr().add(self.at).cast(),
                bytes.len(),
            )
        };
        self.at = end;
    }

    /// Append one byte. Panics past the end of the string.
    #[inline]
    pub fn push_byte(&mut self, byte: u8) {
        self.buf[self.at] = MaybeUninit::new(byte);
        self.at += 1;
    }

    /// Append `n` bytes that `write` initializes, given exactly those
    /// bytes. Panics past the end of the string.
    ///
    /// # Safety
    ///
    /// `write` must initialize all `n` bytes it is given.
    #[inline]
    pub unsafe fn push_with(&mut self, n: usize, write: impl FnOnce(&mut [MaybeUninit<u8>])) {
        let end = self.at + n;
        write(&mut self.buf[self.at..end]);
        self.at = end;
    }
}
impl Deref for SharedStr {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        // SAFETY: only the UTF-8 constructors above create the private
        // byte owner. No mutable byte view is exposed; clones and weak
        // upgrades preserve it.
        unsafe { std::str::from_utf8_unchecked(&self.0) }
    }
}
impl SharedStr {
    /// The shared empty string, or the shared one-character string for a
    /// Latin-1 character (CPython caches the same 257): `None` for
    /// anything longer. Indexing and iterating a string then allocate
    /// nothing for these characters.
    #[inline]
    pub fn small(value: &str) -> Option<Self> {
        let code = match value.as_bytes() {
            [] => return Some(small_table()[256].clone()),
            [b] => usize::from(*b),
            // U+0080..U+00FF encode as two bytes led by 0xC2 or 0xC3.
            [lead @ (0xC2 | 0xC3), cont] => usize::from((lead & 0x1F) << 6 | (cont & 0x3F)),
            _ => return None,
        };
        Some(small_table()[code].clone())
    }
}

/// [`SharedStr::small`]'s strings: the 256 Latin-1 characters by code
/// point, then the empty string.
fn small_table() -> &'static [SharedStr; 257] {
    static TABLE: std::sync::OnceLock<[SharedStr; 257]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|i| {
            let mut buf = [0; 4];
            let text: &str = if i == 256 {
                ""
            } else {
                char::from(i as u8).encode_utf8(&mut buf)
            };
            SharedStr::fresh(text)
        })
    })
}

impl SharedStr {
    /// A newly allocated string holding `value` (see [`SharedStr::small`]
    /// for the shared ones).
    #[inline]
    fn fresh(value: &str) -> Self {
        let len = value.len();
        // SAFETY: exactly `len` bytes of UTF-8 are copied; the count is
        // left for first use.
        Self(unsafe {
            StrStorage::alloc(len, if len == 0 { 0 } else { CHARS_UNKNOWN }, |dst| {
                copy_text(value.as_ptr(), dst, len);
            })
        })
    }
}

impl From<&str> for SharedStr {
    #[inline]
    fn from(value: &str) -> Self {
        if value.len() <= 2 {
            if let Some(small) = Self::small(value) {
                return small;
            }
        }
        let len = value.len();
        // SAFETY: exactly `len` bytes of UTF-8 are copied; the count is
        // left for first use.
        Self(unsafe {
            StrStorage::alloc(len, if len == 0 { 0 } else { CHARS_UNKNOWN }, |dst| {
                copy_text(value.as_ptr(), dst, len);
            })
        })
    }
}
impl From<String> for SharedStr {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}
impl From<&String> for SharedStr {
    fn from(value: &String) -> Self {
        Self::from(value.as_str())
    }
}
impl From<Box<str>> for SharedStr {
    fn from(value: Box<str>) -> Self {
        Self::from(&*value)
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
    fn release_last(last: Arc<Self>) {
        crate::rc::release_nested(last);
    }
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
    fn strings_memoise_code_point_counts_through_appends() {
        for text in [
            "",
            "a",
            "plain ascii",
            "mañana",
            "日本語",
            "🧶🙂",
            "e\u{301}",
        ] {
            let value = SharedStr::from(text);
            assert_eq!(
                SharedStr::known_char_count(&value),
                text.is_empty().then_some(0)
            );
            assert_eq!(SharedStr::char_count(&value), text.chars().count());
            assert_eq!(
                SharedStr::known_char_count(&value),
                Some(text.chars().count())
            );
            assert_eq!(SharedStr::is_ascii(&value), text.is_ascii());
            let mut grown = value.clone();
            drop(value);
            assert!(SharedStr::try_append(&mut grown, "ñx"));
            let expected = format!("{text}ñx");
            assert_eq!(&*grown, expected);
            assert_eq!(
                SharedStr::known_char_count(&grown),
                Some(expected.chars().count())
            );
            assert_eq!(
                SharedStr::hash_cached(&grown),
                crate::object::py_str_hash(&expected)
            );
        }
        let ascii = SharedStr::from_ascii("abc");
        assert_eq!(SharedStr::known_char_count(&ascii), Some(3));
        // SAFETY: ASCII parts and their count.
        let built = unsafe {
            SharedStr::build_unchecked(7, Some(7), |out| {
                out.push(b"ab");
                out.push_byte(b'-');
                out.push(b"cdef");
            })
        };
        assert_eq!(&*built, "ab-cdef");
        assert_eq!(SharedStr::concat(&["x", "ñ", ""]).chars().count(), 2);
        assert_eq!(&*SharedStr::repeat("ab", 3), "ababab");
        assert_eq!(&*SharedStr::repeat("", 3), "");
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
        // As the VM does before starting a thread: counts go atomic.
        crate::sync::revoke_bias_for_spawn();
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
