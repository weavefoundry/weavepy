//! Compile small functions and run them.

use crate::ir::condcodes::{FloatCC, IntCC};
use crate::ir::{types, AbiParam, CallConv, Function, MemFlags, Signature, Type};
use crate::{compile_and_install, FunctionBuilder};

const F: MemFlags = MemFlags::trusted();

fn func(params: &[Type], rets: &[Type]) -> Function {
    let mut f = Function::new();
    for &p in params {
        f.signature.params.push(AbiParam::new(p));
    }
    for &r in rets {
        f.signature.returns.push(AbiParam::new(r));
    }
    f
}

fn build(params: &[Type], rets: &[Type], body: impl FnOnce(&mut FunctionBuilder<'_>)) -> *const u8 {
    let mut f = func(params, rets);
    {
        let mut b = FunctionBuilder::new(&mut f);
        body(&mut b);
    }
    if let Ok(path) = std::env::var("WPCG_DUMP") {
        std::fs::write(path, crate::compile(&mut f.clone()).unwrap()).unwrap();
        eprintln!("{}", f.display());
    }
    match compile_and_install(&mut f.clone()) {
        Ok(p) => p,
        Err(e) => panic!("{e}\n{}", f.display()),
    }
}

fn entry(b: &mut FunctionBuilder<'_>) -> Vec<crate::Value> {
    let e = b.create_block();
    b.append_block_params_for_function_params(e);
    b.switch_to_block(e);
    b.block_params(e).to_vec()
}

type F2 = extern "C" fn(i64, i64) -> i64;
type F1 = extern "C" fn(i64) -> i64;

#[test]
fn add() {
    let p = build(&[types::I64, types::I64], &[types::I64], |b| {
        let a = entry(b);
        let s = b.ins().iadd(a[0], a[1]);
        b.ins().return_(&[s]);
    });
    let f: F2 = unsafe { std::mem::transmute(p) };
    assert_eq!(f(2, 40), 42);
    assert_eq!(f(-5, 3), -2);
}

#[test]
fn alu_mix() {
    let p = build(&[types::I64, types::I64], &[types::I64], |b| {
        let a = entry(b);
        let x = b.ins().isub(a[0], a[1]);
        let y = b.ins().imul(x, a[0]);
        let z = b.ins().bxor(y, a[1]);
        let w = b.ins().band_imm(z, 0xff);
        let s = b.ins().ishl_imm(w, 3);
        let t = b.ins().ushr_imm(s, 1);
        let u = b.ins().bor(t, a[1]);
        let v = b.ins().iadd_imm(u, -7);
        let q = b.ins().isub(a[1], v);
        b.ins().return_(&[q]);
    });
    let f: F2 = unsafe { std::mem::transmute(p) };
    let r = |a: i64, c: i64| {
        let x = a - c;
        let y = x.wrapping_mul(a);
        let z = y ^ c;
        let w = z & 0xff;
        let s = w << 3;
        let t = ((s as u64) >> 1) as i64;
        let u = t | c;
        let v = u - 7;
        c - v
    };
    for (a, c) in [(5, 3), (-100, 77), (123_456_789, -42), (0, 0)] {
        assert_eq!(f(a, c), r(a, c), "{a} {c}");
    }
}

#[test]
fn loop_sum() {
    // sum(0..n)
    let p = build(&[types::I64], &[types::I64], |b| {
        let a = entry(b);
        let head = b.create_block();
        let i = b.append_block_param(head, types::I64);
        let acc = b.append_block_param(head, types::I64);
        let body = b.create_block();
        let exit = b.create_block();
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().jump(head, &[zero.into(), zero.into()]);
        b.switch_to_block(head);
        let c = b.ins().icmp(IntCC::SignedLessThan, i, a[0]);
        b.ins().brif(c, body, &[], exit, &[]);
        b.switch_to_block(body);
        let acc2 = b.ins().iadd(acc, i);
        let i2 = b.ins().iadd_imm(i, 1);
        b.ins().jump(head, &[i2.into(), acc2.into()]);
        b.switch_to_block(exit);
        b.ins().return_(&[acc]);
    });
    let f: F1 = unsafe { std::mem::transmute(p) };
    assert_eq!(f(10), 45);
    assert_eq!(f(0), 0);
    assert_eq!(f(100_000), 4_999_950_000);
}

extern "C" fn helper3(a: i64, b: i64, c: i64) -> i64 {
    a * 100 + b * 10 + c
}

extern "C" fn helper8(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64) -> i64 {
    a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g + 8 * h
}

#[cfg(not(windows))]
extern "C" fn pair(a: i64) -> Pair {
    Pair { a: a + 1, b: a * 2 }
}

#[repr(C)]
struct Pair {
    a: i64,
    b: i64,
}

fn sig(b: &mut FunctionBuilder<'_>, n: usize, rets: usize, conv: CallConv) -> crate::SigRef {
    let mut s = Signature::new(conv);
    for _ in 0..n {
        s.params.push(AbiParam::new(types::I64));
    }
    for _ in 0..rets {
        s.returns.push(AbiParam::new(types::I64));
    }
    b.import_signature(s)
}

#[test]
fn calls_keep_values() {
    // Values live across calls: x, y, z each used after two calls.
    let p = build(&[types::I64, types::I64], &[types::I64], |b| {
        let a = entry(b);
        let s3 = sig(b, 3, 1, CallConv::host());
        let s8 = sig(b, 8, 1, CallConv::host());
        let h3 = b
            .ins()
            .iconst(types::I64, helper3 as *const () as usize as i64);
        let h8 = b
            .ins()
            .iconst(types::I64, helper8 as *const () as usize as i64);
        let x = b.ins().iadd_imm(a[0], 1);
        let y = b.ins().iadd_imm(a[1], 2);
        let z = b.ins().imul(x, y);
        let c1 = b.ins().call_indirect(s3, h3, &[x, y, z]);
        let r1 = b.inst_results(c1)[0];
        let one = b.ins().iconst(types::I64, 1);
        let c2 = b
            .ins()
            .call_indirect(s8, h8, &[x, y, z, r1, one, a[0], a[1], x]);
        let r2 = b.inst_results(c2)[0];
        let t = b.ins().iadd(r2, z);
        let u = b.ins().iadd(t, x);
        let v = b.ins().iadd(u, y);
        let w = b.ins().iadd(v, r1);
        b.ins().return_(&[w]);
    });
    let f: F2 = unsafe { std::mem::transmute(p) };
    let r = |a0: i64, a1: i64| {
        let x = a0 + 1;
        let y = a1 + 2;
        let z = x * y;
        let r1 = helper3(x, y, z);
        let r2 = helper8(x, y, z, r1, 1, a0, a1, x);
        r2 + z + x + y + r1
    };
    assert_eq!(f(3, 4), r(3, 4));
    assert_eq!(f(-9, 17), r(-9, 17));
}

#[test]
fn two_returns() {
    let p = build(&[types::I64], &[types::I64], |b| {
        let a = entry(b);
        let conv = if cfg!(all(windows, target_arch = "x86_64")) {
            CallConv::SystemV
        } else {
            CallConv::host()
        };
        let s = sig(b, 1, 2, conv);
        let h = b
            .ins()
            .iconst(types::I64, pair_sysv as *const () as usize as i64);
        let c = b.ins().call_indirect(s, h, &[a[0]]);
        let (x, y) = (b.inst_results(c)[0], b.inst_results(c)[1]);
        let r = b.ins().isub(y, x);
        b.ins().return_(&[r]);
    });
    let f: F1 = unsafe { std::mem::transmute(p) };
    assert_eq!(f(10), 20 - 11);
}

#[cfg(not(windows))]
use pair as pair_sysv;
#[cfg(windows)]
extern "sysv64" fn pair_sysv(a: i64) -> Pair {
    Pair { a: a + 1, b: a * 2 }
}

#[test]
fn pressure() {
    // 30 values live at once, across a call.
    let p = build(&[types::I64], &[types::I64], |b| {
        let a = entry(b);
        let s1 = sig(b, 1, 1, CallConv::host());
        let h = b.ins().iconst(types::I64, id as *const () as usize as i64);
        let vals: Vec<_> = (0..30).map(|i| b.ins().iadd_imm(a[0], i * 3 + 1)).collect();
        let c = b.ins().call_indirect(s1, h, &[vals[7]]);
        let mut acc = b.inst_results(c)[0];
        for (i, &v) in vals.iter().enumerate() {
            let m = b.ins().imul_imm(v, i as i64 + 1);
            acc = b.ins().iadd(acc, m);
        }
        b.ins().return_(&[acc]);
    });
    let f: F1 = unsafe { std::mem::transmute(p) };
    let r = |a: i64| {
        let vals: Vec<i64> = (0..30).map(|i| a + i * 3 + 1).collect();
        let mut acc = vals[7];
        for (i, v) in vals.iter().enumerate() {
            acc += v * (i as i64 + 1);
        }
        acc
    };
    assert_eq!(f(5), r(5));
}

extern "C" fn id(a: i64) -> i64 {
    a
}

#[test]
fn memory_and_narrow() {
    #[repr(C)]
    struct S {
        tag: u8,
        pad: [u8; 7],
        word: i64,
        small: u32,
    }
    let p = build(&[types::I64], &[types::I64], |b| {
        let a = entry(b);
        let tag = b.ins().uload8(types::I32, F, a[0], 0);
        let word = b.ins().load(types::I64, F, a[0], 8);
        let small = b.ins().uload32(F, a[0], 16);
        let is3 = b.ins().icmp_imm(IntCC::Equal, tag, 3);
        let yes = b.create_block();
        let no = b.create_block();
        let done = b.create_block();
        let r = b.append_block_param(done, types::I64);
        b.ins().brif(is3, yes, &[], no, &[]);
        b.switch_to_block(yes);
        let s = b.ins().iadd(word, small);
        let four = b.ins().iconst(types::I8, 4);
        b.ins().store(F, four, a[0], 0);
        b.ins().store(F, s, a[0], 8);
        b.ins().jump(done, &[s.into()]);
        b.switch_to_block(no);
        let m = b.ins().iconst(types::I64, -1);
        b.ins().jump(done, &[m.into()]);
        b.switch_to_block(done);
        b.ins().return_(&[r]);
    });
    let f: F1 = unsafe { std::mem::transmute(p) };
    let mut s = S {
        tag: 3,
        pad: [0; 7],
        word: 100,
        small: 0xffff_ffff,
    };
    assert_eq!(f((&raw mut s) as i64), 100 + 0xffff_ffff);
    assert_eq!(s.tag, 4);
    assert_eq!(s.word, 100 + 0xffff_ffff);
    assert_eq!(f((&raw mut s) as i64), -1);
    let _ = s.pad;
}

#[test]
fn overflow_branch() {
    // a + b, or -1 on overflow.
    let p = build(&[types::I64, types::I64], &[types::I64], |b| {
        let a = entry(b);
        let (s, o) = b.ins().sadd_overflow(a[0], a[1]);
        let ok = b.create_block();
        let bad = b.create_block();
        b.ins().brif(o, bad, &[], ok, &[]);
        b.switch_to_block(ok);
        b.ins().return_(&[s]);
        b.switch_to_block(bad);
        let m = b.ins().iconst(types::I64, -1);
        b.ins().return_(&[m]);
    });
    let f: F2 = unsafe { std::mem::transmute(p) };
    assert_eq!(f(1, 2), 3);
    assert_eq!(f(i64::MAX, 1), -1);
    assert_eq!(f(i64::MIN, -1), -1);
}

#[test]
fn floats() {
    type FF = extern "C" fn(i64, i64) -> i64;
    // bits(f(a) op ...) where a, b are f64 bits
    let p = build(&[types::I64, types::I64], &[types::I64], |b| {
        let a = entry(b);
        let x = b.ins().bitcast(types::F64, F, a[0]);
        let y = b.ins().bitcast(types::F64, F, a[1]);
        let s = b.ins().fadd(x, y);
        let m = b.ins().fmul(s, x);
        let d = b.ins().fdiv(m, y);
        let n = b.ins().fneg(d);
        let k = b.ins().f64const(0.5);
        let t = b.ins().fsub(n, k);
        let lt = b.ins().fcmp(FloatCC::LessThan, t, x);
        let i = b.ins().iconst(types::I64, 3);
        let fi = b.ins().fcvt_from_sint(types::F64, i);
        let u = b.ins().select(lt, t, fi);
        let r = b.ins().bitcast(types::I64, F, u);
        b.ins().return_(&[r]);
    });
    let f: FF = unsafe { std::mem::transmute(p) };
    let r = |x: f64, y: f64| {
        let t = -((x + y) * x / y) - 0.5;
        if t < x {
            t
        } else {
            3.0
        }
    };
    for (x, y) in [(1.5f64, 2.0f64), (-3.0, 0.25), (10.0, -4.0)] {
        let got = f64::from_bits(f(x.to_bits() as i64, y.to_bits() as i64) as u64);
        assert_eq!(got, r(x, y), "{x} {y}");
    }
    // NaN compares.
    let p = build(&[types::I64, types::I64], &[types::I64], |b| {
        let a = entry(b);
        let x = b.ins().bitcast(types::F64, F, a[0]);
        let y = b.ins().bitcast(types::F64, F, a[1]);
        let mut acc = b.ins().iconst(types::I64, 0);
        for (i, cc) in [
            FloatCC::Equal,
            FloatCC::NotEqual,
            FloatCC::LessThan,
            FloatCC::LessThanOrEqual,
            FloatCC::GreaterThan,
            FloatCC::GreaterThanOrEqual,
            FloatCC::Unordered,
            FloatCC::Ordered,
            FloatCC::OrderedNotEqual,
            FloatCC::UnorderedOrEqual,
            FloatCC::UnorderedOrLessThan,
            FloatCC::UnorderedOrLessThanOrEqual,
            FloatCC::UnorderedOrGreaterThan,
            FloatCC::UnorderedOrGreaterThanOrEqual,
        ]
        .into_iter()
        .enumerate()
        {
            let c = b.ins().fcmp(cc, x, y);
            let w = b.ins().uextend(types::I64, c);
            let s = b.ins().ishl_imm(w, i as i64);
            acc = b.ins().bor(acc, s);
        }
        b.ins().return_(&[acc]);
    });
    let f: FF = unsafe { std::mem::transmute(p) };
    let r = |x: f64, y: f64| {
        let bits = [
            x == y,
            x != y,
            x < y,
            x <= y,
            x > y,
            x >= y,
            x.is_nan() || y.is_nan(),
            !(x.is_nan() || y.is_nan()),
            !(x.is_nan() || y.is_nan()) && x != y,
            x.is_nan() || y.is_nan() || x == y,
            x.is_nan() || y.is_nan() || x < y,
            x.is_nan() || y.is_nan() || x <= y,
            x.is_nan() || y.is_nan() || x > y,
            x.is_nan() || y.is_nan() || x >= y,
        ];
        bits.iter()
            .enumerate()
            .fold(0i64, |acc, (i, &b)| acc | (i64::from(b) << i))
    };
    for (x, y) in [
        (1.0, 2.0),
        (2.0, 1.0),
        (1.0, 1.0),
        (f64::NAN, 1.0),
        (1.0, f64::NAN),
    ] {
        assert_eq!(
            f(x.to_bits() as i64, y.to_bits() as i64),
            r(x, y),
            "{x} {y}"
        );
    }
}

#[test]
fn br_table() {
    let p = build(&[types::I64], &[types::I64], |b| {
        let a = entry(b);
        let blocks: Vec<_> = (0..5).map(|_| b.create_block()).collect();
        let dflt = b.create_block();
        let idx = b.ins().ireduce(types::I32, a[0]);
        let calls: Vec<_> = blocks
            .iter()
            .map(|&bl| b.func.dfg.block_call(bl, &[]))
            .collect();
        let def = b.func.dfg.block_call(dflt, &[]);
        let jt = b.create_jump_table(crate::JumpTableData::new(def, &calls));
        b.ins().br_table(idx, jt);
        for (i, &bl) in blocks.iter().enumerate() {
            b.switch_to_block(bl);
            let v = b.ins().imul_imm(a[0], 10 + i as i64);
            b.ins().return_(&[v]);
        }
        b.switch_to_block(dflt);
        let m = b.ins().iconst(types::I64, -1);
        b.ins().return_(&[m]);
    });
    let f: F1 = unsafe { std::mem::transmute(p) };
    for i in 0..5 {
        assert_eq!(f(i), i * (10 + i));
    }
    assert_eq!(f(5), -1);
    assert_eq!(f(100), -1);
}

#[test]
fn divide_and_shift() {
    let p = build(&[types::I64, types::I64], &[types::I64], |b| {
        let a = entry(b);
        let q = b.ins().udiv(a[0], a[1]);
        let r = b.ins().srem(a[0], a[1]);
        let sh = b.ins().band_imm(a[1], 7);
        let s = b.ins().ishl(q, sh);
        let t = b.ins().sshr(r, sh);
        let u = b.ins().iadd(s, t);
        let w = b.ins().iadd(u, a[0]);
        b.ins().return_(&[w]);
    });
    let f: F2 = unsafe { std::mem::transmute(p) };
    let r = |a: i64, c: i64| {
        let q = ((a as u64) / (c as u64)) as i64;
        let rr = a % c;
        let sh = c & 7;
        (q << sh) + (rr >> sh) + a
    };
    for (a, c) in [(100, 7), (12345, 3), (99, 13), (-50, 6)] {
        assert_eq!(f(a, c), r(a, c), "{a} {c}");
    }
}

/// A frame-JIT-like shape: a state pointer, helpers called with it, and
/// status codes leaving through a shared tail with a parameter.
#[test]
fn state_machine() {
    #[repr(C)]
    struct St {
        locals: *mut i64,
        stack: *mut i64,
        n: i64,
    }
    extern "C" fn h_step(st: *mut St, i: i64) -> i32 {
        let st = unsafe { &mut *st };
        unsafe { *st.stack.add(i as usize % 4) += i };
        st.n += 1;
        i32::from(i == 1000)
    }
    let p = build(&[types::I64], &[types::I32], |b| {
        let a = entry(b);
        let st = a[0];
        let locals = b.ins().load(types::I64, F, st, 0);
        let stack = b.ins().load(types::I64, F, st, 8);
        let tail = b.create_block();
        let status = b.append_block_param(tail, types::I32);
        let head = b.create_block();
        let i = b.append_block_param(head, types::I64);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().jump(head, &[zero.into()]);
        b.switch_to_block(head);
        let sig = {
            let mut s = Signature::new(CallConv::host());
            s.params.push(AbiParam::new(types::I64));
            s.params.push(AbiParam::new(types::I64));
            s.returns.push(AbiParam::new(types::I32));
            b.import_signature(s)
        };
        let h = b
            .ins()
            .iconst(types::I64, h_step as *const () as usize as i64);
        let c = b.ins().call_indirect(sig, h, &[st, i]);
        let r = b.inst_results(c)[0];
        let next = b.create_block();
        b.ins().brif(r, tail, &[r.into()], next, &[]);
        b.switch_to_block(next);
        let l0 = b.ins().load(types::I64, F, locals, 0);
        let l1 = b.ins().iadd(l0, i);
        b.ins().store(F, l1, locals, 0);
        let s0 = b.ins().load(types::I64, F, stack, 0);
        let s1 = b.ins().iadd_imm(s0, 1);
        b.ins().store(F, s1, stack, 0);
        let i2 = b.ins().iadd_imm(i, 1);
        b.ins().jump(head, &[i2.into()]);
        b.set_cold_block(tail);
        b.switch_to_block(tail);
        let two = b.ins().iconst(types::I32, 2);
        let out = b.ins().iadd(status, two);
        b.ins().return_(&[out]);
    });
    let f: extern "C" fn(*mut St) -> i32 = unsafe { std::mem::transmute(p) };
    let mut locals = [0i64; 2];
    let mut stack = [0i64; 4];
    let mut st = St {
        locals: locals.as_mut_ptr(),
        stack: stack.as_mut_ptr(),
        n: 0,
    };
    assert_eq!(f(&raw mut st), 3);
    assert_eq!(st.n, 1001);
    assert_eq!(locals[0], (0..1000).sum::<i64>());
    let mut want = [0i64; 4];
    for i in 0..=1000i64 {
        want[i as usize % 4] += i;
    }
    want[0] += 1000;
    assert_eq!(stack, want);
}

#[test]
fn variables_loop() {
    // sum of i * i over 0..n, with variables for i and the sum.
    let p = build(&[types::I64], &[types::I64], |b| {
        let a = entry(b);
        let i = b.declare_var(types::I64);
        let s = b.declare_var(types::I64);
        let zero = b.ins().iconst(types::I64, 0);
        b.def_var(i, zero);
        b.def_var(s, zero);
        let head = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.ins().jump(head, &[]);
        b.switch_to_block(head);
        let iv = b.use_var(i);
        let c = b.ins().icmp(IntCC::SignedLessThan, iv, a[0]);
        b.ins().brif(c, body, &[], exit, &[]);
        b.switch_to_block(body);
        let iv = b.use_var(i);
        let sv = b.use_var(s);
        let sq = b.ins().imul(iv, iv);
        let s2 = b.ins().iadd(sv, sq);
        b.def_var(s, s2);
        let i2 = b.ins().iadd_imm(iv, 1);
        b.def_var(i, i2);
        b.ins().jump(head, &[]);
        b.switch_to_block(exit);
        let sv = b.use_var(s);
        b.ins().return_(&[sv]);
        b.seal_all_blocks();
    });
    let f: F1 = unsafe { std::mem::transmute(p) };
    assert_eq!(f(10), (0..10).map(|i| i * i).sum::<i64>());
    assert_eq!(f(0), 0);
}

#[test]
fn self_call_fib() {
    let p = build(&[types::I64], &[types::I64], |b| {
        let a = entry(b);
        let mut s = Signature::new(CallConv::host());
        s.params.push(AbiParam::new(types::I64));
        s.returns.push(AbiParam::new(types::I64));
        let sig = b.import_signature(s);
        let me = b.import_function(crate::ExtFuncData {
            target: crate::FuncTarget::SelfFn,
            sig,
        });
        let small = b.create_block();
        let big = b.create_block();
        let c = b.ins().icmp_imm(IntCC::SignedLessThan, a[0], 2);
        b.ins().brif(c, small, &[], big, &[]);
        b.switch_to_block(small);
        b.ins().return_(&[a[0]]);
        b.switch_to_block(big);
        let n1 = b.ins().iadd_imm(a[0], -1);
        let c1 = b.ins().call(me, &[n1]);
        let r1 = b.inst_results(c1)[0];
        let n2 = b.ins().iadd_imm(a[0], -2);
        let c2 = b.ins().call(me, &[n2]);
        let r2 = b.inst_results(c2)[0];
        let r = b.ins().iadd(r1, r2);
        b.ins().return_(&[r]);
    });
    let f: F1 = unsafe { std::mem::transmute(p) };
    assert_eq!(f(20), 6765);
}

extern "C" fn sum_buf(p: *const i64, n: i64) -> i64 {
    (0..n as usize).map(|i| unsafe { *p.add(i) }).sum()
}

#[test]
fn stack_slots_and_direct_calls() {
    let p = build(&[types::I64], &[types::I64], |b| {
        let a = entry(b);
        let slot = b.create_sized_stack_slot(crate::StackSlotData::new(
            crate::StackSlotKind::ExplicitSlot,
            64,
            3,
        ));
        for k in 0..8 {
            let v = b.ins().imul_imm(a[0], k + 1);
            let at = b.ins().stack_addr(types::I64, slot, 8 * k as i32);
            b.ins().store(F, v, at, 0);
        }
        let mut s = Signature::new(CallConv::host());
        s.params.push(AbiParam::new(types::I64));
        s.params.push(AbiParam::new(types::I64));
        s.returns.push(AbiParam::new(types::I64));
        let sig = b.import_signature(s);
        let callee = b.import_function(crate::ExtFuncData {
            target: crate::FuncTarget::Addr(sum_buf as *const () as usize),
            sig,
        });
        let base = b.ins().stack_addr(types::I64, slot, 0);
        let n = b.ins().iconst(types::I64, 8);
        let c = b.ins().call(callee, &[base, n]);
        let r = b.inst_results(c)[0];
        let back = b.ins().load(types::I64, F, base, 56);
        let out = b.ins().iadd(r, back);
        b.ins().return_(&[out]);
    });
    let f: F1 = unsafe { std::mem::transmute(p) };
    assert_eq!(f(3), 3 * 36 + 24);
}

#[test]
fn many_rotating_variables() {
    // 16 variables rotated each iteration: the loop header's parameters
    // outnumber the registers and swap through their slots.
    const N: usize = 16;
    let p = build(&[types::I64], &[types::I64], |b| {
        let a = entry(b);
        let vars: Vec<_> = (0..N).map(|_| b.declare_var(types::I64)).collect();
        for (k, &v) in vars.iter().enumerate() {
            let c = b.ins().iconst(types::I64, k as i64 + 1);
            b.def_var(v, c);
        }
        let i = b.declare_var(types::I64);
        let zero = b.ins().iconst(types::I64, 0);
        b.def_var(i, zero);
        let head = b.create_block();
        let body = b.create_block();
        let exit = b.create_block();
        b.ins().jump(head, &[]);
        b.switch_to_block(head);
        let iv = b.use_var(i);
        let c = b.ins().icmp(IntCC::SignedLessThan, iv, a[0]);
        b.ins().brif(c, body, &[], exit, &[]);
        b.switch_to_block(body);
        let cur: Vec<_> = vars.iter().map(|&v| b.use_var(v)).collect();
        for k in 0..N {
            b.def_var(vars[k], cur[(k + 1) % N]);
        }
        let iv = b.use_var(i);
        let i2 = b.ins().iadd_imm(iv, 1);
        b.def_var(i, i2);
        b.ins().jump(head, &[]);
        b.switch_to_block(exit);
        let mut acc = b.ins().iconst(types::I64, 0);
        for (k, &v) in vars.iter().enumerate() {
            let x = b.use_var(v);
            let m = b.ins().imul_imm(x, k as i64 + 1);
            acc = b.ins().iadd(acc, m);
        }
        b.ins().return_(&[acc]);
        b.seal_all_blocks();
    });
    let f: F1 = unsafe { std::mem::transmute(p) };
    for n in [0i64, 1, 5, 16, 37] {
        let want: i64 = (0..N)
            .map(|k| ((k + n as usize) % N + 1) as i64 * (k as i64 + 1))
            .sum();
        assert_eq!(f(n), want, "n = {n}");
    }
}
