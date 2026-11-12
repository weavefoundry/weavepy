//! The AArch64 assembler: AAPCS64 and Apple's variant of it.

use crate::asm::{bit, is_float, Abi, Asm, Cond, Frame, Label, Opnd, Reg, FLOAT};
use crate::ir::{condcodes::FloatCC, Alu, CallConv, Div, Ext, Fop, Overflow, Type};
use crate::Error;

const X16: Reg = 16;
const X17: Reg = 17;
const LR: Reg = 30;
/// The zero register or the stack pointer, by instruction.
const ZR: Reg = 31;
const SP: Reg = 31;

const fn v(n: u8) -> Reg {
    FLOAT + n
}

const fn mask(lo: u8, hi: u8) -> u64 {
    let mut m = 0;
    let mut i = lo;
    while i <= hi {
        m |= 1u64 << i;
        i += 1;
    }
    m
}

static AAPCS: Abi = Abi {
    int_args: &[0, 1, 2, 3, 4, 5, 6, 7],
    float_args: &[v(0), v(1), v(2), v(3), v(4), v(5), v(6), v(7)],
    positional: false,
    int_rets: &[0, 1],
    float_rets: &[v(0), v(1)],
    // x0 to x17 and the link register; v0 to v7 and v16 to v31 (v8 to
    // v15 keep their low halves, and this code uses no others).
    clobbers: mask(0, 17) | (1 << LR) | (mask(0, 7) << FLOAT) | (mask(16, 31) << FLOAT),
    shadow: 0,
};

fn cc(c: Cond) -> u32 {
    match c {
        Cond::Eq => 0,
        Cond::Ne => 1,
        Cond::Uge => 2,
        Cond::Ult => 3,
        Cond::O => 6,
        Cond::No => 7,
        Cond::Ugt => 8,
        Cond::Ule => 9,
        Cond::Sge => 10,
        Cond::Slt => 11,
        Cond::Sgt => 12,
        Cond::Sle => 13,
    }
}

/// A float condition's flag test after `fcmp`, as one condition code or
/// as either of two.
fn fcc(c: FloatCC) -> (u32, Option<u32>) {
    match c {
        FloatCC::Equal => (0, None),
        FloatCC::NotEqual => (1, None),
        FloatCC::LessThan => (4, None),
        FloatCC::Unordered => (6, None),
        FloatCC::Ordered => (7, None),
        FloatCC::LessThanOrEqual => (9, None),
        FloatCC::GreaterThanOrEqual => (10, None),
        FloatCC::GreaterThan => (12, None),
        FloatCC::UnorderedOrLessThan => (11, None),
        FloatCC::UnorderedOrLessThanOrEqual => (13, None),
        FloatCC::UnorderedOrGreaterThan => (8, None),
        FloatCC::UnorderedOrGreaterThanOrEqual => (2, None),
        // mi or gt; eq or vs.
        FloatCC::OrderedNotEqual => (4, Some(12)),
        FloatCC::UnorderedOrEqual => (0, Some(6)),
    }
}

#[derive(Copy, Clone, Debug)]
enum Fix {
    B26(Label),
    B19(Label),
    Adr(Label),
}

/// The N:immr:imms encoding of `imm` as a logical immediate of `bits`.
fn logical_imm(imm: u64, bits: u32) -> Option<u32> {
    let imm = if bits == 32 {
        (imm & 0xffff_ffff) | (imm << 32)
    } else {
        imm
    };
    if imm == 0 || imm == u64::MAX {
        return None;
    }
    let mut size = 64u32;
    while size > 2 {
        let half = size / 2;
        let m = (1u64 << half) - 1;
        if imm & m != (imm >> half) & m {
            break;
        }
        size = half;
    }
    let m = if size == 64 {
        u64::MAX
    } else {
        (1u64 << size) - 1
    };
    let elt = imm & m;
    let ones = elt.count_ones();
    let pattern = if ones == 64 {
        u64::MAX
    } else {
        (1u64 << ones) - 1
    };
    let ror = |x: u64, r: u32| {
        if r == 0 {
            x
        } else {
            ((x >> r) | (x << (size - r))) & m
        }
    };
    let r = (0..size).find(|&r| ror(pattern, r) == elt)?;
    let n = u32::from(size == 64);
    let imms = ((!(size * 2 - 1)) & 0x3f) | (ones - 1);
    Some(n << 12 | r << 6 | imms)
}

/// An add/sub immediate: `imm12` and its shift, if `imm` is one.
fn arith_imm(imm: i64) -> Option<(u32, u32)> {
    if (0..4096).contains(&imm) {
        Some((imm as u32, 0))
    } else if imm & 0xfff == 0 && (0..4096 << 12).contains(&imm) {
        Some(((imm >> 12) as u32, 1))
    } else {
        None
    }
}

#[derive(Debug)]
pub(crate) struct A64 {
    buf: Vec<u32>,
    labels: Vec<u32>,
    fixups: Vec<(u32, Fix)>,
    tables: Vec<(Label, Vec<Label>)>,
    /// Calls of the function itself, to the prologue.
    self_calls: Vec<u32>,
}

fn r(x: Reg) -> u32 {
    u32::from(x & 31)
}

impl A64 {
    fn put(&mut self, w: u32) {
        self.buf.push(w);
    }

    fn sf(ty: Type) -> u32 {
        if ty.bits() == 64 {
            1 << 31
        } else {
            0
        }
    }

    fn mov_rr(&mut self, d: Reg, s: Reg) {
        if d != s {
            self.put(0xAA00_03E0 | r(s) << 16 | r(d));
        }
    }

    fn fmov(&mut self, d: Reg, s: Reg) {
        if d != s {
            self.put(0x1E60_4000 | r(s) << 5 | r(d));
        }
    }

    fn movimm(&mut self, d: Reg, imm: i64) {
        let u = imm as u64;
        let halves: [u32; 4] = [0, 1, 2, 3].map(|i| ((u >> (16 * i)) & 0xffff) as u32);
        let ffff = halves.iter().filter(|&&h| h == 0xffff).count();
        let zero = halves.iter().filter(|&&h| h == 0).count();
        if ffff > zero {
            // movn, then movk the halves that aren't all ones.
            let first = halves.iter().position(|&h| h != 0xffff).unwrap_or(0);
            self.put(0x9280_0000 | (first as u32) << 21 | (!halves[first] & 0xffff) << 5 | r(d));
            for (i, &h) in halves.iter().enumerate() {
                if i != first && h != 0xffff {
                    self.put(0xF280_0000 | (i as u32) << 21 | h << 5 | r(d));
                }
            }
        } else {
            let first = halves.iter().position(|&h| h != 0).unwrap_or(0);
            self.put(0xD280_0000 | (first as u32) << 21 | halves[first] << 5 | r(d));
            for (i, &h) in halves.iter().enumerate() {
                if i != first && h != 0 {
                    self.put(0xF280_0000 | (i as u32) << 21 | h << 5 | r(d));
                }
            }
        }
    }

    /// `ldr`/`str` of `bytes` at `[base + off]`: `ldr` is the scaled
    /// unsigned-offset opcode, `ldur` the unscaled one.
    fn mem(&mut self, ldr: u32, ldur: u32, bytes: u32, t: Reg, base: Reg, off: i32) {
        let scale = bytes.trailing_zeros();
        if off >= 0 && off % bytes as i32 == 0 && (off >> scale) < 4096 {
            self.put(ldr | ((off as u32) >> scale) << 10 | r(base) << 5 | r(t));
        } else if (-256..256).contains(&off) {
            self.put(ldur | ((off as u32) & 0x1ff) << 12 | r(base) << 5 | r(t));
        } else {
            // x17 = base + off (the extended form, which reads 31 as sp)
            self.movimm(X17, i64::from(off));
            self.put(0x8B20_6000 | r(X17) << 16 | r(base) << 5 | r(X17));
            self.put(ldr | r(X17) << 5 | r(t));
        }
    }

    fn ldr64(&mut self, t: Reg, base: Reg, off: i32) {
        self.mem(0xF940_0000, 0xF840_0000, 8, t, base, off);
    }

    fn str64(&mut self, t: Reg, base: Reg, off: i32) {
        self.mem(0xF900_0000, 0xF800_0000, 8, t, base, off);
    }

    /// `d = op(n, m)` for the shifted-register data-processing opcodes.
    fn rrr(&mut self, op: u32, ty: Type, d: Reg, n: Reg, m: Reg) {
        self.put(op | Self::sf(ty) | r(m) << 16 | r(n) << 5 | r(d));
    }

    /// Extend the low `from` bits of `a` into `d` (32 bits zero, 64 signed).
    fn ext(&mut self, signed: bool, from: u32, d: Reg, a: Reg) {
        match (from, signed) {
            (8, false) => self.put(0x5300_1C00 | r(a) << 5 | r(d)),
            (16, false) => self.put(0x5300_3C00 | r(a) << 5 | r(d)),
            (32, false) => self.put(0x2A00_03E0 | r(a) << 16 | r(d)),
            (8, true) => self.put(0x9340_1C00 | r(a) << 5 | r(d)),
            (16, true) => self.put(0x9340_3C00 | r(a) << 5 | r(d)),
            (32, true) => self.put(0x9340_7C00 | r(a) << 5 | r(d)),
            _ => self.mov_rr(d, a),
        }
    }

    fn b_cond(&mut self, c: u32, l: Label) {
        let at = self.buf.len() as u32;
        self.put(0x5400_0000 | c);
        self.fixups.push((at, Fix::B19(l)));
    }

    fn saved_regs(frame: &Frame) -> Vec<Reg> {
        (0..64).filter(|&r| frame.saved & bit(r) != 0).collect()
    }

    fn frame_size(frame: &Frame) -> u32 {
        (frame.out + frame.slots + 8 * Self::saved_regs(frame).len() as u32).next_multiple_of(16)
    }

    fn save_at(frame: &Frame) -> i32 {
        (frame.out + frame.slots) as i32
    }

    fn sub_sp(&mut self, mut n: u32) {
        while n >= 4096 {
            // sub sp, sp, #1, lsl #12; str xzr, [sp]
            self.put(0xD140_07FF);
            self.put(0xF900_03FF);
            n -= 4096;
        }
        if n > 0 {
            self.put(0xD100_03FF | n << 10);
        }
    }
}

impl Asm for A64 {
    const INT_REGS: &'static [Reg] = &[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 19, 20, 21, 22, 23, 24, 25, 26, 27,
        28,
    ];
    const FLOAT_REGS: &'static [Reg] = &[
        v(0),
        v(1),
        v(2),
        v(3),
        v(4),
        v(5),
        v(6),
        v(7),
        v(16),
        v(17),
        v(18),
        v(19),
        v(20),
        v(21),
        v(22),
        v(23),
        v(24),
        v(25),
        v(26),
        v(27),
        v(28),
        v(29),
        v(30),
    ];
    const MAX_PINNED: usize = 6;
    const SHIFT_REG: Option<Reg> = None;
    const DIV_REGS: Option<(Reg, Reg)> = None;
    const SCRATCH: Reg = X16;
    const FSCRATCH: Reg = v(31);
    const CALL_REG: Reg = X16;
    const SP: Reg = SP;

    fn call_self(&mut self) {
        self.self_calls.push(self.buf.len() as u32);
        self.put(0x9400_0000);
    }

    fn callee_saved(_conv: CallConv) -> u64 {
        mask(19, 28)
    }

    fn abi(conv: CallConv) -> Option<&'static Abi> {
        match conv {
            CallConv::AppleAarch64 | CallConv::Aapcs64 => Some(&AAPCS),
            _ => None,
        }
    }

    fn new() -> Self {
        A64 {
            buf: Vec::with_capacity(1024),
            labels: Vec::new(),
            fixups: Vec::new(),
            tables: Vec::new(),
            self_calls: Vec::new(),
        }
    }

    fn new_label(&mut self) -> Label {
        self.labels.push(u32::MAX);
        Label(self.labels.len() as u32 - 1)
    }

    fn bind(&mut self, l: Label) {
        self.labels[l.0 as usize] = self.buf.len() as u32;
    }

    fn mov(&mut self, dst: Reg, src: Reg) {
        if is_float(dst) {
            self.fmov(dst, src);
        } else {
            self.mov_rr(dst, src);
        }
    }

    fn swap(&mut self, a: Reg, b: Reg) {
        if is_float(a) {
            self.fmov(Self::FSCRATCH, a);
            self.fmov(a, b);
            self.fmov(b, Self::FSCRATCH);
        } else {
            self.mov_rr(X17, a);
            self.mov_rr(a, b);
            self.mov_rr(b, X17);
        }
    }

    fn imm(&mut self, dst: Reg, imm: i64) {
        self.movimm(dst, imm);
    }

    fn fimm(&mut self, dst: Reg, bits: u64, ty: Type) {
        if bits == 0 {
            // fmov d, xzr
            self.put(0x9E67_03E0 | r(dst));
            return;
        }
        self.movimm(X17, bits as i64);
        if ty.bits() == 32 {
            self.put(0x1E27_0000 | r(X17) << 5 | r(dst));
        } else {
            self.put(0x9E67_0000 | r(X17) << 5 | r(dst));
        }
    }

    fn ld_sp(&mut self, dst: Reg, off: i32) {
        if is_float(dst) {
            self.mem(0xFD40_0000, 0xFC40_0000, 8, dst, SP, off);
        } else {
            self.ldr64(dst, SP, off);
        }
    }

    fn st_sp(&mut self, src: Reg, off: i32) {
        if is_float(src) {
            self.mem(0xFD00_0000, 0xFC00_0000, 8, src, SP, off);
        } else {
            self.str64(src, SP, off);
        }
    }

    fn load(&mut self, bytes: u8, ext: Ext, ty: Type, dst: Reg, base: Reg, off: i32) {
        let _ = ty;
        let (ldr, ldur) = if is_float(dst) {
            if bytes == 4 {
                (0xBD40_0000, 0xBC40_0000)
            } else {
                (0xFD40_0000, 0xFC40_0000)
            }
        } else {
            match (bytes, ext) {
                (8, _) => (0xF940_0000, 0xF840_0000),
                (4, Ext::Sign) => (0xB980_0000, 0xB880_0000),
                (4, _) => (0xB940_0000, 0xB840_0000),
                (2, Ext::Sign) => (0x7980_0000, 0x7880_0000),
                (2, _) => (0x7940_0000, 0x7840_0000),
                (_, Ext::Sign) => (0x3980_0000, 0x3880_0000),
                _ => (0x3940_0000, 0x3840_0000),
            }
        };
        self.mem(ldr, ldur, u32::from(bytes), dst, base, off);
    }

    fn store(&mut self, bytes: u8, src: Reg, base: Reg, off: i32) {
        let (str_, stur) = if is_float(src) {
            if bytes == 4 {
                (0xBD00_0000, 0xBC00_0000)
            } else {
                (0xFD00_0000, 0xFC00_0000)
            }
        } else {
            match bytes {
                8 => (0xF900_0000, 0xF800_0000),
                4 => (0xB900_0000, 0xB800_0000),
                2 => (0x7900_0000, 0x7800_0000),
                _ => (0x3900_0000, 0x3800_0000),
            }
        };
        self.mem(str_, stur, u32::from(bytes), src, base, off);
    }

    fn store_imm(&mut self, bytes: u8, imm: i64, base: Reg, off: i32) -> bool {
        if imm != 0 {
            return false;
        }
        self.store(bytes, ZR, base, off);
        true
    }

    fn alu_imm_ok(_op: Alu, _imm: i64) -> bool {
        true
    }

    fn alu(&mut self, op: Alu, ty: Type, dst: Reg, a: Reg, b: Opnd) {
        let sf = Self::sf(ty);
        let bits = ty.bits();
        match op {
            Alu::Shl | Alu::Ushr | Alu::Sshr => {
                // A narrow right shift reads the bits above the value's.
                let a = if bits < 32 && op != Alu::Shl {
                    self.ext(op == Alu::Sshr, bits, X17, a);
                    X17
                } else {
                    a
                };
                let w = bits.max(32);
                let sf = if w == 64 { 1 << 31 } else { 0 };
                match b {
                    Opnd::I(i) => {
                        let s = (i as u32) & (bits - 1);
                        let top = w - 1;
                        let (opc, immr, imms) = match op {
                            Alu::Shl => (0x5300_0000, (w - s) % w, top - s),
                            Alu::Ushr => (0x5300_0000, s, top),
                            _ => (0x1300_0000, s, top),
                        };
                        let n = if w == 64 { 1 << 22 } else { 0 };
                        self.put(opc | sf | n | immr << 16 | imms << 10 | r(a) << 5 | r(dst));
                    }
                    Opnd::R(m) => {
                        let m = if bits < 32 {
                            // and x16, m, #(bits - 1)
                            let enc = logical_imm(u64::from(bits - 1), 32).unwrap();
                            self.put(0x1200_0000 | enc << 10 | r(m) << 5 | r(X16));
                            X16
                        } else {
                            m
                        };
                        let opc = match op {
                            Alu::Shl => 0x1AC0_2000,
                            Alu::Ushr => 0x1AC0_2400,
                            _ => 0x1AC0_2800,
                        };
                        self.put(opc | sf | r(m) << 16 | r(a) << 5 | r(dst));
                    }
                }
            }
            Alu::Add | Alu::Sub => {
                let (add, sub) = (0x0B00_0000, 0x4B00_0000);
                match b {
                    Opnd::I(i) => {
                        let (i, neg) = if op == Alu::Sub {
                            (i.wrapping_neg(), true)
                        } else {
                            (i, false)
                        };
                        let _ = neg;
                        let ty64 = bits == 64;
                        let i = if ty64 { i } else { i64::from(i as i32) };
                        if let Some((imm, sh)) = arith_imm(i) {
                            self.put(0x1100_0000 | sf | sh << 22 | imm << 10 | r(a) << 5 | r(dst));
                        } else if let Some((imm, sh)) = arith_imm(i.wrapping_neg()) {
                            self.put(0x5100_0000 | sf | sh << 22 | imm << 10 | r(a) << 5 | r(dst));
                        } else {
                            self.movimm(X17, i);
                            if a == SP {
                                // (The extended form reads 31 as sp.)
                                self.put(0x8B20_6000 | r(X17) << 16 | r(SP) << 5 | r(dst));
                            } else {
                                self.rrr(add, ty, dst, a, X17);
                            }
                        }
                    }
                    Opnd::R(m) => self.rrr(if op == Alu::Add { add } else { sub }, ty, dst, a, m),
                }
            }
            Alu::Mul => {
                let m = match b {
                    Opnd::I(i) => {
                        self.movimm(X17, i);
                        X17
                    }
                    Opnd::R(m) => m,
                };
                self.put(0x1B00_7C00 | sf | r(m) << 16 | r(a) << 5 | r(dst));
            }
            Alu::And | Alu::Or | Alu::Xor => {
                let (reg, imm) = match op {
                    Alu::And => (0x0A00_0000, 0x1200_0000),
                    Alu::Or => (0x2A00_0000, 0x3200_0000),
                    _ => (0x4A00_0000, 0x5200_0000),
                };
                match b {
                    Opnd::I(i) => {
                        let w = if bits == 64 { 64 } else { 32 };
                        let u = if w == 32 {
                            i as u64 & 0xffff_ffff
                        } else {
                            i as u64
                        };
                        match logical_imm(u, w) {
                            Some(enc) => self.put(imm | sf | enc << 10 | r(a) << 5 | r(dst)),
                            None if u == 0 => match op {
                                Alu::And => self.mov_rr(dst, ZR),
                                _ => self.mov_rr(dst, a),
                            },
                            None => {
                                // All ones.
                                if u == u64::MAX || (w == 32 && u == 0xffff_ffff) {
                                    match op {
                                        Alu::And => self.mov_rr(dst, a),
                                        Alu::Or => self.movimm(dst, -1),
                                        _ => self.put(0x2A20_03E0 | sf | r(a) << 16 | r(dst)),
                                    }
                                } else {
                                    self.movimm(X17, i);
                                    self.rrr(reg, ty, dst, a, X17);
                                }
                            }
                        }
                    }
                    Opnd::R(m) => self.rrr(reg, ty, dst, a, m),
                }
            }
        }
    }

    fn div(&mut self, op: Div, ty: Type, dst: Reg, a: Reg, b: Reg) {
        let bits = ty.bits();
        let signed = matches!(op, Div::Sdiv | Div::Srem);
        let (a, b, ty) = if bits < 32 {
            self.ext(signed, bits, X16, a);
            self.ext(signed, bits, X17, b);
            (X16, X17, crate::ir::types::I32)
        } else {
            (a, b, ty)
        };
        let div = if signed { 0x1AC0_0C00 } else { 0x1AC0_0800 };
        match op {
            Div::Udiv | Div::Sdiv => self.rrr(div, ty, dst, a, b),
            _ => {
                // q = a / b in a scratch, then dst = a - q * b
                let q = if a == X17 || b == X17 { X16 } else { X17 };
                let q = if bits < 32 { dst } else { q };
                self.rrr(div, ty, q, a, b);
                self.put(0x1B00_8000 | Self::sf(ty) | r(b) << 16 | r(a) << 10 | r(q) << 5 | r(dst));
            }
        }
    }

    fn smulhi(&mut self, ty: Type, dst: Reg, a: Reg, b: Reg) {
        if ty.bits() == 64 {
            self.put(0x9B40_7C00 | r(b) << 16 | r(a) << 5 | r(dst));
        } else {
            // smull x, wa, wb; asr x, x, #32
            self.put(0x9B20_7C00 | r(b) << 16 | r(a) << 5 | r(dst));
            self.put(0x9340_FC00 | 32 << 16 | r(dst) << 5 | r(dst));
        }
    }

    fn not(&mut self, ty: Type, dst: Reg, a: Reg) {
        self.put(0x2A20_03E0 | Self::sf(ty) | r(a) << 16 | r(dst));
    }

    fn neg(&mut self, ty: Type, dst: Reg, a: Reg) {
        self.put(0x4B00_03E0 | Self::sf(ty) | r(a) << 16 | r(dst));
    }

    fn extend(&mut self, signed: bool, from: Type, to: Type, dst: Reg, a: Reg) {
        let _ = to;
        self.ext(signed, from.bits(), dst, a);
    }

    fn cmp(&mut self, ty: Type, a: Reg, b: Opnd) {
        let bits = ty.bits();
        if bits < 32 {
            // Compare the values shifted to the top of a word: that keeps
            // both orders and equality.
            let s = 32 - bits;
            // lsl w16, a, #s
            self.put(0x5300_0000 | (32 - s) % 32 << 16 | (31 - s) << 10 | r(a) << 5 | r(X16));
            match b {
                Opnd::I(i) => self.movimm(X17, ((i as u32) << s) as i32 as i64),
                Opnd::R(m) => {
                    self.put(
                        0x5300_0000 | (32 - s) % 32 << 16 | (31 - s) << 10 | r(m) << 5 | r(X17),
                    );
                }
            }
            self.put(0x6B00_001F | r(X17) << 16 | r(X16) << 5);
            return;
        }
        let sf = Self::sf(ty);
        match b {
            Opnd::R(m) => self.put(0x6B00_001F | sf | r(m) << 16 | r(a) << 5),
            Opnd::I(i) => {
                let i = if bits == 64 { i } else { i64::from(i as i32) };
                if let Some((imm, sh)) = arith_imm(i) {
                    self.put(0x7100_001F | sf | sh << 22 | imm << 10 | r(a) << 5);
                } else if let Some((imm, sh)) = arith_imm(i.wrapping_neg()) {
                    self.put(0x3100_001F | sf | sh << 22 | imm << 10 | r(a) << 5);
                } else {
                    self.movimm(X17, i);
                    self.put(0x6B00_001F | sf | r(X17) << 16 | r(a) << 5);
                }
            }
        }
    }

    fn cmp_imm_ok(_imm: i64) -> bool {
        true
    }

    fn test(&mut self, ty: Type, a: Reg) {
        match ty.bits() {
            8 => self.put(0x7200_1C1F | r(a) << 5),
            16 => self.put(0x7200_3C1F | r(a) << 5),
            32 => self.put(0x6A00_001F | r(a) << 16 | r(a) << 5),
            _ => self.put(0xEA00_001F | r(a) << 16 | r(a) << 5),
        }
    }

    fn setcc(&mut self, c: Cond, dst: Reg) {
        // cset: csinc wd, wzr, wzr, !c
        self.put(0x1A9F_07E0 | cc(c.inverse()) << 12 | r(dst));
    }

    fn csel(&mut self, c: Cond, ty: Type, dst: Reg, x: Reg, y: Reg) {
        if ty.is_float() {
            let op = if ty.bits() == 32 {
                0x1E20_0C00
            } else {
                0x1E60_0C00
            };
            self.put(op | r(y) << 16 | cc(c) << 12 | r(x) << 5 | r(dst));
        } else {
            self.put(0x9A80_0000 | r(y) << 16 | cc(c) << 12 | r(x) << 5 | r(dst));
        }
    }

    fn overflow(&mut self, op: Overflow, ty: Type, dst: Reg, a: Reg, b: Reg) -> Cond {
        let sf = Self::sf(ty);
        match op {
            Overflow::Add => {
                self.put(0x2B00_0000 | sf | r(b) << 16 | r(a) << 5 | r(dst));
                Cond::O
            }
            Overflow::Sub => {
                self.put(0x6B00_0000 | sf | r(b) << 16 | r(a) << 5 | r(dst));
                Cond::O
            }
            Overflow::Mul => {
                if ty.bits() == 64 {
                    // x16 = low, x17 = high; overflow when high isn't low's
                    // sign.
                    self.put(0x9B00_7C00 | r(b) << 16 | r(a) << 5 | r(X16));
                    self.put(0x9B40_7C00 | r(b) << 16 | r(a) << 5 | r(X17));
                    // cmp x17, x16, asr #63
                    self.put(0xEB80_001F | r(X16) << 16 | 63 << 10 | r(X17) << 5);
                } else {
                    // smull x16, wa, wb; cmp x16, w16, sxtw
                    self.put(0x9B20_7C00 | r(b) << 16 | r(a) << 5 | r(X16));
                    self.put(0xEB20_C01F | r(X16) << 16 | r(X16) << 5);
                }
                self.mov_rr(dst, X16);
                Cond::Ne
            }
        }
    }

    fn fop(&mut self, op: Fop, ty: Type, dst: Reg, a: Reg, b: Reg) {
        let base = if ty.bits() == 32 {
            0x1E20_0000
        } else {
            0x1E60_0000
        };
        let code = match op {
            Fop::Add => 0x2800,
            Fop::Sub => 0x3800,
            Fop::Mul => 0x0800,
            Fop::Div => 0x1800,
        };
        self.put(base | code | r(b) << 16 | r(a) << 5 | r(dst));
    }

    fn fneg(&mut self, ty: Type, dst: Reg, a: Reg) {
        let op = if ty.bits() == 32 {
            0x1E21_4000
        } else {
            0x1E61_4000
        };
        self.put(op | r(a) << 5 | r(dst));
    }

    fn fabs(&mut self, ty: Type, dst: Reg, a: Reg) {
        let op = if ty.bits() == 32 {
            0x1E20_C000
        } else {
            0x1E60_C000
        };
        self.put(op | r(a) << 5 | r(dst));
    }

    fn fsqrt(&mut self, ty: Type, dst: Reg, a: Reg) {
        let op = if ty.bits() == 32 {
            0x1E21_C000
        } else {
            0x1E61_C000
        };
        self.put(op | r(a) << 5 | r(dst));
    }

    fn fcvt_from_sint(&mut self, from: Type, to: Type, dst: Reg, a: Reg) {
        let a = if from.bits() < 32 {
            self.ext(true, from.bits(), X17, a);
            X17
        } else {
            a
        };
        let sf = if from.bits() == 64 { 1 << 31 } else { 0 };
        let ftype = if to.bits() == 32 { 0 } else { 1 << 22 };
        self.put(0x1E22_0000 | sf | ftype | r(a) << 5 | r(dst));
    }

    fn bitcast(&mut self, ty: Type, dst: Reg, a: Reg) {
        let w = ty.bits() == 64;
        match (is_float(dst), w) {
            (true, true) => self.put(0x9E67_0000 | r(a) << 5 | r(dst)),
            (true, false) => self.put(0x1E27_0000 | r(a) << 5 | r(dst)),
            (false, true) => self.put(0x9E66_0000 | r(a) << 5 | r(dst)),
            (false, false) => self.put(0x1E26_0000 | r(a) << 5 | r(dst)),
        }
    }

    fn fcmp(&mut self, _cc: FloatCC, ty: Type, a: Reg, b: Reg) {
        let op = if ty.bits() == 32 {
            0x1E20_2000
        } else {
            0x1E60_2000
        };
        self.put(op | r(b) << 16 | r(a) << 5);
    }

    fn fsetcc(&mut self, c: FloatCC, dst: Reg) {
        let (a, b) = fcc(c);
        self.put(0x1A9F_07E0 | (a ^ 1) << 12 | r(dst));
        if let Some(b) = b {
            // cset w17, b; orr dst, dst, x17
            self.put(0x1A9F_07E0 | (b ^ 1) << 12 | r(X17));
            self.put(0xAA00_0000 | r(X17) << 16 | r(dst) << 5 | r(dst));
        }
    }

    fn fjcc(&mut self, c: FloatCC, l: Label) {
        let (a, b) = fcc(c);
        self.b_cond(a, l);
        if let Some(b) = b {
            self.b_cond(b, l);
        }
    }

    fn jmp(&mut self, l: Label) {
        let at = self.buf.len() as u32;
        self.put(0x1400_0000);
        self.fixups.push((at, Fix::B26(l)));
    }

    fn jcc(&mut self, c: Cond, l: Label) {
        self.b_cond(cc(c), l);
    }

    fn call(&mut self, target: Reg) {
        self.put(0xD63F_0000 | r(target) << 5);
    }

    fn br_table(&mut self, idx: Reg, ty: Type, default: Label, table: &[Label]) {
        let n = table.len() as i64;
        if ty.bits() == 64 {
            self.cmp(ty, idx, Opnd::I(n));
        } else {
            self.cmp(crate::ir::types::I32, idx, Opnd::I(n));
        }
        self.b_cond(cc(Cond::Uge), default);
        let tl = self.new_label();
        let at = self.buf.len() as u32;
        // adr x17, table
        self.put(0x1000_0000 | r(X17));
        self.fixups.push((at, Fix::Adr(tl)));
        // ldrsw x16, [x17, w_idx, uxtw #2] (or x_idx, lsl #2)
        if ty.bits() == 64 {
            self.put(0xB8A0_7800 | r(idx) << 16 | r(X17) << 5 | r(X16));
        } else {
            self.put(0xB8A0_5800 | r(idx) << 16 | r(X17) << 5 | r(X16));
        }
        self.put(0x8B00_0000 | r(X16) << 16 | r(X17) << 5 | r(X17));
        self.put(0xD61F_0000 | r(X17) << 5);
        self.tables.push((tl, table.to_vec()));
    }

    fn epilogue(&mut self, frame: &Frame) {
        let at = Self::save_at(frame);
        for (i, reg) in Self::saved_regs(frame).into_iter().enumerate() {
            self.ldr64(reg, SP, at + 8 * i as i32);
        }
        // mov sp, x29; ldp x29, x30, [sp], #16; ret
        self.put(0x9100_03BF);
        self.put(0xA8C1_7BFD);
        self.put(0xD65F_03C0);
    }

    fn finish(mut self, frame: &Frame) -> Result<crate::Code, Error> {
        let mut entries = Vec::new();
        for (tl, labels) in std::mem::take(&mut self.tables) {
            self.bind(tl);
            for l in labels {
                entries.push((self.buf.len() as u32, tl, l));
                self.put(0);
            }
        }
        let label = |s: &Self, l: Label| -> Result<i64, Error> {
            let at = s.labels[l.0 as usize];
            if at == u32::MAX {
                Err(Error::Internal("an unbound label"))
            } else {
                Ok(i64::from(at))
            }
        };
        for (at, fix) in std::mem::take(&mut self.fixups) {
            let w = &mut self.buf[at as usize];
            match fix {
                Fix::B26(l) => {
                    if self.labels[l.0 as usize] == u32::MAX {
                        return Err(Error::Internal("an unbound label"));
                    }
                    let d = i64::from(self.labels[l.0 as usize]) - i64::from(at);
                    *w |= (d as u32) & 0x03ff_ffff;
                }
                Fix::B19(l) => {
                    let d = i64::from(self.labels[l.0 as usize]) - i64::from(at);
                    if self.labels[l.0 as usize] == u32::MAX
                        || !(-(1 << 18)..(1 << 18)).contains(&d)
                    {
                        return Err(Error::Unsupported("a branch out of range"));
                    }
                    *w |= ((d as u32) & 0x7ffff) << 5;
                }
                Fix::Adr(l) => {
                    if self.labels[l.0 as usize] == u32::MAX {
                        return Err(Error::Internal("an unbound label"));
                    }
                    let d = (i64::from(self.labels[l.0 as usize]) - i64::from(at)) * 4;
                    *w |= ((d as u32) & 3) << 29 | (((d >> 2) as u32) & 0x7ffff) << 5;
                }
            }
        }
        for (at, tl, l) in entries {
            let d = (label(&self, l)? - label(&self, tl)?) * 4;
            self.buf[at as usize] = d as i32 as u32;
        }
        let size = Self::frame_size(frame);
        let save = Self::save_at(frame);
        if save + 8 * Self::saved_regs(frame).len() as i32 > 32_760 {
            return Err(Error::Unsupported("a frame this large"));
        }
        let mut pro = A64::new();
        // stp x29, x30, [sp, #-16]!; mov x29, sp
        pro.put(0xA9BF_7BFD);
        pro.put(0x9100_03FD);
        pro.sub_sp(size);
        for (i, reg) in Self::saved_regs(frame).into_iter().enumerate() {
            pro.str64(reg, SP, save + 8 * i as i32);
        }
        let p = pro.buf.len() as i64;
        for &at in &self.self_calls {
            let d = -(p + i64::from(at));
            self.buf[at as usize] |= (d as u32) & 0x03ff_ffff;
        }
        let mut out = Vec::with_capacity(4 * (pro.buf.len() + self.buf.len()));
        for w in pro.buf.iter().chain(&self.buf) {
            out.extend_from_slice(&w.to_le_bytes());
        }
        Ok(out.into())
    }
}
