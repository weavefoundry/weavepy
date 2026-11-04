//! Executable memory: one arena the process never frees, its functions
//! packed together (so their code shares pages and cache lines).

use std::sync::Mutex;

use crate::Error;

/// The arena grows by chunks at least this large.
const CHUNK: usize = 1 << 20;

struct Arena {
    /// The current chunk's next free byte and its end.
    at: usize,
    end: usize,
    /// Whether chunks are writable and executable at once; otherwise each
    /// function gets pages of its own, made executable once written.
    rwx: bool,
}

static ARENA: Mutex<Arena> = Mutex::new(Arena {
    at: 0,
    end: 0,
    rwx: true,
});

/// Bytes mapped for code, for statistics.
static MAPPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn mapped_bytes() -> usize {
    MAPPED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Copy `code` into executable memory: its address.
pub fn install(code: &[u8]) -> Result<*const u8, Error> {
    let mut arena = ARENA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let len = code.len().max(1);
    if arena.rwx {
        let start = (arena.at + 15) & !15;
        if arena.at == 0 || start + len > arena.end {
            let size = len.next_multiple_of(CHUNK);
            match sys::map(size, true) {
                Some(p) => {
                    MAPPED.fetch_add(size, std::sync::atomic::Ordering::Relaxed);
                    arena.at = p as usize;
                    arena.end = p as usize + size;
                }
                None => arena.rwx = false,
            }
        }
        if arena.rwx {
            let start = (arena.at + 15) & !15;
            arena.at = start + len;
            let p = start as *mut u8;
            // SAFETY: `p..p + len` is in a writable chunk this arena owns,
            // handed out once.
            unsafe { sys::write(p, code) };
            return Ok(p);
        }
    }
    // Pages of its own, written and then made executable.
    let size = len.next_multiple_of(sys::page());
    let p = sys::map(size, false).ok_or(Error::Memory)?;
    MAPPED.fetch_add(size, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: a fresh writable mapping of `size >= len` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(code.as_ptr(), p, code.len());
    }
    if !sys::protect_exec(p, size) {
        return Err(Error::Memory);
    }
    sys::flush(p, len);
    Ok(p)
}

#[cfg(unix)]
mod sys {
    pub(super) fn page() -> usize {
        // SAFETY: sysconf has no preconditions.
        let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if n > 0 {
            n as usize
        } else {
            4096
        }
    }

    /// Map `size` bytes, executable as well as writable if `exec`.
    pub(super) fn map(size: usize, exec: bool) -> Option<*mut u8> {
        let mut prot = libc::PROT_READ | libc::PROT_WRITE;
        let mut flags = libc::MAP_PRIVATE | libc::MAP_ANON;
        if exec {
            prot |= libc::PROT_EXEC;
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            {
                flags |= libc::MAP_JIT;
            }
        }
        // SAFETY: an anonymous mapping, no address requested.
        let p = unsafe { libc::mmap(std::ptr::null_mut(), size, prot, flags, -1, 0) };
        if p == libc::MAP_FAILED {
            None
        } else {
            Some(p.cast())
        }
    }

    pub(super) fn protect_exec(p: *mut u8, size: usize) -> bool {
        // SAFETY: `p..p + size` is a mapping this module made.
        unsafe { libc::mprotect(p.cast(), size, libc::PROT_READ | libc::PROT_EXEC) == 0 }
    }

    /// Write `code` at `p` in a writable and executable chunk.
    ///
    /// # Safety
    /// `p..p + code.len()` must be in such a chunk, and not running.
    pub(super) unsafe fn write(p: *mut u8, code: &[u8]) {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        // SAFETY: toggles this thread's view of MAP_JIT pages.
        unsafe {
            pthread_jit_write_protect_np(0);
        }
        // SAFETY: per the caller.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), p, code.len()) };
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        // SAFETY: as above.
        unsafe {
            pthread_jit_write_protect_np(1);
        }
        flush(p, code.len());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    extern "C" {
        fn pthread_jit_write_protect_np(enabled: libc::c_int);
        fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
    }

    #[allow(unused_variables)]
    pub(super) fn flush(p: *mut u8, len: usize) {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        // SAFETY: `p..p + len` is mapped.
        unsafe {
            sys_icache_invalidate(p.cast(), len);
        }
        #[cfg(all(not(target_os = "macos"), target_arch = "aarch64"))]
        super::flush_aarch64(p, len);
    }
}

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READ,
        PAGE_EXECUTE_READWRITE, PAGE_READWRITE,
    };

    pub(super) fn page() -> usize {
        4096
    }

    pub(super) fn map(size: usize, exec: bool) -> Option<*mut u8> {
        let prot = if exec {
            PAGE_EXECUTE_READWRITE
        } else {
            PAGE_READWRITE
        };
        // SAFETY: a fresh reservation, no address requested.
        let p = unsafe { VirtualAlloc(std::ptr::null(), size, MEM_COMMIT | MEM_RESERVE, prot) };
        if p.is_null() {
            None
        } else {
            Some(p.cast())
        }
    }

    pub(super) fn protect_exec(p: *mut u8, size: usize) -> bool {
        let mut old = 0;
        // SAFETY: `p..p + size` is an allocation this module made.
        unsafe { VirtualProtect(p.cast(), size, PAGE_EXECUTE_READ, &mut old) != 0 }
    }

    /// # Safety
    /// `p..p + code.len()` must be writable, and not running.
    pub(super) unsafe fn write(p: *mut u8, code: &[u8]) {
        // SAFETY: per the caller.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), p, code.len()) };
    }

    pub(super) fn flush(_p: *mut u8, _len: usize) {}
}

/// Make freshly written code at `p..p + len` visible to instruction
/// fetch.
#[cfg(all(not(target_os = "macos"), target_arch = "aarch64"))]
fn flush_aarch64(p: *mut u8, len: usize) {
    let ctr: u64;
    // SAFETY: reads the cache type register, readable at EL0.
    unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr) };
    let dline = 4usize << ((ctr >> 16) & 15);
    let iline = 4usize << (ctr & 15);
    let start = p as usize;
    let end = start + len;
    let mut a = start & !(dline - 1);
    while a < end {
        // SAFETY: cleans a mapped line to the point of unification.
        unsafe { std::arch::asm!("dc cvau, {}", in(reg) a) };
        a += dline;
    }
    // SAFETY: barriers.
    unsafe { std::arch::asm!("dsb ish") };
    let mut a = start & !(iline - 1);
    while a < end {
        // SAFETY: invalidates a mapped line in the instruction cache.
        unsafe { std::arch::asm!("ic ivau, {}", in(reg) a) };
        a += iline;
    }
    // SAFETY: barriers.
    unsafe { std::arch::asm!("dsb ish", "isb") };
}
