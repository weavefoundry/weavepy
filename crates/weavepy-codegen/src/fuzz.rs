//! Random functions, compiled and interpreted, compared.

use crate::ir::condcodes::{FloatCC, IntCC};
use crate::ir::{
    types, AbiParam, Alu, Block, CallConv, Div, Ext, Fop, Function, MemFlags, Op, Overflow,
    Signature, Type, Value, ValueDef,
};
use crate::{compile_and_install, FunctionBuilder};

const F: MemFlags = MemFlags::trusted();
const MEM: usize = 128;

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }
}

macro_rules! mix {
    ($name:ident, $($a:ident),*) => {
        extern "C" fn $name($($a: i64),*) -> i64 {
            let mut h: i64 = 0x9e37_79b9;
            $(h = h.wrapping_mul(31).wrapping_add($a) ^ (h >> 7);)*
            h
        }
    };
}
mix!(mix1, a);
mix!(mix2, a, b);
mix!(mix3, a, b, c);
mix!(mix5, a, b, c, d, e);
mix!(mix8, a, b, c, d, e, f, g, h);

fn mix_fn(n: usize) -> (usize, usize) {
    match n {
        1 => (mix1 as *const () as usize, 1),
        2 => (mix2 as *const () as usize, 2),
        3 => (mix3 as *const () as usize, 3),
        4 | 5 => (mix5 as *const () as usize, 5),
        _ => (mix8 as *const () as usize, 8),
    }
}

fn call_mix(n: usize, a: &[i64]) -> i64 {
    match n {
        1 => mix1(a[0]),
        2 => mix2(a[0], a[1]),
        3 => mix3(a[0], a[1], a[2]),
        5 => mix5(a[0], a[1], a[2], a[3], a[4]),
        _ => mix8(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]),
    }
}

const INT_CCS: [IntCC; 10] = [
    IntCC::Equal,
    IntCC::NotEqual,
    IntCC::SignedLessThan,
    IntCC::SignedGreaterThanOrEqual,
    IntCC::SignedGreaterThan,
    IntCC::SignedLessThanOrEqual,
    IntCC::UnsignedLessThan,
    IntCC::UnsignedGreaterThanOrEqual,
    IntCC::UnsignedGreaterThan,
    IntCC::UnsignedLessThanOrEqual,
];

const FLOAT_CCS: [FloatCC; 14] = [
    FloatCC::OrderedNotEqual,
    FloatCC::UnorderedOrEqual,
    FloatCC::UnorderedOrLessThan,
    FloatCC::UnorderedOrLessThanOrEqual,
    FloatCC::UnorderedOrGreaterThan,
    FloatCC::UnorderedOrGreaterThanOrEqual,
    FloatCC::Equal,
    FloatCC::NotEqual,
    FloatCC::LessThan,
    FloatCC::LessThanOrEqual,
    FloatCC::GreaterThan,
    FloatCC::GreaterThanOrEqual,
    FloatCC::Ordered,
    FloatCC::Unordered,
];

/// Generate a random function of `(a, b, mem) -> i64`.
fn generate(seed: u64) -> Function {
    let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1);
    let mut f = Function::new();
    for _ in 0..3 {
        f.signature.params.push(AbiParam::new(types::I64));
    }
    f.signature.returns.push(AbiParam::new(types::I64));
    let nblocks = 2 + rng.below(7);
    {
        let mut b = FunctionBuilder::new(&mut f);
        let blocks: Vec<Block> = (0..nblocks).map(|_| b.create_block()).collect();
        let mut params: Vec<Vec<Value>> = vec![Vec::new(); nblocks];
        b.append_block_params_for_function_params(blocks[0]);
        for (i, &bl) in blocks.iter().enumerate().skip(1) {
            // (Now and then more parameters than registers.)
            let n = if rng.below(6) == 0 {
                10 + rng.below(12)
            } else {
                rng.below(3)
            };
            for _ in 0..n {
                params[i].push(b.append_block_param(bl, types::I64));
            }
            if rng.below(4) == 0 {
                b.set_cold_block(bl);
            }
        }
        let entry_params = b.block_params(blocks[0]).to_vec();
        let mem = entry_params[2];
        let mut entry_vals: Vec<Value> = entry_params[..2].to_vec();
        for i in 0..nblocks {
            b.switch_to_block(blocks[i]);
            let mut ints: Vec<Value> = entry_vals.clone();
            ints.extend_from_slice(&params[i]);
            let n = 3 + rng.below(25);
            for _ in 0..n {
                let x = rng.pick(&ints);
                let y = rng.pick(&ints);
                let v = match rng.below(20) {
                    0 => b.ins().iconst(types::I64, rng.next() as i64 >> rng.below(64)),
                    1 => {
                        let op = rng.pick(&[Alu::Add, Alu::Sub, Alu::Mul, Alu::And, Alu::Or, Alu::Xor]);
                        match op {
                            Alu::Add => b.ins().iadd(x, y),
                            Alu::Sub => b.ins().isub(x, y),
                            Alu::Mul => b.ins().imul(x, y),
                            Alu::And => b.ins().band(x, y),
                            Alu::Or => b.ins().bor(x, y),
                            _ => b.ins().bxor(x, y),
                        }
                    }
                    2 => {
                        let imm = rng.next() as i64 >> rng.below(64);
                        match rng.below(6) {
                            0 => b.ins().iadd_imm(x, imm),
                            1 => b.ins().imul_imm(x, imm),
                            2 => b.ins().band_imm(x, imm),
                            3 => b.ins().bor_imm(x, imm),
                            4 => b.ins().bxor_imm(x, imm),
                            _ => b.ins().iadd_imm(x, rng.below(9) as i64 - 4),
                        }
                    }
                    3 => {
                        let s = rng.below(70) as i64;
                        match rng.below(6) {
                            0 => b.ins().ishl_imm(x, s),
                            1 => b.ins().ushr_imm(x, s),
                            2 => b.ins().sshr_imm(x, s),
                            3 => b.ins().ishl(x, y),
                            4 => b.ins().ushr(x, y),
                            _ => b.ins().sshr(x, y),
                        }
                    }
                    4 => {
                        let c = b.ins().icmp(rng.pick(&INT_CCS), x, y);
                        b.ins().uextend(types::I64, c)
                    }
                    5 => {
                        let c = b.ins().icmp_imm(rng.pick(&INT_CCS), x, rng.next() as i64 >> rng.below(64));
                        let z = rng.pick(&ints);
                        b.ins().select(c, y, z)
                    }
                    6 => {
                        // Narrow arithmetic.
                        let ty = rng.pick(&[types::I8, types::I16, types::I32]);
                        let xn = b.ins().ireduce(ty, x);
                        let yn = b.ins().ireduce(ty, y);
                        let r = match rng.below(5) {
                            0 => b.ins().iadd(xn, yn),
                            1 => b.ins().isub(xn, yn),
                            2 => b.ins().imul(xn, yn),
                            3 => b.ins().ushr_imm(xn, rng.below(9) as i64),
                            _ => b.ins().sshr(xn, yn),
                        };
                        if rng.below(2) == 0 {
                            let c = b.ins().icmp(rng.pick(&INT_CCS), r, xn);
                            b.ins().uextend(types::I64, c)
                        } else if rng.below(2) == 0 {
                            b.ins().uextend(types::I64, r)
                        } else {
                            b.ins().sextend(types::I64, r)
                        }
                    }
                    7 => {
                        let d = b.ins().band_imm(y, 0xff);
                        let d = b.ins().bor_imm(d, 1);
                        match rng.below(4) {
                            0 => b.ins().udiv(x, d),
                            1 => b.ins().urem(x, d),
                            2 => b.ins().sdiv(x, d),
                            _ => b.ins().srem(x, d),
                        }
                    }
                    8 => {
                        let off = 8 * rng.below(MEM / 8 - 1) as i32;
                        let (mem, off) = if rng.below(2) == 0 {
                            // A derived address, offset back.
                            let k = 8 * rng.below(4) as i64;
                            let p = b.ins().iadd_imm(mem, k);
                            let p = b.ins().iadd_imm(p, 8);
                            (p, off - k as i32 - 8)
                        } else {
                            (mem, off)
                        };
                        match rng.below(5) {
                            0 => b.ins().load(types::I64, F, mem, off),
                            1 => b.ins().uload8(types::I64, F, mem, off + rng.below(8) as i32),
                            2 => b.ins().uload32(F, mem, off),
                            3 => b.ins().sload32(F, mem, off + 4),
                            _ => {
                                let t = b.ins().uload16(types::I32, F, mem, off + 2);
                                b.ins().uextend(types::I64, t)
                            }
                        }
                    }
                    9 => {
                        let off = 8 * rng.below(MEM / 8 - 1) as i32;
                        let (mem, off) = if rng.below(2) == 0 {
                            let p = b.ins().iadd_imm(mem, 16);
                            (p, off - 16)
                        } else {
                            (mem, off)
                        };
                        match rng.below(3) {
                            0 => {
                                b.ins().store(F, x, mem, off);
                            }
                            1 => {
                                let k = b.ins().iconst(types::I8, rng.next() as i64);
                                b.ins().store(F, k, mem, off + rng.below(8) as i32);
                            }
                            _ => {
                                let t = b.ins().ireduce(types::I32, x);
                                b.ins().store(F, t, mem, off + 4);
                            }
                        }
                        continue;
                    }
                    10 | 11 => {
                        let (addr, arity) = mix_fn(1 + rng.below(8));
                        let mut s = Signature::new(CallConv::host());
                        for _ in 0..arity {
                            s.params.push(AbiParam::new(types::I64));
                        }
                        s.returns.push(AbiParam::new(types::I64));
                        let sig = b.import_signature(s);
                        let callee = b.ins().iconst(types::I64, addr as i64);
                        let args: Vec<Value> = (0..arity).map(|_| rng.pick(&ints)).collect();
                        let c = b.ins().call_indirect(sig, callee, &args);
                        b.inst_results(c)[0]
                    }
                    12 => {
                        let (s, o) = match rng.below(3) {
                            0 => b.ins().sadd_overflow(x, y),
                            1 => b.ins().ssub_overflow(x, y),
                            _ => b.ins().smul_overflow(x, y),
                        };
                        let o = b.ins().uextend(types::I64, o);
                        b.ins().bxor(s, o)
                    }
                    13 => {
                        // Floats from small integers, so nothing is NaN.
                        let xs = b.ins().sshr_imm(x, 40);
                        let ys = b.ins().sshr_imm(y, 40);
                        let fx = b.ins().fcvt_from_sint(types::F64, xs);
                        let fy = b.ins().fcvt_from_sint(types::F64, ys);
                        let r = match rng.below(4) {
                            0 => b.ins().fadd(fx, fy),
                            1 => b.ins().fsub(fx, fy),
                            2 => b.ins().fmul(fx, fy),
                            _ => b.ins().fneg(fx),
                        };
                        if rng.below(2) == 0 {
                            let c = b.ins().fcmp(rng.pick(&FLOAT_CCS), r, fy);
                            b.ins().uextend(types::I64, c)
                        } else {
                            b.ins().bitcast(types::I64, F, r)
                        }
                    }
                    14 => b.ins().bnot(x),
                    15 => b.ins().ineg(x),
                    16 => {
                        let t = b.ins().ireduce(types::I32, x);
                        b.ins().sextend(types::I64, t)
                    }
                    _ => b.ins().iadd(x, y),
                };
                ints.push(v);
            }
            if i == 0 {
                entry_vals = ints.clone();
                entry_vals.truncate(entry_vals.len().min(12));
            }
            // The terminator: forward only, so every run ends.
            if i + 1 == nblocks {
                let r = rng.pick(&ints);
                b.ins().return_(&[r]);
                continue;
            }
            let edge = |rng: &mut Rng, t: usize| -> (Block, Vec<crate::BlockArg>) {
                let args = params[t].iter().map(|_| rng.pick(&ints).into()).collect();
                (blocks[t], args)
            };
            let t1 = i + 1 + rng.below(nblocks - i - 1);
            let t2 = i + 1 + rng.below(nblocks - i - 1);
            match rng.below(6) {
                0 => {
                    let (bl, args) = edge(&mut rng, t1);
                    b.ins().jump(bl, &args);
                }
                1 if params[t1].is_empty() && params[t2].is_empty() => {
                    let x = rng.pick(&ints);
                    let idx = b.ins().band_imm(x, 3);
                    let idx = b.ins().ireduce(types::I32, idx);
                    let calls: Vec<_> = (0..3)
                        .map(|k| {
                            let t = if k % 2 == 0 { t1 } else { t2 };
                            b.func.dfg.block_call(blocks[t], &[])
                        })
                        .collect();
                    let def = b.func.dfg.block_call(blocks[t2], &[]);
                    let jt = b.create_jump_table(crate::JumpTableData::new(def, &calls));
                    b.ins().br_table(idx, jt);
                }
                _ => {
                    let x = rng.pick(&ints);
                    let y = rng.pick(&ints);
                    let c = if rng.below(2) == 0 {
                        b.ins().icmp(rng.pick(&INT_CCS), x, y)
                    } else {
                        x
                    };
                    let (b1, a1) = edge(&mut rng, t1);
                    let (b2, a2) = edge(&mut rng, t2);
                    b.ins().brif(c, b1, &a1, b2, &a2);
                }
            }
        }
    }
    f
}

/// Normalize a value to its type's width, sign-extended.
fn norm(ty: Type, x: i64) -> i64 {
    crate::builder::canon_imm(ty, x)
}

fn zext(ty: Type, x: i64) -> u64 {
    if ty.bits() == 64 {
        x as u64
    } else {
        (x as u64) & ((1u64 << ty.bits()) - 1)
    }
}

fn icmp(cc: IntCC, ty: Type, x: i64, y: i64) -> bool {
    let (sx, sy) = (norm(ty, x), norm(ty, y));
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

/// Run `f` on `args`, with `mem` its memory.
fn interpret(f: &Function, args: &[i64], mem: &mut [u8; MEM]) -> i64 {
    let dfg = &f.dfg;
    let mut vals = vec![0i64; dfg.values.len()];
    let ty = |v: Value| dfg.value_type(v);
    let get = |vals: &[i64], v: Value| -> i64 {
        if let ValueDef::Result(inst, 0) = dfg.values[v.index()].def {
            match dfg.insts[inst.index()].op {
                Op::Iconst(i) => return i,
                Op::Fconst(b) => return b as i64,
                _ => {}
            }
        }
        vals[v.index()]
    };
    let mut block = f.layout[0];
    let mut incoming: Vec<i64> = args.to_vec();
    let fl = |x: i64| f64::from_bits(x as u64);
    loop {
        let bd = &dfg.blocks[block.index()];
        for (p, x) in bd.params.iter().zip(&incoming) {
            vals[p.index()] = *x;
        }
        for &inst in &bd.insts {
            let d = &dfg.insts[inst.index()];
            let a = dfg.slice(d.args);
            let res = dfg.slice(d.results);
            let arg = |k: usize| get(&vals, a[k]);
            let rt = d.ty;
            let out: Option<i64> = match &d.op {
                Op::Iconst(i) => Some(*i),
                Op::Fconst(b) => Some(*b as i64),
                Op::Load { bytes, ext, off } => {
                    let at = (arg(0) - mem.as_ptr() as i64 + i64::from(*off)) as usize;
                    let mut raw = [0u8; 8];
                    raw[..*bytes as usize].copy_from_slice(&mem[at..at + *bytes as usize]);
                    let u = u64::from_le_bytes(raw);
                    let x = match (bytes, ext) {
                        (8, _) => u as i64,
                        (_, Ext::Sign) => {
                            let s = 64 - 8 * u32::from(*bytes);
                            ((u << s) as i64) >> s
                        }
                        _ => u as i64,
                    };
                    Some(norm(rt, x))
                }
                Op::Store { bytes, off } => {
                    let at = (arg(1) - mem.as_ptr() as i64 + i64::from(*off)) as usize;
                    let raw = arg(0).to_le_bytes();
                    mem[at..at + *bytes as usize].copy_from_slice(&raw[..*bytes as usize]);
                    None
                }
                Op::Icmp(cc) => Some(i64::from(icmp(*cc, ty(a[0]), arg(0), arg(1)))),
                Op::IcmpImm(cc, imm) => Some(i64::from(icmp(*cc, ty(a[0]), arg(0), *imm))),
                Op::Fcmp(cc) => {
                    let (x, y) = (fl(arg(0)), fl(arg(1)));
                    let uno = x.is_nan() || y.is_nan();
                    Some(i64::from(match cc {
                        FloatCC::Equal => x == y,
                        FloatCC::NotEqual => x != y,
                        FloatCC::LessThan => x < y,
                        FloatCC::LessThanOrEqual => x <= y,
                        FloatCC::GreaterThan => x > y,
                        FloatCC::GreaterThanOrEqual => x >= y,
                        FloatCC::Ordered => !uno,
                        FloatCC::Unordered => uno,
                        FloatCC::OrderedNotEqual => !uno && x != y,
                        FloatCC::UnorderedOrEqual => uno || x == y,
                        FloatCC::UnorderedOrLessThan => uno || x < y,
                        FloatCC::UnorderedOrLessThanOrEqual => uno || x <= y,
                        FloatCC::UnorderedOrGreaterThan => uno || x > y,
                        FloatCC::UnorderedOrGreaterThanOrEqual => uno || x >= y,
                    }))
                }
                Op::Alu(op) | Op::AluImm(op, _) => {
                    let x = arg(0);
                    let y = match &d.op {
                        Op::AluImm(_, i) => *i,
                        _ => arg(1),
                    };
                    let bits = rt.bits();
                    let s = (y as u32) & (bits - 1);
                    Some(norm(
                        rt,
                        match op {
                            Alu::Add => x.wrapping_add(y),
                            Alu::Sub => x.wrapping_sub(y),
                            Alu::Mul => x.wrapping_mul(y),
                            Alu::And => x & y,
                            Alu::Or => x | y,
                            Alu::Xor => x ^ y,
                            Alu::Shl => x.wrapping_shl(s),
                            Alu::Ushr => (zext(rt, x) >> s) as i64,
                            Alu::Sshr => norm(rt, x) >> s,
                        },
                    ))
                }
                Op::Div(op) => {
                    let (x, y) = (arg(0), arg(1));
                    Some(norm(
                        rt,
                        match op {
                            Div::Udiv => (zext(rt, x) / zext(rt, y)) as i64,
                            Div::Urem => (zext(rt, x) % zext(rt, y)) as i64,
                            Div::Sdiv => norm(rt, x).wrapping_div(norm(rt, y)),
                            Div::Srem => norm(rt, x).wrapping_rem(norm(rt, y)),
                        },
                    ))
                }
                Op::Bnot => Some(norm(rt, !arg(0))),
                Op::Ineg => Some(norm(rt, arg(0).wrapping_neg())),
                Op::Smulhi => Some(((i128::from(arg(0)) * i128::from(arg(1))) >> 64) as i64),
                Op::Select => Some(if zext(ty(a[0]), arg(0)) != 0 { arg(1) } else { arg(2) }),
                Op::Ireduce => Some(norm(rt, arg(0))),
                Op::Uextend => Some(zext(ty(a[0]), arg(0)) as i64),
                Op::Sextend => Some(norm(ty(a[0]), arg(0))),
                Op::Overflow(op) => {
                    let (x, y) = (arg(0), arg(1));
                    let (r, o) = match op {
                        Overflow::Add => x.overflowing_add(y),
                        Overflow::Sub => x.overflowing_sub(y),
                        Overflow::Mul => x.overflowing_mul(y),
                    };
                    vals[res[0].index()] = r;
                    vals[res[1].index()] = i64::from(o);
                    None
                }
                Op::Bitcast => Some(arg(0)),
                Op::Fop(op) => {
                    let (x, y) = (fl(arg(0)), fl(arg(1)));
                    Some(
                        match op {
                            Fop::Add => x + y,
                            Fop::Sub => x - y,
                            Fop::Mul => x * y,
                            Fop::Div => x / y,
                        }
                        .to_bits() as i64,
                    )
                }
                Op::Fneg => Some((-fl(arg(0))).to_bits() as i64),
                Op::Fabs => Some(fl(arg(0)).abs().to_bits() as i64),
                Op::Sqrt => Some(fl(arg(0)).sqrt().to_bits() as i64),
                Op::FcvtFromSint => Some((norm(ty(a[0]), arg(0)) as f64).to_bits() as i64),
                Op::CallIndirect(sig) => {
                    let n = dfg.sigs[sig.index()].params.len();
                    let xs: Vec<i64> = (1..=n).map(arg).collect();
                    Some(call_mix(n, &xs))
                }
                Op::Jump(e) => {
                    incoming = dfg.slice(e.args).iter().map(|&v| get(&vals, v)).collect();
                    block = e.block;
                    break;
                }
                Op::Brif(t, e) => {
                    let e = if zext(ty(a[0]), arg(0)) != 0 { t } else { e };
                    incoming = dfg.slice(e.args).iter().map(|&v| get(&vals, v)).collect();
                    block = e.block;
                    break;
                }
                Op::BrTable(jt) => {
                    let t = &dfg.tables[jt.index()];
                    let i = zext(ty(a[0]), arg(0)) as usize;
                    let c = t.table.get(i).unwrap_or(&t.default);
                    incoming = c.args.iter().map(|&v| get(&vals, v)).collect();
                    block = c.block;
                    break;
                }
                Op::Return => return arg(0),
                Op::Call(_) | Op::StackAddr(..) => unreachable!("not generated"),
            };
            if let Some(x) = out {
                vals[res[0].index()] = x;
            }
        }
    }
}

#[test]
fn random_functions() {
    let n: u64 = std::env::var("WPCG_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    for seed in 0..n {
        let f = generate(seed);
        let p = match compile_and_install(&mut f.clone()) {
            Ok(p) => p,
            Err(e) => panic!("seed {seed}: {e}\n{}", f.display()),
        };
        // SAFETY: compiled for exactly this signature.
        let jit: extern "C" fn(i64, i64, *mut u8) -> i64 = unsafe { std::mem::transmute(p) };
        for (k, &(a, b)) in [(3i64, -7i64), (1 << 40, 12345), (-1, i64::MIN), (0, 0)].iter().enumerate() {
            let mut m1 = [0u8; MEM];
            for (i, x) in m1.iter_mut().enumerate() {
                *x = (i as u8).wrapping_mul(37).wrapping_add(k as u8);
            }
            let mut m2 = m1;
            let want = interpret(&f, &[a, b, m1.as_mut_ptr() as i64], &mut m1);
            let got = jit(a, b, m2.as_mut_ptr());
            assert!(
                got == want && m1 == m2,
                "seed {seed} args ({a}, {b}): got {got}, want {want}, memory {}\n{}",
                if m1 == m2 { "same" } else { "differs" },
                f.display()
            );
        }
    }
}

/// Compile speed over random functions: `WPCG_BENCH=1 cargo test --release
/// compile_speed -- --nocapture`.
#[test]
fn compile_speed() {
    if std::env::var_os("WPCG_BENCH").is_none() {
        return;
    }
    let funcs: Vec<Function> = (0..2000).map(generate).collect();
    let insts: usize = funcs.iter().map(|f| f.dfg.num_insts()).sum();
    let rounds: usize = std::env::var("WPCG_BENCH").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    for round in 0..rounds {
        let t = std::time::Instant::now();
        let mut bytes = 0;
        for f in &funcs {
            bytes += crate::compile(&mut f.clone()).unwrap().len();
        }
        let e = t.elapsed();
        eprintln!(
            "round {round}: {insts} insts in {e:?}: {:.0} ns/inst, {bytes} bytes",
            e.as_nanos() as f64 / insts as f64
        );
    }
}
