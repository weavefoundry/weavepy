//! Releasing the binary's own code pages once the process is done with
//! them.
//!
//! A run executes far more code than it keeps running. Start-up builds the
//! builtin types and modules, imports parse, compile or unmarshal, and the
//! JITs compile, each once; what a program then runs steadily is a few
//! hundred kilobytes. Every page of code a process has ever run stays in its
//! resident set, though, so that one-time code (several megabytes of the
//! binary) counts toward its memory for as long as it lives, and is most of
//! what a small program's resident set holds over CPython's.
//!
//! So now and then (see [`grown`], which the memory tick calls) the process
//! releases every page of its code and its constants (the read-only part of
//! the binary, which the file backs as it is): on Linux with
//! `madvise(MADV_DONTNEED)`, on macOS by mapping the file over them again
//! (as `alloc.rs` releases heap pages there, and for the same reason:
//! nothing else takes pages out of the resident set at once). On Linux that
//! part is larger still, since a fault there maps the sixteen pages around
//! the one touched. The pages stay in the file cache, and the code the
//! program is still running comes back a soft fault a page, about two
//! microseconds each. What doesn't come back is everything the process only
//! passed through.
//!
//! A release costs the process two or three milliseconds, nearly all of it
//! those faults (a program in the middle of its imports runs through about
//! 5 MB of code pages, 1,300 of them), so it happens only where it lowers
//! the most memory the process ever holds, has something to free, and is a
//! small part of the time the process has already taken:
//!
//! - the process is at least [`FIRST_AFTER`] old. A script that's over in
//!   tens of milliseconds would spend a tenth of them on a release (and a
//!   kernel of a millisecond or two, timed right after one, takes half as
//!   long again), for memory it's about to give back anyway;
//! - the resident set is about to pass its highest so far (within
//!   [`margin`] of it), and is at least [`FIRST_AT`], so an interpreter that
//!   only starts up, or runs a small script, pays nothing;
//! - imports are over for now: no module body is running and none has
//!   started or finished for [`QUIET`]. Start-up and imports are where the
//!   one-time code runs, and a release in the middle of them gets nearly
//!   all of it back at once; right after them is when a program's own heap
//!   starts to grow. (A module body that has run for [`BODY`] without
//!   importing anything counts as the program.)
//! - the file-backed part of the resident set (which the kernel reports)
//!   has grown by [`WORTH`]: over what the last release left, if imports
//!   have run since, and otherwise over what had come back once the wait
//!   after it was over, which is what the program keeps running. A heap
//!   that grows by itself releases nothing, and neither does a program
//!   that has settled;
//! - and the last release was a while ago, twice as long as the one before
//!   it was from its own, from [`FIRST_WAIT`] up.
//!
//! Most programs that live that long so release once: a millisecond or two
//! after their last import, or when they turn [`FIRST_AFTER`] old.
//!
//! A binary linked in a profile's order doesn't wait that long for most of
//! it. `tools/pgo/order.py` puts the code programs keep running first,
//! between two markers ([`weavepy_text_steady_start`] and
//! [`weavepy_text_steady_end`]); everything past the second is what
//! start-up, imports and compiles run. Releasing only that costs the
//! program next to nothing, since little of it comes back, so it happens
//! as soon as imports are over, whatever the process's age, and on a
//! thread of its own, which takes the system call's time too (see
//! [`rest`]).
//!
//! The pages are the file's, so nothing is lost: with one exception, a
//! debugger's breakpoints, which live in private copies of code pages. A
//! traced process is left alone. `WEAVEPY_TEXT_TRIM=0` turns the whole thing
//! off, and `WEAVEPY_TEXT_TRIM=debug` reports each release on stderr.
//!
//! How much comes back, and so how much stays resident, depends on how the
//! code a program keeps running is laid out: the linker's order file puts it
//! together (see `tools/pgo/order.py`).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};

/// The resident set size below which a process is left alone.
const FIRST_AT: usize = 10 << 20;

/// How old the process is at its first release, at the youngest, in
/// microseconds: fifty times what a release costs it.
const FIRST_AFTER: u64 = 100_000;

/// How long after the first release the second may happen, in microseconds;
/// each later one waits twice as long as the one before, up to
/// [`LONGEST_WAIT`].
const FIRST_WAIT: u64 = 16_000;
const LONGEST_WAIT: u64 = 30_000_000;

/// How long after an import last started or finished a release waits, in
/// microseconds, when no module body is running: long enough to tell the
/// end of a run of imports from the gap between two of them, and no
/// longer, since a small program's heap reaches its peak within
/// milliseconds of its last import.
const QUIET: u64 = 1_000;

/// How long a running module body has gone without an import starting or
/// finishing before a release stops waiting for it to end, in
/// microseconds: a body that runs this long is the program (a script run
/// by importing it), not a module setting itself up.
const BODY: u64 = 50_000;

/// How near its highest so far the resident set comes before a release:
/// about what a busy process grows between two looks.
fn margin(highest: usize) -> usize {
    (highest / 16).max(1 << 20)
}

/// How much file-backed memory the resident set gains before another
/// release (see the module docs for over what): less is a release that
/// frees nothing for long.
const WORTH: usize = 3 << 20;

/// A process's resident memory: all of it, and the part files back (code,
/// constants and mapped files).
#[derive(Clone, Copy)]
pub(crate) struct Resident {
    pub(crate) all: usize,
    pub(crate) files: usize,
}

const UNSET: u8 = 0;
const READY: u8 = 1;
const OFF: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNSET);
/// The whole pages of the binary's code and constants: up to [`RANGES`]
/// runs, each a start and a length (zero for none).
static RANGE: [[AtomicUsize; 2]; RANGES] =
    [const { [AtomicUsize::new(0), AtomicUsize::new(0)] }; RANGES];
const RANGES: usize = 4;
/// The resident set size from which the first release may happen, and the
/// highest the resident set has been seen.
static NEXT_AT: AtomicUsize = AtomicUsize::new(FIRST_AT);
static HIGHEST: AtomicUsize = AtomicUsize::new(0);
/// The file-backed part of the resident set the last release left, and at
/// the first look after the wait that followed it (what had come back by
/// then), with whether that look has happened.
static FILES_LEFT: AtomicUsize = AtomicUsize::new(0);
static BACK: AtomicUsize = AtomicUsize::new(0);
static SETTLED: AtomicBool = AtomicBool::new(true);
/// The runtime's count of import events at the last release.
static IMPORTS: AtomicUsize = AtomicUsize::new(0);
/// When the next release may happen (in the tick's microseconds), with the
/// wait after it.
static NEXT_TIME: AtomicU64 = AtomicU64::new(FIRST_AFTER);
static NEXT_WAIT: AtomicU64 = AtomicU64::new(FIRST_WAIT);
/// See [`QUIET`] and [`WORTH`] (tuning aids set them).
static QUIET_FOR: AtomicU64 = AtomicU64::new(QUIET);
static WORTH_OF: AtomicUsize = AtomicUsize::new(WORTH);
static BUSY: AtomicBool = AtomicBool::new(false);
static DEBUG: AtomicBool = AtomicBool::new(false);

/// Where the code a program keeps running begins in a binary linked in a
/// profile's order (see the module docs). The value only keeps the two
/// markers from being folded into one function.
#[no_mangle]
#[inline(never)]
pub(crate) extern "C" fn weavepy_text_steady_start() -> u32 {
    0x7374_6172
}

/// Where that code ends: the code past it runs once, or now and then.
#[no_mangle]
#[inline(never)]
pub(crate) extern "C" fn weavepy_text_steady_end() -> u32 {
    0x7374_656e
}

/// How much code lies between the markers in a binary linked in a
/// profile's order (a megabyte or two there; a few bytes in any other
/// binary, where the two are neighbors wherever the linker put them).
const STEADY: std::ops::RangeInclusive<usize> = (256 << 10)..=(16 << 20);

/// Where the code past the steady part starts (a page boundary inside the
/// first range), or zero for a binary that isn't linked that way; and
/// whether that part has been released ahead of the first full release.
static REST_FROM: AtomicUsize = AtomicUsize::new(0);
static REST_DONE: AtomicBool = AtomicBool::new(false);

/// Find this binary's code and constants, for [`grown`] to release as the
/// process grows (see the module docs). Does nothing with
/// `WEAVEPY_TEXT_TRIM=0`, or where they can't be found or the binary's file
/// isn't what's mapped.
pub(crate) fn install() {
    match std::env::var_os("WEAVEPY_TEXT_TRIM") {
        Some(v) if v == "0" => {
            STATE.store(OFF, Ordering::Relaxed);
            return;
        }
        Some(v) if v == "debug" => DEBUG.store(true, Ordering::Relaxed),
        _ => {}
    }
    // (Tuning aids: the resident set size of the first release and the
    // file-backed growth of a later one, in bytes, and the process's age at
    // the first and the wait after it and after an import, in microseconds.)
    let tuned = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok());
    if let Some(at) = tuned("WEAVEPY_TEXT_TRIM_AT") {
        NEXT_AT.store(at as usize, Ordering::Relaxed);
    }
    if let Some(worth) = tuned("WEAVEPY_TEXT_TRIM_WORTH") {
        WORTH_OF.store(worth as usize, Ordering::Relaxed);
    }
    if let Some(after) = tuned("WEAVEPY_TEXT_TRIM_AFTER") {
        NEXT_TIME.store(after, Ordering::Relaxed);
    }
    if let Some(wait) = tuned("WEAVEPY_TEXT_TRIM_WAIT") {
        NEXT_WAIT.store(wait.max(1), Ordering::Relaxed);
    }
    if let Some(quiet) = tuned("WEAVEPY_TEXT_TRIM_QUIET") {
        QUIET_FOR.store(quiet, Ordering::Relaxed);
    }
    let ready = sys::locate().is_some_and(|image| {
        for (slot, (start, len)) in RANGE.iter().zip(image.ranges) {
            slot[0].store(start, Ordering::Relaxed);
            slot[1].store(len, Ordering::Relaxed);
        }
        image.ranges[0].1 != 0
    });
    if ready && std::env::var_os("WEAVEPY_TEXT_TRIM_REST").is_none_or(|v| v != "0") {
        let (from, to) = (
            weavepy_text_steady_start as *const () as usize,
            weavepy_text_steady_end as *const () as usize,
        );
        // (The range the second marker is in is the one with the code. The
        // first may lie just ahead of it, on the header's own page.)
        let holds = |slot: &[AtomicUsize; 2]| {
            let (start, len) = (
                slot[0].load(Ordering::Relaxed),
                slot[1].load(Ordering::Relaxed),
            );
            (start..start + len).contains(&to)
        };
        if from < to && STEADY.contains(&(to - from)) && RANGE.iter().any(holds) {
            let page = sys::page();
            REST_FROM.store((to + page - 1) & !(page - 1), Ordering::Relaxed);
        }
    }
    STATE.store(if ready { READY } else { OFF }, Ordering::Release);
}

/// The process's resident memory, where the platform has a cheap way to
/// ask.
pub(crate) fn resident() -> Option<Resident> {
    sys::resident()
}

/// The binary's code and constants: runs of whole pages (each a start and a
/// length, zero for none).
struct Image {
    ranges: [(usize, usize); RANGES],
}

/// The whole pages within `span` (a start and an end), as a start and a
/// length.
#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
fn whole_pages(span: (usize, usize), page: usize) -> Option<(usize, usize)> {
    let start = span.0.checked_add(page - 1)? & !(page - 1);
    let end = span.1 & !(page - 1);
    (start < end).then(|| (start, end - start))
}

/// Release what lies past the steady code (and the other ranges: the
/// constants), on a thread of its own: the system call takes about a
/// millisecond, which a script timed in milliseconds would notice, and
/// the pages are ones the program has mostly finished with, so few fault
/// back in. Once a process; the full releases follow by their own rule,
/// counting from what this one left (`events` is the runtime's count of
/// import events now).
fn rest(at: u64, events: usize) {
    let spawned = std::thread::Builder::new()
        .name("weavepy-trim".into())
        .stack_size(64 << 10)
        .spawn(move || {
            if STATE.load(Ordering::Acquire) != READY
                || sys::traced()
                || BUSY.swap(true, Ordering::Acquire)
            {
                return;
            }
            let began = std::time::Instant::now();
            let before = sys::resident();
            let from = REST_FROM.load(Ordering::Relaxed);
            for slot in &RANGE {
                let (start, len) = (
                    slot[0].load(Ordering::Relaxed),
                    slot[1].load(Ordering::Relaxed),
                );
                // (The range with the steady code gives up what follows it.)
                let (start, len) = if (start..start + len).contains(&from) {
                    (from, start + len - from)
                } else {
                    (start, len)
                };
                // SAFETY: whole pages of this binary's code and constants,
                // which the file backs unchanged (`locate` checked).
                if len != 0 && !unsafe { sys::release(start, len) } {
                    STATE.store(OFF, Ordering::Release);
                }
            }
            let now = sys::resident();
            if DEBUG.load(Ordering::Relaxed) {
                if let (Some(before), Some(now)) = (before, now) {
                    eprintln!(
                        "text trim: at {} ms, past the steady code, resident {} -> {} KiB (files {} -> {}) in {} us",
                        at / 1000,
                        before.all >> 10,
                        now.all >> 10,
                        before.files >> 10,
                        now.files >> 10,
                        began.elapsed().as_micros()
                    );
                }
            }
            // (As a full release leaves things: the next one wants the
            // file-backed part to have grown over what's left now, or over
            // what has come back by the first look it's old enough for.)
            if let Some(now) = now {
                FILES_LEFT.store(now.files, Ordering::Relaxed);
            }
            IMPORTS.store(events, Ordering::Relaxed);
            SETTLED.store(false, Ordering::Relaxed);
            BUSY.store(false, Ordering::Release);
        });
    drop(spawned);
}

/// The process's resident memory is `resident`, `at` microseconds on the
/// tick's clock, with the runtime's imports at `imports`: release the
/// code's pages if it's time (see the module docs).
pub(crate) fn grown(resident: Resident, at: u64, imports: weavepy_vm::ImportActivity) {
    if STATE.load(Ordering::Acquire) != READY {
        return;
    }
    let Resident { all, files } = resident;
    let highest = HIGHEST.load(Ordering::Relaxed);
    if all > highest {
        HIGHEST.store(all, Ordering::Relaxed);
    }
    if at < NEXT_TIME.load(Ordering::Relaxed) {
        // Too young for a full release: the part past the steady code can
        // go as soon as imports are over (see the module docs).
        if REST_FROM.load(Ordering::Relaxed) != 0
            && !REST_DONE.load(Ordering::Relaxed)
            && all >= NEXT_AT.load(Ordering::Relaxed)
            && files >= WORTH_OF.load(Ordering::Relaxed)
            && imports.running == 0
            && imports.quiet_micros >= QUIET_FOR.load(Ordering::Relaxed)
            && !REST_DONE.swap(true, Ordering::Relaxed)
        {
            rest(at, imports.events);
        }
        return;
    }
    if !SETTLED.swap(true, Ordering::Relaxed) {
        BACK.store(files, Ordering::Relaxed);
    }
    // What the code that has run since the last release came to: imports
    // are done with theirs, a program at work only with what's new to it.
    let before = if imports.events == IMPORTS.load(Ordering::Relaxed) {
        BACK.load(Ordering::Relaxed)
    } else {
        FILES_LEFT.load(Ordering::Relaxed)
    };
    let quiet = if imports.running == 0 {
        QUIET_FOR.load(Ordering::Relaxed)
    } else {
        BODY
    };
    if all < NEXT_AT.load(Ordering::Relaxed)
        || all + margin(highest) < highest
        || files < before + WORTH_OF.load(Ordering::Relaxed)
        || imports.quiet_micros < quiet
        || BUSY.swap(true, Ordering::Acquire)
    {
        return;
    }
    let began = std::time::Instant::now();
    // (A traced process's debugger may have written breakpoints, which
    // live in private copies of code pages.)
    if !sys::traced() {
        for slot in &RANGE {
            let (start, len) = (
                slot[0].load(Ordering::Relaxed),
                slot[1].load(Ordering::Relaxed),
            );
            // SAFETY: whole pages of this binary's code and constants, which
            // the file backs unchanged (`locate` checked).
            if len != 0 && !unsafe { sys::release(start, len) } {
                STATE.store(OFF, Ordering::Release);
            }
        }
    }
    let now = sys::resident().unwrap_or(resident);
    let wait = NEXT_WAIT.load(Ordering::Relaxed);
    if DEBUG.load(Ordering::Relaxed) {
        eprintln!(
            "text trim: at {} ms, resident {} -> {} KiB (files {} -> {}, from {}) in {} us, next in {} ms",
            at / 1000,
            all >> 10,
            now.all >> 10,
            files >> 10,
            now.files >> 10,
            before >> 10,
            began.elapsed().as_micros(),
            wait / 1000
        );
    }
    FILES_LEFT.store(now.files, Ordering::Relaxed);
    IMPORTS.store(imports.events, Ordering::Relaxed);
    SETTLED.store(false, Ordering::Relaxed);
    NEXT_TIME.store(at + wait, Ordering::Relaxed);
    NEXT_WAIT.store(wait.saturating_mul(2).min(LONGEST_WAIT), Ordering::Relaxed);
    BUSY.store(false, Ordering::Release);
}

#[cfg(target_vendor = "apple")]
mod sys {
    use super::{whole_pages, Image, Resident};
    use std::ffi::{c_char, c_int, c_ulong, c_void};

    extern "C" {
        fn _dyld_get_image_header(index: u32) -> *const c_void;
        fn _NSGetExecutablePath(buf: *mut c_char, size: *mut u32) -> c_int;
        fn getsegmentdata(
            header: *const c_void,
            segment: *const c_char,
            size: *mut c_ulong,
        ) -> *mut u8;
    }

    /// The process's resident memory: `task_info`'s `TASK_VM_INFO`, whose
    /// `external` is the part an external pager (a file) backs.
    pub(super) fn resident() -> Option<Resident> {
        /// The flavor, the words of its first revision (`struct
        /// task_vm_info`, packed to four bytes), and where the two sizes
        /// are in it.
        const TASK_VM_INFO: u32 = 22;
        const WORDS: usize = 38;
        const RESIDENT_SIZE: usize = 16;
        const EXTERNAL: usize = 64;
        let mut info = [0u32; WORDS];
        let mut count = WORDS as u32;
        // SAFETY: `info` has room for `count` words of this flavor.
        #[allow(deprecated)] // (`libc::mach_task_self`)
        let kr = unsafe {
            libc::task_info(
                libc::mach_task_self(),
                TASK_VM_INFO,
                info.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        if kr != 0 || (count as usize) < WORDS {
            return None;
        }
        let field = |at: usize| {
            // SAFETY: within the buffer the kernel filled in.
            unsafe {
                info.as_ptr()
                    .cast::<u8>()
                    .add(at)
                    .cast::<u64>()
                    .read_unaligned() as usize
            }
        };
        Some(Resident {
            all: field(RESIDENT_SIZE),
            files: field(EXTERNAL),
        })
    }

    /// Whether a debugger traces the process: the `P_TRACED` flag of its
    /// `kinfo_proc` (Apple's QA1361). Unknown counts as traced.
    pub(super) fn traced() -> bool {
        /// `sizeof(struct kinfo_proc)`, and where its `kp_proc.p_flag` is.
        const KINFO_PROC: usize = 648;
        const P_FLAG: usize = 32;
        const P_TRACED: i32 = 0x800;
        let mut info = [0u64; KINFO_PROC / 8];
        let mut size = KINFO_PROC;
        // SAFETY: `getpid` has no preconditions.
        let mut name = [
            libc::CTL_KERN,
            libc::KERN_PROC,
            libc::KERN_PROC_PID,
            unsafe { libc::getpid() },
        ];
        // SAFETY: `info` has room for `size` bytes.
        let r = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                name.len() as u32,
                info.as_mut_ptr().cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 || size != KINFO_PROC {
            return true;
        }
        // SAFETY: within the buffer the kernel filled in.
        let flag = unsafe {
            info.as_ptr()
                .cast::<u8>()
                .add(P_FLAG)
                .cast::<i32>()
                .read_unaligned()
        };
        flag & P_TRACED != 0
    }

    /// The system's page size.
    pub(super) fn page() -> usize {
        // SAFETY: `sysconf` has no preconditions.
        usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096)
    }

    /// The executable's `__TEXT` segment (its code and constants): `None`
    /// when this code isn't in the executable (the runtime is in a library
    /// some other program loaded), or the executable's path can't be had.
    ///
    /// (Not through `dladdr`, which reads the image's whole symbol table to
    /// name the address: megabytes of pages for the resident set.)
    pub(super) fn locate() -> Option<Image> {
        // SAFETY: image 0 is the executable, mapped for the process's life.
        let header = unsafe { _dyld_get_image_header(0) };
        if header.is_null() {
            return None;
        }
        let mut size: c_ulong = 0;
        // SAFETY: a loaded image's header; the name is NUL-terminated.
        let text = unsafe { getsegmentdata(header, c"__TEXT".as_ptr(), &raw mut size) } as usize;
        // (The segment starts with the header itself.)
        if text == 0 || text != header as usize {
            return None;
        }
        // SAFETY: `sysconf` has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        let segment = (text, text + size as usize);
        if !(segment.0..segment.1).contains(&(locate as *const () as usize)) {
            return None;
        }
        // (Not the header's own page, which the loader and unwinders read.)
        let pages = whole_pages((text + page, segment.1), page)?;
        PATH.set((executable_path()?, text, page)).ok()?;
        let mut ranges = [(0, 0); super::RANGES];
        ranges[0] = pages;
        Some(Image { ranges })
    }

    /// The executable's absolute path (taken while the process is still in
    /// the directory it started in), where the image's first byte is mapped,
    /// and the page size.
    static PATH: std::sync::OnceLock<(std::ffi::CString, usize, usize)> =
        std::sync::OnceLock::new();

    /// Which file the executable is (its device and inode), once the first
    /// release has checked that the file at [`PATH`] is the mapped image
    /// from its first byte on: `None` if it isn't (a universal binary, whose
    /// slice starts elsewhere in the file, or a file replaced since the
    /// process started).
    static FILE: std::sync::OnceLock<Option<(libc::dev_t, libc::ino_t)>> =
        std::sync::OnceLock::new();

    fn executable_path() -> Option<std::ffi::CString> {
        let mut given = [0 as c_char; libc::PATH_MAX as usize];
        let mut len = given.len() as u32;
        // SAFETY: `given` has room for `len` bytes.
        if unsafe { _NSGetExecutablePath(given.as_mut_ptr(), &raw mut len) } != 0 {
            return None;
        }
        // SAFETY: NUL-terminated by the call above.
        let path = unsafe { std::ffi::CStr::from_ptr(given.as_ptr()) };
        if path.to_bytes().first() == Some(&b'/') {
            return Some(path.to_owned());
        }
        let mut real = [0 as c_char; libc::PATH_MAX as usize];
        // SAFETY: a NUL-terminated path, and room for `PATH_MAX` bytes.
        if unsafe { libc::realpath(given.as_ptr(), real.as_mut_ptr()) }.is_null() {
            return None;
        }
        // SAFETY: `realpath` NUL-terminated it.
        Some(unsafe { std::ffi::CStr::from_ptr(real.as_ptr()) }.to_owned())
    }

    /// The file at `path`, open for reading, and which file it is.
    fn open(path: &std::ffi::CStr) -> Option<(c_int, (libc::dev_t, libc::ino_t))> {
        // SAFETY: a NUL-terminated path; `stat` has room for the result; the
        // descriptor is ours.
        unsafe {
            let fd = libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
            if fd < 0 {
                return None;
            }
            let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
            if libc::fstat(fd, stat.as_mut_ptr()) != 0 {
                libc::close(fd);
                return None;
            }
            let stat = stat.assume_init();
            Some((fd, (stat.st_dev, stat.st_ino)))
        }
    }

    /// Whether the file `fd` starts with the page mapped at `base`: the
    /// image's own header and load commands (its UUID among them).
    fn starts_with_image(fd: c_int, base: usize, page: usize) -> bool {
        let mut first = vec![0u8; page];
        // SAFETY: `first` has room for `page` bytes; the header's page is
        // mapped.
        unsafe {
            libc::pread(fd, first.as_mut_ptr().cast(), page, 0) == page as isize
                && first == std::slice::from_raw_parts(base as *const u8, page)
        }
    }

    /// Take the pages of `start..start + len` out of the resident set by
    /// mapping the file over them. `false` if that can't be done, which
    /// leaves the old mapping in place.
    ///
    /// (The file is opened for each release rather than held open: a
    /// program may close or reuse any descriptor number.)
    ///
    /// # Safety
    ///
    /// The range is whole pages of the image [`locate`] found.
    pub(super) unsafe fn release(start: usize, len: usize) -> bool {
        let Some((path, base, page)) = PATH.get() else {
            return false;
        };
        let Some((fd, which)) = open(path) else {
            return false;
        };
        let known = *FILE.get_or_init(|| starts_with_image(fd, *base, *page).then_some(which));
        // SAFETY: the caller's contract; the new mapping has the bytes the
        // old one had, and replaces it atomically. The descriptor is ours.
        unsafe {
            let mapped = known == Some(which)
                && libc::mmap(
                    start as *mut c_void,
                    len,
                    libc::PROT_READ | libc::PROT_EXEC,
                    libc::MAP_PRIVATE | libc::MAP_FIXED,
                    fd,
                    (start - base) as libc::off_t,
                ) as usize
                    == start;
            libc::close(fd);
            mapped
        }
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use super::{whole_pages, Image, Resident, RANGES};
    use std::ffi::{c_int, c_void};
    use std::os::unix::fs::FileExt;

    /// One number of a `/proc/self` file, after `key`.
    fn proc_number(file: &std::fs::File, key: &[u8], skip_fields: usize) -> Option<usize> {
        let mut buf = [0u8; 2048];
        let n = file.read_at(&mut buf, 0).ok()?;
        let text = &buf[..n];
        let at = if key.is_empty() {
            0
        } else {
            text.windows(key.len()).position(|w| w == key)? + key.len()
        };
        std::str::from_utf8(&text[at..])
            .ok()?
            .split_ascii_whitespace()
            .nth(skip_fields)?
            .parse()
            .ok()
    }

    fn statm() -> Option<&'static std::fs::File> {
        static STATM: std::sync::OnceLock<Option<std::fs::File>> = std::sync::OnceLock::new();
        STATM
            .get_or_init(|| std::fs::File::open("/proc/self/statm").ok())
            .as_ref()
    }

    /// The process's resident memory: `/proc/self/statm`'s second and third
    /// numbers, in pages (the third is the file-backed part).
    pub(super) fn resident() -> Option<Resident> {
        // SAFETY: `sysconf` has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        let statm = statm()?;
        Some(Resident {
            all: proc_number(statm, b"", 1)? * page,
            files: proc_number(statm, b"", 2)? * page,
        })
    }

    /// Whether a debugger traces the process (unknown counts as traced).
    pub(super) fn traced() -> bool {
        std::fs::File::open("/proc/self/status")
            .ok()
            .and_then(|f| proc_number(&f, b"TracerPid:", 0))
            != Some(0)
    }

    struct Find {
        addr: usize,
        page: usize,
        ranges: [(usize, usize); RANGES],
    }

    /// `dl_iterate_phdr`'s callback: the read-only segments (code and
    /// constants, which the file backs as they are) of the object whose
    /// code holds `Find::addr`.
    unsafe extern "C" fn segments(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        data: *mut c_void,
    ) -> c_int {
        // SAFETY: the loader passes a valid object description, and `data`
        // is `locate`'s `Find`.
        unsafe {
            let find = &mut *data.cast::<Find>();
            let info = &*info;
            let headers = std::slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum));
            let span = |ph: &libc::Elf64_Phdr| {
                let lo = info.dlpi_addr as usize + ph.p_vaddr as usize;
                (lo, lo + ph.p_memsz as usize)
            };
            let loads = || headers.iter().filter(|ph| ph.p_type == libc::PT_LOAD);
            let ours = loads().any(|ph| {
                let (lo, hi) = span(ph);
                ph.p_flags & libc::PF_X != 0 && (lo..hi).contains(&find.addr)
            });
            if !ours {
                return 0;
            }
            let read_only = loads()
                .filter(|ph| ph.p_flags & libc::PF_W == 0 && ph.p_filesz == ph.p_memsz)
                .filter_map(|ph| whole_pages(span(ph), find.page));
            for (slot, range) in find.ranges.iter_mut().zip(read_only) {
                *slot = range;
            }
            1
        }
    }

    /// The system's page size.
    pub(super) fn page() -> usize {
        // SAFETY: `sysconf` has no preconditions.
        usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096)
    }

    /// The read-only segments of the object this code is in.
    pub(super) fn locate() -> Option<Image> {
        // SAFETY: `sysconf` has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        let mut find = Find {
            addr: locate as *const () as usize,
            page,
            ranges: [(0, 0); RANGES],
        };
        // SAFETY: the callback reads what the loader hands it.
        unsafe { libc::dl_iterate_phdr(Some(segments), (&raw mut find).cast()) };
        Some(Image {
            ranges: find.ranges,
        })
    }

    /// Take the pages of `start..start + len` out of the resident set.
    ///
    /// # Safety
    ///
    /// The range is whole pages of a read-only, file-backed mapping.
    pub(super) unsafe fn release(start: usize, len: usize) -> bool {
        // SAFETY: the caller's contract: the pages come back from the file.
        unsafe { libc::madvise(start as *mut c_void, len, libc::MADV_DONTNEED) == 0 }
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
mod sys {
    use super::{Image, Resident};

    pub(super) fn resident() -> Option<Resident> {
        None
    }

    pub(super) fn locate() -> Option<Image> {
        None
    }

    pub(super) fn traced() -> bool {
        true
    }

    pub(super) fn page() -> usize {
        4096
    }

    pub(super) unsafe fn release(_start: usize, _len: usize) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::whole_pages;

    const PAGE: usize = 4096;

    #[test]
    fn a_release_takes_whole_pages_only() {
        assert_eq!(
            whole_pages((0x10_0900, 0x90_0010), PAGE),
            Some((0x10_1000, 0x7f_f000))
        );
        assert_eq!(
            whole_pages((0x10_0000, 0x90_0000), PAGE),
            Some((0x10_0000, 0x80_0000))
        );
    }

    #[test]
    fn code_within_a_page_has_none_to_release() {
        assert_eq!(whole_pages((0x10_0900, 0x10_0f00), PAGE), None);
        assert_eq!(whole_pages((0x10_0900, 0x10_1f00), PAGE), None);
        assert_eq!(whole_pages((usize::MAX - 10, usize::MAX), PAGE), None);
    }
}
