//! Tuple elements and their successful Python hash share one allocation.

use crate::shared_value::ThinArc;
use std::alloc::Layout;
use std::ops::{Deref, DerefMut};

use crate::object::Object;
use crate::sync::CachedHash;
use std::sync::Arc as Rc;

pub type SharedTuple = ThinArc<TupleStorage>;

/// Immutable tuple storage with length and cached hash in its allocation.
#[repr(C)]
#[derive(Debug)]
pub struct TupleStorage<T: ?Sized = [Object]> {
    len: usize,
    hash: CachedHash,
    items: T,
}

impl TupleStorage {
    /// Move a fixed-size array directly into its final allocation.
    pub fn from_array<const N: usize>(items: [Object; N]) -> SharedTuple {
        let arc: Rc<Self> = Rc::new(TupleStorage {
            len: N,
            hash: CachedHash::default(),
            items,
        });
        ThinArc::from_arc(arc)
    }

    /// Move a dynamically sized sequence into one reference-counted block.
    pub fn from_vec(mut items: Vec<Object>) -> SharedTuple {
        let len = items.len();
        // SAFETY: the elements are moved whole into the tuple's slots and
        // the vector forgets them at once (it keeps only its buffer).
        unsafe {
            Self::filled(len, |destination| {
                std::ptr::copy_nonoverlapping(items.as_ptr(), destination, len);
                items.set_len(0);
            })
        }
    }

    /// [`Self::from_vec`] straight from an iterator that knows its
    /// length: the elements are moved into the tuple's own allocation
    /// without the intermediate vector. A `*args` parameter is bound
    /// from the tail of the call's argument vector, which used to be
    /// collected into a `Vec` only to be moved out of again.
    pub fn from_exact_iter<I>(items: I) -> SharedTuple
    where
        I: ExactSizeIterator<Item = Object>,
    {
        let len = items.len();
        // SAFETY: the iterator yields exactly `len` elements, each moved
        // into its slot.
        unsafe {
            Self::filled(len, |destination| {
                for (index, item) in items.enumerate() {
                    destination.add(index).write(item);
                }
            })
        }
    }

    /// A tuple of clones of `items`: the elements copied whole in one
    /// pass, then their counts taken in a second (an element moved
    /// through an iterator goes by way of the stack in two unaligned
    /// halves, which costs about twenty instructions each).
    pub fn from_cloned_slice(items: &[Object]) -> SharedTuple {
        // SAFETY: every slot is written by the copy (which neither runs
        // code nor unwinds).
        unsafe {
            Self::filled(items.len(), |destination| {
                crate::object::clone_objects_into(items, destination);
            })
        }
    }

    /// A tuple of clones of `count` runs of `items`, one after another
    /// (`t * n`).
    pub fn from_repeated_slice(items: &[Object], count: usize) -> SharedTuple {
        let len = items
            .len()
            .checked_mul(count)
            .expect("tuple elements exceed layout limit");
        // SAFETY: the `count` runs fill exactly `len` slots.
        unsafe {
            Self::filled(len, |destination| {
                for run in 0..count {
                    crate::object::clone_objects_into(items, destination.add(run * items.len()));
                }
            })
        }
    }

    /// A tuple of clones of `head`'s elements, then `tail`'s (`a + b`).
    pub fn from_cloned_pair(head: &[Object], tail: &[Object]) -> SharedTuple {
        // SAFETY: the two runs fill exactly the slots asked for.
        unsafe {
            Self::filled(head.len() + tail.len(), |destination| {
                crate::object::clone_objects_into(head, destination);
                crate::object::clone_objects_into(tail, destination.add(head.len()));
            })
        }
    }

    /// A tuple of `len` elements written by `fill` into its own
    /// allocation.
    ///
    /// # Safety
    ///
    /// `fill` initializes all `len` slots at the pointer it's given, and
    /// doesn't unwind.
    #[inline(always)]
    unsafe fn filled(len: usize, fill: impl FnOnce(*mut Object)) -> SharedTuple {
        let (prefix, _) = Layout::new::<usize>()
            .extend(Layout::new::<CachedHash>())
            .expect("tuple prefix exceeds layout limit");
        let (layout, _) = prefix
            .extend(Layout::array::<Object>(len).expect("tuple elements exceed layout limit"))
            .expect("tuple header exceeds layout limit");
        let layout = layout.pad_to_align();
        // Allocate words with exactly the target payload's alignment and
        // size. The hash-cell option covers targets such as i686, where
        // a 64-bit atomic has stricter alignment than a plain integer.
        macro_rules! allocate {
            ($($word:ty),*) => {
                $(if layout.align() == std::mem::align_of::<$word>() {
                    // SAFETY: per the caller.
                    return unsafe { Self::filled_aligned::<$word>(len, layout, fill) };
                })*
            };
        }
        allocate!(u8, u16, u32, u64, CachedHash, u128);
        panic!("unsupported tuple payload alignment: {}", layout.align());
    }

    /// # Safety
    ///
    /// As [`Self::filled`].
    #[inline(always)]
    unsafe fn filled_aligned<Word>(
        len: usize,
        layout: Layout,
        fill: impl FnOnce(*mut Object),
    ) -> SharedTuple {
        assert_eq!(layout.align(), std::mem::align_of::<Word>());
        assert_eq!(layout.size() % std::mem::size_of::<Word>(), 0);
        let storage = Rc::<[Word]>::new_uninit_slice(layout.size() / std::mem::size_of::<Word>());
        let data = Rc::into_raw(storage).cast::<Object>().cast_mut();
        let tuple = std::ptr::slice_from_raw_parts_mut(data, len) as *mut Self;
        // SAFETY: repr(C) places length and hash before the final slice, with the
        // offset and trailing padding computed by Layout::extend above.
        // The original MaybeUninit<Word> slice and this DST have identical
        // payload size/alignment. Arc::from_raw permits this conversion;
        // it derives the same reference-count header and deallocation
        // layout without depending on Arc's private field layout.
        //
        // The raw pointer comes from the only Arc. No references to the
        // uninitialized payload exist. Per the caller, `fill` writes
        // exactly len elements and doesn't unwind.
        // Changing the fat pointer's metadata preserves its data address
        // and provenance. The final Arc drops each moved Object once;
        // the original MaybeUninit words have no destructors.
        unsafe {
            std::ptr::addr_of_mut!((*tuple).len).write(len);
            std::ptr::addr_of_mut!((*tuple).hash).write(CachedHash::default());
            let destination = std::ptr::addr_of_mut!((*tuple).items).cast::<Object>();
            fill(destination);
            let result = Rc::from_raw(tuple);
            debug_assert_eq!(Layout::for_value(result.as_ref()), layout);
            ThinArc::from_arc(result)
        }
    }

    #[inline]
    pub fn cached_hash(&self) -> Option<i64> {
        self.hash.get()
    }

    #[inline]
    pub(crate) fn store_hash(&self, value: i64) {
        self.hash.store(value);
    }
}

impl Deref for TupleStorage {
    type Target = [Object];

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl DerefMut for TupleStorage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Only unique ownership, obtained through ThinArc::get_mut, permits
        // free-list reuse. Clear the previous tuple's hash before exposing
        // any writable elements, including an empty mutable slice.
        self.hash = CachedHash::default();
        &mut self.items
    }
}

impl<'a> IntoIterator for &'a TupleStorage {
    type Item = &'a Object;
    type IntoIter = std::slice::Iter<'a, Object>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<'a> IntoIterator for &'a mut TupleStorage {
    type Item = &'a mut Object;
    type IntoIter = std::slice::IterMut<'a, Object>;

    fn into_iter(self) -> Self::IntoIter {
        self.deref_mut().iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_value::SharedStr;

    #[test]
    fn dynamic_layout_preserves_values_and_reference_ownership() {
        for len in 0..128 {
            let text = SharedStr::from("retained tuple element");
            let tuple = TupleStorage::from_vec(vec![Object::Str(text.clone()); len]);
            assert_eq!(tuple.len(), len);
            assert_eq!(SharedStr::strong_count(&text), len + 1);
            assert!(tuple
                .iter()
                .all(|value| matches!(value, Object::Str(s) if SharedStr::ptr_eq(s, &text))));
            let weak = ThinArc::downgrade(&tuple);
            let clone = tuple.clone();
            drop(tuple);
            assert!(weak.upgrade().is_some());
            drop(clone);
            assert_eq!(SharedStr::strong_count(&text), 1);
            assert!(weak.upgrade().is_none());
        }
    }

    #[test]
    fn fixed_arrays_and_unique_mutation_reset_the_hash() {
        let mut tuple = TupleStorage::from_array([Object::Int(1), Object::Int(2)]);
        tuple.store_hash(123);
        assert_eq!(tuple.cached_hash(), Some(123));
        ThinArc::get_mut(&mut tuple).unwrap()[0] = Object::Int(3);
        assert_eq!(tuple.cached_hash(), None);
        assert!(matches!(tuple[0], Object::Int(3)));
        assert!(TupleStorage::from_array([]).is_empty());
    }

    #[test]
    fn shared_cache_publication_does_not_change_elements() {
        let tuple = TupleStorage::from_vec(vec![Object::Int(7), Object::Int(9)]);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let tuple = tuple.clone();
                scope.spawn(move || {
                    for _ in 0..100 {
                        tuple.store_hash(i64::MIN);
                        assert_eq!(tuple.cached_hash(), Some(i64::MIN));
                        assert!(matches!(tuple[1], Object::Int(9)));
                    }
                });
            }
        });
    }
}
