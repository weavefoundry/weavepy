//! The process allocator: mimalloc, entered through its plain allocation
//! functions whenever they already satisfy the requested alignment.
//!
//! Every mimalloc block is at least word aligned, and every block of 16
//! bytes or more is 16-byte aligned (`MI_MAX_ALIGN_SIZE`), so a layout
//! aligned to that much needs no aligned entry point. The aligned entry points
//! take their fast path only when the size class's free list happens to
//! offer a suitably aligned block, and otherwise fall to a generic path
//! that may over-allocate. Rust asks for alignment on every allocation,
//! so going through them would put every allocation on that path.

use std::alloc::{GlobalAlloc, Layout};
use std::ffi::{c_int, c_long, c_void};

use libmimalloc_sys::{
    mi_free, mi_malloc, mi_malloc_aligned, mi_realloc, mi_realloc_aligned, mi_zalloc,
    mi_zalloc_aligned,
};

/// The largest alignment every mimalloc block already has.
const WORD_ALIGN: usize = std::mem::size_of::<usize>();

/// mimalloc's `MI_MAX_ALIGN_SIZE`: the alignment of every block at least
/// this large.
const MAX_ALIGN: usize = 16;

/// Whether the plain entry points already satisfy `align` for `size`.
#[inline(always)]
fn plain(size: usize, align: usize) -> bool {
    align <= WORD_ALIGN || (align <= MAX_ALIGN && size >= align)
}

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
            if plain(layout.size(), layout.align()) {
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
            if plain(layout.size(), layout.align()) {
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
            if plain(new_size, layout.align()) {
                mi_realloc(ptr.cast::<c_void>(), new_size).cast()
            } else {
                mi_realloc_aligned(ptr.cast::<c_void>(), new_size, layout.align()).cast()
            }
        }
    }
}

extern "C" {
    fn mi_option_set(option: c_int, value: c_long);
    #[cfg(target_os = "linux")]
    fn mi_manage_os_memory(
        start: *mut libc::c_void,
        size: libc::size_t,
        is_committed: bool,
        is_pinned: bool,
        is_zero: bool,
        numa_node: c_int,
    ) -> bool;
}

/// mimalloc 3's `mi_option_arena_reserve` (its position in `mi_option_t`).
const MI_OPTION_ARENA_RESERVE: c_int = 23;

/// The size of each arena mimalloc reserves, in KiB, unless the
/// environment sets `MIMALLOC_ARENA_RESERVE`.
///
/// mimalloc's default reserves one 1 GiB arena, and in an arena that
/// large the pages it hands out after a burst of frees are often fresh
/// ones rather than the ones just freed, which stay resident: a program
/// that repeatedly builds and drops a large structure peaked about 9 MB
/// higher (the generator tree benchmark, 46 MB against 37 MB). Arenas of
/// 64 MiB keep reuse tight. (Later arenas still grow geometrically, so a
/// large heap doesn't need many.)
const ARENA_RESERVE_KIB: c_long = 64 * 1024;

/// mimalloc 3's `mi_option_arena_max_object_size` (its position in
/// `mi_option_t`).
#[cfg(not(target_os = "linux"))]
const MI_OPTION_ARENA_MAX_OBJECT_SIZE: c_int = 45;

/// The largest page or object mimalloc places in its arenas, in KiB,
/// unless the environment sets `MIMALLOC_ARENA_MAX_OBJECT_SIZE`: larger
/// ones (the 4 MiB pages that hold blocks of 84 to 512 KiB, and objects
/// over 2 MiB) are mapped from the OS and unmapped when freed.
///
/// An arena places a 4 MiB page only in a fully free, aligned 4 MiB range.
/// Once small pages are scattered through the arena, freed ranges rarely
/// qualify, so each new such page (a compile's register-allocator tables,
/// a growing list's buffer) took never-touched space while the ranges
/// freed before stayed resident: a program that rebuilds a large list of
/// objects (`float_points`) peaked at 59 MB against a 30 MB heap. Mapped
/// from the OS, a freed page leaves the resident set at once: peak RSS
/// fell about 2.7% over the workload suite (`float_points` 20%, `go` 6%), though
/// `async_tree`, which keeps many such pages partly used, rose 10%.
///
/// Not on Linux: there mimalloc's arenas are backed by transparent huge
/// pages (`madvise` mode, the distributions' default), but memory mapped
/// for one object isn't, so a large buffer faults in 4 KiB at a time: a
/// program growing and dropping large deques took eight times the page
/// faults and ran 6% to 25% slower (the bench gate's `deque_ops`).
#[cfg(not(target_os = "linux"))]
const ARENA_MAX_OBJECT_KIB: c_long = 2 * 1024;

/// The size of the first arena on Linux: mimalloc's smallest (see
/// [`reserve_lean_arena`]).
#[cfg(target_os = "linux")]
const LEAN_ARENA: libc::size_t = 32 << 20;

/// Give mimalloc a first arena on Linux that doesn't use transparent huge
/// pages, unless the environment sets `MIMALLOC_ALLOW_THP`.
///
/// mimalloc advises huge pages for its arenas, and the kernel then backs
/// the first touch of each aligned 2 MiB range with a whole page, though
/// the heap uses a fraction of it. Without them a small program's peak
/// halves (`deltablue` 62 to 33 MB, `str_methods` 50 to 25 MB, `startup` 30
/// to 20 MB) at the same speed. Large heaps do gain from huge pages (a
/// program growing large deques ran 11% slower without them, with ten
/// times the page faults), and they keep them: once this arena is full,
/// mimalloc reserves its own arenas, with its usual advice.
///
/// `madvise` applies to this mapping alone, where `prctl`'s
/// `PR_SET_THP_DISABLE`, which is how mimalloc's `allow_thp` option turns
/// them off, would be inherited by every child process.
#[cfg(target_os = "linux")]
fn reserve_lean_arena() {
    // (One slice more, as mimalloc aligns the start to its 64 KiB slices.)
    let size = LEAN_ARENA + (64 << 10);
    // SAFETY: an anonymous mapping of our own, handed over to mimalloc for
    // the rest of the process; `getenv` with a NUL-terminated name.
    unsafe {
        if !libc::getenv(c"MIMALLOC_ALLOW_THP".as_ptr()).is_null() {
            return;
        }
        let start = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        );
        if start == libc::MAP_FAILED {
            return;
        }
        libc::madvise(start, size, libc::MADV_NOHUGEPAGE);
        if !mi_manage_os_memory(start, size, true, false, true, -1) {
            libc::munmap(start, size);
        }
    }
}

/// Configure the allocator before its first allocation (mimalloc reads an
/// option when it first needs it; the first arena is reserved by the first
/// allocation). Runs from the executable's initializer list, before
/// `main`, so it must not allocate.
pub extern "C" fn configure() {
    // SAFETY: `getenv` with a NUL-terminated name; no other thread runs
    // yet. `mi_option_set` only stores the value.
    unsafe {
        if libc::getenv(c"MIMALLOC_ARENA_RESERVE".as_ptr()).is_null() {
            mi_option_set(MI_OPTION_ARENA_RESERVE, ARENA_RESERVE_KIB);
        }
        #[cfg(not(target_os = "linux"))]
        if libc::getenv(c"MIMALLOC_ARENA_MAX_OBJECT_SIZE".as_ptr()).is_null() {
            mi_option_set(MI_OPTION_ARENA_MAX_OBJECT_SIZE, ARENA_MAX_OBJECT_KIB);
        }
    }
    #[cfg(target_os = "linux")]
    reserve_lean_arena();
}
