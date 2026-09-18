//! A thread-caching front end for the system allocator.
//!
//! The interpreter allocates and frees small blocks constantly — iterators,
//! tuples, list and dict storage, strings, boxed payloads — and a system
//! `malloc`/`free` pair costs ~20ns on macOS. [`ThreadCacheAlloc`] keeps
//! per-thread free lists of recently freed small blocks (16-byte size
//! classes up to [`MAX_SMALL`] bytes) and serves allocations of the same
//! class from them, which turns the common alloc/free pair into a handful
//! of loads and stores.
//!
//! Every block, cached or not, is a genuine system `malloc` block whose
//! usable size is at least its class size: a small request is rounded up
//! to its class before it reaches the system, and a block only enters a
//! class list when its layout maps to that class. So a block may be
//! handed back to the system (`free`, `realloc`) at any time, from any
//! thread, whatever list it last sat on.
//!
//! Caching is opt-in per thread ([`enable_for_current_thread`]): enabling
//! registers a thread-exit guard that returns the thread's cached blocks
//! to the system, so short-lived threads cannot strand memory. Threads
//! that never opt in (and a thread past its exit flush) go straight to the
//! system allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::UnsafeCell;
use std::ptr;

/// Largest request served from the class lists.
const MAX_SMALL: usize = 512;
/// Size-class granularity (also the guaranteed alignment of a class block).
const QUANTUM: usize = 16;
const NCLASSES: usize = MAX_SMALL / QUANTUM;
/// Bytes one class list may retain.
const CLASS_BUDGET: usize = 8 * 1024;

struct Lists {
    /// Whether this thread caches (see [`enable_for_current_thread`]).
    enabled: bool,
    heads: [*mut u8; NCLASSES],
    counts: [u32; NCLASSES],
}

thread_local! {
    /// This thread's class lists (intrusive: a free block's first word
    /// links to the next).
    static LISTS: UnsafeCell<Lists> = const {
        UnsafeCell::new(Lists {
            enabled: false,
            heads: [ptr::null_mut(); NCLASSES],
            counts: [0; NCLASSES],
        })
    };
    /// Flushes the lists when the thread exits.
    static EXIT_GUARD: ExitGuard = const { ExitGuard };
}

struct ExitGuard;

impl Drop for ExitGuard {
    fn drop(&mut self) {
        let _ = LISTS.try_with(|l| {
            // SAFETY: this thread's own lists; caching goes off first, so
            // the frees below go straight to the system.
            let lists = unsafe { &mut *l.get() };
            lists.enabled = false;
            for class in 0..NCLASSES {
                let mut p = lists.heads[class];
                while !p.is_null() {
                    // SAFETY: every listed block is a live system block
                    // whose first word holds the next link.
                    let next = unsafe { *(p as *mut *mut u8) };
                    unsafe { libc_free(p) };
                    p = next;
                }
                lists.heads[class] = ptr::null_mut();
                lists.counts[class] = 0;
            }
        });
    }
}

extern "C" {
    #[link_name = "free"]
    fn libc_free(p: *mut u8);
}

/// Turn on block caching for the calling thread (idempotent). Registers
/// the thread-exit flush first, while caching is still off, so the
/// registration's own allocations go straight to the system.
pub fn enable_for_current_thread() {
    // SAFETY (both reads/writes): this thread's own lists, outside any
    // allocator call.
    let on = LISTS
        .try_with(|l| unsafe { (*l.get()).enabled })
        .unwrap_or(true);
    if on {
        return;
    }
    let _ = EXIT_GUARD.try_with(|_| ());
    let _ = LISTS.try_with(|l| unsafe { (*l.get()).enabled = true });
}

/// The class of a small layout, or `None` for one the lists never hold.
#[inline]
fn class_of(layout: &Layout) -> Option<usize> {
    let size = layout.size();
    if size == 0 || size > MAX_SMALL || layout.align() > QUANTUM {
        return None;
    }
    Some((size - 1) / QUANTUM)
}

#[inline]
fn class_layout(class: usize) -> Layout {
    // SAFETY: a multiple of 16 no larger than MAX_SMALL, aligned to 16.
    unsafe { Layout::from_size_align_unchecked((class + 1) * QUANTUM, QUANTUM) }
}

/// The global allocator: [`System`] behind per-thread class lists.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThreadCacheAlloc;

unsafe impl GlobalAlloc for ThreadCacheAlloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if let Some(class) = class_of(&layout) {
            let got = LISTS
                .try_with(|l| {
                    // SAFETY: this thread's own lists; the allocator is
                    // never re-entered while they are borrowed.
                    let lists = unsafe { &mut *l.get() };
                    if !lists.enabled {
                        return ptr::null_mut();
                    }
                    let head = lists.heads[class];
                    if !head.is_null() {
                        // SAFETY: a listed block's first word links on.
                        lists.heads[class] = unsafe { *(head as *mut *mut u8) };
                        lists.counts[class] -= 1;
                    }
                    head
                })
                .unwrap_or(ptr::null_mut());
            if !got.is_null() {
                return got;
            }
            // SAFETY: a valid non-zero layout.
            return unsafe { System.alloc(class_layout(class)) };
        }
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(class) = class_of(&layout) {
            let kept = LISTS
                .try_with(|l| {
                    // SAFETY: as in `alloc`.
                    let lists = unsafe { &mut *l.get() };
                    if !lists.enabled {
                        return false;
                    }
                    let cap = (CLASS_BUDGET / ((class + 1) * QUANTUM)).max(8) as u32;
                    if lists.counts[class] >= cap {
                        return false;
                    }
                    // SAFETY: the block is ours now and at least 16 bytes.
                    unsafe { *(ptr as *mut *mut u8) = lists.heads[class] };
                    lists.heads[class] = ptr;
                    lists.counts[class] += 1;
                    true
                })
                .unwrap_or(false);
            if !kept {
                // SAFETY: a system block allocated at its class layout.
                unsafe { System.dealloc(ptr, class_layout(class)) };
            }
            return;
        }
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.align() > QUANTUM {
            // The generic path: fresh block, copy, release.
            // SAFETY: the caller's layout invariants hold for `new_layout`.
            let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
            let new = unsafe { self.alloc(new_layout) };
            if !new.is_null() {
                unsafe {
                    ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size));
                    self.dealloc(ptr, layout);
                }
            }
            return new;
        }
        let old_class = class_of(&layout);
        // SAFETY: `new_size` is non-zero and fits the caller's layout rules.
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        let new_class = class_of(&new_layout);
        if old_class.is_some() && old_class == new_class {
            // Same class: the block already has the room.
            return ptr;
        }
        // Resize the underlying system block to what the new layout's
        // eventual `dealloc` will assume (its class size, when small).
        let target = match new_class {
            Some(class) => (class + 1) * QUANTUM,
            None => new_size,
        };
        let old_system = match old_class {
            Some(class) => class_layout(class),
            None => layout,
        };
        // SAFETY: `ptr` is a live system block of `old_system`.
        unsafe { System.realloc(ptr, old_system, target) }
    }
}
