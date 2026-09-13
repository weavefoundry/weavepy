//! Unapplied ownership prototype, including headers and destructible tuple elements.
use std::alloc::Layout;
use std::marker::PhantomData;
use std::mem::{align_of, size_of, ManuallyDrop};
use std::ptr::{self, NonNull};
use std::sync::{atomic::AtomicU64, Arc, Weak};

#[repr(C)]
struct Payload<H, T: ?Sized> {
    len: usize,
    header: H,
    items: T,
}

struct ThinOwner<H, T> {
    data: NonNull<usize>,
    ownership: PhantomData<Arc<Payload<H, [T]>>>,
}

// SAFETY: the immutable shared allocation and all refcounts are managed by Arc.
// These bounds are the same bounds Arc requires for shared cross-thread access.
unsafe impl<H: Send + Sync, T: Send + Sync> Send for ThinOwner<H, T> {}
unsafe impl<H: Send + Sync, T: Send + Sync> Sync for ThinOwner<H, T> {}

struct WeakOwner<H, T>(Weak<Payload<H, [T]>>);

impl<H, T> ThinOwner<H, T> {
    fn from_array<const N: usize>(header: H, items: [T; N]) -> Self {
        let arc: Arc<Payload<H, [T]>> = Arc::new(Payload { len: N, header, items });
        Self::from_arc(arc)
    }

    fn from_vec(header: H, items: Vec<T>) -> Self {
        let (prefix, _) = Layout::new::<usize>().extend(Layout::new::<H>()).expect("header layout");
        let (layout, _) = prefix.extend(Layout::array::<T>(items.len()).expect("items layout")).expect("payload layout");
        let layout = layout.pad_to_align();
        macro_rules! allocate {
            ($($word:ty),*) => {
                $(if layout.align() == align_of::<$word>() && layout.size() % size_of::<$word>() == 0 {
                    return Self::from_vec_aligned::<$word>(header, items, layout);
                })*
            };
        }
        allocate!(u8, u16, u32, usize, u64, AtomicU64, u128);
        panic!("unsupported prototype payload alignment");
    }

    fn from_vec_aligned<Word>(header: H, items: Vec<T>, layout: Layout) -> Self {
        let len = items.len();
        let allocation = Arc::<[Word]>::new_uninit_slice(layout.size() / size_of::<Word>());
        let data = Arc::into_raw(allocation).cast::<T>().cast_mut();
        let raw = ptr::slice_from_raw_parts_mut(data, len) as *mut Payload<H, [T]>;
        // SAFETY: repr(C) layout equals the original padded word-slice layout.
        // The pointer came from Arc::into_raw with exactly one strong owner.
        // Initialize all fields and move exactly len elements. No callbacks,
        // allocations, or fallible operations occur after ownership is raw.
        unsafe {
            ptr::addr_of_mut!((*raw).len).write(len);
            ptr::addr_of_mut!((*raw).header).write(header);
            let dst = ptr::addr_of_mut!((*raw).items).cast::<T>();
            for (i, item) in items.into_iter().enumerate() {
                dst.add(i).write(item);
            }
            let arc = Arc::from_raw(raw);
            assert_eq!(Layout::for_value(arc.as_ref()), layout);
            Self::from_arc(arc)
        }
    }

    fn from_arc(arc: Arc<Payload<H, [T]>>) -> Self {
        let raw = Arc::into_raw(arc).cast::<usize>().cast_mut();
        Self {
            // SAFETY: Arc::into_raw returns a non-null allocation pointer.
            data: unsafe { NonNull::new_unchecked(raw) },
            ownership: PhantomData,
        }
    }

    fn raw(&self) -> *const Payload<H, [T]> {
        // SAFETY: this owner retains a live strong reference. Weak handles
        // keep their metadata and never read a destroyed length/header.
        let len = unsafe { self.data.as_ptr().read() };
        ptr::slice_from_raw_parts(self.data.as_ptr().cast::<T>(), len) as *const Payload<H, [T]>
    }

    fn arc_view(&self) -> ManuallyDrop<Arc<Payload<H, [T]>>> {
        // SAFETY: borrow the existing accounted strong reference; don't drop it.
        ManuallyDrop::new(unsafe { Arc::from_raw(self.raw()) })
    }

    fn items(&self) -> &[T] {
        // SAFETY: the payload is initialized and kept alive by self.
        unsafe { &(*self.raw()).items }
    }

    fn header(&self) -> &H {
        // SAFETY: the payload is initialized and kept alive by self.
        unsafe { &(*self.raw()).header }
    }

    fn get_mut(&mut self) -> Option<(&mut H, &mut [T])> {
        let mut view = self.arc_view();
        let raw = Arc::get_mut(&mut view)? as *mut Payload<H, [T]>;
        // SAFETY: Arc verified unique strong ownership and no weak handles.
        // The two fields are disjoint and their borrows are tied to &mut self.
        // The temporary ManuallyDrop view neither releases nor adds ownership.
        unsafe { Some((&mut (*raw).header, &mut (*raw).items)) }
    }

    fn downgrade(&self) -> WeakOwner<H, T> {
        WeakOwner(Arc::downgrade(&self.arc_view()))
    }
}

impl<H, T> Clone for ThinOwner<H, T> {
    fn clone(&self) -> Self { Self::from_arc(Arc::clone(&self.arc_view())) }
}

impl<H, T> Drop for ThinOwner<H, T> {
    fn drop(&mut self) {
        // SAFETY: release precisely this owner's strong reference. Arc drops
        // H and every T once, retains weak metadata, then deallocates normally.
        unsafe { drop(Arc::from_raw(self.raw())) }
    }
}

impl<H, T> WeakOwner<H, T> {
    fn upgrade(&self) -> Option<ThinOwner<H, T>> { self.0.upgrade().map(ThinOwner::from_arc) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct DropItem(Arc<AtomicUsize>, usize);
    impl Drop for DropItem {
        fn drop(&mut self) { self.0.fetch_add(1, Ordering::SeqCst); }
    }

    #[test]
    fn destructors_run_at_last_strong_owner_before_weak_release() {
        for n in 0..65 {
            let drops = Arc::new(AtomicUsize::new(0));
            let items = (0..n).map(|i| DropItem(drops.clone(), i)).collect();
            let owner = ThinOwner::from_vec(DropItem(drops.clone(), usize::MAX), items);
            assert_eq!(owner.items().len(), n);
            assert_eq!(owner.header().1, usize::MAX);
            for (i, value) in owner.items().iter().enumerate() { assert_eq!(value.1, i); }
            let weak = owner.downgrade();
            let other = owner.clone();
            drop(owner);
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            assert_eq!(weak.upgrade().unwrap().items().len(), n);
            drop(other);
            assert_eq!(drops.load(Ordering::SeqCst), n + 1);
            assert!(weak.upgrade().is_none());
            drop(weak);
            assert_eq!(drops.load(Ordering::SeqCst), n + 1);
        }
    }

    #[test]
    fn fixed_arrays_and_unique_mutation_preserve_ownership() {
        let mut owner = ThinOwner::from_array(7_u64, [1_u32, 2, 3]);
        let clone = owner.clone();
        assert!(owner.get_mut().is_none());
        drop(clone);
        let weak = owner.downgrade();
        assert!(owner.get_mut().is_none());
        drop(weak);
        let (header, items) = owner.get_mut().unwrap();
        *header = 99;
        items[1] = 88;
        assert_eq!(*owner.header(), 99);
        assert_eq!(owner.items(), &[1, 88, 3]);
        let mut empty = ThinOwner::from_array((), [] as [u8; 0]);
        assert!(empty.get_mut().unwrap().1.is_empty());
    }

    #[test]
    fn atomic_header_and_elements_survive_thread_handoffs() {
        let owner = ThinOwner::from_vec(AtomicU64::new(0), vec![11_u32, 22, 33]);
        let weak = owner.downgrade();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let owner = owner.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..100 {
                        owner.header().fetch_add(1, Ordering::Relaxed);
                        assert_eq!(owner.items(), &[11, 22, 33]);
                    }
                });
            }
            barrier.wait();
        });
        assert_eq!(owner.header().load(Ordering::Relaxed), 200);
        drop(owner);
        assert!(weak.upgrade().is_none());
    }
}
