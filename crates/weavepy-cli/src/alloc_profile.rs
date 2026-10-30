//! Opt-in allocation-site profiler (`--features alloc-profile`, macOS).
//!
//! The global allocator samples one allocation per [`SAMPLE`] bytes and
//! records its call stack. A sampled block is charged [`SAMPLE`] bytes to
//! its stack while it lives, so at exit the table estimates the *live* heap
//! by allocation site. With `WEAVEPY_ALLOC_PROFILE=<file>` set, the live
//! samples are written to `<file>`: one line per sample, the charged bytes
//! (the interval, or the block size when that is larger),
//! the block's own size (`s<bytes>`), then the slide-adjusted return
//! addresses (resolve them with `atos`).
//! `WEAVEPY_ALLOC_SAMPLE=<bytes>` changes the sampling interval, and the
//! file's first line (starting `#`) gives the exact live and peak heap
//! totals, which tell transient allocations apart from retained ones.
//! `<file>.peak` holds the samples live when the heap last grew past a
//! new high-water mark (within about 6% of the peak), in the same form.
//! `<file>.vm` holds the process's virtual memory regions at the resident
//! set's high-water mark, as seen from a watcher thread: one line per
//! region, with its tag, resident and dirty pages and mapped file.

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
const SLOTS: usize = 1 << 16;

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
    start_vm_sampler();
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
    if p == libc::MAP_FAILED {
        return None;
    }
    // Touch it all now, so that the table's own pages don't move the
    // resident set the watcher thread follows.
    // SAFETY: the mapping is `bytes` long and writable.
    unsafe { std::ptr::write_bytes(p.cast::<u8>(), 0, bytes) };
    Some(p as usize)
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
            note_alloc(p as usize);
            sample(p as usize, layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ENABLED.load(Ordering::Relaxed) {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            note_free(ptr as usize);
            forget(ptr as usize);
        }
        // SAFETY: forwarded unchanged.
        unsafe { crate::alloc::Mimalloc.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            note_free(ptr as usize);
            forget(ptr as usize);
        }
        // SAFETY: forwarded unchanged.
        let p = unsafe { crate::alloc::Mimalloc.realloc(ptr, layout, new_size) };
        if ENABLED.load(Ordering::Relaxed) && !p.is_null() {
            grow(new_size);
            note_alloc(p as usize);
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
    write_vm(&path);
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
        // A block larger than the mean gap is (nearly) always sampled:
        // charge its own size.
        out.push_str(&format!("{} s{}", interval.max(s.size), s.size));
        for &a in s.stack.iter().take_while(|a| **a != 0) {
            out.push_str(&format!(" {:x}", a.wrapping_sub(slide)));
        }
        out.push('\n');
    }
    out
}

// ---------- the resident set by region, at its peak ----------

/// `vm_region_submap_info_64` (19 words).
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct RegionInfo {
    protection: i32,
    max_protection: i32,
    inheritance: u32,
    offset: u64,
    user_tag: u32,
    pages_resident: u32,
    pages_shared_now_private: u32,
    pages_swapped_out: u32,
    pages_dirtied: u32,
    ref_count: u32,
    shadow_depth: u16,
    external_pager: u8,
    share_mode: u8,
    is_submap: i32,
    behavior: i32,
    object_id: u32,
    user_wired_count: u16,
    pages_reusable: u32,
    object_id_full: u64,
}

#[derive(Clone, Copy)]
struct Region {
    addr: u64,
    size: u64,
    info: RegionInfo,
}

extern "C" {
    fn mach_vm_region_recurse(
        task: libc::mach_port_t,
        address: *mut u64,
        size: *mut u64,
        depth: *mut u32,
        info: *mut RegionInfo,
        count: *mut u32,
    ) -> libc::c_int;
}

const MAX_REGIONS: usize = 4096;
static VM_PEAK: AtomicUsize = AtomicUsize::new(0);
static VM_LEN: AtomicUsize = AtomicUsize::new(0);
static VM_LIVE: AtomicUsize = AtomicUsize::new(0);
static VM_BUF: AtomicUsize = AtomicUsize::new(0);

/// `(resident, phys_footprint)` of this task.
fn resident() -> (u64, u64) {
    // `task_vm_info` starts: virtual_size u64, region_count i32, page_size
    // i32, resident_size u64, resident_size_peak u64, device.. (6 u64s up to
    // internal..) and phys_footprint is the 19th word pair. Ask for the
    // whole structure as raw words.
    let mut words = [0u32; 128];
    let mut count: libc::mach_msg_type_number_t = 128;
    // SAFETY: `TASK_VM_INFO` (22) fills at most `count` words of `words`.
    let kr = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            22,
            words.as_mut_ptr().cast(),
            &raw mut count,
        )
    };
    if kr != 0 {
        return (0, 0);
    }
    let w = |i: usize| u64::from(words[i]) | (u64::from(words[i + 1]) << 32);
    // virtual_size 0, region_count 2, page_size 3, resident_size 4,
    // resident_size_peak 6, device 8, device_peak 10, internal 12,
    // internal_peak 14, external 16, external_peak 18, reusable 20,
    // reusable_peak 22, purgeable_volatile_pmap 24, _resident 26,
    // _virtual 28, compressed 30, compressed_peak 32, compressed_lifetime 34,
    // phys_footprint 36.
    (w(4), w(36))
}

/// The regions, into `buf`; how many.
fn walk(buf: *mut Region) -> usize {
    let mut addr = 0u64;
    let mut depth = 0u32;
    let mut n = 0;
    while n < MAX_REGIONS {
        let mut size = 0u64;
        let mut count = 19u32;
        // SAFETY: `RegionInfo` has room for 19 words; `mach_task_self` is
        // this task, so no task port rights are needed.
        let mut info: RegionInfo = unsafe { std::mem::zeroed() };
        let kr = unsafe {
            mach_vm_region_recurse(
                libc::mach_task_self(),
                &raw mut addr,
                &raw mut size,
                &raw mut depth,
                &raw mut info,
                &raw mut count,
            )
        };
        if kr != 0 {
            break;
        }
        if info.is_submap != 0 {
            depth += 1;
            continue;
        }
        // SAFETY: `n < MAX_REGIONS`, inside the mapping.
        unsafe { *buf.add(n) = Region { addr, size, info } };
        n += 1;
        addr += size;
    }
    n
}

/// Watch the resident set from a thread that allocates nothing, and keep
/// the region map from each new high-water mark.
pub(crate) fn start_vm_sampler() {
    let Some(buf) = map_bytes(MAX_REGIONS * std::mem::size_of::<Region>()) else {
        return;
    };
    let Some(scratch) = map_bytes(MAX_REGIONS * std::mem::size_of::<Region>()) else {
        return;
    };
    VM_BUF.store(buf, Ordering::Release);
    extern "C" fn watch(arg: *mut libc::c_void) -> *mut libc::c_void {
        let scratch = arg.cast::<Region>();
        let buf = VM_BUF.load(Ordering::Acquire) as *mut Region;
        loop {
            let (res, _) = resident();
            if res as usize > VM_PEAK.load(Ordering::Relaxed) + (32 << 10) {
                let n = walk(scratch);
                // SAFETY: both mappings hold `MAX_REGIONS` regions.
                unsafe { std::ptr::copy_nonoverlapping(scratch, buf, n) };
                VM_LEN.store(n, Ordering::Relaxed);
                // SAFETY: `n` regions were just written.
                snapshot_classes(unsafe { std::slice::from_raw_parts(scratch, n) });
                VM_LIVE.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
                VM_PEAK.store(res as usize, Ordering::Relaxed);
            }
            // SAFETY: plain sleep.
            unsafe { libc::usleep(500) };
        }
    }
    let mut t: libc::pthread_t = unsafe { std::mem::zeroed() };
    // SAFETY: `watch` takes the scratch mapping and never returns.
    unsafe {
        libc::pthread_create(
            &raw mut t,
            std::ptr::null(),
            watch,
            scratch as *mut libc::c_void,
        );
    }
}

fn map_bytes(bytes: usize) -> Option<usize> {
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

/// Write the peak region map to `<path>.vm`.
fn write_vm(path: &std::ffi::OsStr) {
    let n = VM_LEN.load(Ordering::Relaxed);
    let buf = VM_BUF.load(Ordering::Acquire) as *const Region;
    if buf.is_null() {
        return;
    }
    let (res, fp) = resident();
    let mut out = format!(
        "# peak resident {} heap live then {} at exit: resident {} footprint {}\n",
        VM_PEAK.load(Ordering::Relaxed),
        VM_LIVE.load(Ordering::Relaxed),
        res,
        fp
    );
    // SAFETY: `n` regions were copied in.
    let regions = unsafe { std::slice::from_raw_parts(buf, n) };
    for r in regions {
        let i = r.info;
        let mut name = [0u8; 1024];
        // SAFETY: `name` has room for the requested length.
        let len = unsafe {
            libc::proc_regionfilename(
                libc::getpid(),
                r.addr,
                name.as_mut_ptr().cast(),
                name.len() as u32,
            )
        };
        let file = String::from_utf8_lossy(&name[..len.max(0) as usize]).into_owned();
        let tag = i.user_tag;
        let prot = i.protection;
        let (res, dirty, swapped, share) = (
            i.pages_resident,
            i.pages_dirtied,
            i.pages_swapped_out,
            i.share_mode,
        );
        out.push_str(&format!(
            "{:x} {} tag={} prot={} res={} dirty={} swap={} share={} ext={} {}\n",
            r.addr,
            r.size,
            tag,
            prot,
            res,
            dirty,
            swapped,
            share,
            { i.external_pager },
            if i.external_pager != 0 {
                file.as_str()
            } else {
                ""
            }
        ));
    }
    write_classes(path);
    let mut p = path.to_owned();
    p.push(".vm");
    let _ = std::fs::write(p, out);
}

// ---------- live and resident bytes by thread and size class ----------

const THREADS: usize = 6;
const CLASSES: usize = 64;
const SLICE_SHIFT: usize = 16;
const SLICES: usize = 1 << 20;

#[allow(clippy::declare_interior_mutable_const)]
const ZERO_I: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
#[allow(clippy::declare_interior_mutable_const)]
const ZERO_ROW: [std::sync::atomic::AtomicIsize; CLASSES] = [ZERO_I; CLASSES];
static LIVE_C: [[std::sync::atomic::AtomicIsize; CLASSES]; THREADS] = [ZERO_ROW; THREADS];
static PEAK_C: [[std::sync::atomic::AtomicIsize; CLASSES]; THREADS] = [ZERO_ROW; THREADS];
static AT_PEAK_LIVE: [[std::sync::atomic::AtomicIsize; CLASSES]; THREADS] = [ZERO_ROW; THREADS];
static AT_PEAK_RES: [[std::sync::atomic::AtomicIsize; CLASSES]; THREADS] = [ZERO_ROW; THREADS];
static OWNER: [std::sync::atomic::AtomicU8; SLICES] =
    [const { std::sync::atomic::AtomicU8::new(0) }; SLICES];
static CLASS_OF: [std::sync::atomic::AtomicU8; SLICES] =
    [const { std::sync::atomic::AtomicU8::new(0) }; SLICES];
static NEXT_THREAD: AtomicUsize = AtomicUsize::new(1);

thread_local! {
    static THREAD_IDX: Cell<usize> = const { Cell::new(0) };
}

extern "C" {
    fn mi_usable_size(p: *const std::ffi::c_void) -> usize;
}

fn thread_idx() -> usize {
    THREAD_IDX
        .try_with(|t| {
            if t.get() == 0 {
                t.set(NEXT_THREAD.fetch_add(1, Ordering::Relaxed).min(THREADS - 1));
            }
            t.get()
        })
        .unwrap_or(0)
}

/// Four classes per doubling, like mimalloc's bins.
fn class_of(size: usize) -> usize {
    let size = size.max(8);
    let log = (usize::BITS - 1 - size.leading_zeros()) as usize;
    let frac = if log >= 2 { (size >> (log - 2)) & 3 } else { 0 };
    ((log - 3) * 4 + frac).min(CLASSES - 1)
}

fn class_size(c: usize) -> usize {
    let log = c / 4 + 3;
    (1usize << log) + ((c % 4) << (log - 2))
}

fn note_alloc(p: usize) {
    // SAFETY: `p` is a live mimalloc block.
    let us = unsafe { mi_usable_size(p as *const _) };
    let (t, c) = (thread_idx(), class_of(us));
    let mut slice = p >> SLICE_SHIFT;
    let end = (p + us.max(1) - 1) >> SLICE_SHIFT;
    while slice <= end {
        OWNER[slice & (SLICES - 1)].store(t as u8, Ordering::Relaxed);
        CLASS_OF[slice & (SLICES - 1)].store(c as u8, Ordering::Relaxed);
        slice += 1;
    }
    let live = LIVE_C[t][c].fetch_add(us as isize, Ordering::Relaxed) + us as isize;
    PEAK_C[t][c].fetch_max(live, Ordering::Relaxed);
}

fn note_free(p: usize) {
    // SAFETY: `p` is a live mimalloc block, about to be freed.
    let us = unsafe { mi_usable_size(p as *const _) };
    let t = OWNER[(p >> SLICE_SHIFT) & (SLICES - 1)].load(Ordering::Relaxed) as usize;
    LIVE_C[t.min(THREADS - 1)][class_of(us)].fetch_sub(us as isize, Ordering::Relaxed);
}

/// Attribute the resident pages of `regions`' mimalloc mappings to the
/// thread and size class that last allocated in each 64 KiB slice.
fn snapshot_classes(regions: &[Region]) {
    for t in 0..THREADS {
        for c in 0..CLASSES {
            AT_PEAK_LIVE[t][c].store(LIVE_C[t][c].load(Ordering::Relaxed), Ordering::Relaxed);
            AT_PEAK_RES[t][c].store(0, Ordering::Relaxed);
        }
    }
    let mut vec = [0i8; 16];
    for r in regions {
        if { r.info.user_tag } != 100 {
            continue;
        }
        let mut a = r.addr as usize;
        let end = a + r.size as usize;
        while a < end {
            let n = (end - a).min(1 << SLICE_SHIFT).min(16 * 4096);
            // SAFETY: `vec` has one byte per page of the 64 KiB asked for.
            let rc = unsafe { libc::mincore(a as *const _, n, vec.as_mut_ptr().cast()) };
            if rc == 0 {
                let pages = n / 4096;
                let res = vec[..pages].iter().filter(|&&v| v & 1 != 0).count();
                let slice = (a >> SLICE_SHIFT) & (SLICES - 1);
                let (t, c) = (
                    OWNER[slice].load(Ordering::Relaxed) as usize,
                    CLASS_OF[slice].load(Ordering::Relaxed) as usize,
                );
                AT_PEAK_RES[t.min(THREADS - 1)][c]
                    .fetch_add((res * 4096) as isize, Ordering::Relaxed);
            }
            a += n;
        }
    }
}

fn write_classes(path: &std::ffi::OsStr) {
    let mut out =
        String::from("# thread class_size live_at_rss_peak resident_at_rss_peak class_peak_live\n");
    for t in 0..THREADS {
        let (mut l, mut r, mut p) = (0, 0, 0);
        for c in 0..CLASSES {
            let (li, re, pe) = (
                AT_PEAK_LIVE[t][c].load(Ordering::Relaxed),
                AT_PEAK_RES[t][c].load(Ordering::Relaxed),
                PEAK_C[t][c].load(Ordering::Relaxed),
            );
            l += li;
            r += re;
            p += pe;
            if li != 0 || re != 0 || pe != 0 {
                out.push_str(&format!("{t} {} {li} {re} {pe}\n", class_size(c)));
            }
        }
        out.push_str(&format!("{t} total {l} {r} {p}\n"));
    }
    let mut pth = path.to_owned();
    pth.push(".classes");
    let _ = std::fs::write(pth, out);
}
