//! RFC 0058 WS4 — analyzer tests over real compiled Python shapes:
//! `range` loop recognition, guarded `LOAD_GLOBAL` burn-in, and mixed
//! int/float lanes. These stop at [`analyze`] (no VM), so they check
//! the *decisions*; execution is covered by weavepy-vm's `jit_*` tests.

use weavepy_compiler::{compile_module, CodeObject, Constant};
use weavepy_jit::{analyze, JitVerdict, ResolvedGlobal, TFunc, TTerm};
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

/// Resolver where `range` is the canonical builtin and everything else
/// is opaque.
fn range_only(name: &str) -> ResolvedGlobal {
    if name == "range" {
        ResolvedGlobal::RangeBuiltin
    } else {
        ResolvedGlobal::Opaque
    }
}

fn analyze_fn(
    src: &str,
    resolve: &mut dyn FnMut(&str) -> ResolvedGlobal,
) -> Result<TFunc, JitVerdict> {
    let code = compile_first_fn(src);
    analyze(&code, resolve)
}

#[test]
fn simple_range_loop_analyzes() {
    let tfunc = analyze_fn(
        "def kernel(n):\n    total = 0\n    for i in range(n):\n        total = total + i * 2\n    return total\n",
        &mut range_only,
    )
    .expect("range loop should be jitable");
    // Two synthetic slots appended after the three real locals.
    assert_eq!(tfunc.n_locals, 5);
    assert_eq!(tfunc.range_loops.len(), 1);
    assert_eq!(tfunc.global_guards.len(), 1);
    assert_eq!(tfunc.global_guards[0].name, "range");
    assert!(tfunc
        .blocks
        .iter()
        .any(|b| matches!(b.term, TTerm::ForRange { .. })));
}

#[test]
fn nested_range_loops_get_distinct_synthetics() {
    let tfunc = analyze_fn(
        "def kernel(n):\n    t = 0\n    for i in range(n):\n        for j in range(n):\n            t = t + i + j\n    return t\n",
        &mut range_only,
    )
    .expect("nested range loops should be jitable");
    assert_eq!(tfunc.range_loops.len(), 2);
    let a = tfunc.range_loops[0];
    let b = tfunc.range_loops[1];
    assert_ne!(a.cur_slot, b.cur_slot);
    // Outermost first: the outer loop's live span encloses the inner's.
    assert!(a.live_from < b.live_from && b.live_to < a.live_to);
}

#[test]
fn two_arg_range_and_unit_step_analyze() {
    for src in [
        "def k(a, b):\n    s = 0\n    for i in range(a, b):\n        s = s + i\n    return s\n",
        "def k(a, b):\n    s = 0\n    for i in range(a, b, 1):\n        s = s + i\n    return s\n",
    ] {
        analyze_fn(src, &mut range_only).expect("unit-step range should be jitable");
    }
}

#[test]
fn non_unit_step_stays_interpreted() {
    let err = analyze_fn(
        "def k(n):\n    s = 0\n    for i in range(0, n, 2):\n        s = s + i\n    return s\n",
        &mut range_only,
    )
    .unwrap_err();
    assert!(matches!(err, JitVerdict::UnsupportedOpcode(_)), "{err:?}");
}

#[test]
fn shadowed_range_stays_interpreted() {
    let err = analyze_fn(
        "def k(n):\n    s = 0\n    for i in range(n):\n        s = s + i\n    return s\n",
        &mut |_| ResolvedGlobal::Opaque,
    )
    .unwrap_err();
    assert!(matches!(err, JitVerdict::UnsupportedOpcode(_)), "{err:?}");
}

#[test]
fn non_range_iterable_stays_interpreted() {
    // RFC 0071 WS4 — with no list- or param-probe evidence the
    // analyzer asks for the seeded retry (`TypeUnknown`) instead of
    // rejecting outright: a list or identity-iterable argument would
    // take the `ForList`/`ForIter` path there. Without evidence the
    // frame still stays interpreted.
    let err = analyze_fn(
        "def k(xs):\n    s = 0\n    for x in xs:\n        s = s + x\n    return s\n",
        &mut range_only,
    )
    .unwrap_err();
    assert!(matches!(err, JitVerdict::TypeUnknown), "{err:?}");
}

#[test]
fn break_in_range_loop_analyzes() {
    analyze_fn(
        "def k(n):\n    s = 0\n    for i in range(n):\n        if i > 90:\n            break\n        s = s + i\n    return s\n",
        &mut range_only,
    )
    .expect("break in a range loop should be jitable");
}

#[test]
fn const_global_burns_in_with_guard() {
    let tfunc = analyze_fn(
        "def k(n):\n    s = 0\n    for i in range(n):\n        s = s + N\n    return s\n",
        &mut |name| match name {
            "range" => ResolvedGlobal::RangeBuiltin,
            "N" => ResolvedGlobal::ConstInt(5),
            _ => ResolvedGlobal::Opaque,
        },
    )
    .expect("const global should burn in");
    let names: Vec<&str> = tfunc
        .global_guards
        .iter()
        .map(|g| g.name.as_str())
        .collect();
    assert!(
        names.contains(&"range") && names.contains(&"N"),
        "{names:?}"
    );
}

#[test]
fn opaque_global_disqualifies() {
    let err = analyze_fn("def k(n):\n    return n + M\n", &mut |_| {
        ResolvedGlobal::Opaque
    })
    .unwrap_err();
    assert!(matches!(err, JitVerdict::UnsupportedOpcode(_)), "{err:?}");
}

#[test]
fn mixed_int_float_arith_analyzes() {
    analyze_fn(
        "def k(n):\n    s = 0.0\n    i = 0\n    while i < n:\n        s = s + i\n        i = i + 1\n    return s\n",
        &mut range_only,
    )
    .expect("mixed int/float arithmetic should be jitable");
}

#[test]
fn params_are_entry_guarded() {
    // Regression: parameters flow in from the caller and must be listed
    // in `livein_locals` so the VM type-guards them (a float argument to
    // an int-typed kernel must skip native entry, not compute garbage).
    let tfunc = analyze_fn(
        "def kernel(n):\n    s = 0\n    i = 0\n    while i < n:\n        s = s + i\n        i = i + 1\n    return s\n",
        &mut range_only,
    )
    .expect("int kernel");
    assert!(
        tfunc.livein_locals.contains(&0),
        "param slot must be entry-guarded: {:?}",
        tfunc.livein_locals
    );
}
