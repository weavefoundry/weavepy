//! Constant folding, and division by constants as multiplication.
//!
//! Instructions whose operands are all constants become constants, a
//! branch on a constant becomes a jump, and a signed division or remainder
//! by a constant becomes a high multiply and shifts (Hacker's Delight,
//! 10-1), sharing the quotient between a division and a remainder of the
//! same operands. The frame JIT's and tier 2's guards against zero and
//! overflowing divisors fold away with the constants they test.

use crate::fxhash::FxHashMap as HashMap;

use crate::builder::canon_imm;
use crate::ir::{
    condcodes::IntCC, types, Alu, Block, Div, Function, Inst, InstData, Op, Type, Value, ValueDef,
};

fn resolve(alias: &[Value], mut v: Value) -> Value {
    while alias[v.index()] != v {
        v = alias[v.index()];
    }
    v
}

fn konst(f: &Function, alias: &[Value], v: Value) -> Option<i64> {
    match f.dfg.values[resolve(alias, v).index()].def {
        ValueDef::Result(inst, 0) => match f.dfg.insts[inst.index()].op {
            Op::Iconst(i) => Some(i),
            _ => None,
        },
        _ => None,
    }
}

/// The value `x` zero-extended from `ty`'s width.
fn zext(ty: Type, x: i64) -> u64 {
    if ty.bits() == 64 {
        x as u64
    } else {
        (x as u64) & ((1u64 << ty.bits()) - 1)
    }
}

fn alu(op: Alu, ty: Type, x: i64, y: i64) -> i64 {
    let bits = ty.bits();
    let s = (y as u32) & (bits - 1);
    canon_imm(
        ty,
        match op {
            Alu::Add => x.wrapping_add(y),
            Alu::Sub => x.wrapping_sub(y),
            Alu::Mul => x.wrapping_mul(y),
            Alu::And => x & y,
            Alu::Or => x | y,
            Alu::Xor => x ^ y,
            Alu::Shl => x.wrapping_shl(s),
            Alu::Ushr => (zext(ty, x) >> s) as i64,
            Alu::Sshr => canon_imm(ty, x) >> s,
        },
    )
}

fn icmp(cc: IntCC, ty: Type, x: i64, y: i64) -> bool {
    let (sx, sy) = (canon_imm(ty, x), canon_imm(ty, y));
    let (ux, uy) = (zext(ty, x), zext(ty, y));
    match cc {
        IntCC::Equal => ux == uy,
        IntCC::NotEqual => ux != uy,
        IntCC::SignedLessThan => sx < sy,
        IntCC::SignedGreaterThanOrEqual => sx >= sy,
        IntCC::SignedGreaterThan => sx > sy,
        IntCC::SignedLessThanOrEqual => sx <= sy,
        IntCC::UnsignedLessThan => ux < uy,
        IntCC::UnsignedGreaterThanOrEqual => ux >= uy,
        IntCC::UnsignedGreaterThan => ux > uy,
        IntCC::UnsignedLessThanOrEqual => ux <= uy,
    }
}

/// `x op k` when `k` decides it whatever `x` is.
fn annihilated(op: Alu, ty: Type, k: i64) -> Option<i64> {
    match (op, canon_imm(ty, k)) {
        (Alu::And | Alu::Mul, 0) => Some(0),
        (Alu::Or, -1) => Some(canon_imm(ty, -1)),
        _ => None,
    }
}

/// For a branch on, or a test against zero of, a zero-extended value: the
/// value before the extension.
fn widened(f: &Function, args: &[Value], op: &Op) -> Option<Value> {
    if !matches!(
        op,
        Op::Brif(..) | Op::IcmpImm(IntCC::Equal | IntCC::NotEqual, 0)
    ) {
        return None;
    }
    let ValueDef::Result(def, 0) = f.dfg.values[args.first()?.index()].def else {
        return None;
    };
    let d = &f.dfg.insts[def.index()];
    matches!(d.op, Op::Uextend).then(|| f.dfg.slice(d.args)[0])
}

/// Whether `v` is a comparison's 0 or 1.
fn is_bool(f: &Function, v: Value) -> bool {
    match f.dfg.values[v.index()].def {
        ValueDef::Result(def, 0) => matches!(
            f.dfg.insts[def.index()].op,
            Op::Icmp(_) | Op::IcmpImm(..) | Op::Fcmp(_)
        ),
        _ => false,
    }
}

/// The comparison `v` is, inverted.
fn negate(e: &mut Emit<'_>, v: Value) -> Value {
    let ValueDef::Result(def, 0) = e.f.dfg.values[v.index()].def else {
        unreachable!("a comparison");
    };
    let d = e.f.dfg.insts[def.index()].clone();
    let args = e.f.dfg.slice(d.args).to_vec();
    let op = match d.op {
        Op::Icmp(cc) => Op::Icmp(cc.inverse()),
        Op::IcmpImm(cc, k) => Op::IcmpImm(cc.inverse(), k),
        Op::Fcmp(cc) => Op::Fcmp(cc.inverse()),
        _ => unreachable!("a comparison"),
    };
    e.push(op, &args, types::I8)
}

/// The magic multiplier and shift for signed division by `d` (`|d| >= 2`)
/// at `bits` (32 or 64): Hacker's Delight's `magic`, in `bits`-bit
/// unsigned arithmetic.
pub(crate) fn magic(d: i64, bits: u32) -> (i64, u32) {
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let two = 1u64 << (bits - 1);
    let ad = d.unsigned_abs() & mask;
    let t = two + ((d as u64 & mask) >> (bits - 1));
    let anc = t - 1 - t % ad;
    let mut p = bits - 1;
    let (mut q1, mut r1) = (two / anc, two - (two / anc) * anc);
    let (mut q2, mut r2) = (two / ad, two - (two / ad) * ad);
    loop {
        p += 1;
        q1 = q1.wrapping_mul(2) & mask;
        r1 = r1.wrapping_mul(2) & mask;
        if r1 >= anc {
            q1 = (q1 + 1) & mask;
            r1 = (r1 - anc) & mask;
        }
        q2 = q2.wrapping_mul(2) & mask;
        r2 = r2.wrapping_mul(2) & mask;
        if r2 >= ad {
            q2 = (q2 + 1) & mask;
            r2 = (r2 - ad) & mask;
        }
        let delta = (ad - r2) & mask;
        if !(q1 < delta || (q1 == delta && r1 == 0)) {
            break;
        }
    }
    let m = (q2 + 1) & mask;
    let m = if bits == 64 {
        m as i64
    } else {
        i64::from(m as u32 as i32)
    };
    let m = if d < 0 { m.wrapping_neg() } else { m };
    (
        canon_imm(if bits == 64 { types::I64 } else { types::I32 }, m),
        p - bits,
    )
}

struct Emit<'a> {
    f: &'a mut Function,
    alias: &'a mut Vec<Value>,
    out: Vec<Inst>,
}

impl Emit<'_> {
    fn push(&mut self, op: Op, args: &[Value], ty: Type) -> Value {
        let dfg = &mut self.f.dfg;
        let inst = Inst(dfg.insts.len() as u32);
        let v = dfg.make_value(ty, ValueDef::Result(inst, 0));
        let args = dfg.intern(args);
        let results = dfg.intern(&[v]);
        dfg.insts.push(InstData {
            op,
            args,
            results,
            ty,
        });
        self.alias.push(v);
        self.out.push(inst);
        v
    }

    fn iconst(&mut self, ty: Type, i: i64) -> Value {
        self.push(Op::Iconst(canon_imm(ty, i)), &[], ty)
    }

    fn imm(&mut self, op: Alu, ty: Type, x: Value, i: i64) -> Value {
        self.push(Op::AluImm(op, canon_imm(ty, i)), &[x], ty)
    }

    fn bin(&mut self, op: Alu, ty: Type, x: Value, y: Value) -> Value {
        self.push(Op::Alu(op), &[x, y], ty)
    }

    /// `x / d`, truncating, for a constant `d` other than 0, 1 and -1.
    fn sdiv(&mut self, ty: Type, x: Value, d: i64) -> Value {
        let bits = ty.bits();
        let ad = d.unsigned_abs();
        let q = if ad.is_power_of_two() {
            let k = ad.trailing_zeros();
            // (x + (x < 0 ? 2^k - 1 : 0)) >> k
            let sign = self.imm(Alu::Sshr, ty, x, i64::from(bits - 1));
            let bias = self.imm(Alu::Ushr, ty, sign, i64::from(bits - k));
            let t = self.bin(Alu::Add, ty, x, bias);
            self.imm(Alu::Sshr, ty, t, i64::from(k))
        } else {
            let (m, s) = magic(d, bits);
            let mv = self.iconst(ty, m);
            let mut q = self.push(Op::Smulhi, &[x, mv], ty);
            if d > 0 && m < 0 {
                q = self.bin(Alu::Add, ty, q, x);
            } else if d < 0 && m > 0 {
                q = self.bin(Alu::Sub, ty, q, x);
            }
            if s > 0 {
                q = self.imm(Alu::Sshr, ty, q, i64::from(s));
            }
            let t = self.imm(Alu::Ushr, ty, q, i64::from(bits - 1));
            return self.bin(Alu::Add, ty, q, t);
        };
        if d < 0 {
            self.push(Op::Ineg, &[q], ty)
        } else {
            q
        }
    }
}

/// Fold constants and divide by constants, in every block of `f`; the
/// replaced values' new values go in `alias` (grown for the values this
/// makes).
pub(crate) fn fold(f: &mut Function, alias: &mut Vec<Value>) {
    for b in f.layout.clone() {
        let insts = std::mem::take(&mut f.dfg.blocks[b.index()].insts);
        let mut e = Emit {
            f,
            alias,
            out: Vec::with_capacity(insts.len()),
        };
        // Quotients made in this block, by dividend and divisor.
        let mut quotients: HashMap<(Value, i64), Value> = HashMap::default();
        for inst in insts {
            fold_one(&mut e, b, inst, &mut quotients);
        }
        let out = std::mem::take(&mut e.out);
        f.dfg.blocks[b.index()].insts = out;
    }
}

fn fold_one(e: &mut Emit<'_>, b: Block, inst: Inst, quotients: &mut HashMap<(Value, i64), Value>) {
    let d = e.f.dfg.insts[inst.index()].clone();
    let args: Vec<Value> =
        e.f.dfg
            .slice(d.args)
            .iter()
            .map(|&v| resolve(e.alias, v))
            .collect();
    let c = |e: &Emit<'_>, k: usize| konst(e.f, e.alias, args[k]);
    let ty = d.ty;
    let folded: Option<i64> = match &d.op {
        Op::Alu(op) if ty.is_int() => match (c(e, 0), c(e, 1)) {
            (Some(x), Some(y)) => Some(alu(*op, ty, x, y)),
            (Some(k), None) | (None, Some(k)) => annihilated(*op, ty, k),
            _ => None,
        },
        Op::AluImm(op, y) if ty.is_int() => c(e, 0)
            .map(|x| alu(*op, ty, x, *y))
            .or_else(|| annihilated(*op, ty, *y)),
        Op::Icmp(cc) => {
            let aty = e.f.dfg.value_type(args[0]);
            match (c(e, 0), c(e, 1)) {
                (Some(x), Some(y)) => Some(i64::from(icmp(*cc, aty, x, y))),
                _ => None,
            }
        }
        Op::IcmpImm(cc, y) => {
            let aty = e.f.dfg.value_type(args[0]);
            c(e, 0).map(|x| i64::from(icmp(*cc, aty, x, *y)))
        }
        Op::Uextend => {
            let aty = e.f.dfg.value_type(args[0]);
            c(e, 0).map(|x| canon_imm(ty, zext(aty, x) as i64))
        }
        Op::Sextend => {
            let aty = e.f.dfg.value_type(args[0]);
            c(e, 0).map(|x| canon_imm(ty, canon_imm(aty, x)))
        }
        Op::Ireduce => c(e, 0).map(|x| canon_imm(ty, x)),
        Op::Bnot if ty.is_int() => c(e, 0).map(|x| canon_imm(ty, !x)),
        Op::Ineg if ty.is_int() => c(e, 0).map(|x| canon_imm(ty, x.wrapping_neg())),
        _ => None,
    };
    if let Some(k) = folded {
        let insn = &mut e.f.dfg.insts[inst.index()];
        insn.op = Op::Iconst(k);
        insn.args = (0, 0);
        e.out.push(inst);
        return;
    }
    // A compare with a constant takes it as its immediate.
    if let Op::Icmp(cc) = d.op {
        let (k0, k1) = (c(e, 0), c(e, 1));
        let swap = match (k0, k1) {
            (None, Some(k)) => Some((cc, args[0], k)),
            (Some(k), None) => Some((cc.swapped(), args[1], k)),
            _ => None,
        };
        if let Some((cc, x, k)) = swap {
            let aty = e.f.dfg.value_type(x);
            let a = e.f.dfg.intern(&[x]);
            let insn = &mut e.f.dfg.insts[inst.index()];
            insn.op = Op::IcmpImm(cc, canon_imm(aty, k));
            insn.args = a;
            return fold_one(e, b, inst, quotients);
        }
    }
    // Booleans widened only to be tested: test them as they are.
    if let Some(src) = widened(e.f, &args, &d.op) {
        match &d.op {
            Op::Brif(..) => {
                let a = e.f.dfg.intern(&[src]);
                e.f.dfg.insts[inst.index()].args = a;
                return fold_one(e, b, inst, quotients);
            }
            Op::IcmpImm(cc, 0) => {
                let r = e.f.dfg.slice(d.results)[0];
                if e.f.dfg.value_type(src) == types::I8 && is_bool(e.f, src) {
                    let v = if *cc == IntCC::NotEqual {
                        src
                    } else {
                        negate(e, src)
                    };
                    e.alias[r.index()] = v;
                    return;
                }
                let a = e.f.dfg.intern(&[src]);
                e.f.dfg.insts[inst.index()].args = a;
            }
            _ => {}
        }
    }
    match &d.op {
        Op::Select => {
            if let Some(cv) = c(e, 0) {
                let cty = e.f.dfg.value_type(args[0]);
                let pick = if zext(cty, cv) != 0 { args[1] } else { args[2] };
                let r = e.f.dfg.slice(d.results)[0];
                e.alias[r.index()] = pick;
                return;
            }
        }
        Op::Brif(t, el) => {
            if let Some(cv) = c(e, 0) {
                let cty = e.f.dfg.value_type(args[0]);
                let edge = if zext(cty, cv) != 0 { *t } else { *el };
                let insn = &mut e.f.dfg.insts[inst.index()];
                insn.op = Op::Jump(edge);
                insn.args = (0, 0);
            }
        }
        Op::Div(op) if ty.bits() >= 32 => {
            if let Some(dv) = c(e, 1) {
                let dv = canon_imm(ty, dv);
                let x = args[0];
                let r = e.f.dfg.slice(d.results)[0];
                let signed = matches!(op, Div::Sdiv | Div::Srem);
                let rem = matches!(op, Div::Srem | Div::Urem);
                let min = if ty.bits() == 64 {
                    i64::MIN
                } else {
                    i64::from(i32::MIN)
                };
                let out = if dv == 1 {
                    Some(if rem { e.iconst(ty, 0) } else { x })
                } else if signed && dv != 0 && dv != -1 && dv != min {
                    let q = match quotients.get(&(x, dv)) {
                        Some(&q) => q,
                        None => {
                            let q = e.sdiv(ty, x, dv);
                            quotients.insert((x, dv), q);
                            q
                        }
                    };
                    Some(if rem {
                        let p = e.imm(Alu::Mul, ty, q, dv);
                        e.bin(Alu::Sub, ty, x, p)
                    } else {
                        q
                    })
                } else if !signed && dv != 0 && zext(ty, dv).is_power_of_two() {
                    let k = zext(ty, dv).trailing_zeros();
                    Some(if rem {
                        e.imm(Alu::And, ty, x, zext(ty, dv) as i64 - 1)
                    } else {
                        e.imm(Alu::Ushr, ty, x, i64::from(k))
                    })
                } else {
                    None
                };
                if let Some(v) = out {
                    e.alias[r.index()] = v;
                    return;
                }
            }
        }
        _ => {}
    }
    e.out.push(inst);
}

#[cfg(test)]
mod tests {
    use super::magic;

    /// `x / d` the way the lowering computes it.
    fn by_magic(x: i64, d: i64, bits: u32) -> i64 {
        let (m, s) = magic(d, bits);
        let (x, m) = if bits == 32 {
            (i64::from(x as i32), i64::from(m as i32))
        } else {
            (x, m)
        };
        let mut q = if bits == 64 {
            ((i128::from(x) * i128::from(m)) >> 64) as i64
        } else {
            (x * m) >> 32
        };
        if d > 0 && m < 0 {
            q = q.wrapping_add(x);
        } else if d < 0 && m > 0 {
            q = q.wrapping_sub(x);
        }
        q >>= s;
        let sign = if bits == 64 {
            (q as u64 >> 63) as i64
        } else {
            i64::from((q as u32) >> 31)
        };
        let q = q.wrapping_add(sign);
        if bits == 32 {
            i64::from(q as i32)
        } else {
            q
        }
    }

    #[test]
    fn magic_divides() {
        let xs = [
            0i64,
            1,
            -1,
            2,
            7,
            -7,
            100,
            -100,
            12345,
            -98765,
            i64::MAX,
            i64::MIN + 1,
            1 << 40,
            -(1 << 50),
        ];
        for bits in [32u32, 64] {
            for d in
                (-3000i64..3000).chain([65537, -65537, 1 << 30, 1_000_000_007, i64::from(i32::MAX)])
            {
                if (-1..=1).contains(&d) || (d.unsigned_abs().is_power_of_two()) {
                    continue;
                }
                for &x in &xs {
                    let x = if bits == 32 { i64::from(x as i32) } else { x };
                    let want = if bits == 32 {
                        i64::from((x as i32).wrapping_div(d as i32))
                    } else {
                        x.wrapping_div(d)
                    };
                    assert_eq!(by_magic(x, d, bits), want, "{x} / {d} at {bits} bits");
                }
            }
        }
    }
}
