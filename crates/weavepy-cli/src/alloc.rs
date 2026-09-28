//! The process allocator: mimalloc, entered through its plain allocation
//! functions whenever they already satisfy the requested alignment.
//!
//! Every mimalloc block is at least word aligned, so a layout aligned to
//! 8 bytes or less needs no aligned entry point. The aligned entry points
//! take their fast path only when the size class's free list happens to
//! offer a suitably aligned block, and otherwise fall to a generic path
//! that may over-allocate. Rust asks for alignment on every allocation,
//! so going through them would put every allocation on that path.

use std::alloc::{GlobalAlloc, Layout};
use std::ffi::c_void;

use libmimalloc_sys::{
    mi_free, mi_malloc, mi_malloc_aligned, mi_realloc, mi_realloc_aligned, mi_zalloc,
    mi_zalloc_aligned,
};

/// The largest alignment every mimalloc block already has.
const WORD_ALIGN: usize = std::mem::size_of::<usize>();

/// mimalloc as the global allocator (see the module docs).
pub(crate) struct Mimalloc;

// SAFETY: every block comes from mimalloc with at least the layout's
// alignment (word-aligned blocks, or the aligned entry points for larger
// alignments), and every block is released or resized through mimalloc.
unsafe impl GlobalAlloc for Mimalloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: plain FFI allocation calls.
        unsafe {
            if layout.align() <= WORD_ALIGN {
                mi_malloc(layout.size()).cast()
            } else {
                mi_malloc_aligned(layout.size(), layout.align()).cast()
            }
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as in `alloc`.
        unsafe {
            if layout.align() <= WORD_ALIGN {
                mi_zalloc(layout.size()).cast()
            } else {
                mi_zalloc_aligned(layout.size(), layout.align()).cast()
            }
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        // SAFETY: `ptr` came from this allocator (the caller's contract).
        unsafe { mi_free(ptr.cast::<c_void>()) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` came from this allocator with `layout`.
        unsafe {
            if layout.align() <= WORD_ALIGN {
                mi_realloc(ptr.cast::<c_void>(), new_size).cast()
            } else {
                mi_realloc_aligned(ptr.cast::<c_void>(), new_size, layout.align()).cast()
            }
        }
    }
}
