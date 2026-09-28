//! Opt-in allocation-site profiler (`--features alloc-profile`, macOS).
//!
//! The global allocator samples one allocation per [`SAMPLE`] bytes and
//! records its call stack. A sampled block is charged [`SAMPLE`] bytes to
//! its stack while it lives, so at exit the table estimates the *live* heap
//! by allocation site. With `WEAVEPY_ALLOC_PROFILE=<file>` set, the live
//! samples are written to `<file>`: one line per sample, the charged bytes
//! then the slide-adjusted return addresses (resolve them with `atos`).

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Bytes between samples.
const SAMPLE: usize = 16 * 1024;
/// Return addresses kept per sample.
const DEPTH: usize = 24;
/// Sampled blocks tracked at once (open addressing, power of two).
const SLOTS: usize = 1 << 18;

struct Sample {
    ptr: usize,
    stack: [usize; DEPTH],
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static LOCK: AtomicBool = AtomicBool::new(false);
static TABLE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
    static UNTIL_SAMPLE: Cell<usize> = const { Cell::new(SAMPLE) };
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
    let bytes = SLOTS * std::mem::size_of::<Sample>();
    // SAFETY: a fresh zeroed mapping owned for the rest of the process.
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
    if p == libc::MAP_FAILED {
        return;
    }
    TABLE.store(p as usize, Ordering::Release);
    ENABLED.store(true, Ordering::Release);
}

fn record(ptr: usize) {
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
        if s.ptr == 0 || s.ptr == ptr {
            s.ptr = ptr;
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
        let p = unsafe { mimalloc::MiMalloc.alloc(layout) };
        if ENABLED.load(Ordering::Relaxed) && !p.is_null() {
            sample(p as usize, layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ENABLED.load(Ordering::Relaxed) {
            forget(ptr as usize);
        }
        // SAFETY: forwarded unchanged.
        unsafe { mimalloc::MiMalloc.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            forget(ptr as usize);
        }
        // SAFETY: forwarded unchanged.
        let p = unsafe { mimalloc::MiMalloc.realloc(ptr, layout, new_size) };
        if ENABLED.load(Ordering::Relaxed) && !p.is_null() {
            sample(p as usize, new_size);
        }
        p
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
                left.set(SAMPLE);
                true
            } else {
                left.set(l - size);
                false
            }
        });
        if due {
            hook.set(true);
            record(ptr);
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
    let t = table();
    // SAFETY: the main executable is image 0.
    let slide = unsafe { _dyld_get_image_vmaddr_slide(0) } as usize;
    let mut out = String::new();
    lock();
    for i in 0..SLOTS {
        // SAFETY: `i < SLOTS`.
        let s = unsafe { &*t.add(i) };
        if s.ptr == 0 || s.ptr == usize::MAX {
            continue;
        }
        out.push_str(&SAMPLE.to_string());
        for &a in s.stack.iter().take_while(|a| **a != 0) {
            out.push_str(&format!(" {:x}", a.wrapping_sub(slide)));
        }
        out.push('\n');
    }
    unlock();
    let _ = std::fs::write(path, out);
}
