//! A frame's operand stack and local variables (RFC 0078, stage 1).
//!
//! Each is a type of its own with the operations frames use, so that where
//! the values live can change without touching the interpreter, the frame
//! JIT and generators: a later stage moves both into an activation record
//! on a per-thread stack. Both are heap buffers for now.

use std::ops::{Deref, DerefMut, RangeBounds};

use crate::object::Object;
use crate::sync::{Rc, RefCell};

/// A frame's operand stack: a growable run of values, the top last.
///
/// A vector's parts with a 32-bit depth and capacity, so the stack takes two
/// words of the frame instead of three: every suspended generator keeps its
/// frame boxed, and the word puts the box in a smaller allocation class.
/// Operations a vector does better take the parts as one for the moment
/// (see [`Self::with_vec`]).
pub(crate) struct FrameStack {
    ptr: std::ptr::NonNull<Object>,
    len: u32,
    cap: u32,
}

// SAFETY: the stack owns its values exactly as a `Vec<Object>` would.
unsafe impl Send for FrameStack {}
// SAFETY: as above; shared access only reads.
unsafe impl Sync for FrameStack {}

impl Default for FrameStack {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl FrameStack {
    #[inline]
    pub(crate) const fn new() -> Self {
        FrameStack {
            ptr: std::ptr::NonNull::dangling(),
            len: 0,
            cap: 0,
        }
    }

    #[inline]
    pub(crate) fn with_capacity(n: usize) -> Self {
        Self::from_vec(Vec::with_capacity(n))
    }

    /// The stack over `v`'s buffer and values.
    #[inline]
    fn from_vec(v: Vec<Object>) -> Self {
        let mut v = std::mem::ManuallyDrop::new(v);
        let (len, cap) = (v.len(), v.capacity());
        assert!(u32::try_from(cap).is_ok(), "operand stack too deep");
        FrameStack {
            // SAFETY: a vector's pointer is never null.
            ptr: unsafe { std::ptr::NonNull::new_unchecked(v.as_mut_ptr()) },
            len: len as u32,
            cap: cap as u32,
        }
    }

    /// Run `f` on the stack as a vector (its parts are put back after,
    /// even if `f` unwinds).
    #[inline]
    fn with_vec<R>(&mut self, f: impl FnOnce(&mut Vec<Object>) -> R) -> R {
        struct Back<'a> {
            stack: &'a mut FrameStack,
            v: std::mem::ManuallyDrop<Vec<Object>>,
        }
        impl Drop for Back<'_> {
            fn drop(&mut self) {
                // SAFETY: taken once; the field isn't used again.
                let v = unsafe { std::mem::ManuallyDrop::take(&mut self.v) };
                let parts = FrameStack::from_vec(v);
                // SAFETY: the stack's old parts went to the vector:
                // overwritten without a drop.
                unsafe { std::ptr::write(self.stack, parts) };
            }
        }
        // SAFETY: the parts are a vector's (see `from_vec`), handed to it
        // for the call: the stack's own are overwritten afterwards, without
        // a drop.
        let v =
            unsafe { Vec::from_raw_parts(self.ptr.as_ptr(), self.len as usize, self.cap as usize) };
        let mut back = Back {
            stack: self,
            v: std::mem::ManuallyDrop::new(v),
        };
        f(&mut back.v)
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, v: Object) {
        self.push_fast(v);
    }

    /// [`Self::push`] with the room check in line.
    #[inline(always)]
    pub(crate) fn push_fast(&mut self, v: Object) {
        if self.len < self.cap {
            // SAFETY: `len < cap`: the slot is allocated and unused.
            unsafe { self.ptr.as_ptr().add(self.len as usize).write(v) };
            self.len += 1;
        } else {
            self.push_grow(v);
        }
    }

    #[cold]
    #[inline(never)]
    fn push_grow(&mut self, v: Object) {
        self.with_vec(|b| b.push(v));
    }

    #[inline(always)]
    pub(crate) fn pop(&mut self) -> Option<Object> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: the slot was initialized and is forgotten by the depth.
        Some(unsafe { self.ptr.as_ptr().add(self.len as usize).read() })
    }

    /// Release the values above `len`, the top first, as CPython closes a
    /// frame's operands (finalizers see the order).
    #[inline(always)]
    pub(crate) fn truncate(&mut self, len: usize) {
        if len < self.len as usize {
            self.release_above(len);
        }
    }

    /// [`Self::truncate`]'s release (out of line: there are many callers).
    #[inline(never)]
    fn release_above(&mut self, len: usize) {
        let n = self.len as usize;
        // SAFETY: the stack forgets the values first; each is released
        // exactly once.
        unsafe {
            self.len = len as u32;
            let base = self.ptr.as_ptr();
            let mut k = n;
            while k > len {
                k -= 1;
                std::ptr::drop_in_place(base.add(k));
            }
        }
    }

    #[inline]
    pub(crate) fn clear(&mut self) {
        self.truncate(0);
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
        debug_assert!(len <= self.cap as usize);
        self.len = len as u32;
    }

    /// The depth.
    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        self.len as usize
    }

    #[inline(always)]
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The base of the stack's storage, valid up to its capacity (the
    /// native code writes slots above the depth).
    #[inline(always)]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut Object {
        self.ptr.as_ptr()
    }

    #[inline(always)]
    pub(crate) fn as_ptr(&self) -> *const Object {
        self.ptr.as_ptr()
    }

    #[inline(always)]
    pub(crate) fn capacity(&self) -> usize {
        self.cap as usize
    }

    /// Make room for `additional` more values.
    #[inline]
    pub(crate) fn reserve(&mut self, additional: usize) {
        if (self.cap - self.len) as usize >= additional {
            return;
        }
        self.with_vec(|b| b.reserve(additional));
    }

    #[inline]
    pub(crate) fn shrink_to(&mut self, min: usize) {
        self.with_vec(|b| b.shrink_to(min));
    }

    #[inline]
    pub(crate) fn extend(&mut self, values: impl IntoIterator<Item = Object>) {
        self.with_vec(|b| b.extend(values));
    }

    #[inline]
    pub(crate) fn remove(&mut self, at: usize) -> Object {
        self.with_vec(|b| b.remove(at))
    }

    /// The values from `range`'s start up, moved out (the stack ends where
    /// they began).
    #[inline]
    pub(crate) fn drain(&mut self, range: std::ops::RangeFrom<usize>) -> StackDrain<'_> {
        self.take_from(range.start)
    }

    /// The values from `at` up, moved out.
    #[inline]
    pub(crate) fn split_off(&mut self, at: usize) -> Vec<Object> {
        self.with_vec(|b| b.split_off(at))
    }

    /// Replace the values in `range` with `with`.
    #[inline]
    pub(crate) fn splice(
        &mut self,
        range: impl RangeBounds<usize>,
        with: impl IntoIterator<Item = Object>,
    ) {
        self.with_vec(|b| {
            b.splice(range, with);
        });
    }
}

impl Drop for FrameStack {
    #[inline]
    fn drop(&mut self) {
        self.clear();
        if self.cap != 0 {
            // SAFETY: the parts are a vector's, now empty.
            drop(unsafe { Vec::from_raw_parts(self.ptr.as_ptr(), 0, self.cap as usize) });
        }
    }
}

/// The values a [`FrameStack`] hands over from a depth up (see
/// [`FrameStack::drain`]): the stack already ends below them, and those
/// not taken are released when this is dropped.
pub(crate) struct StackDrain<'a> {
    base: *mut Object,
    next: usize,
    end: usize,
    _stack: std::marker::PhantomData<&'a mut FrameStack>,
}

impl Iterator for StackDrain<'_> {
    type Item = Object;

    #[inline(always)]
    fn next(&mut self) -> Option<Object> {
        if self.next == self.end {
            return None;
        }
        // SAFETY: an initialized slot above the stack's depth, read once.
        let v = unsafe { self.base.add(self.next).read() };
        self.next += 1;
        Some(v)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end - self.next;
        (n, Some(n))
    }
}

impl ExactSizeIterator for StackDrain<'_> {}

impl DoubleEndedIterator for StackDrain<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Object> {
        if self.next == self.end {
            return None;
        }
        self.end -= 1;
        // SAFETY: as `next`'s.
        Some(unsafe { self.base.add(self.end).read() })
    }
}

impl Drop for StackDrain<'_> {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: the values not taken, released once.
        unsafe {
            std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(
                self.base.add(self.next),
                self.end - self.next,
            ));
        }
    }
}

/// A run of operands whose top values move off it: a frame's stack, or a
/// staging vector.
pub(crate) trait Operands {
    /// The values [`Self::take_from`] hands over.
    type Taken<'a>: ExactSizeIterator<Item = Object>
    where
        Self: 'a;

    fn depth(&self) -> usize;

    /// The values from `at` up, moved out.
    fn take_from(&mut self, at: usize) -> Self::Taken<'_>;
}

impl Operands for FrameStack {
    type Taken<'a> = StackDrain<'a>;

    #[inline]
    fn depth(&self) -> usize {
        self.len as usize
    }

    #[inline]
    fn take_from(&mut self, at: usize) -> StackDrain<'_> {
        let end = self.len as usize;
        assert!(at <= end, "draining past the stack's depth");
        self.len = at as u32;
        StackDrain {
            base: self.ptr.as_ptr(),
            next: at,
            end,
            _stack: std::marker::PhantomData,
        }
    }
}

impl Operands for Vec<Object> {
    type Taken<'a> = std::vec::Drain<'a, Object>;

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
        // SAFETY: the first `len` slots are initialized.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len as usize) }
    }
}

impl DerefMut for FrameStack {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut [Object] {
        // SAFETY: as `deref`'s, exclusively.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len as usize) }
    }
}

/// The handle frame objects and the spine's shells hold on a frame's
/// locals, so that `f_locals` reads the live values (see `Frame::locals`).
pub(crate) type SharedLocals = Rc<RefCell<Vec<Object>>>;

/// A frame's local variables: one slot per name, fixed while the frame
/// runs.
pub(crate) struct FrameLocals {
    rc: SharedLocals,
}

impl FrameLocals {
    #[inline]
    pub(crate) fn from_shared(rc: SharedLocals) -> Self {
        FrameLocals { rc }
    }

    #[inline]
    pub(crate) fn into_shared(self) -> SharedLocals {
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: moved out once; `this` is never dropped.
        unsafe { std::ptr::read(&raw const this.rc) }
    }

    /// The handle a frame object or shell holds, to read the live values.
    #[inline(always)]
    pub(crate) fn shared(&self) -> &SharedLocals {
        &self.rc
    }

    /// The frame's identity while it runs (its locals' storage, which no
    /// other frame shares).
    #[inline(always)]
    pub(crate) fn identity(&self) -> *const () {
        Rc::as_ptr(&self.rc).cast()
    }

    /// Whether anything else (a frame object, a shell, a traceback's
    /// frame) holds the locals.
    #[inline(always)]
    pub(crate) fn is_shared(&self) -> bool {
        Rc::strong_count(&self.rc) > 1
    }

    /// The number of slots, read without the borrow flag (the running
    /// frame is the only writer).
    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        // SAFETY: a read of the vector's length; nothing resizes the
        // locals while the frame runs.
        unsafe { (*self.rc.as_ptr()).len() }
    }

    /// The first slot, for the native code and the binding paths.
    #[inline(always)]
    pub(crate) fn as_mut_ptr(&self) -> *mut Object {
        // SAFETY: as `len`'s.
        unsafe { (*self.rc.as_ptr()).as_mut_ptr() }
    }

    /// The live slots, read without the borrow flag.
    ///
    /// # Safety
    ///
    /// Nothing writes the locals while the result is held (the running
    /// frame's own reads, under the GIL, with no Python code in between).
    #[inline(always)]
    pub(crate) unsafe fn slots(&self) -> &[Object] {
        // SAFETY: the caller's contract.
        unsafe { &*self.rc.as_ptr() }
    }

    /// The storage itself, without the borrow flag: the binding paths
    /// size, fill and empty it through this (RFC 0078 stage 2 replaces
    /// them).
    ///
    /// # Safety
    ///
    /// No borrow of the locals (a frame object's read, say) is live, and
    /// the result isn't held across code that could take one.
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn storage(&self) -> &mut Vec<Object> {
        // SAFETY: the caller's contract.
        unsafe { &mut *self.rc.as_ptr() }
    }

    #[inline]
    pub(crate) fn borrow(&self) -> crate::sync::Ref<'_, Vec<Object>> {
        self.rc.borrow()
    }

    #[inline]
    pub(crate) fn borrow_mut(&self) -> crate::sync::RefMut<'_, Vec<Object>> {
        self.rc.borrow_mut()
    }

    #[inline]
    pub(crate) fn try_borrow_mut(
        &self,
    ) -> Result<crate::sync::RefMut<'_, Vec<Object>>, crate::sync::BorrowMutError> {
        self.rc.try_borrow_mut()
    }

    /// The storage when no borrow is live (see `GilCell::peek_mut`).
    ///
    /// # Safety
    ///
    /// As `GilCell::peek_mut`'s.
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn peek_mut(&self) -> Option<&mut Vec<Object>> {
        // SAFETY: the caller's contract.
        unsafe { self.rc.peek_mut() }
    }
}

impl Drop for FrameLocals {
    /// A frame dropped with its locals still in it (one that didn't finish
    /// through a retiring path) releases them as those paths do.
    #[inline]
    fn drop(&mut self) {
        if Rc::strong_count(&self.rc) == 1 {
            // SAFETY: the last handle: nothing else reaches the vector.
            release_values(unsafe { &mut *self.rc.as_ptr() });
        }
    }
}

/// A frame's cells: its cell variables, then its free variables.
pub(crate) type FrameCells = Rc<Vec<Rc<RefCell<Object>>>>;

/// Empty `values`, the last value first (the order CPython's
/// `_PyFrame_ClearLocals` releases a frame's locals in).
#[inline]
pub(crate) fn release_values(values: &mut Vec<Object>) {
    // SAFETY: nothing else borrows the vector.
    unsafe { crate::release_locals(values) }
}

/// Release the cells a finished frame owns and its locals, as
/// `_PyFrame_ClearLocals` walks CPython's `localsplus` from the top down: the
/// free variables, then the cell variables that aren't arguments, then the
/// locals, an argument's cell in its argument's slot. Cells something else
/// also holds through this vector (a frame object) stay, to go with it.
///
/// `cells` must hold a count of its own: a lean activation's borrowed handle
/// looks unique when its function is the only owner. A frame with only free
/// variables has nothing of its own here: its vector is its function's
/// closure, which further calls still need.
#[inline]
pub(crate) fn release_cells_and_locals(
    code: &weavepy_compiler::CodeObject,
    cells: &mut FrameCells,
    locals: &mut Vec<Object>,
) {
    if cells.is_empty() || code.cellvars.is_empty() {
        release_values(locals);
    } else {
        release_held_cells_and_locals(code, cells, locals);
    }
}

/// [`release_cells_and_locals`] with cells.
#[inline(never)]
fn release_held_cells_and_locals(
    code: &weavepy_compiler::CodeObject,
    cells: &mut FrameCells,
    locals: &mut Vec<Object>,
) {
    let Some(held) = Rc::get_mut(cells) else {
        return release_values(locals);
    };
    // The slot of each cell variable that is an argument (its value moved
    // out of the slot into the cell).
    let args: &[u32] = crate::code_vm_ext(code).map_or(&[], |ext| &ext.cell_args);
    let args = &args[..args.len().min(held.len())];
    let Some(top_arg) = args.iter().copied().filter(|&s| s != u32::MAX).max() else {
        while let Some(cell) = held.pop() {
            drop(cell);
        }
        return release_values(locals);
    };
    let ncells = held.len();
    let cell_base = held.as_mut_ptr();
    let nlocals = locals.len();
    let local_base = locals.as_mut_ptr();
    // SAFETY: both vectors forget their values first, and each value is
    // released exactly once: a cell that isn't an argument's in the first
    // loop, an argument's at its slot or, past the locals, in the last.
    unsafe {
        held.set_len(0);
        locals.set_len(0);
        for i in (0..ncells).rev() {
            if args.get(i).is_none_or(|&s| s == u32::MAX) {
                std::ptr::drop_in_place(cell_base.add(i));
            }
        }
        for k in (0..nlocals).rev() {
            std::ptr::drop_in_place(local_base.add(k));
            if k <= top_arg as usize {
                if let Some(i) = args.iter().position(|&s| s as usize == k) {
                    std::ptr::drop_in_place(cell_base.add(i));
                }
            }
        }
        for (i, &s) in args.iter().enumerate() {
            if s != u32::MAX && s as usize >= nlocals {
                std::ptr::drop_in_place(cell_base.add(i));
            }
        }
    }
}
