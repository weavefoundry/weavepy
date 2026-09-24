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

    fn chain_function(reads: usize) -> TFunc {
        let mut ops = vec![TOp::PushConstInt(99), TOp::LoadLocal(0)];
        for site in 0..reads {
            ops.push(TOp::AttrGet {
                site: site as u32,
                out: if site + 1 == reads {
                    JitType::Int
                } else {
                    JitType::Obj
                },
            });
        }
        ops.push(TOp::IntArith(ArithKind::Add));
        let mut tfunc = function(&ops);
        tfunc.local_types[0] = Some(JitType::Obj);
        tfunc.attr_sites = (0..reads)
            .map(|site| AttrSiteMeta {
                slot: 0,
                path: (0..site).map(|i| format!("field_{i}")).collect(),
                name: format!("field_{site}"),
                lane: if site + 1 == reads {
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
        tfunc
    }

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
            let reads = tfunc.attr_sites.len() as u64;
            assert_eq!(frame.ret_bits, 106 + reads * (reads + 1) / 2);
        }
        STATE.with(|state| state.take())
    }

    crate::runtime::register_attr_helpers(get, set);
    let mut engine = super::JitEngine::new().expect("host ISA");
    let mut tfunc = chain_function(4);
    let ordinary = run(&mut engine, &tfunc, false);
    assert_eq!((ordinary.singles, ordinary.chains), (4, 0));

    crate::runtime::register_attr_get_chain_helper(chain);
    let fused = run(&mut engine, &tfunc, false);
    assert_eq!((fused.singles, fused.chains), (0, 1));
    assert_eq!(fused.chain, (7, 0, 4));
    let failed = run(&mut engine, &tfunc, true);
    assert_eq!((failed.singles, failed.chains), (0, 1));

    for reads in [2, 8, 9] {
        let deeper = chain_function(reads);
        let fused = run(&mut engine, &deeper, false);
        assert_eq!((fused.singles, fused.chains), (usize::from(reads == 9), 1));
        assert_eq!(fused.chain, (7, 0, reads.min(8) as i64));
        let failed = run(&mut engine, &deeper, true);
        assert_eq!((failed.singles, failed.chains), (0, 1));
    }

    for (i, stmt) in tfunc.blocks[0].stmts.iter_mut().enumerate() {
        stmt.pc = (i * 2) as u32;
    }
    let separated = run(&mut engine, &tfunc, false);
    assert_eq!((separated.singles, separated.chains), (4, 0));
}

#[test]
fn dynamic_attribute_reads_publish_each_cache_pc() {
    use crate::runtime::{JitFrame, JitStatus, SlotTag};
    const CHILD: &str = "WEAVEPY_DYN_ATTR_PC_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "engine::tests::dynamic_attribute_reads_publish_each_cache_pc",
            ])
            .env(CHILD, "1")
            .status()
            .expect("spawn cache-PC test");
        assert!(status.success(), "cache-PC test failed: {status}");
        return;
    }
    thread_local! {
        static READS: std::cell::RefCell<Vec<(u32, i64, i64)>> = const {
            std::cell::RefCell::new(Vec::new())
        };
    }
    unsafe extern "C" fn get(frame: *mut JitFrame, pin: i64, name: i64) -> i64 {
        // SAFETY: the test keeps the frame alive through native return.
        let frame = unsafe { &mut *frame };
        READS.with(|reads| reads.borrow_mut().push((frame.deopt_pc, pin, name)));
        frame.ret_bits = (pin + name + 1) as u64;
        0
    }
    unsafe extern "C" fn set(_: *mut JitFrame, _: i64, _: i64) -> i64 {
        3
    }
    unsafe extern "C" fn unused_unary(_: *mut JitFrame, _: i64) -> i64 {
        3
    }
    unsafe extern "C" fn unused_ternary(_: *mut JitFrame, _: i64, _: i64, _: i64) -> i64 {
        3
    }
    unsafe extern "C" fn unused_call(_: *mut JitFrame, _: i64, _: u32, _: u32, _: u32) -> i64 {
        3
    }
    // Compilation requires the whole frame-coverage helper group. These
    // additional helpers are never emitted by this two-read function.
    crate::runtime::register_call_dyn_helper(unused_call);
    crate::runtime::register_global_obj_helper(unused_unary);
    crate::runtime::register_iter_new_helper(unused_unary);
    crate::runtime::register_iter_next_pair_helper(unused_ternary);
    crate::runtime::register_str_format_helpers(unused_ternary, unused_ternary);
    let mut tfunc = function(&[
        TOp::LoadLocal(0),
        TOp::DynAttrGet { name: 0 },
        TOp::DynAttrGet { name: 1 },
    ]);
    tfunc.local_types[0] = Some(JitType::Obj);
    tfunc.ret_lane = Some(JitType::Obj);
    crate::runtime::register_dyn_attr_helpers(get, set);
    let mut engine = super::JitEngine::new().expect("host ISA");
    let compiled = engine.compile_tfunc(&tfunc).expect("dynamic read frame");
    let mut locals = [7u64, 0];
    let mut spill = [0u64; 3];
    let mut tags = [0u32; 3];
    let mut frame = JitFrame {
        locals: locals.as_mut_ptr(),
        n_locals: 2,
        entry_pc: 0,
        ret_bits: 0,
        ret_tag: 0,
        deopt_pc: 987,
        stack_spill: spill.as_mut_ptr(),
        stack_tags: tags.as_mut_ptr(),
        stack_len: 0,
        stack_cap: 3,
        ctx: std::ptr::null_mut(),
        call_args: std::ptr::null_mut(),
        call_tags: std::ptr::null_mut(),
    };
    // SAFETY: the engine, frame, and buffers outlive this entry.
    assert_eq!(
        unsafe { compiled.enter(&raw mut frame) },
        JitStatus::Returned
    );
    assert_eq!(frame.ret_tag, SlotTag::ObjPin as u32);
    assert_eq!(frame.ret_bits, 10);
    READS.with(|reads| assert_eq!(*reads.borrow(), [(1, 7, 0), (2, 8, 1)]));
}

#[test]
fn cached_attribute_chains_keep_native_fallback_and_exact_exits() {
    use crate::ir::{AttrSiteMeta, CalleeSpanMeta};
    use crate::runtime::{self, JitFrame, JitStatus, SlotTag};
    const CHILD: &str = "WEAVEPY_CACHED_CHAIN_LOWERING_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "engine::tests::cached_attribute_chains_keep_native_fallback_and_exact_exits",
            ])
            .env(CHILD, "1")
            .status()
            .expect("spawn cached-chain test");
        assert!(status.success(), "cached-chain test failed: {status}");
        return;
    }
    #[derive(Default, Debug)]
    struct State {
        chains: Vec<(u32, i64, i64, i64, i64, i64)>,
        singles: usize,
        prefixes: usize,
        dynamic: Vec<(u32, i64)>,
        unboxes: usize,
        miss: bool,
        prefix_only: bool,
        exit: Option<(u32, i64)>,
        unbox_miss: bool,
    }
    thread_local! {
        static STATE: std::cell::RefCell<State> = std::cell::RefCell::new(State::default());
    }
    unsafe extern "C" fn get(frame: *mut JitFrame, pin: i64, _: i64) -> i64 {
        STATE.with(|s| s.borrow_mut().singles += 1);
        // SAFETY: every test entry retains this frame and its buffers.
        unsafe {
            (*frame).ret_bits = (pin + 1) as u64;
        }
        0
    }
    unsafe extern "C" fn prefix(frame: *mut JitFrame, pin: i64, _: i64, count: i64) -> i64 {
        STATE.with(|s| s.borrow_mut().prefixes += 1);
        // SAFETY: as for get.
        unsafe {
            (*frame).ret_bits = (pin + count) as u64;
        }
        0
    }
    unsafe extern "C" fn dynamic(frame: *mut JitFrame, pin: i64, _: i64) -> i64 {
        // SAFETY: as for get.
        let frame = unsafe { &mut *frame };
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.dynamic.push((frame.deopt_pc, pin));
            if let Some((pc, status)) = s.exit {
                if pc == frame.deopt_pc {
                    return status;
                }
            }
            frame.ret_bits = (pin + 1) as u64;
            0
        })
    }
    unsafe extern "C" fn unbox(frame: *mut JitFrame, pin: i64) -> i64 {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.unboxes += 1;
            if s.unbox_miss {
                return 1;
            }
            // SAFETY: as for get.
            unsafe {
                (*frame).ret_bits = pin as u64;
            }
            0
        })
    }
    unsafe extern "C" fn chain(
        frame: *mut JitFrame,
        pin: i64,
        site: i64,
        guarded: i64,
        total: i64,
        int_result: i64,
    ) -> i64 {
        // SAFETY: as for get.
        let frame = unsafe { &mut *frame };
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.chains
                .push((frame.deopt_pc, pin, site, guarded, total, int_result));
            if s.miss {
                if s.prefix_only && guarded > 0 {
                    frame.ret_bits = (pin + guarded) as u64;
                    return 2;
                }
                return 1;
            }
            frame.ret_bits = (pin + total) as u64;
            0
        })
    }
    unsafe extern "C" fn unused3(_: *mut JitFrame, _: i64, _: i64) -> i64 {
        3
    }
    unsafe extern "C" fn unused4(_: *mut JitFrame, _: i64, _: i64, _: i64) -> i64 {
        3
    }
    unsafe extern "C" fn unused_call(_: *mut JitFrame, _: i64, _: u32, _: u32, _: u32) -> i64 {
        3
    }
    runtime::register_attr_helpers(get, unused3);
    runtime::register_attr_get_chain_helper(prefix);
    runtime::register_dyn_attr_helpers(dynamic, unused3);
    runtime::register_unbox_int_helper(unbox);
    runtime::register_call_dyn_helper(unused_call);
    runtime::register_global_obj_helper(unbox);
    runtime::register_iter_new_helper(unbox);
    runtime::register_iter_next_pair_helper(unused4);
    runtime::register_str_format_helpers(unused4, unused4);

    fn chain_function(guarded: usize, total: usize, integer: bool) -> TFunc {
        let mut ops = Vec::new();
        if integer {
            ops.push(TOp::PushConstInt(99));
        }
        ops.push(TOp::LoadLocal(0));
        for i in 0..total {
            ops.push(if i < guarded {
                TOp::AttrGet {
                    site: i as u32,
                    out: JitType::Obj,
                }
            } else {
                TOp::DynAttrGet { name: i as u32 }
            });
        }
        if integer {
            ops.push(TOp::UnboxInt { depth: 0 });
            ops.push(TOp::IntArith(ArithKind::Add));
        }
        let mut f = function(&ops);
        f.local_types[0] = Some(JitType::Obj);
        f.ret_lane = Some(if integer { JitType::Int } else { JitType::Obj });
        f.attr_sites = (0..guarded)
            .map(|i| AttrSiteMeta {
                slot: 0,
                path: vec![],
                name: format!("field_{i}"),
                lane: JitType::Obj,
                store: false,
                new_key: false,
                ctor: None,
                self_ctor: None,
            })
            .collect();
        f
    }
    fn run(
        engine: &mut super::JitEngine,
        f: &TFunc,
        state: State,
        expected: JitStatus,
    ) -> (State, u32, Vec<u64>, Vec<u32>) {
        let compiled = engine.compile_tfunc(f).expect("compile cached chain");
        STATE.with(|s| *s.borrow_mut() = state);
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
        // SAFETY: all buffers fit the compiled metadata and outlive this call.
        assert_eq!(unsafe { compiled.enter(&raw mut frame) }, expected);
        if expected == JitStatus::Returned {
            let reads = f.blocks[0]
                .stmts
                .iter()
                .filter(|s| matches!(s.op, TOp::AttrGet { .. } | TOp::DynAttrGet { .. }))
                .count() as u64;
            let integer = f.ret_lane == Some(JitType::Int);
            assert_eq!(frame.ret_bits, 7 + reads + if integer { 99 } else { 0 });
            assert_eq!(
                frame.ret_tag,
                if integer {
                    SlotTag::Int
                } else {
                    SlotTag::ObjPin
                } as u32
            );
        }
        (
            STATE.with(|s| s.take()),
            frame.deopt_pc,
            spill[..frame.stack_len as usize].to_vec(),
            tags[..frame.stack_len as usize].to_vec(),
        )
    }
    let mut engine = super::JitEngine::new().unwrap();
    let f = chain_function(2, 4, true);
    let (s, ..) = run(&mut engine, &f, State::default(), JitStatus::Returned);
    assert!(s.chains.is_empty());
    assert_eq!((s.prefixes, s.dynamic.len(), s.unboxes), (1, 2, 1));
    runtime::register_cached_attr_chain_helper(chain);
    for (guarded, total, integer) in [
        (2, 4, true),
        (8, 16, true),
        (0, 4, false),
        (0, 32, true),
        (0, 33, true),
    ] {
        let f = chain_function(guarded, total, integer);
        let (s, ..) = run(&mut engine, &f, State::default(), JitStatus::Returned);
        assert_eq!(
            s.chains,
            [(
                if integer { 2 } else { 1 },
                7,
                0,
                guarded as i64,
                total.min(32) as i64,
                i64::from(integer && total <= 32)
            )]
        );
        assert_eq!(
            (s.singles, s.prefixes, s.dynamic.len(), s.unboxes),
            (0, 0, usize::from(total > 32), usize::from(total > 32))
        );
        let (s, ..) = run(
            &mut engine,
            &f,
            State {
                miss: true,
                ..State::default()
            },
            JitStatus::Returned,
        );
        assert_eq!(s.prefixes, usize::from(guarded >= 2));
        assert_eq!(s.dynamic.len(), total - guarded);
        assert_eq!(s.unboxes, usize::from(integer));
        let (s, ..) = run(
            &mut engine,
            &f,
            State {
                miss: true,
                prefix_only: true,
                ..State::default()
            },
            JitStatus::Returned,
        );
        assert_eq!(s.prefixes, 0);
        assert_eq!(s.dynamic.len(), total - guarded);
        assert_eq!(s.unboxes, usize::from(integer));
    }
    // A failed fused read leaves the originals responsible for their own PCs,
    // receiver/result stacks, and distinct reject, parked, and raised statuses.
    for prefix_only in [false, true] {
        for status in [1, 2, 3] {
            let (s, pc, spill, tags) = run(
                &mut engine,
                &f,
                State {
                    miss: true,
                    prefix_only,
                    exit: Some((5, status)),
                    ..State::default()
                },
                if status == 1 {
                    JitStatus::Raised
                } else {
                    JitStatus::Deopt
                },
            );
            assert_eq!(s.dynamic, [(4, 9), (5, 10)]);
            assert_eq!(pc, if status == 2 { 6 } else { 5 });
            assert_eq!(spill, if status == 3 { vec![99, 10] } else { vec![99] });
            assert_eq!(
                tags,
                if status == 3 {
                    vec![SlotTag::Int as u32, SlotTag::ObjPin as u32]
                } else {
                    vec![SlotTag::Int as u32]
                }
            );
        }
    }
    let (_, pc, spill, _) = run(
        &mut engine,
        &f,
        State {
            miss: true,
            unbox_miss: true,
            ..State::default()
        },
        JitStatus::Deopt,
    );
    assert_eq!((pc, spill), (6, vec![99, 11]));
    let mut separated = f.clone();
    for stmt in &mut separated.blocks[0].stmts {
        stmt.pc *= 2;
    }
    let (s, ..) = run(
        &mut engine,
        &separated,
        State::default(),
        JitStatus::Returned,
    );
    assert!(s.chains.is_empty());
    assert_eq!((s.singles, s.prefixes, s.dynamic.len()), (2, 0, 2));
    let mut discontinuous = f.clone();
    discontinuous.blocks[0].stmts[3].op = TOp::AttrGet {
        site: 0,
        out: JitType::Obj,
    };
    let (s, ..) = run(
        &mut engine,
        &discontinuous,
        State::default(),
        JitStatus::Returned,
    );
    assert_eq!(s.singles, 1);
    assert_eq!(s.chains, [(3, 8, 0, 1, 3, 1)]);
    let mut later_unbox = f.clone();
    later_unbox.blocks[0].stmts[6].pc += 2;
    let (s, ..) = run(
        &mut engine,
        &later_unbox,
        State::default(),
        JitStatus::Returned,
    );
    assert_eq!(s.chains, [(2, 7, 0, 2, 4, 0)]);
    assert_eq!(s.unboxes, 1);
    let mut method = chain_function(1, 2, false);
    method.null_spans.push(CalleeSpanMeta {
        live_from: 2,
        live_to: 3,
        token: 0,
        interp_depth: 1,
    });
    let (s, ..) = run(&mut engine, &method, State::default(), JitStatus::Returned);
    assert!(s.chains.is_empty());
    assert_eq!((s.singles, s.dynamic.len()), (1, 1));
}
