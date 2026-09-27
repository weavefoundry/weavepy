//! Biased reference counting for the VM heap.
//!
//! [`Rc`] and [`Weak`] wrap [`std::sync::Arc`] and [`std::sync::Weak`]
//! with the same API, but a strong-count increment or decrement is a plain
//! load and store instead of a locked read-modify-write while one thread
//! owns every object. That's the common case: a program that never starts a
//! second thread, running under the GIL. A `lock xadd` pair costs about four
//! times a plain pair on x86, and the interpreter performs roughly one pair
//! per bytecode, so atomics were a leading cost of every workload.
//!
//! The bias is the one [`crate::sync`] already maintains for cell borrows:
//! it holds until a second thread registers to run VM code or free-threading
//! starts, and it's never restored. The registering thread revokes the bias
//! while holding the GIL; the previous owner released the GIL before that,
//! and reacquires it before touching another object, so every plain update
//! happens before the revocation and every later update is atomic. Threads
//! that touch objects without the GIL announce themselves with
//! [`crate::sync::mark_cells_shared`] before they begin, and code that
//! spawns a thread which will run VM code revokes the bias first (see
//! [`revoke_refcount_bias`]).
//!
//! Deallocation always goes through `Arc`'s own drop, so the payload,
//! weak references, and the allocation are released exactly as before.
//! Only the counter arithmetic changes, and only for owners other than the
//! last one.

use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Whether strong counts may be updated with plain loads and stores.
///
/// Debug builds always use atomics: the unit-test binary runs many
/// interpreters on concurrent threads with no GIL between them, and they
/// share static objects.
#[inline(always)]
pub(crate) fn refcounts_biased() -> bool {
    #[cfg(debug_assertions)]
    {
        false
    }
    #[cfg(not(debug_assertions))]
    {
        crate::sync::bias_held()
    }
}

/// Revoke the single-thread bias before starting a thread that may touch VM
/// objects before it acquires the GIL. The caller's later updates observe
/// the revocation in program order, and the new thread observes it through
/// the spawn.
pub fn revoke_refcount_bias() {
    crate::sync::revoke_bias_for_spawn();
}

/// The strong counter of the `Arc` allocation holding `value`.
///
/// `ArcInner` is `#[repr(C)]` with the strong count first, then the weak
/// count, then the payload at the next offset aligned for the payload. The
/// standard library relies on this layout for `Arc::from_raw`; the
/// `strong_word_matches_arc_layout` test pins it for every payload shape
/// the VM uses.
#[inline(always)]
fn strong_word<T: ?Sized>(value: *const T) -> *const AtomicUsize {
    let align = std::mem::align_of_val(unsafe { &*value });
    let header = 2 * std::mem::size_of::<usize>();
    let offset = (header + align - 1) & !(align - 1);
    // SAFETY: the payload lives `offset` bytes into its ArcInner, whose
    // first field is the strong count.
    unsafe { value.cast::<u8>().sub(offset).cast::<AtomicUsize>() }
}

/// Add one strong owner of the `Arc` payload at `value`.
///
/// # Safety
///
/// `value` must point at the payload of a live `Arc` allocation.
#[inline(always)]
pub(crate) unsafe fn increment_strong<T: ?Sized>(value: *const T) {
    if refcounts_biased() {
        // SAFETY: live allocation, per the caller.
        let word = unsafe { &*strong_word(value) };
        word.store(word.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    } else {
        // SAFETY: as above.
        unsafe { Arc::increment_strong_count(value) };
    }
}

/// Release one strong owner of the `Arc` payload at `value` without
/// destroying it. Returns `false`, having changed nothing, when this is the
/// last owner (or the bias is off); the caller must then drop an `Arc`
/// normally.
///
/// # Safety
///
/// `value` must point at the payload of a live `Arc` allocation, and the
/// caller must own one of its strong references.
#[inline(always)]
pub(crate) unsafe fn try_release_shared<T: ?Sized>(value: *const T) -> bool {
    if !refcounts_biased() {
        return false;
    }
    // SAFETY: live allocation, per the caller.
    let word = unsafe { &*strong_word(value) };
    let n = word.load(Ordering::Relaxed);
    if n == 1 {
        return false;
    }
    word.store(n - 1, Ordering::Relaxed);
    true
}

/// Convert an [`Rc`] to one of an unsized type, such as a trait object:
/// `rc_unsize!(rc => dyn Trait)`. Stable Rust only coerces its own smart
/// pointers, so the conversion passes through `Arc`.
#[macro_export]
macro_rules! rc_unsize {
    ($rc:expr => $ty:ty) => {
        $crate::sync::Rc::<$ty>::from_arc($crate::sync::Rc::into_arc($rc) as ::std::sync::Arc<$ty>)
    };
}

/// A reference-counted shared pointer with biased counting. See the module
/// documentation.
#[repr(transparent)]
pub struct Rc<T: ?Sized>(ManuallyDrop<Arc<T>>);

/// A weak reference to an [`Rc`] allocation.
#[repr(transparent)]
pub struct Weak<T: ?Sized>(std::sync::Weak<T>);

impl<T> Rc<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        Self(ManuallyDrop::new(Arc::new(value)))
    }

    pub fn try_unwrap(this: Self) -> Result<T, Self> {
        Arc::try_unwrap(Self::into_arc(this)).map_err(Self::from_arc)
    }

    pub fn into_inner(this: Self) -> Option<T> {
        Arc::into_inner(Self::into_arc(this))
    }

    pub fn unwrap_or_clone(this: Self) -> T
    where
        T: Clone,
    {
        Arc::unwrap_or_clone(Self::into_arc(this))
    }

    pub fn new_cyclic(data_fn: impl FnOnce(&Weak<T>) -> T) -> Self {
        Self::from_arc(Arc::new_cyclic(|weak| {
            // SAFETY: Weak is a transparent wrapper over std::sync::Weak.
            data_fn(unsafe { &*ptr::from_ref(weak).cast::<Weak<T>>() })
        }))
    }
}

impl<T: ?Sized> Rc<T> {
    #[inline]
    pub fn from_arc(arc: Arc<T>) -> Self {
        Self(ManuallyDrop::new(arc))
    }

    #[inline]
    pub fn into_arc(this: Self) -> Arc<T> {
        let this = ManuallyDrop::new(this);
        // SAFETY: moves the owned Arc out; `this` is never dropped.
        unsafe { ptr::read(&*this.0) }
    }

    #[inline]
    pub fn as_arc(this: &Self) -> &Arc<T> {
        &this.0
    }

    #[inline]
    pub fn as_ptr(this: &Self) -> *const T {
        Arc::as_ptr(&this.0)
    }

    #[inline]
    pub fn ptr_eq(this: &Self, other: &Self) -> bool {
        Arc::ptr_eq(&this.0, &other.0)
    }

    #[inline]
    pub fn strong_count(this: &Self) -> usize {
        Arc::strong_count(&this.0)
    }

    #[inline]
    pub fn weak_count(this: &Self) -> usize {
        Arc::weak_count(&this.0)
    }

    #[inline]
    pub fn downgrade(this: &Self) -> Weak<T> {
        Weak(Arc::downgrade(&this.0))
    }

    #[inline]
    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        Arc::get_mut(&mut this.0)
    }

    #[inline]
    pub fn into_raw(this: Self) -> *const T {
        Arc::into_raw(Self::into_arc(this))
    }

    /// # Safety
    ///
    /// As [`Arc::from_raw`].
    #[inline]
    pub unsafe fn from_raw(ptr: *const T) -> Self {
        // SAFETY: forwarded to the caller.
        Self::from_arc(unsafe { Arc::from_raw(ptr) })
    }

    /// # Safety
    ///
    /// As [`Arc::increment_strong_count`].
    #[inline]
    pub unsafe fn increment_strong_count(ptr: *const T) {
        // SAFETY: forwarded to the caller.
        unsafe { increment_strong(ptr) }
    }

    /// # Safety
    ///
    /// As [`Arc::decrement_strong_count`].
    #[inline]
    pub unsafe fn decrement_strong_count(ptr: *const T) {
        // SAFETY: forwarded to the caller.
        unsafe { drop(Self::from_raw(ptr)) }
    }
}

impl<T: Clone> Rc<T> {
    #[inline]
    pub fn make_mut(this: &mut Self) -> &mut T {
        Arc::make_mut(&mut this.0)
    }
}

impl<T: ?Sized> Clone for Rc<T> {
    #[inline(always)]
    fn clone(&self) -> Self {
        // SAFETY: `self` keeps the allocation alive, and the new owner
        // accounts for the added reference.
        unsafe {
            increment_strong(Arc::as_ptr(&self.0));
            Self(ManuallyDrop::new(ptr::read(&*self.0)))
        }
    }
}

impl<T: ?Sized> Drop for Rc<T> {
    #[inline(always)]
    fn drop(&mut self) {
        // SAFETY: this owner holds one strong reference, released exactly
        // once: either here, or by Arc's own drop.
        unsafe {
            if !try_release_shared(Arc::as_ptr(&self.0)) {
                ManuallyDrop::drop(&mut self.0);
            }
        }
    }
}

impl<T: ?Sized> Deref for Rc<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: ?Sized> AsRef<T> for Rc<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: ?Sized> Borrow<T> for Rc<T> {
    fn borrow(&self) -> &T {
        self
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Rc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for Rc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

impl<T: ?Sized> fmt::Pointer for Rc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Pointer::fmt(&Self::as_ptr(self), f)
    }
}

impl<T: ?Sized + PartialEq> PartialEq for Rc<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: ?Sized + Eq> Eq for Rc<T> {}

impl<T: ?Sized + PartialOrd> PartialOrd for Rc<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: ?Sized + Ord> Ord for Rc<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (**self).cmp(&**other)
    }
}

impl<T: ?Sized + Hash> Hash for Rc<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

impl<T: Default> Default for Rc<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for Rc<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: ?Sized> From<Box<T>> for Rc<T> {
    fn from(value: Box<T>) -> Self {
        Self::from_arc(Arc::from(value))
    }
}

impl<T> From<Vec<T>> for Rc<[T]> {
    fn from(value: Vec<T>) -> Self {
        Self::from_arc(Arc::from(value))
    }
}

impl From<&str> for Rc<str> {
    fn from(value: &str) -> Self {
        Self::from_arc(Arc::from(value))
    }
}

impl From<String> for Rc<str> {
    fn from(value: String) -> Self {
        Self::from_arc(Arc::from(value))
    }
}

impl<T: ?Sized> From<Arc<T>> for Rc<T> {
    fn from(value: Arc<T>) -> Self {
        Self::from_arc(value)
    }
}

impl<T> Weak<T> {
    pub const fn new() -> Self {
        Self(std::sync::Weak::new())
    }
}

impl<T: ?Sized> Weak<T> {
    #[inline]
    pub fn upgrade(&self) -> Option<Rc<T>> {
        self.0.upgrade().map(Rc::from_arc)
    }

    pub fn strong_count(&self) -> usize {
        self.0.strong_count()
    }

    pub fn weak_count(&self) -> usize {
        self.0.weak_count()
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }

    pub fn as_ptr(&self) -> *const T {
        self.0.as_ptr()
    }
}

impl<T: ?Sized> Clone for Weak<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Default for Weak<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ?Sized> fmt::Debug for Weak<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(Weak)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check<T: ?Sized>(arc: &Arc<T>) {
        let before = Arc::strong_count(arc);
        // SAFETY: `arc` is live.
        let word = unsafe { &*strong_word(Arc::as_ptr(arc)) };
        assert_eq!(word.load(Ordering::Relaxed), before);
        let extra = arc.clone();
        assert_eq!(word.load(Ordering::Relaxed), before + 1);
        drop(extra);
    }

    #[test]
    fn strong_word_matches_arc_layout() {
        #[repr(align(64))]
        struct Wide(#[allow(dead_code)] u8);
        check(&Arc::new(1u8));
        check(&Arc::new(7u64));
        check(&Arc::new(Wide(3)));
        check(&Arc::new(String::from("x")));
        check::<str>(&Arc::from("text"));
        check::<[u64]>(&Arc::from(vec![1u64, 2, 3]));
        check::<dyn std::any::Any + Send + Sync>(&(Arc::new(5u32) as Arc<_>));
    }

    #[test]
    fn biased_updates_keep_arc_counts() {
        let rc = Rc::new(vec![1, 2, 3]);
        let copies: Vec<_> = (0..10).map(|_| rc.clone()).collect();
        assert_eq!(Rc::strong_count(&rc), 11);
        drop(copies);
        assert_eq!(Rc::strong_count(&rc), 1);
        let weak = Rc::downgrade(&rc);
        assert!(weak.upgrade().is_some());
        drop(rc);
        assert!(weak.upgrade().is_none());
    }
}
