//! Scalar-leaf certification excludes contextual helpers and nonlocal effects.
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
        comp_target_slots: vec![],
        cold_exits: vec![],
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
fn scalar_math_leaves_need_no_embedder_context() {
    use crate::ir::{GlobalGuard, MathFunc, ResolvedGlobal};
    use crate::runtime::{JitFrame, JitStatus, SlotTag};

    extern "C" fn sin(x: f64) -> f64 {
        x.sin()
    }
    extern "C" fn cos(x: f64) -> f64 {
        x.cos()
    }
    extern "C" fn unused_binary(_: f64, _: f64) -> f64 {
        panic!("a scalar math leaf must not call a binary helper")
    }
    crate::runtime::register_math_helpers(sin, cos, unused_binary, unused_binary);
    let mut engine = super::JitEngine::new().expect("host ISA");
    for (op, input, expected) in [
        (MathFunc::Sqrt, 4.0_f64, Some(2.0_f64)),
        (MathFunc::Sqrt, -0.0, Some(-0.0)),
        (MathFunc::Sqrt, -1.0, None),
        (MathFunc::Fabs, -3.0, Some(3.0)),
        (MathFunc::Sin, 0.5, Some(0.5_f64.sin())),
        (MathFunc::Cos, 0.5, Some(0.5_f64.cos())),
        (MathFunc::Sin, f64::INFINITY, None),
        (MathFunc::Cos, f64::NEG_INFINITY, None),
    ] {
        let mut tf = function(&[TOp::LoadLocal(0), TOp::MathIntrinsic(op)]);
        tf.local_types = vec![Some(JitType::Float), Some(JitType::Float)];
        tf.ret_lane = Some(JitType::Float);
        // The embedder validates this metadata before entering a leaf.
        tf.global_guards.push(GlobalGuard {
            name: "math".to_owned(),
            expect: ResolvedGlobal::MathModule,
        });
        let cf = engine.compile_tfunc(&tf).expect("compile math leaf");
        assert!(cf.is_scalar_leaf());
        let mut locals = [input.to_bits(), 0];
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
        // SAFETY: only certified scalar operations run, buffers fit, and
        // the context-free math helpers were registered before compilation.
        let status = unsafe { cf.enter(&raw mut frame) };
        if let Some(value) = expected {
            assert_eq!(status, JitStatus::Returned);
            assert_eq!(frame.ret_tag, SlotTag::Float as u32);
            assert_eq!(frame.ret_bits, value.to_bits());
        } else {
            assert_eq!(status, JitStatus::Deopt);
            assert_eq!(frame.deopt_pc, 1);
            assert_eq!(frame.stack_len, 1);
            assert_eq!(spill[0], input.to_bits());
            assert_eq!(tags[0], SlotTag::Float as u32);
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

#[test]
fn engine_drop_releases_native_mappings() {
    // Use a separate process so concurrently running tests cannot allocate
    // new code at the address being checked after its engine is destroyed.
    const CHILD: &str = "WEAVEPY_ENGINE_DROP_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "engine::tests::engine_drop_releases_native_mappings",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .status()
            .expect("spawn mapping test");
        assert!(status.success(), "isolated mapping test failed: {status}");
        return;
    }
    let mut engine = super::JitEngine::new().expect("host ISA");
    let compiled = engine
        .compile_tfunc(&function(&[
            TOp::LoadLocal(0),
            TOp::LoadLocal(1),
            TOp::IntArith(ArithKind::Add),
        ]))
        .expect("compile scalar function");
    let address = compiled.func as *const ();
    assert!(region::query(address)
        .expect("live native mapping")
        .protection()
        .contains(region::Protection::EXECUTE));
    drop(engine);
    assert!(
        matches!(region::query(address), Err(region::Error::UnmappedRegion)),
        "dropping the engine must unmap its native code"
    );
    // Metadata may outlive the engine, but entering it would violate the
    // unsafe entry contract. Dropping metadata must not free anything twice.
    drop(compiled);
}

#[test]
fn attribute_chains_keep_the_first_read_deopt_snapshot() {
    use crate::ir::AttrSiteMeta;
    use crate::runtime::{JitFrame, JitStatus, SlotTag};

    const CHILD: &str = "WEAVEPY_ATTR_CHAIN_LOWERING_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "engine::tests::attribute_chains_keep_the_first_read_deopt_snapshot",
            ])
            .env(CHILD, "1")
            .status()
            .expect("spawn isolated helper-registration test");
        assert!(status.success(), "attribute-chain test failed: {status}");
        return;
    }

    #[derive(Default, Debug)]
    struct State {
        singles: usize,
        chains: usize,
        fail: bool,
        chain: (i64, i64, i64),
    }

    thread_local! {
        static STATE: std::cell::RefCell<State> = std::cell::RefCell::new(State::default());
    }

    unsafe extern "C" fn get(frame: *mut JitFrame, pin: i64, site: i64) -> i64 {
        // SAFETY: the test supplies a live frame until native return.
        let frame = unsafe { &mut *frame };
        STATE.with(|state| state.borrow_mut().singles += 1);
        frame.ret_bits = (pin + site + 1) as u64;
        0
    }

    unsafe extern "C" fn set(_: *mut JitFrame, _: i64, _: i64) -> i64 {
        1
    }

    unsafe extern "C" fn chain(frame: *mut JitFrame, pin: i64, site: i64, count: i64) -> i64 {
        // SAFETY: as for get.
        let frame = unsafe { &mut *frame };
        let fail = STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.chains += 1;
            state.chain = (pin, site, count);
            state.fail
        });
        if fail {
            return 1;
        }
        frame.ret_bits = (pin + (site..site + count).map(|i| i + 1).sum::<i64>()) as u64;
        0
    }

    let mut tfunc = function(&[
        TOp::PushConstInt(99),
        TOp::LoadLocal(0),
        TOp::AttrGet {
            site: 0,
            out: JitType::Obj,
        },
        TOp::AttrGet {
            site: 1,
            out: JitType::Obj,
        },
        TOp::AttrGet {
            site: 2,
            out: JitType::Obj,
        },
        TOp::AttrGet {
            site: 3,
            out: JitType::Int,
        },
        TOp::IntArith(ArithKind::Add),
    ]);
    tfunc.local_types[0] = Some(JitType::Obj);
    tfunc.attr_sites = (0..4)
        .map(|site| AttrSiteMeta {
            slot: 0,
            path: (0..site).map(|i| format!("field_{i}")).collect(),
            name: format!("field_{site}"),
            lane: if site == 3 {
                JitType::Int
            } else {
                JitType::Obj
            },
            store: false,
            new_key: false,
            ctor: None,
            self_ctor: None,
        })
        .collect();

    fn run(engine: &mut super::JitEngine, tfunc: &TFunc, fail: bool) -> State {
        let compiled = engine
            .compile_tfunc(tfunc)
            .expect("compile attribute chain");
        STATE.with(|state| {
            *state.borrow_mut() = State {
                fail,
                ..State::default()
            }
        });
        let mut locals = [7u64, 0];
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
        // SAFETY: all buffers and the registered helper context remain live;
        // the owning engine outlives the call.
        let status = unsafe { compiled.enter(&raw mut frame) };
        if fail {
            assert_eq!(status, JitStatus::Deopt);
            assert_eq!(frame.deopt_pc, 2);
            assert_eq!(frame.stack_len, 2);
            assert_eq!(&spill[..2], &[99, 7]);
            assert_eq!(&tags[..2], &[SlotTag::Int as u32, SlotTag::ObjPin as u32]);
            assert_eq!(locals[0], 7);
        } else {
            assert_eq!(status, JitStatus::Returned);
            assert_eq!(frame.ret_tag, SlotTag::Int as u32);
            assert_eq!(frame.ret_bits, 116);
        }
        STATE.with(|state| state.take())
    }

    crate::runtime::register_attr_helpers(get, set);
    let mut engine = super::JitEngine::new().expect("host ISA");
    let ordinary = run(&mut engine, &tfunc, false);
    assert_eq!((ordinary.singles, ordinary.chains), (4, 0));

    crate::runtime::register_attr_get_chain_helper(chain);
    let fused = run(&mut engine, &tfunc, false);
    assert_eq!((fused.singles, fused.chains), (0, 1));
    assert_eq!(fused.chain, (7, 0, 4));
    let failed = run(&mut engine, &tfunc, true);
    assert_eq!((failed.singles, failed.chains), (0, 1));

    for (i, stmt) in tfunc.blocks[0].stmts.iter_mut().enumerate() {
        stmt.pc = (i * 2) as u32;
    }
    let separated = run(&mut engine, &tfunc, false);
    assert_eq!((separated.singles, separated.chains), (4, 0));
}
