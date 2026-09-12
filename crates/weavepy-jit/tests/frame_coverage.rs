//! RFC 0074 — analyzer tests over real compiled Python shapes for the
//! frame-coverage wave: object globals, the opaque-call lane, dynamic
//! attribute access, tuple-target (pair) loops with trained lanes, and
//! the `str` %-formatting / slice lanes. These stop at the analyzer
//! (no VM), so they check the *decisions*; execution is covered by
//! weavepy-vm's `jit_*` tests.

use weavepy_compiler::{compile_module, CodeObject, Constant};
use weavepy_jit::{
    analyze_frame, JitType, JitVerdict, PathArena, Probes, ResolvedGlobal, SliceOrigin, TFunc, TOp,
    TTerm,
};
use weavepy_parser::parse_module;

/// Compile `src` and return the code object of the first `def`.
fn compile_first_fn(src: &str) -> CodeObject {
    let module = parse_module(src).expect("parse");
    let code = compile_module(&module).expect("compile");
    for c in &code.constants {
        if let Constant::Code(inner) = c {
            return (**inner).clone();
        }
    }
    panic!("no function in {src:?}");
}

/// Probe bundle for the wave-10 shapes: `range`/`enumerate` certified,
/// every other resolving name graded through a token-assigning
/// obj-global probe, and optional trained container lanes.
#[derive(Default)]
struct Cfg {
    /// `(slot, elem lane)` answered by the list probe.
    list: Option<(u32, JitType)>,
    /// `(slot, key lane, value lane)` answered by the dict probe.
    dict: Option<(u32, JitType, JitType)>,
    /// Slots the (seeded) param probe grades `Obj`.
    obj_params: Vec<u32>,
    /// Explicit parameter lanes, including exact bytes.
    typed_params: Vec<(u32, JitType)>,
    /// Names the resolver treats as genuinely missing (`Opaque` and
    /// no obj-global token — the `NameError` shape).
    missing: Vec<&'static str>,
}

fn analyze_cfg(src: &str, cfg: &Cfg) -> Result<TFunc, JitVerdict> {
    let code = compile_first_fn(src);
    analyze_code_cfg(&code, cfg)
}

fn analyze_code_cfg(code: &CodeObject, cfg: &Cfg) -> Result<TFunc, JitVerdict> {
    let missing = cfg.missing.clone();
    let mut resolve = |name: &str| -> ResolvedGlobal {
        match name {
            "range" => ResolvedGlobal::RangeBuiltin,
            "enumerate" => ResolvedGlobal::EnumerateBuiltin,
            "len" => ResolvedGlobal::LenBuiltin,
            _ => ResolvedGlobal::Opaque,
        }
    };
    let mut tokens: Vec<String> = Vec::new();
    let missing2 = missing.clone();
    let mut obj_global = move |name: &str| -> Option<(u32, JitType)> {
        if missing2.contains(&name) {
            return None;
        }
        if let Some(i) = tokens.iter().position(|t| t == name) {
            return Some((i as u32, JitType::Obj));
        }
        tokens.push(name.to_owned());
        Some(((tokens.len() - 1) as u32, JitType::Obj))
    };
    let list = cfg.list;
    let dict = cfg.dict;
    let obj_params = cfg.obj_params.clone();
    let typed_params = cfg.typed_params.clone();
    let mut paths = PathArena::default();
    let mut probes = Probes {
        list: &mut move |s| list.filter(|(slot, _)| *slot == s).map(|(_, e)| e),
        dict: &mut move |s| dict.filter(|(slot, ..)| *slot == s).map(|(_, k, v)| (k, v)),
        attr: &mut |_, _, _, _| None,
        method: &mut |_, _, _| None,
        math: &mut |_, _| false,
        ctor_field: &mut |_, _| None,
        param: &mut move |s| {
            if let Some((_, lane)) = typed_params.iter().find(|(slot, _)| *slot == s) {
                Some(*lane)
            } else if obj_params.contains(&s) {
                Some(JitType::Obj)
            } else {
                None
            }
        },
        kw_slot: &mut |_, _| None,
        obj_global: &mut obj_global,
        cell: &mut |_| None,
        obj: &mut |_| false,
        paths: &mut paths,
    };
    analyze_frame(code, &mut resolve, &mut probes)
}

fn has_op(tf: &TFunc, pred: impl Fn(&TOp) -> bool) -> bool {
    tf.blocks
        .iter()
        .any(|b| b.stmts.iter().any(|s| pred(&s.op)))
}

#[test]
fn tuple_constants_pin_without_copying_or_scalar_restrictions() {
    for literal in ["()", "(1, 2, 3)", "(None, (1, 'two words'), b'bytes')"] {
        let src = format!(
            "def k(n):\n    out = {literal}\n    for i in range(n):\n        out = {literal}\n    return out\n"
        );
        let tf = analyze_cfg(&src, &Cfg::default()).expect("tuple constant should compile");
        assert!(has_op(&tf, |op| matches!(op, TOp::PushConstTuple { .. })));
        assert!(tf.local_types.contains(&Some(JitType::Obj)));
    }
}

#[test]
fn tuple_constant_length_has_a_runtime_shape_guard() {
    let tf = analyze_cfg(
        "def k(n):\n    seq = (1, 2, 3)\n    out = 0\n    for i in range(n):\n        out += len(seq)\n    return out\n",
        &Cfg::default(),
    )
    .expect("tuple length should compile");
    assert!(has_op(&tf, |op| matches!(op, TOp::PushConstTuple { .. })));
    assert!(has_op(&tf, |op| matches!(op, TOp::TupleLen)));
    assert!(
        !tf.len_spans.is_empty(),
        "deopt must restore the len callee"
    );
}

#[test]
fn object_parameter_length_keeps_the_tuple_guard() {
    let tf = analyze_cfg(
        "def k(seq, n):\n    out = 0\n    for i in range(n):\n        out += len(seq)\n    return out\n",
        &Cfg { obj_params: vec![0], ..Cfg::default() },
    )
    .expect("an object parameter can use guarded tuple length");
    assert!(has_op(&tf, |op| matches!(op, TOp::TupleLen)));
}

#[test]
fn opaque_integer_call_results_guard_each_operand_position() {
    for (expression, depth) in [("total + callback(i)", 0), ("callback(i) + total", 1)] {
        let source = format!(
            "def k(n, callback):\n    total = 0\n    for i in range(n):\n        total = {expression}\n    return total\n"
        );
        let tf = analyze_cfg(
            &source,
            &Cfg {
                obj_params: vec![1],
                ..Cfg::default()
            },
        )
        .expect("integer consumers can guard an opaque call result");
        assert!(has_op(
            &tf,
            |op| matches!(op, TOp::CallDyn { int_result, .. } if *int_result == (depth == 0))
        ));
        assert_eq!(
            has_op(
                &tf,
                |op| matches!(op, TOp::UnboxInt { depth: found } if *found == depth)
            ),
            depth != 0
        );
        assert!(tf.local_types.contains(&Some(JitType::Int)));
    }
}

#[test]
fn guarded_integer_division_retains_float_result() {
    let tf = analyze_cfg(
        "def k(n, callback):\n    total = 0.0\n    for i in range(n):\n        total += 10 / callback(i)\n    return total\n",
        &Cfg { obj_params: vec![1], ..Cfg::default() },
    )
    .expect("checked integer division should analyze");
    assert!(has_op(&tf, |op| matches!(
        op,
        TOp::CallDyn {
            int_result: true,
            ..
        }
    )));
    assert!(has_op(&tf, |op| matches!(op, TOp::IntTrueDiv)));
    assert!(tf.local_types.contains(&Some(JitType::Float)));
}

#[test]
fn two_opaque_arithmetic_operands_do_not_guess_a_scalar_lane() {
    assert!(matches!(
        analyze_cfg(
            "def k(a, b):\n    return a + b\n",
            &Cfg {
                obj_params: vec![0, 1],
                ..Cfg::default()
            },
        ),
        Err(JitVerdict::MixedArithTypes)
    ));
}

#[test]
fn empty_list_accepts_native_scalar_appends() {
    for value in ["i * i", "i + 0.5", "i < 2"] {
        let source = format!(
            "def k(n):\n    out = []\n    for i in range(n):\n        out.append({value})\n    return out\n"
        );
        let tf = analyze_cfg(&source, &Cfg::default()).expect("scalar append should compile");
        assert!(has_op(&tf, |op| matches!(op, TOp::ListAppend)));
        assert!(tf.local_types.contains(&Some(JitType::ListObj)));
    }
}

#[test]
fn typed_list_still_rejects_mismatched_append() {
    let err = analyze_cfg(
        "def k(n):\n    out = [1]\n    for i in range(n):\n        out.append(0.5)\n    return out\n",
        &Cfg::default(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        JitVerdict::UnsupportedOpcode("append (value lane)")
    ));
}

// ---------------------------------------------------------------- WS1

#[test]
fn obj_global_burns_with_guard() {
    // `FLAGS` is an arbitrary object: it must burn as an identity-
    // guarded obj-global pin instead of rejecting the frame.
    let tf = analyze_cfg(
        "def k(n):\n    out = None\n    for i in range(n):\n        out = f2(FLAGS)\n    return out\n",
        &Cfg::default(),
    )
    .expect("obj global should burn");
    assert!(has_op(&tf, |op| matches!(op, TOp::PushGlobalObj { .. })));
    assert!(
        tf.global_guards.iter().any(|g| g.name == "FLAGS"),
        "{:?}",
        tf.global_guards
    );
}

#[test]
fn missing_global_still_rejects() {
    // A name that doesn't resolve at all is the NameError shape: the
    // frame stays interpreted.
    let err = analyze_cfg(
        "def k(n):\n    return f2(MISSING)\n",
        &Cfg {
            missing: vec!["MISSING"],
            ..Cfg::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, JitVerdict::UnsupportedOpcode(_)), "{err:?}");
}

// ---------------------------------------------------------------- WS2

#[test]
fn opaque_global_callee_rides_call_dyn() {
    // `helper` resolves to an arbitrary object; its call takes the
    // opaque-call lane (result on the object lane, stored untouched).
    let tf = analyze_cfg(
        "def k(n):\n    out = None\n    for i in range(n):\n        out = helper(i)\n    return out\n",
        &Cfg::default(),
    )
    .expect("opaque callee should ride CallDyn");
    assert!(has_op(&tf, |op| matches!(op, TOp::CallDyn { .. })));
}

#[test]
fn param_callee_rides_call_dyn() {
    // A callable parameter (graded `Obj` by the seeded retry) is a
    // dynamic callee too.
    let tf = analyze_cfg(
        "def k(f, n):\n    out = None\n    for i in range(n):\n        out = f(i)\n    return out\n",
        &Cfg {
            obj_params: vec![0],
            ..Cfg::default()
        },
    )
    .expect("param callee should ride CallDyn");
    assert!(has_op(&tf, |op| matches!(op, TOp::CallDyn { .. })));
}

#[test]
fn call_dyn_result_arithmetic_checks_the_returned_integer() {
    // The immediate consumer lets the call guard its completed return
    // directly, without first allocating an object pin for an integer.
    let tf = analyze_cfg(
        "def k(f, n):\n    t = 0\n    for i in range(n):\n        t = t + f(i)\n    return t\n",
        &Cfg {
            obj_params: vec![0],
            ..Cfg::default()
        },
    )
    .expect("an integer consumer can guard the opaque result");
    assert!(has_op(&tf, |op| matches!(
        op,
        TOp::CallDyn {
            int_result: true,
            ..
        }
    )));
    assert!(!has_op(&tf, |op| matches!(op, TOp::UnboxInt { .. })));
}

#[test]
fn stored_call_result_keeps_its_object_lane() {
    let tf = analyze_cfg(
        "def k(callback):\n    value = callback(1)\n    total = 0\n    total += value\n    return value\n",
        &Cfg { obj_params: vec![0], ..Cfg::default() },
    ).expect("a stored result keeps a separate arithmetic guard");
    assert!(has_op(&tf, |op| matches!(
        op,
        TOp::CallDyn {
            int_result: false,
            ..
        }
    )));
    assert!(has_op(&tf, |op| matches!(op, TOp::UnboxInt { depth: 0 })));
}

// ---------------------------------------------------------------- WS3

#[test]
fn dict_items_pair_loop_trains_lanes() {
    // `for k, v in d.items():` — the attr load falls back to the
    // generic lane, the call to CallDyn, the capture materializes, and
    // the recognized pair source trains (Str, Int) so `t += v` types.
    let tf = analyze_cfg(
        "def k(d):\n    t = 0\n    for key, v in d.items():\n        t = t + v\n    return t\n",
        &Cfg {
            dict: Some((0, JitType::Str, JitType::Int)),
            ..Cfg::default()
        },
    )
    .expect("items pair loop should analyze");
    let pair = tf.blocks.iter().find_map(|b| match b.term {
        TTerm::ForIterPair { elem1, elem2, .. } => Some((elem1, elem2)),
        _ => None,
    });
    assert_eq!(pair, Some((JitType::Str, JitType::Int)), "{pair:?}");
    assert!(has_op(&tf, |op| matches!(op, TOp::DynAttrGet { .. })));
    assert!(has_op(&tf, |op| matches!(op, TOp::CallDyn { .. })));
}

#[test]
fn enumerate_pair_loop_trains_lanes() {
    // `for i, x in enumerate(xs):` over a probed int list — the
    // certified builtin trains (Int, Int).
    let tf = analyze_cfg(
        "def k(xs):\n    t = 0\n    for i, x in enumerate(xs):\n        t = t + i * x\n    return t\n",
        &Cfg {
            list: Some((0, JitType::Int)),
            ..Cfg::default()
        },
    )
    .expect("enumerate pair loop should analyze");
    let pair = tf.blocks.iter().find_map(|b| match b.term {
        TTerm::ForIterPair { elem1, elem2, .. } => Some((elem1, elem2)),
        _ => None,
    });
    assert_eq!(pair, Some((JitType::Int, JitType::Int)), "{pair:?}");
}

#[test]
fn unrecognized_pair_source_defaults_to_obj_lanes() {
    // A pair loop over an arbitrary iterable parameter: both variables
    // ride the object lane (the step helper re-validates per element).
    let tf = analyze_cfg(
        "def k(pairs):\n    out = None\n    for a, b in pairs:\n        out = b\n    return out\n",
        &Cfg {
            obj_params: vec![0],
            ..Cfg::default()
        },
    )
    .expect("generic pair loop should analyze");
    let pair = tf.blocks.iter().find_map(|b| match b.term {
        TTerm::ForIterPair { elem1, elem2, .. } => Some((elem1, elem2)),
        _ => None,
    });
    assert_eq!(pair, Some((JitType::Obj, JitType::Obj)), "{pair:?}");
}

#[test]
fn enumerate_bytes_pair_loop_trains_integer_lanes() {
    let tf = analyze_cfg(
        "def k(xs):\n    t = 0\n    for i, x in enumerate(xs):\n        t = t + (i ^ x)\n    return t\n",
        &Cfg {
            typed_params: vec![(0, JitType::Bytes)],
            ..Cfg::default()
        },
    )
    .expect("exact bytes enumeration should analyze");
    let pair = tf.blocks.iter().find_map(|b| match b.term {
        TTerm::ForIterPair { elem1, elem2, .. } => Some((elem1, elem2)),
        _ => None,
    });
    assert_eq!(pair, Some((JitType::Int, JitType::Int)));
    assert_eq!(tf.local_types[0], Some(JitType::Bytes));
}

#[test]
fn enumerate_bytes_requests_parameter_training() {
    assert!(matches!(
        analyze_cfg(
            "def k(xs):\n    t = 0\n    for i, x in enumerate(xs):\n        t = t + (i ^ x)\n    return t\n",
            &Cfg::default(),
        ),
        Err(JitVerdict::TypeUnknown)
    ));
}

#[test]
fn enumerate_generic_source_keeps_integer_index() {
    let tf = analyze_cfg(
        "def k(xs):\n    t = 0\n    for i, x in enumerate(xs):\n        t = t + i\n    return t\n",
        &Cfg {
            obj_params: vec![0],
            ..Cfg::default()
        },
    )
    .expect("enumerate's index is an int for generic iterables too");
    let pair = tf.blocks.iter().find_map(|b| match b.term {
        TTerm::ForIterPair { elem1, elem2, .. } => Some((elem1, elem2)),
        _ => None,
    });
    assert_eq!(pair, Some((JitType::Int, JitType::Obj)));
}

#[test]
fn enumerate_local_object_keeps_integer_index() {
    let tf = analyze_cfg(
        "def k():\n    xs = DATA\n    t = 0\n    for i, x in enumerate(xs):\n        t = t + i\n    return t\n",
        &Cfg::default(),
    )
    .expect("a known object local doesn't need a parameter probe");
    let pair = tf.blocks.iter().find_map(|b| match b.term {
        TTerm::ForIterPair { elem1, elem2, .. } => Some((elem1, elem2)),
        _ => None,
    });
    assert_eq!(pair, Some((JitType::Int, JitType::Obj)));
}

#[test]
fn shadowed_enumerate_gets_no_trained_lanes() {
    // Without the canonical certification the pair source is not
    // recognized; both values stay Obj. Arithmetic must check its
    // index operand even when it speculates on an integer result.
    let code = compile_first_fn(
        "def k(xs):\n    t = 0\n    for i, x in enumerate(xs):\n        t = t + i\n    return t\n",
    );
    let mut resolve = |name: &str| -> ResolvedGlobal {
        if name == "range" {
            ResolvedGlobal::RangeBuiltin
        } else {
            ResolvedGlobal::Opaque // enumerate rebound: no certification
        }
    };
    let mut obj_global = |_: &str| Some((0u32, JitType::Obj));
    let mut paths = PathArena::default();
    let mut probes = Probes {
        list: &mut |_| Some(JitType::Int),
        dict: &mut |_| None,
        attr: &mut |_, _, _, _| None,
        method: &mut |_, _, _| None,
        math: &mut |_, _| false,
        ctor_field: &mut |_, _| None,
        param: &mut |_| Some(JitType::Obj),
        kw_slot: &mut |_, _| None,
        obj_global: &mut obj_global,
        cell: &mut |_| None,
        obj: &mut |_| false,
        paths: &mut paths,
    };
    let tf = analyze_frame(&code, &mut resolve, &mut probes)
        .expect("shadowed enumeration can use a checked integer consumer");
    let pair = tf.blocks.iter().find_map(|b| match b.term {
        TTerm::ForIterPair { elem1, elem2, .. } => Some((elem1, elem2)),
        _ => None,
    });
    assert_eq!(pair, Some((JitType::Obj, JitType::Obj)));
    assert!(has_op(&tf, |op| matches!(op, TOp::UnboxInt { depth: 0 })));
}

// ---------------------------------------------------------------- WS4

#[test]
fn dyn_attr_get_and_set_fall_back() {
    // Attribute traffic on an object parameter with no probed shape
    // (a property receiver): both directions ride the generic lane.
    let tf = analyze_cfg(
        "def k(o, n):\n    for i in range(n):\n        o.value = i\n    return o.value\n",
        &Cfg {
            obj_params: vec![0],
            ..Cfg::default()
        },
    )
    .expect("dyn attr traffic should analyze");
    assert!(has_op(&tf, |op| matches!(op, TOp::DynAttrGet { .. })));
    assert!(has_op(&tf, |op| matches!(op, TOp::DynAttrSet { .. })));
}

#[test]
fn dict_receiver_attr_trains_container_lane() {
    // `d.items` alone (no live dict probe would leave the local
    // untyped and abort): the fallback trains the receiver from the
    // dict probe so the frame compiles with an entry-guarded Dict
    // local.
    let tf = analyze_cfg(
        "def k(d):\n    m = d.items\n    return m\n",
        &Cfg {
            dict: Some((0, JitType::Str, JitType::Int)),
            ..Cfg::default()
        },
    )
    .expect("dict receiver attr should analyze");
    assert!(has_op(&tf, |op| matches!(op, TOp::DynAttrGet { .. })));
    assert_eq!(tf.local_types[0], Some(JitType::Dict));
}

// ---------------------------------------------------------------- WS5

#[test]
fn str_mod_and_slice_lanes() {
    let tf = analyze_cfg(
        "def k(n):\n    s = \"\"\n    u = \"\"\n    for i in range(n):\n        s = \"item-%d\" % i\n        u = s[2:6]\n    return u\n",
        &Cfg::default(),
    )
    .expect("str mod + slice should analyze");
    assert!(has_op(&tf, |op| matches!(op, TOp::StrMod)));
    assert!(has_op(&tf, |op| matches!(op, TOp::StrSlice { .. })));
}

#[test]
fn open_ended_str_slice_analyzes() {
    // `s[:4]` / `s[5:]` — the BUILD_SLICE `None` bounds erase.
    analyze_cfg(
        "def k(n):\n    t = 0\n    for i in range(n):\n        s = \"item-%d\" % i\n        u = s[:4]\n        w = s[5:]\n        t = t + 1\n    return u + w\n",
        &Cfg::default(),
    )
    .expect("open-ended str slices should analyze");
}

#[test]
fn slice_fallback_preserves_two_bound_producer() {
    use weavepy_compiler::OpCode;
    for lane in [JitType::ListInt, JitType::Str] {
        let mut code = compile_first_fn("def k(items, stop):\n    return items[:stop:None]\n");
        let pc = code
            .instructions
            .iter()
            .position(|ins| ins.op == OpCode::BuildSlice)
            .unwrap();
        assert_eq!(code.instructions[pc].arg, 3);
        assert_eq!(code.instructions[pc - 1].op, OpCode::LoadConst);
        code.instructions[pc - 1].op = OpCode::Nop;
        code.instructions[pc - 1].arg = 0;
        code.instructions[pc].arg = 2;
        let tf = analyze_code_cfg(
            &code,
            &Cfg {
                list: (lane == JitType::ListInt).then_some((0, JitType::Int)),
                typed_params: vec![(0, lane), (1, JitType::Int)],
                ..Cfg::default()
            },
        )
        .expect("two-bound slice should analyze");
        assert!(tf.blocks.iter().flat_map(|block| &block.stmts).any(|stmt| {
            stmt.pc == pc as u32
                && matches!(
                    stmt.op,
                    TOp::ListSlice {
                        origin: SliceOrigin::TwoBounds,
                        ..
                    } | TOp::StrSlice {
                        origin: SliceOrigin::TwoBounds,
                        ..
                    }
                )
        }));
    }
}

#[test]
fn slice_fallback_preserves_explicit_none_step() {
    for lane in [JitType::ListInt, JitType::Str] {
        let tf = analyze_cfg(
            "def k(items, stop):\n    return items[:stop:None]\n",
            &Cfg {
                list: (lane == JitType::ListInt).then_some((0, JitType::Int)),
                typed_params: vec![(0, lane), (1, JitType::Int)],
                ..Cfg::default()
            },
        )
        .expect("three-bound slice should analyze");
        assert!(has_op(&tf, |op| matches!(
            op,
            TOp::ListSlice {
                origin: SliceOrigin::ThreeBounds,
                ..
            } | TOp::StrSlice {
                origin: SliceOrigin::ThreeBounds,
                ..
            }
        )));
    }
}

#[test]
fn folded_slice_fallback_resumes_the_constant_load() {
    for lane in [JitType::ListInt, JitType::Str] {
        let tf = analyze_cfg(
            "def k(items):\n    return items[1:4]\n",
            &Cfg {
                list: (lane == JitType::ListInt).then_some((0, JitType::Int)),
                typed_params: vec![(0, lane)],
                ..Cfg::default()
            },
        )
        .expect("folded slice should analyze");
        assert!(has_op(&tf, |op| matches!(
            op,
            TOp::ListSlice {
                origin: SliceOrigin::Constant,
                ..
            } | TOp::StrSlice {
                origin: SliceOrigin::Constant,
                ..
            }
        )));
    }
}
