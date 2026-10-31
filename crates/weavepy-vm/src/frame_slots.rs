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

    /// Release the values above `len`, the top first, as CPython closes a
    /// frame's operands (finalizers see the order).
    #[inline(always)]
    pub(crate) fn truncate(&mut self, len: usize) {
        if len < self.buf.len() {
            self.release_above(len);
        }
    }

    /// [`Self::truncate`]'s release (out of line: there are many callers).
    #[inline(never)]
    fn release_above(&mut self, len: usize) {
        let n = self.buf.len();
        // SAFETY: the vector forgets the values first; each is released
        // exactly once.
        unsafe {
            self.buf.set_len(len);
            let base = self.buf.as_mut_ptr();
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

impl Drop for FrameStack {
    #[inline]
    fn drop(&mut self) {
        self.clear();
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
