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
//! releases every page of its code: on Linux with `madvise(MADV_DONTNEED)`,
//! on macOS by mapping the file over the section again (as `alloc.rs`
//! releases heap pages there, and for the same reason: nothing else takes
//! pages out of the resident set at once). The pages stay in the file
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

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicU8, AtomicUsize, Ordering};

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
/// The code's whole pages: their start and length.
static TEXT_START: AtomicUsize = AtomicUsize::new(0);
static TEXT_LEN: AtomicUsize = AtomicUsize::new(0);
/// On macOS, the binary (open for reading) and where its first byte is
/// mapped.
static IMAGE_FD: AtomicI32 = AtomicI32::new(-1);
static IMAGE_BASE: AtomicUsize = AtomicUsize::new(0);
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

/// Find this binary's code, for [`grown`] to release as the process grows
/// (see the module docs). Does nothing with `WEAVEPY_TEXT_TRIM=0`, or where
/// the code can't be found or its file isn't what's mapped.
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
        TEXT_START.store(image.text.0, Ordering::Relaxed);
        TEXT_LEN.store(image.text.1, Ordering::Relaxed);
        IMAGE_FD.store(image.fd, Ordering::Relaxed);
        IMAGE_BASE.store(image.base, Ordering::Relaxed);
        true
    });
    STATE.store(if ready { READY } else { OFF }, Ordering::Release);
}

/// The process's resident set size, where the platform has a cheap way to
/// ask.
pub(crate) fn resident() -> Option<usize> {
    sys::resident()
}

/// The binary's code: its whole pages (a start and a length), and on macOS
/// the file and where its first byte is mapped.
struct Image {
    text: (usize, usize),
    fd: i32,
    base: usize,
}

/// The whole pages within `text` (a start and an end), as a start and a
/// length.
#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
fn whole_pages(text: (usize, usize), page: usize) -> Option<(usize, usize)> {
    let start = text.0.checked_add(page - 1)? & !(page - 1);
    let end = text.1 & !(page - 1);
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
    let start = TEXT_START.load(Ordering::Relaxed);
    let len = TEXT_LEN.load(Ordering::Relaxed);
    // SAFETY: the range is whole pages of this binary's code, which the file
    // backs unchanged (`locate` checked, and `release` skips a traced
    // process, whose debugger may have written breakpoints).
    let kept = unsafe { sys::release(start, len) };
    if !kept {
        STATE.store(OFF, Ordering::Release);
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
    use super::{whole_pages, Image, Ordering, IMAGE_BASE, IMAGE_FD};
    use std::ffi::{c_char, c_int, c_ulong, c_void};

    extern "C" {
        fn _dyld_get_image_header(index: u32) -> *const c_void;
        fn _NSGetExecutablePath(buf: *mut c_char, size: *mut u32) -> c_int;
        fn getsectiondata(
            header: *const c_void,
            segment: *const c_char,
            section: *const c_char,
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
    fn traced() -> bool {
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

    /// The executable's `__text` section, and the file open: `None` when
    /// this code isn't in the executable (the runtime is in a library some
    /// other program loaded), or the file at its path isn't what's mapped
    /// (a universal binary, whose slice starts elsewhere in the file, or a
    /// file replaced since).
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
        // SAFETY: a loaded image's header; the names are NUL-terminated.
        let text = unsafe {
            getsectiondata(
                header,
                c"__TEXT".as_ptr(),
                c"__text".as_ptr(),
                &raw mut size,
            )
        } as usize;
        if text == 0 {
            return None;
        }
        let mut path = [0 as c_char; libc::PATH_MAX as usize];
        let mut path_len = path.len() as u32;
        // SAFETY: `path` has room for `path_len` bytes.
        if unsafe { _NSGetExecutablePath(path.as_mut_ptr(), &raw mut path_len) } != 0 {
            return None;
        }
        let header = header as usize;
        // SAFETY: `sysconf` has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        let section = (text, text + size as usize);
        if !(section.0..section.1).contains(&(locate as *const () as usize)) {
            return None;
        }
        let pages = whole_pages(section, page)?;
        // SAFETY: the executable's path, NUL-terminated.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        // The file's first page is the image's own header and load commands
        // (its UUID among them): the mapping is the file from its start.
        let mut first = vec![0u8; page];
        // SAFETY: `first` has room for `page` bytes; the header's page is
        // mapped.
        let same = unsafe {
            libc::pread(fd, first.as_mut_ptr().cast(), page, 0) == page as isize
                && first == std::slice::from_raw_parts(header as *const u8, page)
        };
        if !same {
            // SAFETY: our descriptor.
            unsafe { libc::close(fd) };
            return None;
        }
        Some(Image {
            text: pages,
            fd,
            base: header,
        })
    }

    /// Take the pages of `start..start + len` out of the resident set by
    /// mapping the file over them. `false` if that failed, which leaves the
    /// old mapping in place.
    ///
    /// # Safety
    ///
    /// The range is whole pages of the image [`locate`] opened.
    pub(super) unsafe fn release(start: usize, len: usize) -> bool {
        if traced() {
            return true;
        }
        let fd: c_int = IMAGE_FD.load(Ordering::Relaxed);
        let offset = start - IMAGE_BASE.load(Ordering::Relaxed);
        // SAFETY: the caller's contract; the new mapping has the bytes the
        // old one had, and replaces it atomically.
        let mapped = unsafe {
            libc::mmap(
                start as *mut c_void,
                len,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_FIXED,
                fd,
                offset as libc::off_t,
            )
        };
        mapped as usize == start
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use super::{whole_pages, Image};
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
    fn traced() -> bool {
        std::fs::File::open("/proc/self/status")
            .ok()
            .and_then(|f| proc_number(&f, b"TracerPid:", 0))
            != Some(0)
    }

    struct Find {
        addr: usize,
        text: Option<(usize, usize)>,
    }

    /// `dl_iterate_phdr`'s callback: the executable segment holding
    /// `Find::addr`.
    unsafe extern "C" fn segment(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        data: *mut c_void,
    ) -> c_int {
        // SAFETY: the loader passes a valid object description, and `data`
        // is `locate`'s `Find`.
        unsafe {
            let find = &mut *data.cast::<Find>();
            let info = &*info;
            for i in 0..usize::from(info.dlpi_phnum) {
                let ph = &*info.dlpi_phdr.add(i);
                if ph.p_type != libc::PT_LOAD || ph.p_flags & libc::PF_X == 0 {
                    continue;
                }
                let lo = info.dlpi_addr as usize + ph.p_vaddr as usize;
                let hi = lo + ph.p_memsz as usize;
                if (lo..hi).contains(&find.addr) {
                    find.text = Some((lo, hi));
                    return 1;
                }
            }
            0
        }
    }

    /// The executable segment this code is in.
    pub(super) fn locate() -> Option<Image> {
        let mut find = Find {
            addr: locate as *const () as usize,
            text: None,
        };
        // SAFETY: the callback reads what the loader hands it.
        unsafe { libc::dl_iterate_phdr(Some(segment), (&raw mut find).cast()) };
        // SAFETY: `sysconf` has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        Some(Image {
            text: whole_pages(find.text?, page)?,
            fd: -1,
            base: 0,
        })
    }

    /// Take the pages of `start..start + len` out of the resident set.
    ///
    /// # Safety
    ///
    /// The range is whole pages of a read-only, file-backed mapping.
    pub(super) unsafe fn release(start: usize, len: usize) -> bool {
        if traced() {
            return true;
        }
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
