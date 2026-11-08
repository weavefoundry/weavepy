//! Executable memory: arenas whose functions are packed together (so
//! their code shares pages and cache lines). The process-wide arena is
//! never freed; a [`CodeArena`] unmaps its chunks when dropped.
//!
//! On x86-64, code memory comes from one region reserved within a 32-bit
//! displacement of this program's own code where the system allows (see
//! [`near`]): a call of one of its functions is then a 5-byte `call rel32`
//! instead of a 10-byte address load and an indirect call, and an address
//! of its code or data a 7-byte `lea` (see [`reaches`]).

use std::sync::Mutex;

use crate::{Code, Error};

/// Whether code installed anywhere in code memory reaches `target` with a
/// 32-bit displacement.
#[inline]
pub(crate) fn reaches(target: u64) -> bool {
    #[cfg(all(unix, target_arch = "x86_64"))]
    {
        near::region().is_some_and(|r| r.reaches(target))
    }
    #[cfg(not(all(unix, target_arch = "x86_64")))]
    {
        let _ = target;
        false
    }
}

/// A chunk of `size` bytes (a multiple of the page size) for code,
/// writable and executable, and whether it's in the near region (which
/// code with displacements needs).
fn map_code(size: usize) -> Option<(*mut u8, bool)> {
    #[cfg(all(unix, target_arch = "x86_64"))]
    if let Some(p) = near::region().and_then(|r| r.chunk(size)) {
        return Some((p, true));
    }
    sys::map(size, true).map(|p| (p, false))
}

/// The code region near this program's code (x86-64 Unix).
#[cfg(all(unix, target_arch = "x86_64"))]
mod near {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;

    /// The address space reserved (never more than code needs is mapped).
    const SIZE: usize = 1 << 30;
    /// The farthest a displacement may reach, with room to spare.
    const LIMIT: u64 = (1 << 31) - (1 << 16);
    /// How far from the anchor this program's code may lie and still be
    /// in reach from all of the region.
    const TEXT: u64 = 256 << 20;

    pub(crate) struct Region {
        lo: usize,
        hi: usize,
        next: AtomicUsize,
    }

    static REGION: OnceLock<Option<Region>> = OnceLock::new();

    fn dist(a: u64, b: u64) -> u64 {
        a.abs_diff(b)
    }

    /// Reserve the region: an inaccessible mapping where the system puts
    /// one near the anchor, a function of this program. `None` when no
    /// hint lands in reach (`WPCG_FAR=1` declines, a tuning aid).
    fn reserve() -> Option<Region> {
        if std::env::var_os("WPCG_FAR").is_some_and(|v| v != "0") {
            return None;
        }
        let anchor = reserve as fn() -> Option<Region> as usize as u64;
        let mb = 1i64 << 20;
        for off in [256 * mb, 512 * mb, -(1280 * mb), -(1536 * mb), 640 * mb] {
            let hint = (anchor as i64 + off) as u64 & !0xffff;
            // SAFETY: an anonymous, inaccessible mapping at no fixed address.
            let p = unsafe {
                libc::mmap(
                    hint as *mut libc::c_void,
                    SIZE,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                )
            };
            if p == libc::MAP_FAILED {
                continue;
            }
            let (lo, hi) = (p as u64, p as u64 + SIZE as u64);
            if dist(lo, anchor) + TEXT < LIMIT && dist(hi, anchor) + TEXT < LIMIT {
                return Some(Region {
                    lo: lo as usize,
                    hi: hi as usize,
                    next: AtomicUsize::new(lo as usize),
                });
            }
            // SAFETY: the mapping just made.
            unsafe { libc::munmap(p, SIZE) };
        }
        None
    }

    pub(crate) fn region() -> Option<&'static Region> {
        REGION.get_or_init(reserve).as_ref()
    }

    impl Region {
        pub(crate) fn reaches(&self, target: u64) -> bool {
            dist(target, self.lo as u64) < LIMIT && dist(target, self.hi as u64) < LIMIT
        }

        /// The next `size` bytes of the region, made writable and
        /// executable.
        pub(crate) fn chunk(&self, size: usize) -> Option<*mut u8> {
            let at = self.next.fetch_add(size, Ordering::Relaxed);
            if at + size > self.hi {
                return None;
            }
            let prot = libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC;
            // SAFETY: part of the reservation, handed out once.
            (unsafe { libc::mprotect(at as *mut libc::c_void, size, prot) } == 0)
                .then_some(at as *mut u8)
        }
    }
}

/// The process-wide arena grows by chunks at least this large; a
/// [`CodeArena`] starts smaller and doubles.
const CHUNK: usize = 1 << 20;
const FIRST_OWN_CHUNK: usize = 64 << 10;

/// Bytes mapped for code, for statistics.
static MAPPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn mapped_bytes() -> usize {
    MAPPED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Code memory: chunks mapped writable and executable at once (or, where
/// the system refuses that, a mapping per function, made executable once
/// written), packed from the newest chunk.
#[derive(Debug)]
pub struct CodeArena {
    /// The current chunk's next free byte and its end.
    at: usize,
    end: usize,
    rwx: bool,
    /// Whether the current chunk is in the near region.
    near: bool,
    /// Every mapping, for the drop.
    maps: Vec<(usize, usize)>,
    /// The next chunk's size.
    next: usize,
}

static GLOBAL: Mutex<CodeArena> = Mutex::new(CodeArena {
    at: 0,
    end: 0,
    rwx: true,
    near: false,
    maps: Vec::new(),
    next: CHUNK,
});

impl Default for CodeArena {
    fn default() -> Self {
        CodeArena::new()
    }
}

impl CodeArena {
    pub const fn new() -> Self {
        CodeArena {
            at: 0,
            end: 0,
            rwx: true,
            near: false,
            maps: Vec::new(),
            next: FIRST_OWN_CHUNK,
        }
    }

    /// Copy `code` into executable memory, its displacements written: its
    /// address, valid until the arena drops.
    pub fn install(&mut self, code: &Code) -> Result<*const u8, Error> {
        let len = code.len().max(1);
        if self.rwx {
            let start = (self.at + 15) & !15;
            // (Code with displacements goes in the near region only.)
            if self.at == 0 || start + len > self.end || (!self.near && !code.relocs.is_empty()) {
                let size = len.max(self.next).next_multiple_of(sys::page());
                match map_code(size) {
                    Some((p, near)) => {
                        MAPPED.fetch_add(size, std::sync::atomic::Ordering::Relaxed);
                        self.maps.push((p as usize, size));
                        self.at = p as usize;
                        self.end = p as usize + size;
                        self.near = near;
                        self.next = (self.next * 2).min(CHUNK);
                    }
                    None => self.rwx = false,
                }
            }
            if self.rwx && (self.near || code.relocs.is_empty()) {
                let start = (self.at + 15) & !15;
                let bytes = code.placed(start)?;
                crate::dump(&bytes);
                self.at = start + len;
                let p = start as *mut u8;
                // SAFETY: `p..p + len` is in a writable chunk this arena
                // owns, handed out once.
                unsafe { sys::write(p, &bytes) };
                return Ok(p);
            }
        }
        if !code.relocs.is_empty() {
            return Err(Error::Memory);
        }
        let code = &code.bytes;
        crate::dump(code);
        // Pages of its own, written and then made executable.
        let size = len.next_multiple_of(sys::page());
        let p = sys::map(size, false).ok_or(Error::Memory)?;
        MAPPED.fetch_add(size, std::sync::atomic::Ordering::Relaxed);
        self.maps.push((p as usize, size));
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
}

impl Drop for CodeArena {
    fn drop(&mut self) {
        for &(p, size) in &self.maps {
            // SAFETY: a mapping this arena made; its code is no longer
            // entered (the owner's contract).
            unsafe { sys::unmap(p as *mut u8, size) };
            MAPPED.fetch_sub(size, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Copy `code` into the process-wide arena: its address, valid for the
/// process.
pub(crate) fn install(code: &Code) -> Result<*const u8, Error> {
    GLOBAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .install(code)
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
        #[allow(unused_mut)]
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

    /// # Safety
    /// `p..p + size` must be a mapping this module made.
    pub(super) unsafe fn unmap(p: *mut u8, size: usize) {
        // SAFETY: per the caller.
        unsafe { libc::munmap(p.cast(), size) };
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
        VirtualAlloc, VirtualFree, VirtualProtect, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
        PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_READWRITE,
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

    /// # Safety
    /// `p` must be an allocation this module made.
    pub(super) unsafe fn unmap(p: *mut u8, _size: usize) {
        // SAFETY: per the caller.
        unsafe { VirtualFree(p.cast(), 0, MEM_RELEASE) };
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
