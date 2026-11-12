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
//! the one touched. The pages stay in the file
//! cache, and the code the program is still running comes back a soft fault
//! a page, which costs too little to measure: six thousand of them in a
//! second-long run left its cycles where they were. What doesn't come back
//! is everything the process only passed through.
//!
//! The first release waits for the resident set to reach [`FIRST_AT`], so an
//! interpreter that only starts up, or runs a small script, pays nothing.
//! Each one after that waits twice as long as the one before, from
//! [`FIRST_WAIT`] up: a program's one-time code runs mostly early (its
//! imports, then its compiles), and however long it runs, it releases only a
//! handful of times more.
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

/// How long after the first release the second may happen, in microseconds;
/// each later one waits twice as long as the one before, up to
/// [`LONGEST_WAIT`].
const FIRST_WAIT: u64 = 2_000;
const LONGEST_WAIT: u64 = 30_000_000;

/// The least the resident set grows between two releases: with less than
/// this back since the last one, there's nothing to release.
const STEP: usize = 1 << 20;

const UNSET: u8 = 0;
const READY: u8 = 1;
const OFF: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNSET);
/// The whole pages of the binary's code and constants: up to [`RANGES`]
/// runs, each a start and a length (zero for none).
static RANGE: [[AtomicUsize; 2]; RANGES] =
    [const { [AtomicUsize::new(0), AtomicUsize::new(0)] }; RANGES];
const RANGES: usize = 4;
/// The resident set size from which the next release may happen, and when
/// (in microseconds since [`started`]), with the wait after it.
static NEXT_AT: AtomicUsize = AtomicUsize::new(FIRST_AT);
static NEXT_TIME: AtomicU64 = AtomicU64::new(0);
static NEXT_WAIT: AtomicU64 = AtomicU64::new(FIRST_WAIT);
static BUSY: AtomicBool = AtomicBool::new(false);
static DEBUG: AtomicBool = AtomicBool::new(false);

/// When [`install`] ran.
fn started() -> std::time::Instant {
    static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *STARTED.get_or_init(std::time::Instant::now)
}

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
    started();
    let ready = sys::locate().is_some_and(|image| {
        for (slot, (start, len)) in RANGE.iter().zip(image.ranges) {
            slot[0].store(start, Ordering::Relaxed);
            slot[1].store(len, Ordering::Relaxed);
        }
        image.ranges[0].1 != 0
    });
    STATE.store(if ready { READY } else { OFF }, Ordering::Release);
}

/// The process's resident set size, where the platform has a cheap way to
/// ask.
pub(crate) fn resident() -> Option<usize> {
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

/// The process's resident set is `resident` bytes: release the code's pages
/// if it's time (see the module docs).
pub(crate) fn grown(resident: usize) {
    if STATE.load(Ordering::Acquire) != READY || resident < NEXT_AT.load(Ordering::Relaxed) {
        return;
    }
    let at = started().elapsed().as_micros() as u64;
    if at < NEXT_TIME.load(Ordering::Relaxed) || BUSY.swap(true, Ordering::Acquire) {
        return;
    }
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
            "text trim: at {} ms, resident {} -> {} KiB in {} us, next in {} ms",
            at / 1000,
            resident >> 10,
            now >> 10,
            (started().elapsed().as_micros() as u64).saturating_sub(at),
            wait / 1000
        );
    }
    NEXT_AT.store(now + STEP, Ordering::Relaxed);
    NEXT_TIME.store(at + wait, Ordering::Relaxed);
    NEXT_WAIT.store(wait.saturating_mul(2).min(LONGEST_WAIT), Ordering::Relaxed);
    BUSY.store(false, Ordering::Release);
}

#[cfg(target_vendor = "apple")]
mod sys {
    use super::{whole_pages, Image};
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

    /// The process's resident set size.
    pub(super) fn resident() -> Option<usize> {
        let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        // SAFETY: `info` has room for `count` words of this flavor.
        #[allow(deprecated)] // (`libc::mach_task_self`)
        let kr = unsafe {
            libc::task_info(
                libc::mach_task_self(),
                libc::MACH_TASK_BASIC_INFO,
                info.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        // SAFETY: the kernel filled it in.
        (kr == 0).then(|| unsafe { info.assume_init() }.resident_size as usize)
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
    use super::{whole_pages, Image, RANGES};
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

    /// The process's resident set size.
    pub(super) fn resident() -> Option<usize> {
        // SAFETY: `sysconf` has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        Some(proc_number(statm()?, b"", 1)? * page)
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
    use super::Image;

    pub(super) fn resident() -> Option<usize> {
        None
    }

    pub(super) fn locate() -> Option<Image> {
        None
    }

    pub(super) fn traced() -> bool {
        true
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
