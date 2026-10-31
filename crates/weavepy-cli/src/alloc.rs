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
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

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
        if p.is_null() {
            return outside_compile_arena(layout, false);
        }
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
        if p.is_null() {
            return outside_compile_arena(layout, true);
        }
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
        if p.is_null() {
            // SAFETY: the caller's contract, which makes the new layout valid.
            return unsafe { realloc_outside_compile_arena(ptr, layout, new_size) };
        }
        p
    }
}

/// Allocate from the heap for compile memory that doesn't fit the compile
/// arena, when the thread's default heap declined.
///
/// The compile heap allocates only in its own arena, so it declines what an
/// arena can't hold (see `ARENA_MAX_OBJECT_KIB`) and what no longer fits.
#[cold]
#[inline(never)]
fn outside_compile_arena(layout: Layout, zeroed: bool) -> *mut u8 {
    // SAFETY: plain FFI allocation calls. (No compile heap means this is a
    // request the interpreter's heap failed: it can try once more.)
    unsafe {
        let mut heap = COMPILE_LARGE_HEAP.load(Ordering::Acquire);
        if heap.is_null() {
            heap = mi_heap_main();
        }
        if zeroed {
            mi_heap_zalloc_aligned(heap, layout.size(), layout.align()).cast()
        } else {
            mi_heap_malloc_aligned(heap, layout.size(), layout.align()).cast()
        }
    }
}

/// [`GlobalAlloc::realloc`] for a block the compile heap declined to resize.
///
/// # Safety
///
/// As [`GlobalAlloc::realloc`].
#[cold]
#[inline(never)]
unsafe fn realloc_outside_compile_arena(ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    // SAFETY: the caller's contract makes the layout valid and the block live.
    unsafe {
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        let fresh = outside_compile_arena(new_layout, false);
        if !fresh.is_null() {
            std::ptr::copy_nonoverlapping(ptr, fresh, layout.size().min(new_size));
            mi_free(ptr.cast::<c_void>());
        }
        fresh
    }
}

extern "C" {
    fn mi_option_set(option: c_int, value: c_long);
    fn mi_collect(force: bool);
    fn mi_heap_main() -> *mut c_void;
    fn mi_heap_malloc_aligned(heap: *mut c_void, size: usize, alignment: usize) -> *mut c_void;
    fn mi_heap_zalloc_aligned(heap: *mut c_void, size: usize, alignment: usize) -> *mut c_void;
    fn mi_heap_new() -> *mut c_void;
    #[cfg(unix)]
    fn mi_heap_new_in_arena(arena_id: *mut c_void) -> *mut c_void;
    fn mi_heap_delete(heap: *mut c_void);
    fn mi_heap_collect(heap: *mut c_void, force: bool);
    #[cfg(unix)]
    fn mi_heap_theap(heap: *mut c_void) -> *mut c_void;
    #[cfg(unix)]
    fn mi_theap_get_default() -> *mut c_void;
    /// Internal: mimalloc 3's header declares `mi_theap_set_default`, but
    /// no source defines it, and this is what it would call. (MSVC builds
    /// mimalloc as C++, which mangles this name; the compile arena that
    /// needs it is Unix's alone.)
    #[cfg(unix)]
    fn _mi_theap_default_set(theap: *mut c_void);
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

/// Give mimalloc a first arena on macOS (see [`commit_first_arena_range`]).
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
            Some(commit_first_arena_range),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }
}

/// How many threads are purging compile memory (see [`purge_compile_memory`]).
static PURGING_COMPILE_MEMORY: AtomicUsize = AtomicUsize::new(0);

/// Commit or decommit a range of the interpreter's first arena.
///
/// A purge forced after a compile takes every arena's free ranges, but it's
/// meant for the compile arena: the interpreter's heap may want its free ranges
/// again in a moment, and they'd fault back in a page at a time. (A program
/// that compiles while it churns through pages, as `stdlib_utils` does, ran 30%
/// slower on Linux.) So this declines those purges, and the arena's ranges
/// leave on mimalloc's usual delay.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
unsafe extern "C" fn commit_first_arena_range(
    commit: bool,
    start: *mut c_void,
    size: usize,
    is_zero: *mut bool,
    arg: *mut c_void,
) -> bool {
    if !commit && PURGING_COMPILE_MEMORY.load(Ordering::Relaxed) != 0 {
        return false;
    }
    // SAFETY: the caller's contract.
    unsafe { commit_arena_range(commit, start, size, is_zero, arg) }
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

/// The size of the compile arena (see [`compile_heap`]), which holds a few
/// megabytes at a time.
#[cfg(unix)]
const COMPILE_ARENA: libc::size_t = 64 << 20;

/// mimalloc's `mi_commit_fun_t`.
#[cfg(unix)]
type CommitFn = unsafe extern "C" fn(bool, *mut c_void, usize, *mut bool, *mut c_void) -> bool;

/// A new arena for compile memory only; `None` if it can't be had.
#[cfg(unix)]
fn reserve_compile_arena() -> Option<*mut c_void> {
    // (One slice more, as mimalloc aligns the start to its 64 KiB slices.)
    let size = COMPILE_ARENA + (64 << 10);
    #[cfg(target_vendor = "apple")]
    let tag = OS_TAG;
    #[cfg(not(target_vendor = "apple"))]
    let tag = -1;
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    let commit: Option<CommitFn> = Some(commit_arena_range);
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    let commit: Option<CommitFn> = None;
    let mut arena = std::ptr::null_mut();
    // SAFETY: an anonymous mapping of our own, handed over to mimalloc for
    // the rest of the process.
    unsafe {
        let start = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            tag,
            0,
        );
        if start == libc::MAP_FAILED {
            return None;
        }
        #[cfg(target_os = "linux")]
        libc::madvise(start, size, libc::MADV_NOHUGEPAGE);
        if !mi_manage_memory(
            start,
            size,
            true,
            false,
            true,
            -1,
            true,
            commit,
            std::ptr::null_mut(),
            &raw mut arena,
        ) {
            libc::munmap(start, size);
            return None;
        }
    }
    Some(arena)
}

/// A heap for compile memory (see `weavepy::vm::malloc_stats::CompileHeap`),
/// in an arena of its own, so that a purge after a compile takes only compile
/// memory.
fn new_compile_heap() -> *mut c_void {
    #[cfg(unix)]
    if let Some(arena) = reserve_compile_arena() {
        // SAFETY: a plain FFI call with an arena just made.
        let heap = unsafe { mi_heap_new_in_arena(arena) };
        if !heap.is_null() {
            return heap;
        }
    }
    // SAFETY: a plain FFI call.
    unsafe { mi_heap_new() }
}

/// The heap of compile memory, made on first use, and the heap for what it
/// declines (see [`outside_compile_arena`]).
static COMPILE_HEAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static COMPILE_LARGE_HEAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

fn compile_heap() -> *mut c_void {
    let heap = COMPILE_HEAP.load(Ordering::Acquire);
    if !heap.is_null() {
        return heap;
    }
    // SAFETY: a plain FFI call. (A heap that loses the race goes unused.)
    let large = unsafe { mi_heap_new() };
    let _ = COMPILE_LARGE_HEAP.compare_exchange(
        std::ptr::null_mut(),
        large,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
    let fresh = new_compile_heap();
    match COMPILE_HEAP.compare_exchange(
        std::ptr::null_mut(),
        fresh,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => fresh,
        Err(won) => {
            // SAFETY: a heap nothing allocated from. (Its arena goes unused.)
            unsafe { mi_heap_delete(fresh) };
            won
        }
    }
}

/// Make this thread allocate from the compile heap; returns its previous
/// default for [`restore_heap`].
#[cfg(unix)]
pub(crate) fn enter_compile_heap() -> *mut c_void {
    // SAFETY: plain FFI calls; the theap is this thread's own.
    unsafe {
        let previous = mi_theap_get_default();
        _mi_theap_default_set(mi_heap_theap(compile_heap()));
        previous
    }
}

/// Compile memory stays in this thread's default heap without a compile
/// arena.
#[cfg(not(unix))]
pub(crate) fn enter_compile_heap() -> *mut c_void {
    std::ptr::null_mut()
}

/// Restore this thread's default heap.
#[cfg(unix)]
pub(crate) fn restore_heap(previous: *mut c_void) {
    // SAFETY: `previous` came from `enter_compile_heap` on this thread.
    unsafe { _mi_theap_default_set(previous) };
}

/// See [`enter_compile_heap`].
#[cfg(not(unix))]
pub(crate) fn restore_heap(_previous: *mut c_void) {}

/// Run a collection that forces a purge, which should release compile memory
/// only (see [`commit_first_arena_range`]).
fn purge_compile_memory(collect: impl FnOnce()) {
    PURGING_COMPILE_MEMORY.fetch_add(1, Ordering::Relaxed);
    collect();
    PURGING_COMPILE_MEMORY.fetch_sub(1, Ordering::Relaxed);
}

/// Return this thread's empty compile pages to the arena.
pub(crate) fn collect_compile_heap() {
    // SAFETY: plain FFI calls; the theaps are this thread's own.
    unsafe {
        mi_heap_collect(COMPILE_LARGE_HEAP.load(Ordering::Acquire), false);
        mi_heap_collect(compile_heap(), false);
    }
}

/// Return free compile memory to the OS, on a thread that does nothing but
/// compile, when it runs out of work.
pub(crate) fn release_free_memory() {
    // SAFETY: plain FFI calls; the theaps are this thread's own.
    purge_compile_memory(|| unsafe {
        mi_heap_collect(COMPILE_LARGE_HEAP.load(Ordering::Acquire), true);
        mi_heap_collect(compile_heap(), true);
        mi_collect(true);
    });
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
        #[cfg(target_vendor = "apple")]
        if libc::getenv(c"MIMALLOC_PURGE_DELAY".as_ptr()).is_null() {
            mi_option_set(MI_OPTION_PURGE_DELAY, PURGE_DELAY_MS);
        }
    }
    #[cfg(target_os = "linux")]
    reserve_lean_arena();
    #[cfg(target_vendor = "apple")]
    reserve_first_arena();
}
