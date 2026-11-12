//! A vector in two words: its buffer, and a 32-bit length and capacity.
//!
//! For lists that sit in objects many of which are alive at once (a
//! future's callbacks and awaiters), where a `Vec`'s third word can put the
//! object in a larger allocation class. Operations other than the common
//! ones run on the parts as a `Vec` for the moment (see
//! [`CompactVec::with_vec`]).

use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

pub struct CompactVec<T> {
    ptr: NonNull<T>,
    len: u32,
    cap: u32,
    _owns: PhantomData<T>,
}

// SAFETY: the vector owns its elements exactly as a `Vec<T>` would.
unsafe impl<T: Send> Send for CompactVec<T> {}
// SAFETY: as above; shared access only reads.
unsafe impl<T: Sync> Sync for CompactVec<T> {}

impl<T> CompactVec<T> {
    pub const fn new() -> Self {
        const { assert!(std::mem::size_of::<T>() != 0) };
        CompactVec {
            ptr: NonNull::dangling(),
            len: 0,
            cap: 0,
            _owns: PhantomData,
        }
    }

    fn from_vec(v: Vec<T>) -> Self {
        const { assert!(std::mem::size_of::<T>() != 0) };
        let mut v = std::mem::ManuallyDrop::new(v);
        let (len, cap) = (v.len(), v.capacity());
        assert!(u32::try_from(cap).is_ok(), "vector too long");
        CompactVec {
            // SAFETY: a vector's pointer is never null.
            ptr: unsafe { NonNull::new_unchecked(v.as_mut_ptr()) },
            len: len as u32,
            cap: cap as u32,
            _owns: PhantomData,
        }
    }

    /// The elements as a `Vec`, the buffer handed over.
    pub fn into_vec(self) -> Vec<T> {
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: the parts are a vector's (see `from_vec`), handed over
        // once: `this` is never dropped.
        unsafe { Vec::from_raw_parts(this.ptr.as_ptr(), this.len as usize, this.cap as usize) }
    }

    /// Run `f` on the parts as a `Vec` (they are put back after, even if
    /// `f` unwinds).
    fn with_vec<R>(&mut self, f: impl FnOnce(&mut Vec<T>) -> R) -> R {
        struct Back<'a, T> {
            this: &'a mut CompactVec<T>,
            v: std::mem::ManuallyDrop<Vec<T>>,
        }
        impl<T> Drop for Back<'_, T> {
            fn drop(&mut self) {
                // SAFETY: taken once; the field isn't used again.
                let v = unsafe { std::mem::ManuallyDrop::take(&mut self.v) };
                // SAFETY: the old parts went to the vector: overwritten
                // without a drop.
                unsafe { std::ptr::write(self.this, CompactVec::from_vec(v)) };
            }
        }
        // SAFETY: as `into_vec`'s; the vector's parts replace these after.
        let v =
            unsafe { Vec::from_raw_parts(self.ptr.as_ptr(), self.len as usize, self.cap as usize) };
        let mut back = Back {
            this: self,
            v: std::mem::ManuallyDrop::new(v),
        };
        f(&mut back.v)
    }

    #[inline]
    pub fn push(&mut self, value: T) {
        if self.len < self.cap {
            // SAFETY: `len < cap`: the slot is allocated and unused.
            unsafe { self.ptr.as_ptr().add(self.len as usize).write(value) };
            self.len += 1;
        } else {
            self.with_vec(|v| v.push(value));
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: the slot was initialized and is forgotten by the length.
        Some(unsafe { self.ptr.as_ptr().add(self.len as usize).read() })
    }

    pub fn reserve_exact(&mut self, additional: usize) {
        if (self.cap - self.len) as usize >= additional {
            return;
        }
        self.with_vec(|v| v.reserve_exact(additional));
    }

    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.with_vec(|v| v.retain(keep));
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for CompactVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T> Default for CompactVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for CompactVec<T> {
    fn drop(&mut self) {
        // SAFETY: as `into_vec`'s; the vector drops the elements and buffer.
        drop(unsafe {
            Vec::from_raw_parts(self.ptr.as_ptr(), self.len as usize, self.cap as usize)
        });
    }
}

impl<T> Deref for CompactVec<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        // SAFETY: the first `len` elements are initialized.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len as usize) }
    }
}

impl<T> DerefMut for CompactVec<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        // SAFETY: as `deref`'s, exclusively.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len as usize) }
    }
}

impl<T: Clone> Clone for CompactVec<T> {
    fn clone(&self) -> Self {
        Self::from_vec(self.to_vec())
    }
}

impl<T> From<Vec<T>> for CompactVec<T> {
    fn from(v: Vec<T>) -> Self {
        Self::from_vec(v)
    }
}

impl<T> FromIterator<T> for CompactVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(items: I) -> Self {
        Self::from_vec(items.into_iter().collect())
    }
}

impl<T> IntoIterator for CompactVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a, T> IntoIterator for &'a CompactVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut CompactVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::CompactVec;

    #[test]
    fn behaves_as_a_vec() {
        assert_eq!(std::mem::size_of::<CompactVec<u64>>(), 16);
        let mut v: CompactVec<String> = CompactVec::new();
        assert!(v.is_empty());
        v.reserve_exact(1);
        for i in 0..40 {
            v.push(i.to_string());
        }
        assert_eq!(v.len(), 40);
        v.retain(|s| s.len() == 1);
        assert_eq!(&v[..], &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]);
        let w = v.clone();
        let taken: Vec<String> = std::mem::take(&mut v).into_iter().collect();
        assert_eq!(taken.len(), 10);
        assert!(v.is_empty());
        assert_eq!(
            w.iter().map(String::as_str).collect::<String>(),
            "0123456789"
        );
        let back: CompactVec<String> = taken.into();
        assert_eq!(back.len(), 10);
    }
}
