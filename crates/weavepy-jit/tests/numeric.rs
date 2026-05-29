//! End-to-end codegen tests over hand-built IR: they compile a
//! [`TFunc`] and actually *run* the native code, checking results, the
//! overflow/zero deopt protocol, and Python's floor-division semantics —
//! all without needing the parser or VM.

use weavepy_jit::{
    ArithKind, CmpKind, JitEngine, JitFrame, JitStatus, JitType, SlotTag, TBlock, TFunc, TOp,
    TStmt, TTerm,
};

/// Allocate buffers, enter the compiled frame with the given locals, and
/// return `(status, ret_bits, ret_tag, spilled_stack, deopt_pc)`.
fn run(tfunc: &TFunc, locals_in: &[u64]) -> (JitStatus, u64, u32, Vec<(u64, u32)>, u32) {
    let mut engine = JitEngine::new().expect("host ISA");
    let cf = engine.compile_tfunc(tfunc).expect("compile");

    let mut locals = vec![0u64; cf.n_locals as usize];
    for (i, v) in locals_in.iter().enumerate() {
        locals[i] = *v;
    }
    let cap = cf.max_stack as usize + 1;
    let mut spill = vec![0u64; cap];
    let mut tags = vec![0u32; cap];

    let mut frame = JitFrame {
        locals: locals.as_mut_ptr(),
        n_locals: cf.n_locals,
        entry_pc: 0,
        ret_bits: 0,
        ret_tag: 0,
        deopt_pc: 0,
        stack_spill: spill.as_mut_ptr(),
        stack_tags: tags.as_mut_ptr(),
        stack_len: 0,
        stack_cap: cap as u32,
    };
    // SAFETY: buffers are sized to n_locals / max_stack; `engine` (and so
    // the backing module) outlives this call.
    let status = unsafe { cf.enter(&raw mut frame) };

    let mut spilled = Vec::new();
    for i in 0..frame.stack_len as usize {
        spilled.push((spill[i], tags[i]));
    }
    (
        status,
        frame.ret_bits,
        frame.ret_tag,
        spilled,
        frame.deopt_pc,
    )
}

fn st(pc: u32, op: TOp) -> TStmt {
    TStmt { pc, op }
}

#[test]
fn add_two_ints() {
    // def f(a, b): return a + b
    let tfunc = TFunc {
        n_locals: 2,
        local_types: vec![Some(JitType::Int), Some(JitType::Int)],
        livein_locals: vec![0, 1],
        max_stack: 2,
        entry_block: 0,
        blocks: vec![TBlock {
            entry_stack: vec![],
            stmts: vec![
                st(0, TOp::LoadLocal(0)),
                st(1, TOp::LoadLocal(1)),
                st(2, TOp::IntArith(ArithKind::Add)),
            ],
            term: TTerm::Return,
        }],
    };
    let (status, bits, tag, _, _) = run(&tfunc, &[(40i64) as u64, (2i64) as u64]);
    assert_eq!(status, JitStatus::Returned);
    assert_eq!(tag, SlotTag::Int as u32);
    assert_eq!(bits as i64, 42);
}

#[test]
fn add_overflow_deopts_with_operands_spilled() {
    // a + b where a = i64::MAX, b = 1 must deopt at the BINARY_OP pc with
    // both operands on the spilled stack.
    let tfunc = TFunc {
        n_locals: 2,
        local_types: vec![Some(JitType::Int), Some(JitType::Int)],
        livein_locals: vec![0, 1],
        max_stack: 2,
        entry_block: 0,
        blocks: vec![TBlock {
            entry_stack: vec![],
            stmts: vec![
                st(10, TOp::LoadLocal(0)),
                st(11, TOp::LoadLocal(1)),
                st(12, TOp::IntArith(ArithKind::Add)),
            ],
            term: TTerm::Return,
        }],
    };
    let (status, _, _, spilled, pc) = run(&tfunc, &[i64::MAX as u64, 1u64]);
    assert_eq!(status, JitStatus::Deopt);
    assert_eq!(pc, 12);
    assert_eq!(spilled.len(), 2);
    assert_eq!(spilled[0].0 as i64, i64::MAX);
    assert_eq!(spilled[1].0 as i64, 1);
    assert_eq!(spilled[0].1, SlotTag::Int as u32);
}

/// Build `def f(n): s=0; i=0; while i<n: s=s+i; i=i+1; return s`.
fn sum_loop() -> TFunc {
    TFunc {
        n_locals: 3, // 0=n, 1=s, 2=i
        local_types: vec![Some(JitType::Int), Some(JitType::Int), Some(JitType::Int)],
        livein_locals: vec![0],
        max_stack: 2,
        entry_block: 0,
        blocks: vec![
            // B0: s=0; i=0; -> B1
            TBlock {
                entry_stack: vec![],
                stmts: vec![
                    st(0, TOp::PushConstInt(0)),
                    st(1, TOp::StoreLocal(1)),
                    st(2, TOp::PushConstInt(0)),
                    st(3, TOp::StoreLocal(2)),
                ],
                term: TTerm::Jump(1),
            },
            // B1 header: if i < n -> B2 else B3
            TBlock {
                entry_stack: vec![],
                stmts: vec![
                    st(4, TOp::LoadLocal(2)),
                    st(5, TOp::LoadLocal(0)),
                    st(6, TOp::IntCmp(CmpKind::Lt)),
                ],
                term: TTerm::BranchFalse {
                    target: 3,
                    fallthrough: 2,
                },
            },
            // B2 body: s=s+i; i=i+1; -> B1
            TBlock {
                entry_stack: vec![],
                stmts: vec![
                    st(7, TOp::LoadLocal(1)),
                    st(8, TOp::LoadLocal(2)),
                    st(9, TOp::IntArith(ArithKind::Add)),
                    st(10, TOp::StoreLocal(1)),
                    st(11, TOp::LoadLocal(2)),
                    st(12, TOp::PushConstInt(1)),
                    st(13, TOp::IntArith(ArithKind::Add)),
                    st(14, TOp::StoreLocal(2)),
                ],
                term: TTerm::Jump(1),
            },
            // B3 exit: return s
            TBlock {
                entry_stack: vec![],
                stmts: vec![st(15, TOp::LoadLocal(1))],
                term: TTerm::Return,
            },
        ],
    }
}

#[test]
fn while_loop_sums() {
    let tfunc = sum_loop();
    let (status, bits, tag, _, _) = run(&tfunc, &[10u64]);
    assert_eq!(status, JitStatus::Returned);
    assert_eq!(tag, SlotTag::Int as u32);
    assert_eq!(bits as i64, 45); // 0+1+..+9
}

#[test]
fn while_loop_zero_iterations() {
    let tfunc = sum_loop();
    let (status, bits, _, _, _) = run(&tfunc, &[0u64]);
    assert_eq!(status, JitStatus::Returned);
    assert_eq!(bits as i64, 0);
}

/// `def f(a, b): return a // b` and `... a % b`, for the floor/modulo
/// semantics that differ from Rust's truncating division on negatives.
fn binop_fn(op: ArithKind) -> TFunc {
    TFunc {
        n_locals: 2,
        local_types: vec![Some(JitType::Int), Some(JitType::Int)],
        livein_locals: vec![0, 1],
        max_stack: 2,
        entry_block: 0,
        blocks: vec![TBlock {
            entry_stack: vec![],
            stmts: vec![
                st(0, TOp::LoadLocal(0)),
                st(1, TOp::LoadLocal(1)),
                st(2, TOp::IntArith(op)),
            ],
            term: TTerm::Return,
        }],
    }
}

#[test]
fn python_floordiv_semantics() {
    let f = binop_fn(ArithKind::FloorDiv);
    let cases = [
        (7i64, 2i64, 3i64),
        (-7, 2, -4),
        (7, -2, -4),
        (-7, -2, 3),
        (6, 3, 2),
        (-6, 3, -2),
    ];
    for (a, b, want) in cases {
        let (status, bits, _, _, _) = run(&f, &[a as u64, b as u64]);
        assert_eq!(status, JitStatus::Returned, "{a} // {b}");
        assert_eq!(bits as i64, want, "{a} // {b}");
    }
}

#[test]
fn python_mod_semantics() {
    let f = binop_fn(ArithKind::Mod);
    let cases = [(7i64, 3i64, 1i64), (-7, 3, 2), (7, -3, -2), (-7, -3, -1)];
    for (a, b, want) in cases {
        let (status, bits, _, _, _) = run(&f, &[a as u64, b as u64]);
        assert_eq!(status, JitStatus::Returned, "{a} % {b}");
        assert_eq!(bits as i64, want, "{a} % {b}");
    }
}

#[test]
fn floordiv_by_zero_deopts() {
    let f = binop_fn(ArithKind::FloorDiv);
    let (status, _, _, spilled, pc) = run(&f, &[5u64, 0u64]);
    assert_eq!(status, JitStatus::Deopt);
    assert_eq!(pc, 2);
    assert_eq!(spilled.len(), 2);
}

#[test]
fn int_truediv_returns_float() {
    // def f(a, b): return a / b  ->  float
    let tfunc = TFunc {
        n_locals: 2,
        local_types: vec![Some(JitType::Int), Some(JitType::Int)],
        livein_locals: vec![0, 1],
        max_stack: 2,
        entry_block: 0,
        blocks: vec![TBlock {
            entry_stack: vec![],
            stmts: vec![
                st(0, TOp::LoadLocal(0)),
                st(1, TOp::LoadLocal(1)),
                st(2, TOp::IntTrueDiv),
            ],
            term: TTerm::Return,
        }],
    };
    let (status, bits, tag, _, _) = run(&tfunc, &[7u64, 2u64]);
    assert_eq!(status, JitStatus::Returned);
    assert_eq!(tag, SlotTag::Float as u32);
    assert!((f64::from_bits(bits) - 3.5).abs() < 1e-12);
}
