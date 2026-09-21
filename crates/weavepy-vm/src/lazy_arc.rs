//! Lazily publish one owning Arc without enlarging a pointer-sized field.
//!
//! A published pointer is never reset through shared access. Every borrowed
//! value remains live until its owner can be exclusively destroyed or replaced.

use std::fmt;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;

pub struct LazyArc<T> {
    pointer: AtomicPtr<T>,
    // AtomicPtr alone would allow Send/Sync regardless of T. Match Arc<T>'s
    // ownership, drop checking, and thread-safety requirements instead.
    owner: PhantomData<Arc<T>>,
}

impl<T> LazyArc<T> {
    pub const fn new() -> Self {
        Self {
            pointer: AtomicPtr::new(ptr::null_mut()),
            owner: PhantomData,
        }
    }

    /// Inspect the value without allocating or increasing its strong count.
    #[inline]
    pub fn get(&self) -> Option<&T> {
        let pointer = self.pointer.load(Ordering::Acquire);
        // SAFETY: a non-null pointer owns a published Arc strong reference.
        // Shared access cannot replace or destroy that reference, and acquire
        // observes the complete initialization before publication.
        unsafe { pointer.as_ref() }
    }

    #[inline]
    pub fn get_or_init(&self, make: impl FnOnce() -> Arc<T>) -> &T {
        let pointer = self.get_or_init_ptr(make);
        // SAFETY: get_or_init_ptr returns self's permanently owned count.
        unsafe { &*pointer }
    }

    #[inline]
    fn get_or_init_ptr(&self, make: impl FnOnce() -> Arc<T>) -> *mut T {
        let pointer = self.pointer.load(Ordering::Acquire);
        if pointer.is_null() {
            self.initialize(make)
        } else {
            pointer
        }
    }

    #[cold]
    fn initialize(&self, make: impl FnOnce() -> Arc<T>) -> *mut T {
        let private = Arc::into_raw(make()).cast_mut();
        match self.pointer.compare_exchange(
            ptr::null_mut(),
            private,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => private,
            Err(published) => {
                // SAFETY: this thread still owns the unpublished count; no
                // reader ever obtained it from self.pointer.
                unsafe { drop(Arc::from_raw(private)) };
                published
            }
        }
    }

    /// Return another owner only when storage has already been published.
    #[inline]
    pub fn get_shared(&self) -> Option<Arc<T>> {
        let pointer = self.pointer.load(Ordering::Acquire);
        if pointer.is_null() {
            None
        } else {
            // SAFETY: self's strong count remains live throughout this call.
            // Incrementing first creates the count consumed by from_raw.
            // Keep the original raw pointer's allocation provenance; don't
            // reconstruct the owner from a narrowed reference to T.
            unsafe {
                Arc::increment_strong_count(pointer);
                Some(Arc::from_raw(pointer))
            }
        }
    }

    pub fn strong_count(&self) -> usize {
        let pointer = self.pointer.load(Ordering::Acquire);
        if pointer.is_null() {
            return 0;
        }
        // SAFETY: temporarily view self's existing count without consuming it.
        // ManuallyDrop prevents releasing that count after the atomic read.
        let borrowed = ManuallyDrop::new(unsafe { Arc::from_raw(pointer) });
        Arc::strong_count(&borrowed)
    }
}

impl<T: Default> LazyArc<T> {
    #[inline]
    pub fn share(&self) -> Arc<T> {
        let pointer = self.get_or_init_ptr(|| Arc::new(T::default()));
        // SAFETY: as in get_shared, self retains one live strong count.
        unsafe {
            Arc::increment_strong_count(pointer);
            Arc::from_raw(pointer)
        }
    }
}

impl<T> Default for LazyArc<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> From<Arc<T>> for LazyArc<T> {
    fn from(value: Arc<T>) -> Self {
        Self {
            pointer: AtomicPtr::new(Arc::into_raw(value).cast_mut()),
            owner: PhantomData,
        }
    }
}

// No `Deref`: an implicit materialisation publishes a default value,
// which for an instance `__dict__` means one with no deferred-tracking
// owner record — and a store through it then leaves the instance
// untracked forever. Callers name the accessor they want
// (`PyInstance::dict_cell` for the instance dictionary), so every
// creator is visible at the call site.

impl<T: Default> Clone for LazyArc<T> {
    fn clone(&self) -> Self {
        // A shallow instance clone must share even a previously empty dict:
        // subsequent writes through either owner must reach the same object.
        Self::from(self.share())
    }
}

impl<T: fmt::Debug> fmt::Debug for LazyArc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LazyArc").field(&self.get()).finish()
    }
}

impl<T> Drop for LazyArc<T> {
    fn drop(&mut self) {
        let pointer = *self.pointer.get_mut();
        if !pointer.is_null() {
            // SAFETY: exclusive access ends every reference returned by get.
            // The original published count is released exactly once here.
            unsafe { drop(Arc::from_raw(pointer)) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Barrier;
    use std::thread;

    struct Counted(Arc<AtomicUsize>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn empty_inspection_does_not_materialize_storage() {
        let lazy = LazyArc::<usize>::new();
        assert!(lazy.get().is_none());
        assert!(lazy.get_shared().is_none());
        assert_eq!(lazy.strong_count(), 0);
        assert_eq!(
            std::mem::size_of_val(&lazy),
            std::mem::size_of::<Arc<usize>>()
        );
    }

    #[test]
    fn sharing_keeps_identity_and_exact_ownership() {
        let drops = Arc::new(AtomicUsize::new(0));
        let first = Arc::new(Counted(drops.clone()));
        let weak = Arc::downgrade(&first);
        let lazy = LazyArc::from(first.clone());
        assert_eq!(lazy.strong_count(), 2);
        let shared = lazy.get_shared().unwrap();
        assert!(Arc::ptr_eq(&first, &shared));
        assert_eq!(lazy.strong_count(), 3);
        drop(first);
        drop(lazy);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert!(weak.upgrade().is_some());
        drop(shared);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn empty_clone_shares_later_mutations() {
        let lazy = LazyArc::<AtomicUsize>::new();
        let other = lazy.clone();
        lazy.share().store(12, Ordering::Relaxed);
        assert_eq!(other.share().load(Ordering::Relaxed), 12);
        assert!(Arc::ptr_eq(&lazy.share(), &other.share()));
        assert_eq!(lazy.strong_count(), 2);
    }

    #[test]
    fn publication_losers_release_their_private_values() {
        let lazy = LazyArc::<Counted>::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let barrier = Barrier::new(4);
        thread::scope(|scope| {
            let readers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        let value = lazy.get_or_init(|| {
                            let value = Arc::new(Counted(drops.clone()));
                            barrier.wait();
                            value
                        });
                        ptr::from_ref(value).addr()
                    })
                })
                .collect();
            let addresses: Vec<_> = readers.into_iter().map(|t| t.join().unwrap()).collect();
            assert!(addresses.iter().all(|address| *address == addresses[0]));
        });
        assert_eq!(drops.load(Ordering::Relaxed), 3);
        assert_eq!(lazy.strong_count(), 1);
        drop(lazy);
        assert_eq!(drops.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn panicking_initialization_does_not_publish_or_poison() {
        let lazy = LazyArc::<usize>::new();
        let failed = std::panic::catch_unwind(|| lazy.get_or_init(|| panic!("test")));
        assert!(failed.is_err());
        assert!(lazy.get().is_none());
        assert_eq!(*lazy.get_or_init(|| Arc::new(17)), 17);
        assert_eq!(*lazy.get_or_init(|| panic!("already initialized")), 17);
    }

    #[test]
    fn exclusive_replacement_releases_only_the_previous_count() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut lazy = LazyArc::from(Arc::new(Counted(drops.clone())));
        let shared = lazy.get_shared().unwrap();
        lazy = LazyArc::from(Arc::new(Counted(drops.clone())));
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(shared);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        drop(lazy);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn reentrant_initializer_keeps_the_first_published_owner() {
        let lazy = LazyArc::<Counted>::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let value = lazy.get_or_init(|| {
            lazy.get_or_init(|| Arc::new(Counted(drops.clone())));
            Arc::new(Counted(drops.clone()))
        });
        assert!(ptr::eq(value, lazy.get().unwrap()));
        assert_eq!(lazy.strong_count(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        drop(lazy);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }
}
