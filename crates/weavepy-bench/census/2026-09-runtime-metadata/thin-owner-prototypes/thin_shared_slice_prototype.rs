//! Unapplied prototype: reuse Arc ownership while storing slice length in its payload.
//! This file is not part of the runtime and establishes no memory-safety or speed claim.
use std::alloc::Layout;
use std::marker::PhantomData;
use std::mem::{align_of, size_of, ManuallyDrop};
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Weak};

mod sealed {
    pub trait Element: Copy + Send + Sync + 'static {}
    impl Element for u8 {}
    impl Element for u32 {}
}

#[repr(C)]
struct Payload<T> {
    len: usize,
    values: [T],
}

struct ThinSlice<T: sealed::Element> {
    header: NonNull<usize>,
    ownership: PhantomData<Arc<Payload<T>>>,
}

// SAFETY: the allocation is immutable and ownership is entirely handled by Arc.
// The sealed element types satisfy the same Send + Sync requirements as Arc.
unsafe impl<T: sealed::Element> Send for ThinSlice<T> {}
unsafe impl<T: sealed::Element> Sync for ThinSlice<T> {}

struct WeakSlice<T: sealed::Element>(Weak<Payload<T>>);

impl<T: sealed::Element> ThinSlice<T> {
    fn new(values: &[T]) -> Self {
        assert!(align_of::<T>() <= align_of::<usize>());
        let (layout, offset) = Layout::new::<usize>()
            .extend(Layout::array::<T>(values.len()).expect("slice layout overflow"))
            .expect("payload layout overflow");
        let layout = layout.pad_to_align();
        assert_eq!(layout.align(), align_of::<usize>());
        assert_eq!(layout.size() % size_of::<usize>(), 0);
        let mut allocation = Arc::<[usize]>::new_uninit_slice(layout.size() / size_of::<usize>());
        let dst = Arc::get_mut(&mut allocation).unwrap().as_mut_ptr().cast::<u8>();
        // SAFETY: this unique allocation has the exact padded payload layout.
        // Initialize the length and all elements; padding need not be initialized.
        unsafe {
            dst.cast::<usize>().write(values.len());
            ptr::copy_nonoverlapping(values.as_ptr(), dst.add(offset).cast::<T>(), values.len());
        }
        // Arc::from_raw permits a different pointee with equal size/alignment
        // and valid initialized fields. The word-slice and Payload<T> layouts
        // agree, including padding, and both use the global allocator.
        let raw = Arc::into_raw(allocation).cast::<usize>().cast_mut();
        Self {
            // SAFETY: Arc::into_raw always returns a non-null allocation pointer.
            header: unsafe { NonNull::new_unchecked(raw) },
            ownership: PhantomData,
        }
    }

    fn raw(&self) -> *const Payload<T> {
        // SAFETY: this owner retains a strong Arc reference, so the initialized
        // header remains alive. Weak handles never call this on a dead payload.
        let len = unsafe { self.header.as_ptr().read() };
        ptr::slice_from_raw_parts(self.header.as_ptr().cast::<T>(), len) as *const Payload<T>
    }

    fn from_arc(arc: Arc<Payload<T>>) -> Self {
        let raw = Arc::into_raw(arc).cast::<usize>().cast_mut();
        Self {
            // SAFETY: an Arc payload pointer is non-null.
            header: unsafe { NonNull::new_unchecked(raw) },
            ownership: PhantomData,
        }
    }

    fn arc_view(&self) -> ManuallyDrop<Arc<Payload<T>>> {
        // SAFETY: raw reconstructs our live accounted owner. ManuallyDrop
        // borrows that ownership without decrementing the strong count.
        ManuallyDrop::new(unsafe { Arc::from_raw(self.raw()) })
    }

    fn downgrade(&self) -> WeakSlice<T> {
        WeakSlice(Arc::downgrade(&self.arc_view()))
    }

    fn ptr_eq(&self, other: &Self) -> bool {
        self.header == other.header
    }
}

impl<T: sealed::Element> Deref for ThinSlice<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        // SAFETY: raw reconstructs a live initialized immutable payload; the
        // borrow can't outlive this strong owner.
        unsafe { &(*self.raw()).values }
    }
}

impl<T: sealed::Element> Clone for ThinSlice<T> {
    fn clone(&self) -> Self {
        Self::from_arc(Arc::clone(&self.arc_view()))
    }
}

impl<T: sealed::Element> Drop for ThinSlice<T> {
    fn drop(&mut self) {
        // SAFETY: relinquish this owner's one strong reference exactly once.
        // Arc handles last-strong destruction, outstanding weak owners, and
        // eventual deallocation with the same layout used at construction.
        unsafe { drop(Arc::from_raw(self.raw())) }
    }
}

impl<T: sealed::Element> WeakSlice<T> {
    fn upgrade(&self) -> Option<ThinSlice<T>> {
        self.0.upgrade().map(ThinSlice::from_arc)
    }
}

impl<T: sealed::Element> Clone for WeakSlice<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[test]
fn every_small_length_survives_clones_and_weak_owners() {
    for n in 0..1025 {
        let bytes: Vec<_> = (0..n).map(|i| (i * 17) as u8).collect();
        let words: Vec<_> = (0..n).map(|i| (i * 65539) as u32).collect();
        let a = ThinSlice::new(&bytes);
        let b = ThinSlice::new(&words);
        assert_eq!(&*a, bytes);
        assert_eq!(&*b, words);
        let ac = a.clone();
        let bc = b.clone();
        let aw = a.downgrade();
        let bw = b.downgrade();
        assert!(a.ptr_eq(&ac));
        assert!(b.ptr_eq(&bc));
        drop(a);
        drop(b);
        assert!(aw.upgrade().unwrap().ptr_eq(&ac));
        assert!(bw.upgrade().unwrap().ptr_eq(&bc));
        drop(ac);
        drop(bc);
        assert!(aw.upgrade().is_none());
        assert!(bw.upgrade().is_none());
        let copy = aw.clone();
        drop(aw);
        assert!(copy.upgrade().is_none());
    }
}

#[test]
fn concurrent_upgrade_and_last_strong_drop() {
    for _ in 0..100 {
        let owner = ThinSlice::new(&[0_u32, 0xd800, 0x10ffff, u32::MAX]);
        let weak = owner.downgrade();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let worker_weak = weak.clone();
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            for _ in 0..100 {
                if let Some(value) = worker_weak.upgrade() {
                    assert_eq!(&*value, &[0, 0xd800, 0x10ffff, u32::MAX]);
                }
            }
        });
        barrier.wait();
        drop(owner);
        worker.join().unwrap();
        assert!(weak.upgrade().is_none());
    }
}
