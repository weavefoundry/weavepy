//! Missing helper registrations must reject compilation, never emit a null call.
//! This integration-test binary deliberately registers no embedder helpers.

use weavepy_jit::{JitEngine, JitType, JitVerdict, TBlock, TFunc, TOp, TStmt, TTerm};

fn function(ops: &[TOp]) -> TFunc {
    TFunc {
        n_locals: 2,
        local_types: vec![Some(JitType::Obj), Some(JitType::Int)],
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
        max_call_args: 1,
        ret_lane: None,
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
fn reject_each_unregistered_constant_and_scalar_helper() {
    let cases: Vec<(Vec<TOp>, &str)> = vec![
        (
            vec![TOp::PushConstStr { idx: 0 }],
            "str constant (no helper registered)",
        ),
        (
            vec![TOp::PushConstTuple { idx: 0 }],
            "tuple constant (no helper registered)",
        ),
        (
            vec![TOp::LoadLocal(0), TOp::TupleLen],
            "tuple length (no helper registered)",
        ),
        (
            vec![TOp::LoadLocal(0), TOp::UnboxInt { depth: 0 }],
            "integer guard (no helper registered)",
        ),
        (
            vec![
                TOp::LoadLocal(0),
                TOp::LoadLocal(1),
                TOp::CallDyn {
                    argc: 1,
                    kwc: 0,
                    names: 0,
                    int_result: true,
                },
            ],
            "integer call (no helper registered)",
        ),
    ];
    let mut engine = JitEngine::new().expect("host ISA");
    for (ops, expected) in cases {
        assert!(
            matches!(engine.compile_tfunc(&function(&ops)), Err(JitVerdict::UnsupportedOpcode(reason)) if reason == expected),
            "{expected}"
        );
    }
}
