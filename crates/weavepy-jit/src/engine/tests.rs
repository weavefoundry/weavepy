//! Scalar-leaf certification excludes helpers and nonlocal effects.
use super::is_scalar_leaf;
use crate::ir::{ArithKind, TBlock, TFunc, TOp, TStmt, TTerm};
use crate::value::JitType;

fn function(ops: &[TOp]) -> TFunc {
    TFunc {
        n_locals: 2,
        local_types: vec![Some(JitType::Int), Some(JitType::Int)],
        livein_locals: vec![0, 1],
        max_stack: 2,
        entry_block: 0,
        global_guards: vec![],
        range_loops: vec![],
        list_loops: vec![],
        iter_loops: vec![],
        comp_saved: vec![],
        resume_entries: vec![],
        callee_spans: vec![],
        len_spans: vec![],
        method_spans: vec![],
        attr_sites: vec![],
        method_sites: vec![],
        str_method_sites: vec![],
        str_method_spans: vec![],
        math_guards: vec![],
        math_spans: vec![],
        null_spans: vec![],
        osr_entries: vec![],
        max_call_args: 0,
        ret_lane: Some(JitType::Int),
        ret_none: false,
        blocks: vec![TBlock {
            entry_stack: vec![],
            stmts: ops
                .iter()
                .enumerate()
                .map(|(pc, &op)| TStmt { pc: pc as u32, op })
                .collect(),
            term: TTerm::Return,
        }],
    }
}

#[test]
fn scalar_leaf_allowlist_is_conservative() {
    let pure = function(&[
        TOp::LoadLocal(0),
        TOp::LoadLocal(1),
        TOp::IntArith(ArithKind::Add),
    ]);
    assert!(is_scalar_leaf(&pure));
    let mut changed = pure.clone();
    changed.blocks.push(changed.blocks[0].clone());
    assert!(!is_scalar_leaf(&changed));
    let mut changed = pure.clone();
    changed.local_types[0] = Some(JitType::Obj);
    assert!(!is_scalar_leaf(&changed));
    let mut changed = pure.clone();
    changed.ret_lane = Some(JitType::Obj);
    assert!(!is_scalar_leaf(&changed));
    let mut changed = pure.clone();
    changed.blocks[0].stmts.resize(
        65,
        TStmt {
            pc: 0,
            op: TOp::Pop,
        },
    );
    assert!(!is_scalar_leaf(&changed));
    for op in [
        TOp::FloatArith(ArithKind::FloorDiv),
        TOp::FloatArith(ArithKind::Mod),
        TOp::UnboxInt { depth: 0 },
        TOp::PushConstStr { idx: 0 },
        TOp::PushConstTuple { idx: 0 },
        TOp::TupleLen,
        TOp::CallDyn {
            argc: 0,
            kwc: 0,
            names: 0,
            int_result: true,
        },
        TOp::ListLen,
        TOp::DynAttrGet { name: 0 },
    ] {
        assert!(!is_scalar_leaf(&function(&[op])), "{op:?}");
    }
}

#[test]
fn scalar_leaf_returns_and_deopts_with_no_embedder_context() {
    use crate::runtime::{JitFrame, JitStatus, SlotTag};
    let tfunc = function(&[
        TOp::LoadLocal(0),
        TOp::LoadLocal(1),
        TOp::IntArith(ArithKind::Add),
    ]);
    let mut engine = super::JitEngine::new().expect("host ISA");
    let cf = engine.compile_tfunc(&tfunc).expect("compile scalar leaf");
    assert!(cf.is_scalar_leaf());
    for (a, b, expected) in [
        (40, 2, JitStatus::Returned),
        (i64::MAX, 1, JitStatus::Deopt),
    ] {
        let mut locals = [a as u64, b as u64];
        let mut spill = [0u64; 3];
        let mut tags = [0u32; 3];
        let mut frame = JitFrame {
            locals: locals.as_mut_ptr(),
            n_locals: 2,
            entry_pc: 0,
            ret_bits: 0,
            ret_tag: 0,
            deopt_pc: 0,
            stack_spill: spill.as_mut_ptr(),
            stack_tags: tags.as_mut_ptr(),
            stack_len: 0,
            stack_cap: 3,
            ctx: std::ptr::null_mut(),
            call_args: std::ptr::null_mut(),
            call_tags: std::ptr::null_mut(),
        };
        // SAFETY: the certified scalar leaf needs no helper context, all
        // buffers fit its metadata, and the owning engine is still alive.
        assert_eq!(unsafe { cf.enter(&raw mut frame) }, expected);
        if expected == JitStatus::Returned {
            assert_eq!(frame.ret_bits, 42);
            assert_eq!(frame.ret_tag, SlotTag::Int as u32);
        } else {
            assert_eq!(frame.deopt_pc, 2);
            assert_eq!(frame.stack_len, 2);
            assert_eq!(&spill[..2], &[a as u64, b as u64]);
            assert_eq!(&tags[..2], &[SlotTag::Int as u32; 2]);
        }
    }
}

#[test]
fn explicit_exit_preserves_operands_and_updated_locals() {
    use crate::runtime::{JitFrame, JitStatus, SlotTag};
    let mut tf = function(&[
        TOp::LoadLocal(0),
        TOp::PushConstInt(99),
        TOp::StoreLocal(0),
        TOp::LoadLocal(1),
    ]);
    tf.blocks[0].term = TTerm::Deopt { pc: 17 };
    assert!(!is_scalar_leaf(&tf));
    let mut engine = super::JitEngine::new().expect("host ISA");
    let compiled = engine.compile_tfunc(&tf).expect("compile explicit exit");
    let mut locals = [41u64, 7];
    let mut spill = [0u64; 3];
    let mut tags = [0u32; 3];
    let mut frame = JitFrame {
        locals: locals.as_mut_ptr(),
        n_locals: 2,
        entry_pc: 0,
        ret_bits: 0,
        ret_tag: 0,
        deopt_pc: 0,
        stack_spill: spill.as_mut_ptr(),
        stack_tags: tags.as_mut_ptr(),
        stack_len: 0,
        stack_cap: 3,
        ctx: std::ptr::null_mut(),
        call_args: std::ptr::null_mut(),
        call_tags: std::ptr::null_mut(),
    };
    // SAFETY: this IR uses only scalar loads/stores and an explicit exit,
    // needs no helpers, and all buffers fit the live compiled metadata.
    assert_eq!(unsafe { compiled.enter(&raw mut frame) }, JitStatus::Deopt);
    assert_eq!(frame.deopt_pc, 17);
    assert_eq!(frame.stack_len, 2);
    assert_eq!(&spill[..2], &[41, 7]);
    assert_eq!(&tags[..2], &[SlotTag::Int as u32; 2]);
    assert_eq!(locals, [99, 7]);
}
