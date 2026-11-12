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
//! The linker's order file (`crates/weavepy-cli/weavepy.order`, or the one
//! `tools/pgo/order.py` writes for a profile-guided build) lays the code a
//! program runs steadily out first, between two marker functions,
//! [`weavepy_text_hot_start`] and [`weavepy_text_hot_end`]. Everything after
//! the second marker is the code a run only passes through. Whenever the
//! process's resident set has grown by a step since the last time (see
//! [`grown`], which the memory tick calls), the pages
//! of that part are released: on Linux with `madvise(MADV_DONTNEED)`, on
//! macOS by mapping the file over the range again (as `alloc.rs` releases
//! heap pages there, and for the same reason: nothing else takes pages out of
//! the resident set at once). The pages stay in the file cache, and a later
//! call into one of them takes a soft fault to map it back, so the cost of
//! being wrong about a page is small, and the steps bound how often it's
//! paid.
//!
//! The pages are the file's, so nothing is lost: with one exception, a
//! debugger's breakpoints, which live in private copies of code pages. A
//! traced process is left alone. `WEAVEPY_TEXT_TRIM=0` turns the whole thing
//! off. A binary linked without an order file (a development build) has its
//! markers side by side, and is left alone too.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicUsize, Ordering};

/// The first function of the order file's hot group: the code after it, up
/// to [`weavepy_text_hot_end`], is what programs run steadily. (The two
/// return different values so that nothing folds them into one function.)
#[no_mangle]
#[inline(never)]
pub(crate) extern "C" fn weavepy_text_hot_start() -> u32 {
    0x7e87_0001
}

/// The function after the order file's hot group (see
/// [`weavepy_text_hot_start`]).
#[no_mangle]
#[inline(never)]
pub(crate) extern "C" fn weavepy_text_hot_end() -> u32 {
    0x7e87_0002
}

/// The least a hot group spans in an ordered binary: an unordered one's
/// markers are next to each other.
const MIN_HOT: usize = 64 << 10;

/// The resident set size below which a process is left alone: an
/// interpreter that has only started up, or runs a small script, is under
/// it, and pays nothing.
const FIRST_AT: usize = 10 << 20;

/// The least the resident set grows between two releases at first (and a
/// sixteenth of it, when that's more). It doubles, up to [`STEP_MAX`], each
/// time a release frees less than [`WORTH`]: what a release frees is what
/// ran in the cold part since the last, which is little once start-up, the
/// imports and the compiles are over.
const STEP: usize = 512 << 10;
const STEP_MAX: usize = 256 << 20;
const WORTH: usize = 2 << 20;
/// The step now.
static STEP_NOW: AtomicUsize = AtomicUsize::new(STEP);

const UNSET: u8 = 0;
const READY: u8 = 1;
const OFF: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNSET);
/// The code's pages, and the cold part's among them (a length of zero for
/// none): each a start and a length.
static TEXT_START: AtomicUsize = AtomicUsize::new(0);
static TEXT_LEN: AtomicUsize = AtomicUsize::new(0);
static COLD_START: AtomicUsize = AtomicUsize::new(0);
static COLD_LEN: AtomicUsize = AtomicUsize::new(0);
/// On macOS, the binary (open for reading) and where its first byte is
/// mapped.
static IMAGE_FD: AtomicI32 = AtomicI32::new(-1);
static IMAGE_BASE: AtomicUsize = AtomicUsize::new(0);
/// Which pages a release takes (`WEAVEPY_TEXT_TRIM=cold|all|tiered`).
static MODE: AtomicU8 = AtomicU8::new(MODE_COLD);
const MODE_COLD: u8 = 0;
const MODE_ALL: u8 = 1;
const MODE_TIERED: u8 = 2;
/// How many releases there have been.
static RELEASES: AtomicUsize = AtomicUsize::new(0);
/// The resident set size at which the next release happens.
static NEXT_AT: AtomicUsize = AtomicUsize::new(FIRST_AT);
static BUSY: AtomicBool = AtomicBool::new(false);
static DEBUG: AtomicBool = AtomicBool::new(false);
/// When [`install`] ran.
fn started() -> std::time::Instant {
    static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *STARTED.get_or_init(std::time::Instant::now)
}

/// Find the cold part of this binary's code, for [`grown`] to release as
/// the process grows (see the module docs). Does nothing when the binary
/// isn't laid out for it, or `WEAVEPY_TEXT_TRIM=0`.
pub(crate) fn install() {
    match std::env::var_os("WEAVEPY_TEXT_TRIM") {
        Some(v) if v == "0" => {
            STATE.store(OFF, Ordering::Relaxed);
            return;
        }
        Some(v) => {
            // (`debug` reports each release on stderr.)
            let v = v.to_string_lossy().into_owned();
            for word in v.split(',') {
                match word {
                    "debug" => {
                        DEBUG.store(true, Ordering::Relaxed);
                        started();
                    }
                    "cold" => MODE.store(MODE_COLD, Ordering::Relaxed),
                    "all" => MODE.store(MODE_ALL, Ordering::Relaxed),
                    "tiered" => MODE.store(MODE_TIERED, Ordering::Relaxed),
                    _ => {}
                }
            }
        }
        None => {}
    }
    let ready = sys::locate().is_some_and(|image| {
        TEXT_START.store(image.text.0, Ordering::Relaxed);
        TEXT_LEN.store(image.text.1, Ordering::Relaxed);
        let (cold_start, cold_len) = image.cold.unwrap_or((0, 0));
        COLD_START.store(cold_start, Ordering::Relaxed);
        COLD_LEN.store(cold_len, Ordering::Relaxed);
        IMAGE_FD.store(image.fd, Ordering::Relaxed);
        IMAGE_BASE.store(image.base, Ordering::Relaxed);
        cold_len != 0 || MODE.load(Ordering::Relaxed) != MODE_COLD
    });
    STATE.store(if ready { READY } else { OFF }, Ordering::Release);
}

/// The process's resident set size, where the platform has a cheap way to
/// ask.
pub(crate) fn resident() -> Option<usize> {
    sys::resident()
}

/// The binary's code: its whole pages and the cold part's (each a start and
/// a length), and on macOS the file and where its first byte is mapped.
struct Image {
    text: (usize, usize),
    cold: Option<(usize, usize)>,
    fd: i32,
    base: usize,
}

/// The whole pages within `text` (a start and an end), as a start and a
/// length.
#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
fn whole_pages(text: (usize, usize), page: usize) -> Option<(usize, usize)> {
    let start = text.0.checked_add(page - 1)? & !(page - 1);
    let end = text.1 & !(page - 1);
    (start < end).then_some((start, end - start))
}

/// The addresses of the hot group's two markers.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
fn markers() -> (usize, usize) {
    (
        weavepy_text_hot_start as *const () as usize,
        weavepy_text_hot_end as *const () as usize,
    )
}

/// The cold part (a start and a length, in whole pages) between the hot
/// group's end and the end of the code `text` (a start and an end), when
/// the markers `hot` show an ordered binary.
#[cfg(any(target_vendor = "apple", target_os = "linux", test))]
fn cold_range(text: (usize, usize), hot: (usize, usize), page: usize) -> Option<(usize, usize)> {
    let (lo, hi) = text;
    let (hot_start, hot_end) = hot;
    if !(lo <= hot_start && hot_start < hot_end && hot_end < hi) || hot_end - hot_start < MIN_HOT {
        return None;
    }
    let start = hot_end.checked_add(page - 1)? & !(page - 1);
    let end = hi & !(page - 1);
    (start < end).then_some((start, end - start))
}

/// The process's resident set is `resident` bytes: release the cold code's
/// pages if that's a step more than after the last release.
pub(crate) fn grown(resident: usize) {
    if STATE.load(Ordering::Acquire) != READY {
        return;
    }
    if resident < NEXT_AT.load(Ordering::Relaxed) || BUSY.swap(true, Ordering::Acquire) {
        return;
    }
    let n = RELEASES.fetch_add(1, Ordering::Relaxed) + 1;
    let all = match MODE.load(Ordering::Relaxed) {
        MODE_ALL => true,
        // (The first, second, fourth, eighth... release.)
        MODE_TIERED => n.is_power_of_two(),
        _ => false,
    };
    let (start, len) = if all || COLD_LEN.load(Ordering::Relaxed) == 0 {
        (
            TEXT_START.load(Ordering::Relaxed),
            TEXT_LEN.load(Ordering::Relaxed),
        )
    } else {
        (
            COLD_START.load(Ordering::Relaxed),
            COLD_LEN.load(Ordering::Relaxed),
        )
    };
    let began = DEBUG.load(Ordering::Relaxed).then(std::time::Instant::now);
    // SAFETY: the range is whole pages of this binary's code, which the file
    // backs unchanged (`locate` checked, and `release` skips a traced
    // process, whose debugger may have written breakpoints).
    let kept = unsafe { sys::release(start, len) };
    if !kept {
        STATE.store(OFF, Ordering::Release);
    }
    let now = sys::resident().unwrap_or(resident);
    // A release that frees little found the cold part already empty, or
    // holding code the program keeps running, which comes straight back:
    // the next one waits for twice the growth.
    let mut step = STEP_NOW.load(Ordering::Relaxed);
    if resident.saturating_sub(now) < WORTH {
        step = step.saturating_mul(2).min(STEP_MAX);
        STEP_NOW.store(step, Ordering::Relaxed);
    }
    if let Some(began) = began {
        eprintln!(
            "text trim: {start:#x}+{len:#x} at {} ms: resident {} -> {} KiB in {} us, next step {} KiB",
            started().elapsed().as_millis(),
            resident >> 10,
            now >> 10,
            began.elapsed().as_micros(),
            step >> 10
        );
    }
    NEXT_AT.store(now + step.max(now / 16), Ordering::Relaxed);
    BUSY.store(false, Ordering::Release);
}

#[cfg(target_vendor = "apple")]
mod sys {
    use super::{cold_range, markers, whole_pages, Image, Ordering, IMAGE_BASE, IMAGE_FD};
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

    /// The executable's `__text` section's cold part, and the file open:
    /// `None` when the markers aren't in the executable (the runtime is in a
    /// library), the binary isn't ordered, or the file at its path isn't
    /// what's mapped (a universal binary, whose slice starts elsewhere in
    /// the file, or a file replaced since).
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
        let pages = whole_pages(section, page)?;
        let cold = cold_range(section, markers(), page);
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
            cold,
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
    use super::{cold_range, markers, whole_pages, Image};
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

    /// The executable segment the markers are in, and its cold part.
    pub(super) fn locate() -> Option<Image> {
        let mut find = Find {
            addr: super::weavepy_text_hot_end as *const () as usize,
            text: None,
        };
        // SAFETY: the callback reads what the loader hands it.
        unsafe { libc::dl_iterate_phdr(Some(segment), (&raw mut find).cast()) };
        // SAFETY: `sysconf` has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        let text = find.text?;
        Some(Image {
            text: whole_pages(text, page)?,
            cold: cold_range(text, markers(), page),
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
    use super::{cold_range, MIN_HOT};

    const PAGE: usize = 4096;
    const TEXT: (usize, usize) = (0x10_0000, 0x90_0000);

    #[test]
    fn the_cold_part_is_whole_pages_after_the_hot_group() {
        let hot = (TEXT.0 + 0x900, TEXT.0 + 0x30_0123);
        assert_eq!(
            cold_range(TEXT, hot, PAGE),
            Some((TEXT.0 + 0x30_1000, 0x80_0000 - 0x30_1000))
        );
        // (An end within a page keeps that page.)
        let text = (TEXT.0, TEXT.1 + 0x10);
        assert_eq!(
            cold_range(text, hot, PAGE),
            Some((TEXT.0 + 0x30_1000, 0x80_0000 - 0x30_1000))
        );
    }

    #[test]
    fn an_unordered_binary_has_no_cold_part() {
        // The markers side by side, as the compiler emitted them.
        assert_eq!(
            cold_range(TEXT, (TEXT.0 + 0x5000, TEXT.0 + 0x5010), PAGE),
            None
        );
        // The wrong way around.
        let far = TEXT.0 + 4 * MIN_HOT;
        assert_eq!(cold_range(TEXT, (far, TEXT.0 + 0x900), PAGE), None);
        // In another image than the code.
        assert_eq!(cold_range(TEXT, (0x900, far), PAGE), None);
        assert_eq!(cold_range(TEXT, (TEXT.0, TEXT.1 + PAGE), PAGE), None);
    }

    #[test]
    fn a_hot_group_up_to_the_last_page_leaves_nothing() {
        assert_eq!(cold_range(TEXT, (TEXT.0, TEXT.1 - 0x10), PAGE), None);
    }
}
