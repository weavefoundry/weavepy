//! Opt-in allocation-site profiler (`--features alloc-profile`, macOS).
//!
//! The global allocator samples one allocation per [`SAMPLE`] bytes and
//! records its call stack. A sampled block is charged [`SAMPLE`] bytes to
//! its stack while it lives, so at exit the table estimates the *live* heap
//! by allocation site. With `WEAVEPY_ALLOC_PROFILE=<file>` set, the live
//! samples are written to `<file>`: one line per sample, the charged bytes,
//! the block's own size (`s<bytes>`), then the slide-adjusted return
//! addresses (resolve them with `atos`).
//! `WEAVEPY_ALLOC_SAMPLE=<bytes>` changes the sampling interval, and the
//! file's first line (starting `#`) gives the exact live and peak heap
//! totals, which tell transient allocations apart from retained ones.
//! `<file>.peak` holds the samples live when the heap last grew past a
//! new high-water mark (within about 6% of the peak), in the same form.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Default bytes between samples.
const SAMPLE: usize = 16 * 1024;

/// Bytes between samples (`WEAVEPY_ALLOC_SAMPLE`, else [`SAMPLE`]).
static INTERVAL: AtomicUsize = AtomicUsize::new(SAMPLE);
/// Exact bytes currently allocated, and the most ever allocated at once.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// The live total past which the next peak snapshot is taken.
static SNAP_AT: AtomicUsize = AtomicUsize::new(1 << 20);
/// The peak snapshot: a mapping of `SLOTS` samples, and how many it holds.
static SNAP: AtomicUsize = AtomicUsize::new(0);
static SNAP_LEN: AtomicUsize = AtomicUsize::new(0);
static SNAP_LIVE: AtomicUsize = AtomicUsize::new(0);
/// Return addresses kept per sample.
const DEPTH: usize = 24;
/// Sampled blocks tracked at once (open addressing, power of two).
const SLOTS: usize = 1 << 18;

struct Sample {
    ptr: usize,
    /// The sampled block's own size.
    size: usize,
    stack: [usize; DEPTH],
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static LOCK: AtomicBool = AtomicBool::new(false);
static TABLE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
    static UNTIL_SAMPLE: Cell<usize> = const { Cell::new(SAMPLE) };
    static RNG: Cell<u64> = const { Cell::new(0x9E37_79B9_7F4A_7C15) };
}

extern "C" {
    fn _dyld_get_image_vmaddr_slide(image_index: u32) -> isize;
}

fn lock() {
    while LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        std::hint::spin_loop();
    }
}

fn unlock() {
    LOCK.store(false, Ordering::Release);
}

fn table() -> *mut Sample {
    TABLE.load(Ordering::Acquire) as *mut Sample
}

fn slot_of(ptr: usize) -> usize {
    (ptr.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 20) & (SLOTS - 1)
}

/// Start sampling (allocates the table from the system allocator).
pub(crate) fn start() {
    if std::env::var_os("WEAVEPY_ALLOC_PROFILE").is_none() {
        return;
    }
    if let Some(n) = std::env::var("WEAVEPY_ALLOC_SAMPLE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        INTERVAL.store(n, Ordering::Relaxed);
    }
    let (Some(p), Some(snap)) = (map_samples(), map_samples()) else {
        return;
    };
    SNAP.store(snap, Ordering::Release);
    TABLE.store(p, Ordering::Release);
    ENABLED.store(true, Ordering::Release);
}

/// A fresh zeroed mapping of `SLOTS` samples, owned for the rest of the
/// process.
fn map_samples() -> Option<usize> {
    let bytes = SLOTS * std::mem::size_of::<Sample>();
    // SAFETY: an anonymous private mapping; nothing else aliases it.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            bytes,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    (p != libc::MAP_FAILED).then_some(p as usize)
}

/// Copy the live samples into the peak snapshot.
fn snapshot(live: usize) {
    let (t, snap) = (table(), SNAP.load(Ordering::Acquire) as *mut Sample);
    lock();
    let mut n = 0;
    for i in 0..SLOTS {
        // SAFETY: `i < SLOTS` and `n <= i`, inside both mappings.
        let s = unsafe { &*t.add(i) };
        if s.ptr != 0 && s.ptr != usize::MAX {
            unsafe {
                (*snap.add(n)).ptr = s.ptr;
                (*snap.add(n)).size = s.size;
                (*snap.add(n)).stack = s.stack;
            }
            n += 1;
        }
    }
    SNAP_LEN.store(n, Ordering::Relaxed);
    SNAP_LIVE.store(live, Ordering::Relaxed);
    unlock();
}

fn record(ptr: usize, size: usize) {
    let mut stack = [0usize; DEPTH];
    // SAFETY: `backtrace` fills at most `DEPTH` entries of the buffer.
    let n = unsafe { libc::backtrace(stack.as_mut_ptr().cast(), DEPTH as libc::c_int) };
    let _ = n;
    let t = table();
    lock();
    let mut i = slot_of(ptr);
    for _ in 0..SLOTS {
        // SAFETY: `i < SLOTS`, inside the mapping.
        let s = unsafe { &mut *t.add(i) };
        // A fresh block's address was forgotten when it was last freed, so
        // a tombstone can take it (a long run would fill the table otherwise).
        if s.ptr == 0 || s.ptr == ptr || s.ptr == usize::MAX {
            s.ptr = ptr;
            s.size = size;
            s.stack = stack;
            break;
        }
        i = (i + 1) & (SLOTS - 1);
    }
    unlock();
}

fn forget(ptr: usize) {
    let t = table();
    lock();
    let mut i = slot_of(ptr);
    for _ in 0..SLOTS {
        // SAFETY: as in `record`.
        let s = unsafe { &mut *t.add(i) };
        if s.ptr == 0 {
            break;
        }
        if s.ptr == ptr {
            // A tombstone keeps later probes of this chain reachable.
            s.ptr = usize::MAX;
            break;
        }
        i = (i + 1) & (SLOTS - 1);
    }
    unlock();
}

/// The profiling allocator: mimalloc plus the sampler.
pub(crate) struct Profiled;

// SAFETY: every allocation is mimalloc's; the sampler only records
// addresses and never touches the blocks.
unsafe impl GlobalAlloc for Profiled {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged.
        let p = unsafe { crate::alloc::Mimalloc.alloc(layout) };
        if ENABLED.load(Ordering::Relaxed) && !p.is_null() {
            grow(layout.size());
            sample(p as usize, layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ENABLED.load(Ordering::Relaxed) {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            forget(ptr as usize);
        }
        // SAFETY: forwarded unchanged.
        unsafe { crate::alloc::Mimalloc.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            forget(ptr as usize);
        }
        // SAFETY: forwarded unchanged.
        let p = unsafe { crate::alloc::Mimalloc.realloc(ptr, layout, new_size) };
        if ENABLED.load(Ordering::Relaxed) && !p.is_null() {
            grow(new_size);
            sample(p as usize, new_size);
        }
        p
    }
}

/// Count `size` more live bytes (blocks allocated before [`start`] are
/// never counted, so their frees may briefly wrap the counter below zero).
fn grow(size: usize) {
    let live = LIVE.fetch_add(size, Ordering::Relaxed).wrapping_add(size);
    if live < usize::MAX / 2 && live > PEAK.load(Ordering::Relaxed) {
        PEAK.fetch_max(live, Ordering::Relaxed);
        let at = SNAP_AT.load(Ordering::Relaxed);
        if live > at
            && SNAP_AT
                .compare_exchange(at, live + live / 16, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            snapshot(live);
        }
    }
}

fn sample(ptr: usize, size: usize) {
    let _ = IN_HOOK.try_with(|hook| {
        if hook.get() {
            return;
        }
        let due = UNTIL_SAMPLE.with(|left| {
            let l = left.get();
            if size >= l {
                // A uniformly random gap with the interval as its mean: a
                // fixed gap aliases with a loop that allocates the same
                // bytes per iteration, charging one site for all of them.
                let r = RNG.with(|rng| {
                    let mut x = rng.get();
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    rng.set(x);
                    x
                });
                let interval = INTERVAL.load(Ordering::Relaxed);
                left.set(1 + (r as usize) % (2 * interval));
                true
            } else {
                left.set(l - size);
                false
            }
        });
        if due {
            hook.set(true);
            record(ptr, size);
            hook.set(false);
        }
    });
}

/// Write the live samples (see the module docs).
pub(crate) fn finish() {
    let Some(path) = std::env::var_os("WEAVEPY_ALLOC_PROFILE") else {
        return;
    };
    if !ENABLED.swap(false, Ordering::AcqRel) {
        return;
    }
    let header = format!(
        "# live {} peak {}\n",
        LIVE.load(Ordering::Relaxed) as isize,
        PEAK.load(Ordering::Relaxed)
    );
    lock();
    let out = render(header, table(), SLOTS);
    unlock();
    let _ = std::fs::write(&path, out);
    let header = format!("# snapshot live {}\n", SNAP_LIVE.load(Ordering::Relaxed));
    let snap = render(
        header,
        SNAP.load(Ordering::Acquire) as *mut Sample,
        SNAP_LEN.load(Ordering::Relaxed),
    );
    let mut peak_path = path;
    peak_path.push(".peak");
    let _ = std::fs::write(peak_path, snap);
}

/// The occupied samples among the first `n` of `t`, one line each after
/// `header`.
fn render(header: String, t: *mut Sample, n: usize) -> String {
    // SAFETY: the main executable is image 0.
    let slide = unsafe { _dyld_get_image_vmaddr_slide(0) } as usize;
    let interval = INTERVAL.load(Ordering::Relaxed);
    let mut out = header;
    for i in 0..n {
        // SAFETY: `i < n <= SLOTS`.
        let s = unsafe { &*t.add(i) };
        if s.ptr == 0 || s.ptr == usize::MAX {
            continue;
        }
        out.push_str(&format!("{interval} s{}", s.size));
        for &a in s.stack.iter().take_while(|a| **a != 0) {
            out.push_str(&format!(" {:x}", a.wrapping_sub(slide)));
        }
        out.push('\n');
    }
    out
}
