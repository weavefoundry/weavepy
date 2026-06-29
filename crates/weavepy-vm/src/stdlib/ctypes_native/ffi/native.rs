//! Self-contained native FFI back-end for the `_ctypes` bridge.
//!
//! This replaces the external `libffi` dependency (whose vendored assembly
//! does not build under the current Apple `clang` toolchain) with a small,
//! hand-written call gate and closure trampoline pool. It provides exactly
//! two mechanisms, both expressed in terms of a register-file image:
//!
//! * [`raw_call`] — given a resolved function address and the general/FP
//!   register file plus any overflow stack words, performs a real C ABI
//!   call and returns the integer (`x0`/`rax`) and floating (`d0`/`xmm0`)
//!   result registers. The caller ([`super`]) places each argument into the
//!   correct register/stack slot per the platform calling convention
//!   (driven by [`super::assign_slots`], using [`NGPR_ARG`]/[`NFPR_ARG`]).
//!
//! * [`alloc_trampoline`]/[`free_trampoline`] — hand out a stable, C-callable
//!   code address backed by a fixed pool of assembly stubs. Each stub records
//!   its own address, jumps to a shared spill routine that snapshots the
//!   argument registers, and calls back into Rust ([`wp_cl_dispatch`] ->
//!   [`super::closure_dispatch`]) with the register-file image. Because the
//!   stubs live in the normal `.text` segment there is no runtime code
//!   generation and therefore no W^X / `MAP_JIT` handling to worry about.
//!
//! Only `aarch64` and `x86_64` have an ABI implementation; on any other
//! target [`SUPPORTED`] is `false` and both entry points degrade cleanly
//! (the frozen `_ctypes.py` treats a missing closure back-end as "callbacks
//! are Python-callable only").

#![allow(dead_code)]

use std::os::raw::c_void;

/// Argument/return register file handed to the assembly call gate. Layout
/// is load-bearing: the field byte offsets are hard-coded in the gate asm.
#[repr(C)]
struct RawCall {
    fnptr: *const c_void, // +0
    gpr: *const u64,      // +8   -> up to 8 integer/pointer registers
    fpr: *const u64,      // +16  -> up to 8 FP registers (low 64 bits each)
    stack: *const u64,    // +24  -> overflow stack words (may be null if 0)
    stack_words: u64,     // +32
    nfpr: u64,            // +40  -> # FP regs used (x86-64 variadic `al`)
    ret_gpr: *mut u64,    // +48  -> receives x0 / rax
    ret_fpr: *mut u64,    // +56  -> receives d0 / xmm0
}

/// A snapshot of the argument registers as seen by a closure trampoline,
/// handed to [`super::closure_dispatch`]. All pointers reference stack
/// storage owned by the trampoline frame and are valid only for the
/// duration of the dispatch call.
pub(super) struct ClosureRegs {
    gpr: *const u64,
    fpr: *const u64,
    stack: *const u64,
    pub(super) ret_gpr: *mut u64,
    pub(super) ret_fpr: *mut u64,
}

impl ClosureRegs {
    /// Read integer/pointer argument register `i` (0-based).
    ///
    /// # Safety
    /// `i` must be a register index the trampoline actually spilled.
    pub(super) unsafe fn gpr(&self, i: usize) -> u64 {
        unsafe { *self.gpr.add(i) }
    }
    /// Read FP argument register `i` (low 64 bits).
    ///
    /// # Safety
    /// `i` must be a register index the trampoline actually spilled.
    pub(super) unsafe fn fpr(&self, i: usize) -> u64 {
        unsafe { *self.fpr.add(i) }
    }
    /// Read overflow stack word `i` (0 == first stacked argument).
    ///
    /// # Safety
    /// `i` must index a word the caller actually pushed.
    pub(super) unsafe fn stack(&self, i: usize) -> u64 {
        unsafe { *self.stack.add(i) }
    }
}

// ================================================================
// aarch64 (AAPCS64, incl. Apple silicon)
// ================================================================

#[cfg(target_arch = "aarch64")]
mod abi {
    /// Integer/pointer argument registers: x0..x7.
    pub(super) const NGPR: usize = 8;
    /// Bytes between consecutive trampoline stubs (`adr`+`b` = 8 bytes).
    pub(super) const STUB_SIZE: usize = 8;
    /// `adr x17, .` yields the stub base itself, so no bias.
    pub(super) const LEA_BIAS: usize = 0;
}

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".p2align 2",
    ".globl {gate}",
    "{gate}:",
    "  stp x29, x30, [sp, #-32]!",
    "  mov x29, sp",
    "  stp x19, x20, [sp, #16]",
    "  mov x19, x0",         // x19 = &RawCall
    "  ldr x9,  [x19, #32]", // stack_words
    "  add x10, x9, #1",     // round up to even for 16-byte stack alignment
    "  and x10, x10, #0xfffffffffffffffe",
    "  lsl x10, x10, #3",    // -> bytes
    "  sub sp, sp, x10",
    "  ldr x11, [x19, #24]", // stack src
    "  cbz x9, 2f",
    "  mov x12, #0",
    "1:",
    "  ldr x13, [x11, x12, lsl #3]",
    "  str x13, [sp, x12, lsl #3]",
    "  add x12, x12, #1",
    "  cmp x12, x9",
    "  b.lo 1b",
    "2:",
    "  ldr x14, [x19, #16]", // fpr src
    "  ldp d0, d1, [x14, #0]",
    "  ldp d2, d3, [x14, #16]",
    "  ldp d4, d5, [x14, #32]",
    "  ldp d6, d7, [x14, #48]",
    "  ldr x20, [x19, #0]",  // fnptr
    "  ldr x16, [x19, #8]",  // gpr src
    "  ldp x0, x1, [x16, #0]",
    "  ldp x2, x3, [x16, #16]",
    "  ldp x4, x5, [x16, #32]",
    "  ldp x6, x7, [x16, #48]",
    "  blr x20",
    "  ldr x9, [x19, #48]",  // ret_gpr
    "  str x0, [x9]",
    "  ldr x9, [x19, #56]",  // ret_fpr
    "  str d0, [x9]",
    "  mov sp, x29",
    "  ldp x19, x20, [sp, #16]",
    "  ldp x29, x30, [sp], #32",
    "  ret",
    gate = sym wp_ffi_call_gate,
);

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".p2align 4",
    ".globl {pool}",
    "{pool}:",
    ".rept {n}",
    "  adr x17, .", // x17 = this stub's address
    "  b 9f",       // -> shared spill routine
    ".endr",
    ".p2align 2",
    "9:",
    "  stp x29, x30, [sp, #-16]!",
    "  mov x29, sp",
    "  sub sp, sp, #160",
    "  stp x0, x1, [sp, #0]", // spill integer args
    "  stp x2, x3, [sp, #16]",
    "  stp x4, x5, [sp, #32]",
    "  stp x6, x7, [sp, #48]",
    "  stp d0, d1, [sp, #64]", // spill FP args
    "  stp d2, d3, [sp, #80]",
    "  stp d4, d5, [sp, #96]",
    "  stp d6, d7, [sp, #112]",
    "  mov x0, x17",   // stub_addr
    "  add x1, sp, #0", // gpr
    "  add x2, sp, #64", // fpr
    "  add x3, x29, #16", // incoming stack args
    "  add x4, sp, #128", // ret_gpr
    "  add x5, sp, #136", // ret_fpr
    "  bl {dispatch}",
    "  ldr x0, [sp, #128]",
    "  ldr d0, [sp, #136]",
    "  add sp, sp, #160",
    "  ldp x29, x30, [sp], #16",
    "  ret",
    pool = sym wp_cl_pool,
    dispatch = sym wp_cl_dispatch,
    n = const POOL_SIZE,
);

// ================================================================
// x86-64 (System V AMD64) — Intel syntax (Rust's default).
// ================================================================

#[cfg(target_arch = "x86_64")]
mod abi {
    /// Integer/pointer argument registers: rdi, rsi, rdx, rcx, r8, r9.
    pub(super) const NGPR: usize = 6;
    /// Bytes between consecutive trampoline stubs (padded to 16).
    pub(super) const STUB_SIZE: usize = 16;
    /// `lea r11, [rip]` yields the address *after* the 7-byte `lea`.
    pub(super) const LEA_BIAS: usize = 7;
}

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".p2align 4",
    ".globl {gate}",
    "{gate}:",
    "  push rbp",
    "  mov rbp, rsp",
    "  push rbx",
    "  push r12",
    "  mov rbx, rdi",      // rbx = &RawCall
    "  mov r12, [rbx+32]", // stack_words
    "  lea rax, [r12*8]",
    "  add rax, 15",
    "  and rax, -16", // 16-byte-aligned overflow area
    "  sub rsp, rax",
    "  mov rcx, [rbx+24]", // stack src
    "  xor r11, r11",
    "3:",
    "  cmp r11, r12",
    "  jae 4f",
    "  mov rax, [rcx + r11*8]",
    "  mov [rsp + r11*8], rax",
    "  add r11, 1",
    "  jmp 3b",
    "4:",
    "  mov rax, [rbx+16]", // fpr src
    "  movsd xmm0, [rax+0]",
    "  movsd xmm1, [rax+8]",
    "  movsd xmm2, [rax+16]",
    "  movsd xmm3, [rax+24]",
    "  movsd xmm4, [rax+32]",
    "  movsd xmm5, [rax+40]",
    "  movsd xmm6, [rax+48]",
    "  movsd xmm7, [rax+56]",
    "  mov r10, [rbx+8]", // gpr src
    "  mov rdi, [r10+0]",
    "  mov rsi, [r10+8]",
    "  mov rdx, [r10+16]",
    "  mov rcx, [r10+24]",
    "  mov r8,  [r10+32]",
    "  mov r9,  [r10+40]",
    "  mov rax, [rbx+40]", // nfpr -> al (variadic)
    "  mov r11, [rbx+0]",  // fnptr
    "  call r11",
    "  mov r10, [rbx+48]", // ret_gpr
    "  mov [r10], rax",
    "  mov r10, [rbx+56]", // ret_fpr
    "  movsd [r10], xmm0",
    "  lea rsp, [rbp-16]",
    "  pop r12",
    "  pop rbx",
    "  pop rbp",
    "  ret",
    gate = sym wp_ffi_call_gate,
);

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".p2align 4",
    ".globl {pool}",
    "{pool}:",
    ".rept {n}",
    "  .p2align 4",     // fixed 16-byte stub stride regardless of jmp encoding
    "  lea r11, [rip]", // r11 = stub base + 7
    "  jmp 9f",
    ".endr",
    ".p2align 4",
    "9:",
    "  push rbp",
    "  mov rbp, rsp",
    "  sub rsp, 128",
    "  mov [rsp+0], rdi", // spill integer args
    "  mov [rsp+8], rsi",
    "  mov [rsp+16], rdx",
    "  mov [rsp+24], rcx",
    "  mov [rsp+32], r8",
    "  mov [rsp+40], r9",
    "  movsd [rsp+48], xmm0", // spill FP args
    "  movsd [rsp+56], xmm1",
    "  movsd [rsp+64], xmm2",
    "  movsd [rsp+72], xmm3",
    "  movsd [rsp+80], xmm4",
    "  movsd [rsp+88], xmm5",
    "  movsd [rsp+96], xmm6",
    "  movsd [rsp+104], xmm7",
    "  mov rdi, r11",     // stub_addr
    "  lea rsi, [rsp+0]", // gpr
    "  lea rdx, [rsp+48]", // fpr
    "  lea rcx, [rbp+16]", // incoming stack args
    "  lea r8,  [rsp+112]", // ret_gpr
    "  lea r9,  [rsp+120]", // ret_fpr
    "  call {dispatch}",
    "  mov rax, [rsp+112]",
    "  movsd xmm0, [rsp+120]",
    "  mov rsp, rbp",
    "  pop rbp",
    "  ret",
    pool = sym wp_cl_pool,
    dispatch = sym wp_cl_dispatch,
    n = const POOL_SIZE,
);

// ================================================================
// Shared machinery (aarch64 + x86-64)
// ================================================================

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use core::sync::atomic::{AtomicPtr, Ordering};
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use std::sync::Mutex;

/// Number of pre-allocated closure trampolines. ctypes callbacks are few in
/// practice; freed slots are recycled ([`free_trampoline`]) so this bounds
/// *live* callbacks, not total ever created.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(super) const POOL_SIZE: usize = 1024;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(super) const NGPR_ARG: usize = abi::NGPR;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(super) const NFPR_ARG: usize = 8;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(super) const SUPPORTED: bool = true;

// Defined by the `global_asm!` blocks above (their `sym` operands resolve
// these names in this module's scope).
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
extern "C" {
    fn wp_ffi_call_gate(call: *const RawCall);
    fn wp_cl_pool();
}

/// Execute a real C ABI call. `gpr`/`fpr` hold the integer and FP register
/// files (only the ABI-relevant prefix is consumed); `stack` holds any
/// overflow words; `nfpr` is the FP-register count for x86-64 variadic
/// calls. Returns `(x0/rax, d0/xmm0)`.
///
/// # Safety
/// `fnptr` must be a valid function whose real C signature matches the
/// register/stack placement the caller performed; pointer arguments must
/// outlive the call.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(super) unsafe fn raw_call(
    fnptr: usize,
    gpr: &[u64; 8],
    fpr: &[u64; 8],
    stack: &[u64],
    nfpr: u64,
) -> (u64, u64) {
    let mut ret_gpr = 0u64;
    let mut ret_fpr = 0u64;
    let call = RawCall {
        fnptr: fnptr as *const c_void,
        gpr: gpr.as_ptr(),
        fpr: fpr.as_ptr(),
        stack: if stack.is_empty() {
            std::ptr::null()
        } else {
            stack.as_ptr()
        },
        stack_words: stack.len() as u64,
        nfpr,
        ret_gpr: &mut ret_gpr,
        ret_fpr: &mut ret_fpr,
    };
    unsafe { wp_ffi_call_gate(&call) };
    (ret_gpr, ret_fpr)
}

/// Per-slot user-data pointers (leaked `ClosureData`), read lock-free by the
/// trampoline dispatch path.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
static SLOT_DATA: [AtomicPtr<c_void>; POOL_SIZE] =
    [const { AtomicPtr::new(std::ptr::null_mut()) }; POOL_SIZE];

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
struct AllocState {
    next: usize,
    free: Vec<usize>,
}
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
static ALLOC: Mutex<AllocState> = Mutex::new(AllocState {
    next: 0,
    free: Vec::new(),
});

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn pool_base() -> usize {
    wp_cl_pool as *const () as usize
}

/// Bind `userdata` to a free trampoline slot and return its C-callable code
/// address, or `None` if the pool is exhausted.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(super) fn alloc_trampoline(userdata: *mut c_void) -> Option<usize> {
    let slot = {
        let mut st = ALLOC.lock().unwrap();
        if let Some(s) = st.free.pop() {
            s
        } else if st.next < POOL_SIZE {
            let s = st.next;
            st.next += 1;
            s
        } else {
            return None;
        }
    };
    SLOT_DATA[slot].store(userdata, Ordering::Release);
    Some(pool_base() + slot * abi::STUB_SIZE)
}

/// Release the trampoline at `code_addr`, returning the `userdata` pointer
/// previously bound (so the caller can reclaim it). Returns `None` if the
/// address is not a live trampoline in this pool.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(super) fn free_trampoline(code_addr: usize) -> Option<*mut c_void> {
    let base = pool_base();
    if code_addr < base {
        return None;
    }
    let off = code_addr - base;
    if off % abi::STUB_SIZE != 0 {
        return None;
    }
    let slot = off / abi::STUB_SIZE;
    if slot >= POOL_SIZE {
        return None;
    }
    let prev = SLOT_DATA[slot].swap(std::ptr::null_mut(), Ordering::AcqRel);
    if prev.is_null() {
        return None;
    }
    ALLOC.lock().unwrap().free.push(slot);
    Some(prev)
}

/// Trampoline dispatch entry (called from the shared spill routine). Recovers
/// the slot from the stub's self-reported address, loads the bound
/// `ClosureData`, and hands the register-file image to the Rust marshaller.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
extern "C" fn wp_cl_dispatch(
    stub_addr: usize,
    gpr: *const u64,
    fpr: *const u64,
    stack: *const u64,
    ret_gpr: *mut u64,
    ret_fpr: *mut u64,
) {
    let base = pool_base();
    let slot = stub_addr
        .wrapping_sub(base)
        .wrapping_sub(abi::LEA_BIAS)
        / abi::STUB_SIZE;
    let userdata = if slot < POOL_SIZE {
        SLOT_DATA[slot].load(Ordering::Acquire)
    } else {
        std::ptr::null_mut()
    };
    let regs = ClosureRegs {
        gpr,
        fpr,
        stack,
        ret_gpr,
        ret_fpr,
    };
    super::closure_dispatch(userdata, &regs);
}

// ================================================================
// Unsupported architectures: clean, no-asm fallbacks.
// ================================================================

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(super) const NGPR_ARG: usize = 8;
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(super) const NFPR_ARG: usize = 8;
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(super) const SUPPORTED: bool = false;

/// # Safety
/// Never called: [`SUPPORTED`] is `false`, so every call site is guarded.
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(super) unsafe fn raw_call(
    _fnptr: usize,
    _gpr: &[u64; 8],
    _fpr: &[u64; 8],
    _stack: &[u64],
    _nfpr: u64,
) -> (u64, u64) {
    (0, 0)
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(super) fn alloc_trampoline(_userdata: *mut c_void) -> Option<usize> {
    None
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(super) fn free_trampoline(_code_addr: usize) -> Option<*mut c_void> {
    None
}
