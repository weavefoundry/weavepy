//! A frame's operand stack and local variables (RFC 0078, stage 1).
//!
//! Each is a type of its own with the operations frames use, so that where
//! the values live can change without touching the interpreter, the frame
//! JIT and generators: a later stage moves both into an activation record
//! on a per-thread stack. Both are heap buffers for now.

use std::ops::{Deref, DerefMut, RangeBounds};

use crate::object::Object;

/// A frame's operand stack: a growable run of values, the top last.
#[derive(Default)]
pub(crate) struct FrameStack {
    buf: Vec<Object>,
}

impl FrameStack {
    #[inline]
    pub(crate) const fn new() -> Self {
        FrameStack { buf: Vec::new() }
    }

    #[inline]
    pub(crate) fn with_capacity(n: usize) -> Self {
        FrameStack {
            buf: Vec::with_capacity(n),
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, v: Object) {
        self.buf.push(v);
    }

    /// [`Self::push`] with the room check in line (see
    /// [`crate::push_fast`]).
    #[inline(always)]
    pub(crate) fn push_fast(&mut self, v: Object) {
        crate::push_fast(&mut self.buf, v);
    }

    #[inline(always)]
    pub(crate) fn pop(&mut self) -> Option<Object> {
        self.buf.pop()
    }

    #[inline(always)]
    pub(crate) fn truncate(&mut self, len: usize) {
        self.buf.truncate(len);
    }

    #[inline]
    pub(crate) fn clear(&mut self) {
        self.buf.clear();
    }

    /// Set the depth, the values above `len` forgotten (not released) or
    /// those below it taken as initialized.
    ///
    /// # Safety
    ///
    /// As [`Vec::set_len`]'s: `len` is within the capacity, and the values
    /// below it are initialized.
    #[inline(always)]
    pub(crate) unsafe fn set_len(&mut self, len: usize) {
        // SAFETY: the caller's contract.
        unsafe { self.buf.set_len(len) }
    }

    /// The depth.
    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline(always)]
    pub(crate) fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The base of the stack's storage, valid up to its capacity (the
    /// native code writes slots above the depth).
    #[inline(always)]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut Object {
        self.buf.as_mut_ptr()
    }

    #[inline(always)]
    pub(crate) fn as_ptr(&self) -> *const Object {
        self.buf.as_ptr()
    }

    #[inline(always)]
    pub(crate) fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// Make room for `additional` more values.
    #[inline]
    pub(crate) fn reserve(&mut self, additional: usize) {
        self.buf.reserve(additional);
    }

    #[inline]
    pub(crate) fn shrink_to(&mut self, min: usize) {
        self.buf.shrink_to(min);
    }

    #[inline]
    pub(crate) fn extend(&mut self, values: impl IntoIterator<Item = Object>) {
        self.buf.extend(values);
    }

    #[inline]
    pub(crate) fn remove(&mut self, at: usize) -> Object {
        self.buf.remove(at)
    }

    /// The values in `range`, moved out (the stack closes over the gap).
    #[inline]
    pub(crate) fn drain(&mut self, range: impl RangeBounds<usize>) -> std::vec::Drain<'_, Object> {
        self.buf.drain(range)
    }

    /// The values from `at` up, moved out.
    #[inline]
    pub(crate) fn split_off(&mut self, at: usize) -> Vec<Object> {
        self.buf.split_off(at)
    }

    /// Replace the values in `range` with `with`.
    #[inline]
    pub(crate) fn splice(
        &mut self,
        range: impl RangeBounds<usize>,
        with: impl IntoIterator<Item = Object>,
    ) {
        self.buf.splice(range, with);
    }
}

/// A run of operands whose top values move off it: a frame's stack, or a
/// staging vector.
pub(crate) trait Operands {
    fn depth(&self) -> usize;

    /// The values from `at` up, moved out.
    fn take_from(&mut self, at: usize) -> std::vec::Drain<'_, Object>;
}

impl Operands for FrameStack {
    #[inline]
    fn depth(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    fn take_from(&mut self, at: usize) -> std::vec::Drain<'_, Object> {
        self.buf.drain(at..)
    }
}

impl Operands for Vec<Object> {
    #[inline]
    fn depth(&self) -> usize {
        self.len()
    }

    #[inline]
    fn take_from(&mut self, at: usize) -> std::vec::Drain<'_, Object> {
        self.drain(at..)
    }
}

impl Deref for FrameStack {
    type Target = [Object];

    #[inline(always)]
    fn deref(&self) -> &[Object] {
        &self.buf
    }
}

impl DerefMut for FrameStack {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut [Object] {
        &mut self.buf
    }
}
