//! The x86-64 assembler: System V and Win64.

use crate::asm::{bit, is_float, Abi, Asm, Cond, Frame, Label, Opnd, Reg, FLOAT};
use crate::ir::{condcodes::FloatCC, Alu, CallConv, Div, Ext, Fop, Overflow, Type};
use crate::Error;

const RAX: Reg = 0;
const RCX: Reg = 1;
const RDX: Reg = 2;
const RBX: Reg = 3;
const RSP: Reg = 4;
const RBP: Reg = 5;
const RSI: Reg = 6;
const RDI: Reg = 7;
const R8: Reg = 8;
const R9: Reg = 9;
const R10: Reg = 10;
const R11: Reg = 11;
const R12: Reg = 12;
const R13: Reg = 13;
const R14: Reg = 14;
const R15: Reg = 15;

const fn xmm(n: u8) -> Reg {
    FLOAT + n
}

const fn mask(regs: &[Reg]) -> u64 {
    let mut m = 0;
    let mut i = 0;
    while i < regs.len() {
        m |= 1u64 << regs[i];
        i += 1;
    }
    m
}

const ALL_XMM: u64 = 0xffff_u64 << FLOAT;

static SYSV: Abi = Abi {
    int_args: &[RDI, RSI, RDX, RCX, R8, R9],
    float_args: &[xmm(0), xmm(1), xmm(2), xmm(3), xmm(4), xmm(5), xmm(6), xmm(7)],
    positional: false,
    int_rets: &[RAX, RDX],
    float_rets: &[xmm(0), xmm(1)],
    clobbers: mask(&[RAX, RCX, RDX, RSI, RDI, R8, R9, R10, R11]) | ALL_XMM,
    shadow: 0,
};

static WIN64: Abi = Abi {
    int_args: &[RCX, RDX, R8, R9],
    float_args: &[xmm(0), xmm(1), xmm(2), xmm(3)],
    positional: true,
    int_rets: &[RAX],
    float_rets: &[xmm(0)],
    clobbers: mask(&[RAX, RCX, RDX, R8, R9, R10, R11]) | (0x3f_u64 << FLOAT),
    shadow: 32,
};

/// Condition codes, as `jcc`/`setcc`/`cmovcc` encode them.
fn cc(c: Cond) -> u8 {
    match c {
        Cond::O => 0x0,
        Cond::No => 0x1,
        Cond::Ult => 0x2,
        Cond::Uge => 0x3,
        Cond::Eq => 0x4,
        Cond::Ne => 0x5,
        Cond::Ule => 0x6,
        Cond::Ugt => 0x7,
        Cond::Slt => 0xc,
        Cond::Sge => 0xd,
        Cond::Sle => 0xe,
        Cond::Sgt => 0xf,
    }
}

const CC_P: u8 = 0xa;
const CC_NP: u8 = 0xb;
const CC_E: u8 = 0x4;
const CC_NE: u8 = 0x5;
const CC_A: u8 = 0x7;
const CC_AE: u8 = 0x3;

/// An instruction's r/m operand.
#[derive(Copy, Clone)]
enum Rm {
    R(Reg),
    /// `[base + disp]`.
    M(Reg, i32),
    /// `[base + index * 8]`.
    Idx8(Reg, Reg),
    /// `[rip + disp]` to a pool constant (fixed up at the end).
    Pool(u32),
}

#[derive(Copy, Clone)]
enum Fix {
    /// A rel32 to a label.
    Rel(Label),
    /// A rel32 to the constant pool's entry.
    Pool(u32),
}

#[derive(Debug)]
pub(crate) struct X64 {
    buf: Vec<u8>,
    labels: Vec<u32>,
    fixups: Vec<(u32, Fix)>,
    pool: Vec<u64>,
    /// Jump tables: their labels and entries.
    tables: Vec<(Label, Vec<Label>)>,
}

impl std::fmt::Debug for Fix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fix::Rel(l) => write!(f, "rel {}", l.0),
            Fix::Pool(i) => write!(f, "pool {i}"),
        }
    }
}

fn fits8(i: i64) -> bool {
    i8::try_from(i).is_ok()
}

fn fits32(i: i64) -> bool {
    i32::try_from(i).is_ok()
}

impl X64 {
    fn byte(&mut self, b: u8) {
        self.buf.push(b);
    }

    fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    fn d32(&mut self, d: i32) {
        self.buf.extend_from_slice(&d.to_le_bytes());
    }

    fn pool_entry(&mut self, bits: u64) -> u32 {
        match self.pool.iter().position(|&b| b == bits) {
            Some(i) => i as u32,
            None => {
                self.pool.push(bits);
                self.pool.len() as u32 - 1
            }
        }
    }

    /// One instruction: `pre` (legacy prefixes), a REX if it needs one,
    /// `op`, then the ModRM for `reg` and `rm`. `w` sets REX.W; `byte`
    /// forces a REX for byte registers 4 to 7.
    fn emit(&mut self, pre: &[u8], w: bool, op: &[u8], reg: u8, rm: Rm, byte: bool) {
        self.bytes(pre);
        let reg = reg & 15;
        let (b, x) = match rm {
            Rm::R(r) => ((r & 15) >> 3, 0),
            Rm::M(base, _) => ((base & 15) >> 3, 0),
            Rm::Idx8(base, idx) => ((base & 15) >> 3, (idx & 15) >> 3),
            Rm::Pool(_) => (0, 0),
        };
        let force = byte
            && ((4..8).contains(&reg)
                || matches!(rm, Rm::R(r) if (4..8).contains(&(r & 15))));
        let rex = 0x40 | (u8::from(w) << 3) | ((reg >> 3) << 2) | (x << 1) | b;
        if rex != 0x40 || force {
            self.byte(rex);
        }
        self.bytes(op);
        let r = reg & 7;
        match rm {
            Rm::R(rr) => self.byte(0xc0 | r << 3 | (rr & 7)),
            Rm::M(base, disp) => {
                let bb = base & 7;
                if disp == 0 && bb != 5 {
                    self.byte(r << 3 | bb);
                    if bb == 4 {
                        self.byte(0x24);
                    }
                } else if fits8(i64::from(disp)) {
                    self.byte(0x40 | r << 3 | bb);
                    if bb == 4 {
                        self.byte(0x24);
                    }
                    self.byte(disp as u8);
                } else {
                    self.byte(0x80 | r << 3 | bb);
                    if bb == 4 {
                        self.byte(0x24);
                    }
                    self.d32(disp);
                }
            }
            Rm::Idx8(base, idx) => {
                let bb = base & 7;
                // (rbp and r13 as a base need a displacement.)
                if bb == 5 {
                    self.byte(0x44 | r << 3);
                    self.byte(0xc0 | (idx & 7) << 3 | bb);
                    self.byte(0);
                } else {
                    self.byte(0x04 | r << 3);
                    self.byte(0xc0 | (idx & 7) << 3 | bb);
                }
            }
            Rm::Pool(i) => {
                self.byte(r << 3 | 5);
                let at = self.buf.len() as u32;
                self.d32(0);
                self.fixups.push((at, Fix::Pool(i)));
            }
        }
    }

    fn rel32(&mut self, l: Label) {
        let at = self.buf.len() as u32;
        self.d32(0);
        self.fixups.push((at, Fix::Rel(l)));
    }

    fn mov64(&mut self, dst: Reg, src: Reg) {
        if dst != src {
            self.emit(&[], true, &[0x8b], dst, Rm::R(src), false);
        }
    }

    fn movaps(&mut self, dst: Reg, src: Reg) {
        if dst != src {
            self.emit(&[], false, &[0x0f, 0x28], dst - FLOAT, Rm::R(src - FLOAT), false);
        }
    }

    /// `op r/m, reg` at `ty`'s width (8-bit forms for bytes).
    fn alu_rr(&mut self, op8: u8, ty: Type, rm: Reg, reg: Reg) {
        match ty.bits() {
            8 => self.emit(&[], false, &[op8 - 1], reg, Rm::R(rm), true),
            16 => self.emit(&[0x66], false, &[op8], reg, Rm::R(rm), false),
            32 => self.emit(&[], false, &[op8], reg, Rm::R(rm), false),
            _ => self.emit(&[], true, &[op8], reg, Rm::R(rm), false),
        }
    }

    /// `op r, imm` (the 0x81 group, `ext` its ModRM.reg), 32- or 64-bit.
    fn alu_ri(&mut self, ext: u8, w: bool, r: Reg, imm: i64) {
        if fits8(imm) {
            self.emit(&[], w, &[0x83], ext, Rm::R(r), false);
            self.byte(imm as u8);
        } else {
            self.emit(&[], w, &[0x81], ext, Rm::R(r), false);
            self.d32(imm as i32);
        }
    }

    fn sse(&mut self, pre: u8, op: u8, dst: Reg, src: Rm) {
        self.emit(&[pre], false, &[0x0f, op], dst - FLOAT, src, false);
    }

    fn sse_rr(&mut self, ty: Type, op: u8, dst: Reg, src: Reg) {
        let pre = if ty.bits() == 32 { 0xf3 } else { 0xf2 };
        self.sse(pre, op, dst, Rm::R(src - FLOAT));
    }

    fn setcc_raw(&mut self, code: u8, dst: Reg) {
        self.emit(&[], false, &[0x0f, 0x90 + code], 0, Rm::R(dst), true);
    }

    fn jcc_raw(&mut self, code: u8, l: Label) {
        self.bytes(&[0x0f, 0x80 + code]);
        self.rel32(l);
    }

    /// Extend `a`'s `from` bits into `dst`'s 32 (or 64, signed) bits.
    fn widen(&mut self, signed: bool, from: u32, dst: Reg, a: Reg) {
        match (from, signed) {
            (8, false) => self.emit(&[], false, &[0x0f, 0xb6], dst, Rm::R(a), true),
            (8, true) => self.emit(&[], true, &[0x0f, 0xbe], dst, Rm::R(a), true),
            (16, false) => self.emit(&[], false, &[0x0f, 0xb7], dst, Rm::R(a), false),
            (16, true) => self.emit(&[], true, &[0x0f, 0xbf], dst, Rm::R(a), false),
            (32, false) => self.emit(&[], false, &[0x8b], dst, Rm::R(a), false),
            (32, true) => self.emit(&[], true, &[0x63], dst, Rm::R(a), false),
            _ => self.mov64(dst, a),
        }
    }

    fn push(&mut self, r: Reg) {
        if r >= 8 {
            self.byte(0x41);
        }
        self.byte(0x50 + (r & 7));
    }

    fn pop(&mut self, r: Reg) {
        if r >= 8 {
            self.byte(0x41);
        }
        self.byte(0x58 + (r & 7));
    }

    fn saved_regs(frame: &Frame) -> Vec<Reg> {
        (0..16).filter(|&r| frame.saved & bit(r) != 0).collect()
    }

    /// The bytes `sub rsp` takes: the slots and the outgoing area, keeping
    /// the stack 16-byte aligned at calls.
    fn frame_size(frame: &Frame) -> u32 {
        let pushed = 8 * Self::saved_regs(frame).len() as u32;
        let mut size = frame.slots + frame.out;
        if (pushed + size) % 16 != 0 {
            size += 8;
        }
        size
    }
}

impl Asm for X64 {
    const INT_REGS: &'static [Reg] = &[
        RAX, RCX, RDX, RSI, RDI, R8, R9, R10, RBX, R12, R13, R14, R15,
    ];
    #[cfg(not(windows))]
    const FLOAT_REGS: &'static [Reg] = &[
        xmm(0),
        xmm(1),
        xmm(2),
        xmm(3),
        xmm(4),
        xmm(5),
        xmm(6),
        xmm(7),
        xmm(8),
        xmm(9),
        xmm(10),
        xmm(11),
        xmm(12),
        xmm(13),
        xmm(14),
    ];
    // (Win64's xmm6 to xmm15 are callee-saved: left alone.)
    #[cfg(windows)]
    const FLOAT_REGS: &'static [Reg] = &[xmm(0), xmm(1), xmm(2), xmm(3), xmm(4)];
    const MAX_PINNED: usize = 3;
    const SHIFT_REG: Option<Reg> = Some(RCX);
    const DIV_REGS: Option<(Reg, Reg)> = Some((RAX, RDX));
    const SCRATCH: Reg = R11;
    #[cfg(not(windows))]
    const FSCRATCH: Reg = xmm(15);
    #[cfg(windows)]
    const FSCRATCH: Reg = xmm(5);
    const CALL_REG: Reg = R11;

    fn callee_saved(conv: CallConv) -> u64 {
        match conv {
            CallConv::WindowsFastcall => mask(&[RBX, RSI, RDI, R12, R13, R14, R15]),
            _ => mask(&[RBX, R12, R13, R14, R15]),
        }
    }

    fn abi(conv: CallConv) -> Option<&'static Abi> {
        match conv {
            CallConv::SystemV => Some(&SYSV),
            CallConv::WindowsFastcall => Some(&WIN64),
            _ => None,
        }
    }

    fn new() -> Self {
        X64 {
            buf: Vec::with_capacity(4096),
            labels: Vec::new(),
            fixups: Vec::new(),
            pool: Vec::new(),
            tables: Vec::new(),
        }
    }

    fn new_label(&mut self) -> Label {
        self.labels.push(u32::MAX);
        Label(self.labels.len() as u32 - 1)
    }

    fn bind(&mut self, l: Label) {
        self.labels[l.0 as usize] = self.buf.len() as u32;
    }

    fn offset(&self) -> usize {
        self.buf.len()
    }

    fn mov(&mut self, dst: Reg, src: Reg) {
        if is_float(dst) {
            self.movaps(dst, src);
        } else {
            self.mov64(dst, src);
        }
    }

    fn swap(&mut self, a: Reg, b: Reg) {
        if is_float(a) {
            self.movaps(Self::FSCRATCH, a);
            self.movaps(a, b);
            self.movaps(b, Self::FSCRATCH);
        } else {
            self.emit(&[], true, &[0x87], a, Rm::R(b), false);
        }
    }

    fn imm(&mut self, dst: Reg, imm: i64) {
        // (Never `xor`: the flags may be live.)
        if (0..=i64::from(u32::MAX)).contains(&imm) {
            if dst >= 8 {
                self.byte(0x41);
            }
            self.byte(0xb8 + (dst & 7));
            self.d32(imm as u32 as i32);
        } else if fits32(imm) {
            self.emit(&[], true, &[0xc7], 0, Rm::R(dst), false);
            self.d32(imm as i32);
        } else {
            self.byte(0x48 | u8::from(dst >= 8));
            self.byte(0xb8 + (dst & 7));
            self.bytes(&imm.to_le_bytes());
        }
    }

    fn fimm(&mut self, dst: Reg, bits: u64, ty: Type) {
        if bits == 0 {
            // (`xorps` leaves the integer flags alone.)
            self.emit(&[], false, &[0x0f, 0x57], dst - FLOAT, Rm::R(dst - FLOAT), false);
            return;
        }
        let i = self.pool_entry(bits);
        let pre = if ty.bits() == 32 { 0xf3 } else { 0xf2 };
        self.sse(pre, 0x10, dst, Rm::Pool(i));
    }

    fn ld_sp(&mut self, dst: Reg, off: i32) {
        if is_float(dst) {
            self.sse(0xf2, 0x10, dst, Rm::M(RSP, off));
        } else {
            self.emit(&[], true, &[0x8b], dst, Rm::M(RSP, off), false);
        }
    }

    fn st_sp(&mut self, src: Reg, off: i32) {
        if is_float(src) {
            self.sse(0xf2, 0x11, src, Rm::M(RSP, off));
        } else {
            self.emit(&[], true, &[0x89], src, Rm::M(RSP, off), false);
        }
    }

    fn load(&mut self, bytes: u8, ext: Ext, ty: Type, dst: Reg, base: Reg, off: i32) {
        let m = Rm::M(base, off);
        if is_float(dst) {
            let pre = if bytes == 4 { 0xf3 } else { 0xf2 };
            self.sse(pre, 0x10, dst, m);
            return;
        }
        let _ = ty;
        match (bytes, ext) {
            (8, _) => self.emit(&[], true, &[0x8b], dst, m, false),
            (4, Ext::Sign) => self.emit(&[], true, &[0x63], dst, m, false),
            (4, _) => self.emit(&[], false, &[0x8b], dst, m, false),
            (2, Ext::Sign) => self.emit(&[], true, &[0x0f, 0xbf], dst, m, false),
            (2, _) => self.emit(&[], false, &[0x0f, 0xb7], dst, m, false),
            (_, Ext::Sign) => self.emit(&[], true, &[0x0f, 0xbe], dst, m, false),
            _ => self.emit(&[], false, &[0x0f, 0xb6], dst, m, false),
        }
    }

    fn store(&mut self, bytes: u8, src: Reg, base: Reg, off: i32) {
        let m = Rm::M(base, off);
        if is_float(src) {
            let pre = if bytes == 4 { 0xf3 } else { 0xf2 };
            self.sse(pre, 0x11, src, m);
            return;
        }
        match bytes {
            8 => self.emit(&[], true, &[0x89], src, m, false),
            4 => self.emit(&[], false, &[0x89], src, m, false),
            2 => self.emit(&[0x66], false, &[0x89], src, m, false),
            _ => self.emit(&[], false, &[0x88], src, m, true),
        }
    }

    fn store_imm(&mut self, bytes: u8, imm: i64, base: Reg, off: i32) -> bool {
        let m = Rm::M(base, off);
        match bytes {
            1 => {
                self.emit(&[], false, &[0xc6], 0, m, false);
                self.byte(imm as u8);
            }
            2 => {
                self.emit(&[0x66], false, &[0xc7], 0, m, false);
                self.bytes(&(imm as u16).to_le_bytes());
            }
            4 => {
                self.emit(&[], false, &[0xc7], 0, m, false);
                self.d32(imm as i32);
            }
            _ if fits32(imm) => {
                self.emit(&[], true, &[0xc7], 0, m, false);
                self.d32(imm as i32);
            }
            _ => return false,
        }
        true
    }

    fn alu_imm_ok(op: Alu, imm: i64) -> bool {
        match op {
            Alu::Shl | Alu::Ushr | Alu::Sshr => true,
            Alu::Sub => fits32(imm) && imm != i64::from(i32::MIN),
            _ => fits32(imm),
        }
    }

    fn alu(&mut self, op: Alu, ty: Type, dst: Reg, a: Reg, b: Opnd) {
        let w = ty.bits() == 64;
        let bits = ty.bits();
        match op {
            Alu::Shl | Alu::Ushr | Alu::Sshr => {
                let ext = match op {
                    Alu::Shl => 4,
                    Alu::Ushr => 5,
                    _ => 7,
                };
                // A narrow right shift reads the bits above the value's.
                if bits < 32 && op != Alu::Shl {
                    self.widen(op == Alu::Sshr, bits, dst, a);
                } else {
                    self.mov64(dst, a);
                }
                match b {
                    Opnd::I(i) => {
                        let n = (i as u32 & (bits - 1)) as u8;
                        if n != 0 {
                            self.emit(&[], w, &[0xc1], ext, Rm::R(dst), false);
                            self.byte(n);
                        }
                    }
                    Opnd::R(_) => {
                        // (The count is in rcx, the driver's copy.)
                        if bits < 32 {
                            self.alu_ri(4, false, RCX, i64::from(bits - 1));
                        }
                        self.emit(&[], w, &[0xd3], ext, Rm::R(dst), false);
                    }
                }
            }
            Alu::Mul => match b {
                Opnd::I(i) => {
                    if fits8(i) {
                        self.emit(&[], w, &[0x6b], dst, Rm::R(a), false);
                        self.byte(i as u8);
                    } else {
                        self.emit(&[], w, &[0x69], dst, Rm::R(a), false);
                        self.d32(i as i32);
                    }
                }
                Opnd::R(rb) => {
                    let (x, y) = if dst == rb { (rb, a) } else { (a, rb) };
                    self.mov64(dst, x);
                    self.emit(&[], w, &[0x0f, 0xaf], dst, Rm::R(y), false);
                }
            },
            _ => {
                let (op8, ext) = match op {
                    Alu::Add => (0x01, 0),
                    Alu::Or => (0x09, 1),
                    Alu::And => (0x21, 4),
                    Alu::Sub => (0x29, 5),
                    _ => (0x31, 6),
                };
                let ty = if bits < 32 { crate::ir::types::I32 } else { ty };
                match b {
                    Opnd::I(i) => {
                        if dst != a && op == Alu::Add {
                            // lea dst, [a + i]
                            self.emit(&[], w, &[0x8d], dst, Rm::M(a, i as i32), false);
                            return;
                        }
                        self.mov64(dst, a);
                        if i != 0 || matches!(op, Alu::And) {
                            self.alu_ri(ext, w, dst, i);
                        }
                    }
                    Opnd::R(rb) => {
                        if dst == a {
                            self.alu_rr(op8, ty, dst, rb);
                        } else if dst == rb {
                            if op == Alu::Sub {
                                // a - b = -b + a
                                self.emit(&[], w, &[0xf7], 3, Rm::R(dst), false);
                                self.alu_rr(0x01, ty, dst, a);
                            } else {
                                self.alu_rr(op8, ty, dst, a);
                            }
                        } else if op == Alu::Add {
                            self.lea_add(w, dst, a, rb);
                        } else {
                            self.mov64(dst, a);
                            self.alu_rr(op8, ty, dst, rb);
                        }
                    }
                }
            }
        }
    }

    fn div(&mut self, op: Div, ty: Type, dst: Reg, a: Reg, b: Reg) {
        let bits = ty.bits();
        let signed = matches!(op, Div::Sdiv | Div::Srem);
        let w = bits == 64;
        let b = if bits < 32 {
            self.widen(signed, bits, R11, b);
            R11
        } else {
            b
        };
        if bits < 32 {
            self.widen(signed, bits, RAX, a);
        } else {
            self.mov64(RAX, a);
        }
        if signed {
            if w {
                self.bytes(&[0x48, 0x99]);
            } else {
                self.byte(0x99);
            }
            self.emit(&[], w, &[0xf7], 7, Rm::R(b), false);
        } else {
            self.emit(&[], false, &[0x31], RDX, Rm::R(RDX), false);
            self.emit(&[], w, &[0xf7], 6, Rm::R(b), false);
        }
        let r = if matches!(op, Div::Udiv | Div::Sdiv) {
            RAX
        } else {
            RDX
        };
        self.mov64(dst, r);
    }

    fn smulhi(&mut self, ty: Type, dst: Reg, a: Reg, b: Reg) {
        let w = ty.bits() == 64;
        self.mov64(RAX, a);
        self.emit(&[], w, &[0xf7], 5, Rm::R(b), false);
        self.mov64(dst, RDX);
    }

    fn not(&mut self, ty: Type, dst: Reg, a: Reg) {
        self.mov64(dst, a);
        self.emit(&[], ty.bits() == 64, &[0xf7], 2, Rm::R(dst), false);
    }

    fn neg(&mut self, ty: Type, dst: Reg, a: Reg) {
        self.mov64(dst, a);
        self.emit(&[], ty.bits() == 64, &[0xf7], 3, Rm::R(dst), false);
    }

    fn extend(&mut self, signed: bool, from: Type, to: Type, dst: Reg, a: Reg) {
        let _ = to;
        self.widen(signed, from.bits(), dst, a);
    }

    fn cmp(&mut self, ty: Type, a: Reg, b: Opnd) {
        match b {
            Opnd::R(rb) => self.alu_rr(0x39, ty, a, rb),
            Opnd::I(i) => match ty.bits() {
                8 => {
                    self.emit(&[], false, &[0x80], 7, Rm::R(a), true);
                    self.byte(i as u8);
                }
                16 => {
                    if fits8(i) {
                        self.emit(&[0x66], false, &[0x83], 7, Rm::R(a), false);
                        self.byte(i as u8);
                    } else {
                        self.emit(&[0x66], false, &[0x81], 7, Rm::R(a), false);
                        self.bytes(&(i as u16).to_le_bytes());
                    }
                }
                bits => self.alu_ri(7, bits == 64, a, i),
            },
        }
    }

    fn cmp_imm_ok(imm: i64) -> bool {
        fits32(imm)
    }

    fn test(&mut self, ty: Type, a: Reg) {
        self.alu_rr(0x85, ty, a, a);
    }

    fn setcc(&mut self, c: Cond, dst: Reg) {
        self.setcc_raw(cc(c), dst);
    }

    fn csel(&mut self, c: Cond, ty: Type, dst: Reg, x: Reg, y: Reg) {
        if ty.is_float() {
            let skip = self.new_label();
            if dst == x {
                self.jcc_raw(cc(c), skip);
                self.movaps(dst, y);
            } else if dst == y {
                self.jcc_raw(cc(c.inverse()), skip);
                self.movaps(dst, x);
            } else {
                self.movaps(dst, y);
                self.jcc_raw(cc(c.inverse()), skip);
                self.movaps(dst, x);
            }
            self.bind(skip);
            return;
        }
        let cmov = |s: &mut Self, c: Cond, d: Reg, src: Reg| {
            s.emit(&[], true, &[0x0f, 0x40 + cc(c)], d, Rm::R(src), false);
        };
        if dst == x {
            cmov(self, c.inverse(), dst, y);
        } else if dst == y {
            cmov(self, c, dst, x);
        } else {
            self.mov64(dst, y);
            cmov(self, c, dst, x);
        }
    }

    fn overflow(&mut self, op: Overflow, ty: Type, dst: Reg, a: Reg, b: Reg) -> Cond {
        let w = ty.bits() == 64;
        let ty = if ty.bits() < 32 { crate::ir::types::I32 } else { ty };
        let run = |s: &mut Self, d: Reg, src: Reg| match op {
            Overflow::Add => s.alu_rr(0x01, ty, d, src),
            Overflow::Sub => s.alu_rr(0x29, ty, d, src),
            Overflow::Mul => s.emit(&[], w, &[0x0f, 0xaf], d, Rm::R(src), false),
        };
        if dst == a {
            run(self, dst, b);
        } else if dst == b {
            if op == Overflow::Sub {
                self.mov64(R11, a);
                run(self, R11, b);
                self.mov64(dst, R11);
            } else {
                run(self, dst, a);
            }
        } else {
            self.mov64(dst, a);
            run(self, dst, b);
        }
        Cond::O
    }

    fn fop(&mut self, op: Fop, ty: Type, dst: Reg, a: Reg, b: Reg) {
        let code = match op {
            Fop::Add => 0x58,
            Fop::Mul => 0x59,
            Fop::Sub => 0x5c,
            Fop::Div => 0x5e,
        };
        let commutes = matches!(op, Fop::Add | Fop::Mul);
        if dst == a {
            self.sse_rr(ty, code, dst, b);
        } else if dst == b && commutes {
            self.sse_rr(ty, code, dst, a);
        } else if dst == b {
            self.movaps(Self::FSCRATCH, a);
            self.sse_rr(ty, code, Self::FSCRATCH, b);
            self.movaps(dst, Self::FSCRATCH);
        } else {
            self.movaps(dst, a);
            self.sse_rr(ty, code, dst, b);
        }
    }

    fn fneg(&mut self, ty: Type, dst: Reg, a: Reg) {
        let m = if ty.bits() == 32 { 0x8000_0000 } else { 1 << 63 };
        let i = self.pool_entry(m);
        self.sse(0xf2, 0x10, Self::FSCRATCH, Rm::Pool(i));
        self.movaps(dst, a);
        self.emit(&[0x66], false, &[0x0f, 0x57], dst - FLOAT, Rm::R(Self::FSCRATCH - FLOAT), false);
    }

    fn fabs(&mut self, ty: Type, dst: Reg, a: Reg) {
        let m = if ty.bits() == 32 {
            0x7fff_ffff
        } else {
            u64::MAX >> 1
        };
        let i = self.pool_entry(m);
        self.sse(0xf2, 0x10, Self::FSCRATCH, Rm::Pool(i));
        self.movaps(dst, a);
        self.emit(&[0x66], false, &[0x0f, 0x54], dst - FLOAT, Rm::R(Self::FSCRATCH - FLOAT), false);
    }

    fn fsqrt(&mut self, ty: Type, dst: Reg, a: Reg) {
        self.sse_rr(ty, 0x51, dst, a);
    }

    fn fcvt_from_sint(&mut self, from: Type, to: Type, dst: Reg, a: Reg) {
        let src = if from.bits() < 32 {
            self.widen(true, from.bits(), R11, a);
            R11
        } else {
            a
        };
        // (A fresh register breaks the dependence on dst's old value.)
        self.emit(&[], false, &[0x0f, 0x57], dst - FLOAT, Rm::R(dst - FLOAT), false);
        let pre = if to.bits() == 32 { 0xf3 } else { 0xf2 };
        self.emit(&[pre], from.bits() == 64, &[0x0f, 0x2a], dst - FLOAT, Rm::R(src), false);
    }

    fn bitcast(&mut self, ty: Type, dst: Reg, a: Reg) {
        let w = ty.bits() == 64;
        if is_float(dst) {
            self.emit(&[0x66], w, &[0x0f, 0x6e], dst - FLOAT, Rm::R(a), false);
        } else {
            self.emit(&[0x66], w, &[0x0f, 0x7e], a - FLOAT, Rm::R(dst), false);
        }
    }

    fn fcmp(&mut self, cc: FloatCC, ty: Type, a: Reg, b: Reg) {
        let (x, y) = match cc {
            FloatCC::LessThan | FloatCC::LessThanOrEqual => (b, a),
            _ => (a, b),
        };
        let pre: &[u8] = if ty.bits() == 32 { &[] } else { &[0x66] };
        self.emit(pre, false, &[0x0f, 0x2e], x - FLOAT, Rm::R(y - FLOAT), false);
    }

    fn fsetcc(&mut self, cc: FloatCC, dst: Reg) {
        match cc {
            FloatCC::Ordered => self.setcc_raw(CC_NP, dst),
            FloatCC::Unordered => self.setcc_raw(CC_P, dst),
            FloatCC::Equal => {
                self.setcc_raw(CC_E, dst);
                self.setcc_raw(CC_NP, R11);
                self.emit(&[], false, &[0x20], R11, Rm::R(dst), true);
            }
            FloatCC::NotEqual => {
                self.setcc_raw(CC_NE, dst);
                self.setcc_raw(CC_P, R11);
                self.emit(&[], false, &[0x08], R11, Rm::R(dst), true);
            }
            FloatCC::GreaterThan | FloatCC::LessThan => self.setcc_raw(CC_A, dst),
            FloatCC::GreaterThanOrEqual | FloatCC::LessThanOrEqual => {
                self.setcc_raw(CC_AE, dst);
            }
        }
    }

    fn fjcc(&mut self, cc: FloatCC, l: Label) {
        match cc {
            FloatCC::Ordered => self.jcc_raw(CC_NP, l),
            FloatCC::Unordered => self.jcc_raw(CC_P, l),
            FloatCC::Equal => {
                let skip = self.new_label();
                self.jcc_raw(CC_P, skip);
                self.jcc_raw(CC_E, l);
                self.bind(skip);
            }
            FloatCC::NotEqual => {
                self.jcc_raw(CC_P, l);
                self.jcc_raw(CC_NE, l);
            }
            FloatCC::GreaterThan | FloatCC::LessThan => self.jcc_raw(CC_A, l),
            FloatCC::GreaterThanOrEqual | FloatCC::LessThanOrEqual => self.jcc_raw(CC_AE, l),
        }
    }

    fn jmp(&mut self, l: Label) {
        self.byte(0xe9);
        self.rel32(l);
    }

    fn jcc(&mut self, c: Cond, l: Label) {
        self.jcc_raw(cc(c), l);
    }

    fn call(&mut self, target: Reg) {
        self.emit(&[], false, &[0xff], 2, Rm::R(target), false);
    }

    fn br_table(&mut self, idx: Reg, ty: Type, default: Label, table: &[Label]) {
        // Zero-extend the index, bound it, then jump to the table's base
        // plus the entry's offset from it.
        if ty.bits() < 64 {
            self.widen(false, ty.bits().max(8), idx, idx);
        }
        self.alu_ri(7, true, idx, table.len() as i64);
        self.jcc_raw(cc(Cond::Uge), default);
        let tl = self.new_label();
        // lea r11, [rip + table]
        self.bytes(&[0x4c, 0x8d, 0x1d]);
        self.rel32(tl);
        // add r11, [r11 + idx*8]
        self.emit(&[], true, &[0x03], R11, Rm::Idx8(R11, idx), false);
        self.emit(&[], false, &[0xff], 4, Rm::R(R11), false);
        self.tables.push((tl, table.to_vec()));
    }

    fn epilogue(&mut self, frame: &Frame) {
        let size = Self::frame_size(frame);
        if size > 0 {
            self.alu_ri(0, true, RSP, i64::from(size));
        }
        for r in Self::saved_regs(frame).into_iter().rev() {
            self.pop(r);
        }
        self.pop(RBP);
        self.byte(0xc3);
    }

    fn finish(mut self, frame: &Frame) -> Result<Vec<u8>, Error> {
        // The pool and the tables after the code, 8-byte aligned.
        while self.buf.len() % 8 != 0 {
            self.byte(0xcc);
        }
        let pool_at = self.buf.len() as u32;
        for &b in &self.pool.clone() {
            self.bytes(&b.to_le_bytes());
        }
        let mut entries = Vec::new();
        for (tl, labels) in std::mem::take(&mut self.tables) {
            self.bind(tl);
            for l in labels {
                entries.push((self.buf.len() as u32, tl, l));
                self.bytes(&[0; 8]);
            }
        }
        let label = |s: &Self, l: Label| -> Result<u32, Error> {
            let at = s.labels[l.0 as usize];
            if at == u32::MAX {
                Err(Error::Internal("an unbound label"))
            } else {
                Ok(at)
            }
        };
        for &(at, fix) in &self.fixups {
            let target = match fix {
                Fix::Rel(l) => label(&self, l)?,
                Fix::Pool(i) => pool_at + 8 * i,
            };
            let rel = target as i64 - (i64::from(at) + 4);
            self.buf[at as usize..at as usize + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        for (at, tl, l) in entries {
            let rel = i64::from(label(&self, l)?) - i64::from(label(&self, tl)?);
            self.buf[at as usize..at as usize + 8].copy_from_slice(&rel.to_le_bytes());
        }
        // The prologue in front: everything above is position-independent.
        let mut pro = X64::new();
        pro.push(RBP);
        pro.emit(&[], true, &[0x8b], RBP, Rm::R(RSP), false);
        for r in Self::saved_regs(frame) {
            pro.push(r);
        }
        let mut size = Self::frame_size(frame);
        // Touch each page of a large frame on the way down.
        while size >= 4096 {
            pro.alu_ri(5, true, RSP, 4096);
            pro.emit(&[], true, &[0x83], 1, Rm::M(RSP, 0), false);
            pro.byte(0);
            size -= 4096;
        }
        if size > 0 {
            pro.alu_ri(5, true, RSP, i64::from(size));
        }
        let mut out = pro.buf;
        out.extend_from_slice(&self.buf);
        Ok(out)
    }
}

impl X64 {
    /// `lea dst, [a + b]`.
    fn lea_add(&mut self, w: bool, dst: Reg, a: Reg, b: Reg) {
        let rex = 0x40 | (u8::from(w) << 3) | ((dst >> 3) << 2) | ((b >> 3) << 1) | (a >> 3);
        if rex != 0x40 {
            self.byte(rex);
        }
        self.byte(0x8d);
        if a & 7 == 5 {
            self.byte(0x44 | (dst & 7) << 3);
            self.byte((b & 7) << 3 | (a & 7));
            self.byte(0);
        } else {
            self.byte(0x04 | (dst & 7) << 3);
            self.byte((b & 7) << 3 | (a & 7));
        }
    }
}
