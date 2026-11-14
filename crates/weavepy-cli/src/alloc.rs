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
        let p: *mut u8 = unsafe {
            if plain(layout.size(), layout.align()) {
                mi_malloc(layout.size()).cast()
            } else {
                mi_malloc_aligned(layout.size(), layout.align()).cast()
            }
        };
        p
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as in `alloc`.
        let p: *mut u8 = unsafe {
            if plain(layout.size(), layout.align()) {
                mi_zalloc(layout.size()).cast()
            } else {
                mi_zalloc_aligned(layout.size(), layout.align()).cast()
            }
        };
        p
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        // SAFETY: `ptr` came from this allocator (the caller's contract).
        unsafe { mi_free(ptr.cast::<c_void>()) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` came from this allocator with `layout`.
        let p: *mut u8 = unsafe {
            if plain(new_size, layout.align()) {
                mi_realloc(ptr.cast::<c_void>(), new_size).cast()
            } else {
                mi_realloc_aligned(ptr.cast::<c_void>(), new_size, layout.align()).cast()
            }
        };
        p
    }
}

extern "C" {
    fn mi_option_set(option: c_int, value: c_long);
    fn mi_collect(force: bool);
    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    fn mi_manage_memory(
        start: *mut c_void,
        size: libc::size_t,
        is_committed: bool,
        is_pinned: bool,
        is_zero: bool,
        numa_node: c_int,
        exclusive: bool,
        commit_fun: Option<CommitFn>,
        commit_fun_arg: *mut c_void,
        arena_id: *mut *mut c_void,
    ) -> bool;
}

/// mimalloc 3's `mi_option_purge_delay` (its position in `mi_option_t`).
#[cfg(target_vendor = "apple")]
const MI_OPTION_PURGE_DELAY: c_int = 15;

/// How long a free range of an arena stays resident on macOS, in
/// milliseconds, unless the environment sets `MIMALLOC_PURGE_DELAY`.
///
/// mimalloc's default of a second is about a whole run of a short program, so
/// a heap that repeatedly grows and shrinks (`async_tree`, `float_points`)
/// peaks as if it never freed anything: it re-used ranges less readily than it
/// took fresh ones.
#[cfg(target_vendor = "apple")]
const PURGE_DELAY_MS: c_long = 50;

/// The resident set size past which a free range of an arena stays resident
/// for [`LARGE_PURGE_DELAY_MS`] instead (see [`note_resident`]).
#[cfg(target_vendor = "apple")]
const LARGE_HEAP: usize = 32 << 20;

/// How long a free range stays resident once the process is large: a
/// program that builds and drops tens of megabytes at a time takes longer
/// than [`PURGE_DELAY_MS`] over each round, so every round's memory was
/// purged and faulted back in, page by page, for the next (`async_tree`: a
/// third more page faults, and 7% more cycles). The few megabytes a short
/// delay saves are a small part of such a process.
#[cfg(target_vendor = "apple")]
const LARGE_PURGE_DELAY_MS: c_long = 500;

/// Whether [`configure`] set the purge delay (the environment didn't).
#[cfg(target_vendor = "apple")]
static PURGE_DELAY_OURS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The process's resident set is `resident` bytes: past [`LARGE_HEAP`], the
/// arenas keep their free ranges resident longer from here on.
pub(crate) fn note_resident(resident: usize) {
    #[cfg(target_vendor = "apple")]
    if resident >= LARGE_HEAP && PURGE_DELAY_OURS.swap(false, std::sync::atomic::Ordering::Relaxed)
    {
        // SAFETY: only stores the value.
        unsafe { mi_option_set(MI_OPTION_PURGE_DELAY, LARGE_PURGE_DELAY_MS) };
    }
    #[cfg(not(target_vendor = "apple"))]
    let _ = resident;
}

/// mimalloc 3's `mi_option_arena_reserve` (its position in `mi_option_t`).
const MI_OPTION_ARENA_RESERVE: c_int = 23;

/// mimalloc 3's `mi_option_page_full_retain` (its position in
/// `mi_option_t`).
const MI_OPTION_PAGE_FULL_RETAIN: c_int = 36;

/// How many full pages of a small size class stay in that class's queue of
/// pages to allocate from, unless the environment sets
/// `MIMALLOC_PAGE_FULL_RETAIN`.
///
/// mimalloc keeps a page that empties only when it's the only page in its
/// queue; otherwise it frees it. With its default of two full pages kept in
/// the queue, the page a burst of short-lived objects spills into is never
/// alone there: each time the burst's objects die it's freed, and the next
/// burst sets up a fresh one, over and over. Whether a workload's bursts
/// cross a page boundary depends on where its long-lived objects happen to
/// sit, so the cost came and went with the environment's size: `nqueens`
/// set up 127,000 pages instead of 141 at one environment size in about
/// twenty on macOS, and every warm iteration took 160 ms instead of 95.
/// Full pages moved out of the queue leave the emptied page alone in it,
/// kept for the next burst.
const PAGE_FULL_RETAIN: c_long = 0;

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

/// The part of the first arena on Linux without huge pages: about what a small
/// program's heap takes (see [`reserve_lean_arena`]).
#[cfg(target_os = "linux")]
const LEAN_ARENA_SMALL: libc::size_t = 16 << 20;

/// Give mimalloc a first arena on Linux that starts without transparent huge
/// pages, unless the environment sets `MIMALLOC_ALLOW_THP`.
///
/// mimalloc advises huge pages for its arenas, and the kernel then backs
/// the first touch of each aligned 2 MiB range with a whole page, though
/// the heap uses a fraction of it. Without them a small program's peak
/// halves (`deltablue` 62 to 33 MB, `str_methods` 50 to 25 MB, `startup` 30
/// to 20 MB) at the same speed. Large heaps do gain from huge pages (a
/// program growing large deques ran 11% slower without them, with ten
/// times the page faults), and they keep them: past [`LEAN_ARENA_SMALL`], and in
/// the arenas mimalloc reserves once this one is full, with its usual advice.
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
        libc::madvise(start, LEAN_ARENA_SMALL, libc::MADV_NOHUGEPAGE);
        libc::madvise(
            start.cast::<u8>().add(LEAN_ARENA_SMALL).cast(),
            size - LEAN_ARENA_SMALL,
            libc::MADV_HUGEPAGE,
        );
        if !manage_first_arena(start, size) {
            libc::munmap(start, size);
        }
    }
}

/// The size of the first arena on macOS, which is what mimalloc reserves by
/// default here.
#[cfg(target_vendor = "apple")]
const FIRST_ARENA: libc::size_t = ARENA_RESERVE_KIB as libc::size_t * 1024;

/// mimalloc's tag for its mappings (`mi_option_os_tag`), as `vm_stat` and
/// `vmmap` show it.
#[cfg(target_vendor = "apple")]
const OS_TAG: libc::c_int = 100 << 24;

/// Give mimalloc a first arena on macOS (see [`commit_arena_range`]).
#[cfg(target_vendor = "apple")]
fn reserve_first_arena() {
    // (One slice more, as mimalloc aligns the start to its 64 KiB slices.)
    let size = FIRST_ARENA + (64 << 10);
    // SAFETY: an anonymous mapping of our own, handed over to mimalloc for
    // the rest of the process.
    unsafe {
        let start = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            OS_TAG,
            0,
        );
        if start == libc::MAP_FAILED {
            return;
        }
        if !manage_first_arena(start, size) {
            libc::munmap(start, size);
        }
    }
}

/// Hand `start..start + size` to mimalloc as the interpreter's first arena.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
unsafe fn manage_first_arena(start: *mut c_void, size: usize) -> bool {
    // SAFETY: the caller's mapping, handed over for the rest of the process.
    unsafe {
        mi_manage_memory(
            start,
            size,
            true,
            false,
            true,
            -1,
            false,
            Some(commit_arena_range),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}

/// Commit or decommit a range of an arena of ours.
///
/// mimalloc decommits with `madvise(MADV_FREE_REUSABLE)` on macOS. That takes
/// the pages off the process's footprint, but they stay resident until the
/// system needs them, so a purged page still counts in the resident set.
/// Mapping fresh pages over the range releases the old ones at once, as
/// Linux's `MADV_DONTNEED` does.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
unsafe extern "C" fn commit_arena_range(
    commit: bool,
    start: *mut c_void,
    size: usize,
    is_zero: *mut bool,
    _arg: *mut c_void,
) -> bool {
    if commit {
        // Nothing to do, as decommitted ranges stay mapped; and a committed
        // range isn't known to be zero.
        if !is_zero.is_null() {
            // SAFETY: mimalloc passes a valid pointer, or null.
            unsafe { *is_zero = false };
        }
        return true;
    }
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: mimalloc owns the range and has no live blocks in it.
        let p = unsafe {
            libc::mmap(
                start,
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
                OS_TAG,
                0,
            )
        };
        // (If the mapping failed the range keeps its pages, and stays usable:
        // report it as not decommitted.)
        p == start
    }
    #[cfg(target_os = "linux")]
    // SAFETY: as above.
    unsafe {
        libc::madvise(start, size, libc::MADV_DONTNEED) == 0
    }
}

/// mimalloc's arena commit hook.
#[cfg(unix)]
type CommitFn = unsafe extern "C" fn(bool, *mut c_void, usize, *mut bool, *mut c_void) -> bool;

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
        if libc::getenv(c"MIMALLOC_PAGE_FULL_RETAIN".as_ptr()).is_null() {
            mi_option_set(MI_OPTION_PAGE_FULL_RETAIN, PAGE_FULL_RETAIN);
        }
        #[cfg(not(target_os = "linux"))]
        if libc::getenv(c"MIMALLOC_ARENA_MAX_OBJECT_SIZE".as_ptr()).is_null() {
            mi_option_set(MI_OPTION_ARENA_MAX_OBJECT_SIZE, ARENA_MAX_OBJECT_KIB);
        }
        #[cfg(target_vendor = "apple")]
        if libc::getenv(c"MIMALLOC_PURGE_DELAY".as_ptr()).is_null() {
            mi_option_set(MI_OPTION_PURGE_DELAY, PURGE_DELAY_MS);
            PURGE_DELAY_OURS.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    #[cfg(target_os = "linux")]
    reserve_lean_arena();
    #[cfg(target_vendor = "apple")]
    reserve_first_arena();
    weavepy_vm::set_jit_worker_idle_hook(collect_thread_heap);
}

/// Hand the calling thread's free pages back (the JIT worker's, between
/// compiles): another thread's allocations can then take them. The compiles
/// are also code the process has just finished running (see `memory`).
extern "C" fn collect_thread_heap() {
    // SAFETY: collects the calling thread's own heap.
    unsafe { mi_collect(true) };
    crate::memory::tick();
}
