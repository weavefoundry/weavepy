//! The `math` built-in module.
//!
//! Tracks CPython 3.13's `math` module: real-valued functions over
//! `float`/`int`, plus the standard constants. Every function takes
//! and returns plain Python numbers — no `Decimal` or `Fraction`
//! interop yet.
//!
//! Functions follow CPython's domain-error conventions: arguments
//! outside the function's domain raise `ValueError`; numeric
//! overflow raises `OverflowError`. `nan`/`inf` propagate.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{overflow_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("math"),
        );
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static(""),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("This module provides access to the mathematical functions."),
        );

        // Constants.
        d.insert(
            DictKey(Object::from_static("pi")),
            Object::Float(std::f64::consts::PI),
        );
        d.insert(
            DictKey(Object::from_static("e")),
            Object::Float(std::f64::consts::E),
        );
        d.insert(
            DictKey(Object::from_static("tau")),
            Object::Float(std::f64::consts::TAU),
        );
        d.insert(
            DictKey(Object::from_static("inf")),
            Object::Float(f64::INFINITY),
        );
        d.insert(DictKey(Object::from_static("nan")), Object::Float(f64::NAN));

        // Total f64 → f64 wrappers, where every input is in-domain.
        // Partial functions like sqrt/log live below as explicit fns
        // so they can raise ValueError on out-of-domain inputs.
        d.insert(
            DictKey(Object::from_static("sqrt")),
            builtin("sqrt", math_sqrt),
        );
        for (name, f) in total_f64() {
            d.insert(DictKey(Object::from_static(name)), builtin(name, *f));
        }
        // Unary functions that follow CPython's `math_1` error discipline
        // (NaN-from-non-NaN → ValueError "math domain error"; Inf-from-finite
        // → OverflowError when `can_overflow`, else ValueError). Registered
        // through a shared trampoline so each gets the identical edge handling
        // CPython's C `math_1`/`math_1a` wrappers provide.
        for &(name, f, can_overflow) in checked_f64() {
            d.insert(
                DictKey(Object::from_static(name)),
                make_checked1(name, f, can_overflow),
            );
        }

        d.insert(
            DictKey(Object::from_static("asin")),
            builtin("asin", math_asin),
        );
        d.insert(
            DictKey(Object::from_static("acos")),
            builtin("acos", math_acos),
        );
        d.insert(
            DictKey(Object::from_static("atan")),
            builtin("atan", math_atan),
        );
        // RFC 0041 — CPython 3.13 `math.fma`: a correctly-rounded fused
        // multiply-add with IEEE-754 invalid/overflow signalling.
        d.insert(
            DictKey(Object::from_static("fma")),
            builtin("fma", math_fma),
        );
        d.insert(
            DictKey(Object::from_static("atan2")),
            builtin("atan2", math_atan2),
        );
        d.insert(
            DictKey(Object::from_static("log")),
            builtin("log", math_log),
        );
        d.insert(
            DictKey(Object::from_static("log2")),
            builtin("log2", math_log2),
        );
        d.insert(
            DictKey(Object::from_static("log10")),
            builtin("log10", math_log10),
        );
        d.insert(
            DictKey(Object::from_static("pow")),
            builtin("pow", math_pow),
        );
        d.insert(
            DictKey(Object::from_static("floor")),
            builtin("floor", math_floor),
        );
        d.insert(
            DictKey(Object::from_static("ceil")),
            builtin("ceil", math_ceil),
        );
        d.insert(
            DictKey(Object::from_static("trunc")),
            builtin("trunc", math_trunc),
        );
        d.insert(
            DictKey(Object::from_static("isnan")),
            builtin("isnan", math_isnan),
        );
        d.insert(
            DictKey(Object::from_static("isinf")),
            builtin("isinf", math_isinf),
        );
        d.insert(
            DictKey(Object::from_static("isfinite")),
            builtin("isfinite", math_isfinite),
        );
        d.insert(
            DictKey(Object::from_static("copysign")),
            builtin("copysign", math_copysign),
        );
        d.insert(
            DictKey(Object::from_static("fmod")),
            builtin("fmod", math_fmod),
        );
        d.insert(
            DictKey(Object::from_static("gcd")),
            builtin("gcd", math_gcd),
        );
        d.insert(
            DictKey(Object::from_static("lcm")),
            builtin("lcm", math_lcm),
        );
        d.insert(
            DictKey(Object::from_static("factorial")),
            builtin("factorial", math_factorial),
        );
        d.insert(
            DictKey(Object::from_static("isclose")),
            Object::Builtin(Rc::new(BuiltinFn::with_kwargs("isclose", math_isclose))),
        );
        // Missing CPython math symbols added in RFC 0030 to widen
        // drop-in compatibility for numpy/scipy-style consumers.
        d.insert(
            DictKey(Object::from_static("fsum")),
            builtin("fsum", math_fsum),
        );
        d.insert(
            DictKey(Object::from_static("prod")),
            Object::Builtin(Rc::new(BuiltinFn::with_kwargs("prod", math_prod))),
        );
        d.insert(
            DictKey(Object::from_static("sumprod")),
            builtin("sumprod", math_sumprod),
        );
        d.insert(
            DictKey(Object::from_static("hypot")),
            builtin("hypot", math_hypot),
        );
        d.insert(
            DictKey(Object::from_static("dist")),
            builtin("dist", math_dist),
        );
        d.insert(
            DictKey(Object::from_static("expm1")),
            builtin("expm1", math_expm1),
        );
        d.insert(
            DictKey(Object::from_static("log1p")),
            builtin("log1p", math_log1p),
        );
        d.insert(
            DictKey(Object::from_static("ldexp")),
            builtin("ldexp", math_ldexp),
        );
        d.insert(
            DictKey(Object::from_static("frexp")),
            builtin("frexp", math_frexp),
        );
        d.insert(
            DictKey(Object::from_static("modf")),
            builtin("modf", math_modf),
        );
        d.insert(
            DictKey(Object::from_static("comb")),
            builtin("comb", math_comb),
        );
        d.insert(
            DictKey(Object::from_static("perm")),
            builtin("perm", math_perm),
        );
        d.insert(
            DictKey(Object::from_static("remainder")),
            builtin("remainder", math_remainder),
        );
        d.insert(
            DictKey(Object::from_static("nextafter")),
            Object::Builtin(Rc::new(BuiltinFn::with_kwargs("nextafter", math_nextafter))),
        );
        d.insert(
            DictKey(Object::from_static("ulp")),
            builtin("ulp", math_ulp),
        );
        d.insert(
            DictKey(Object::from_static("erf")),
            builtin("erf", math_erf),
        );
        d.insert(
            DictKey(Object::from_static("erfc")),
            builtin("erfc", math_erfc),
        );
        d.insert(
            DictKey(Object::from_static("gamma")),
            builtin("gamma", math_gamma),
        );
        d.insert(
            DictKey(Object::from_static("lgamma")),
            builtin("lgamma", math_lgamma),
        );
        d.insert(
            DictKey(Object::from_static("isqrt")),
            builtin("isqrt", math_isqrt),
        );
        d.insert(
            DictKey(Object::from_static("cbrt")),
            builtin("cbrt", math_cbrt),
        );
        d.insert(
            DictKey(Object::from_static("exp2")),
            builtin("exp2", math_exp2),
        );
        d.insert(
            DictKey(Object::from_static("atanh")),
            builtin("atanh", math_atanh),
        );
        d.insert(
            DictKey(Object::from_static("asinh")),
            builtin("asinh", math_asinh),
        );
        d.insert(
            DictKey(Object::from_static("acosh")),
            builtin("acosh", math_acosh),
        );
    }
    Rc::new(PyModule {
        name: "math".to_owned(),
        filename: None,
        dict,
    })
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Total real-valued unary functions over `f64`: every input is
/// in-domain and no finite input overflows, so they need no error
/// post-check. `tanh` is bounded; `fabs`/`radians`/`degrees` are plain
/// arithmetic (CPython's `math.degrees(1e308)` returns `inf` rather than
/// raising, so they must *not* go through the `math_1` overflow check).
fn total_f64() -> &'static [(&'static str, fn(&[Object]) -> Result<Object, RuntimeError>)] {
    &[
        ("tanh", |a| Ok(Object::Float(to_f64(a, "tanh", 0)?.tanh()))),
        ("fabs", |a| Ok(Object::Float(to_f64(a, "fabs", 0)?.abs()))),
        ("radians", |a| {
            Ok(Object::Float(to_f64(a, "radians", 0)?.to_radians()))
        }),
        ("degrees", |a| {
            Ok(Object::Float(to_f64(a, "degrees", 0)?.to_degrees()))
        }),
    ]
}

/// Unary functions that follow CPython's `math_1` domain/overflow
/// discipline. The third field is `can_overflow`: when an infinite result
/// arises from a finite argument, `true` raises `OverflowError`
/// (range error), `false` raises `ValueError` (domain error / pole).
fn checked_f64() -> &'static [(&'static str, fn(f64) -> f64, bool)] {
    &[
        ("sin", f64::sin, false),
        ("cos", f64::cos, false),
        ("tan", f64::tan, false),
        ("sinh", f64::sinh, true),
        ("cosh", f64::cosh, true),
        ("exp", f64::exp, true),
    ]
}

/// CPython's `math_1` result discipline applied to a computed value.
/// Mirrors `mathmodule.c`'s `is_error`: a NaN result from a non-NaN input
/// is an invalid-operation domain error; an infinite result from a finite
/// input is either overflow (range) or a pole (domain), per `can_overflow`.
fn finish1(x: f64, r: f64, can_overflow: bool) -> Result<Object, RuntimeError> {
    if r.is_nan() && !x.is_nan() {
        return Err(value_error("math domain error"));
    }
    if r.is_infinite() && x.is_finite() {
        return Err(if can_overflow {
            overflow_error("math range error")
        } else {
            value_error("math domain error")
        });
    }
    Ok(Object::Float(r))
}

fn make_checked1(name: &'static str, f: fn(f64) -> f64, can_overflow: bool) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |args: &[Object]| {
            let x = to_f64(args, name, 0)?;
            finish1(x, f(x), can_overflow)
        }),
        call_kw: None,
    }))
}

/// `math.fma(x, y, z)` — CPython 3.13's correctly-rounded fused
/// multiply-add. Rust's `f64::mul_add` lowers to the hardware FMA (a
/// single rounding). The non-finite handling matches `math_fma_impl`:
/// a NaN result from finite/inf inputs is the IEEE invalid operation
/// (`inf*0`, `inf - inf`) → `ValueError`; an infinite result from finite
/// inputs is overflow → `OverflowError`; non-finite results that come
/// from a non-finite input propagate unchanged.
fn math_fma(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "fma", 0)?;
    let y = to_f64(args, "fma", 1)?;
    let z = to_f64(args, "fma", 2)?;
    let r = x.mul_add(y, z);
    if r.is_finite() {
        return Ok(Object::Float(r));
    }
    if r.is_nan() {
        if x.is_nan() || y.is_nan() || z.is_nan() {
            return Ok(Object::Float(r));
        }
        return Err(value_error("invalid operation in fma"));
    }
    // r is an infinity.
    if x.is_infinite() || y.is_infinite() || z.is_infinite() {
        return Ok(Object::Float(r));
    }
    Err(overflow_error("overflow in fma"))
}

fn to_f64(args: &[Object], func: &str, idx: usize) -> Result<f64, RuntimeError> {
    match args.get(idx) {
        Some(other) => match crate::builtins::coerce_f64_opt(other)? {
            Some(f) => Ok(f),
            None => Err(type_error(format!(
                "{func}() argument must be int or float, not '{}'",
                other.type_name()
            ))),
        },
        None => Err(type_error(format!(
            "{func}() takes at least {} argument(s)",
            idx + 1
        ))),
    }
}

/// CPython's `PyNumber_Index` over a single object: accept `bool`/`int`/big
/// `int` and integer-backed subclasses directly, and any object exposing
/// `__index__` via interpreter reentry. Floats, strings, `Decimal`, etc.
/// raise the CPython "object cannot be interpreted as an integer" TypeError
/// (note: `__int__`/`__trunc__` are deliberately *not* honoured, matching
/// `comb`/`perm`/`gcd`/`factorial`).
fn index_bigint(o: &Object) -> Result<num_bigint::BigInt, RuntimeError> {
    if let Some(bi) = o.as_bigint() {
        return Ok(bi);
    }
    if let Some(native) = o.native_value() {
        if let Some(bi) = native.as_bigint() {
            return Ok(bi);
        }
    }
    if let Object::Instance(_) = o {
        if let Some(method) = crate::instance_method(o, "__index__") {
            if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
                // SAFETY: the pointer was published by an enclosing VM frame
                // still live on this thread; the GIL keeps the access exclusive.
                let interp = unsafe { &mut *ptr };
                let globals = interp.builtins_dict();
                let r = interp.call_object_with_globals(&method, &[], &[], &globals)?;
                if let Some(bi) = r.as_bigint() {
                    return Ok(bi);
                }
                return Err(type_error(format!(
                    "__index__ returned non-int (type {})",
                    r.type_name()
                )));
            }
        }
    }
    Err(type_error(format!(
        "'{}' object cannot be interpreted as an integer",
        o.type_name()
    )))
}

/// Enforce an exact positional arity, raising CPython's
/// "takes exactly N argument(s) (M given)" TypeError otherwise. Used by the
/// fixed-arity functions (`atan2`, `ceil`, `trunc`, …) that CPython compiles
/// with Argument Clinic signatures.
fn expect_nargs(args: &[Object], name: &str, n: usize) -> Result<(), RuntimeError> {
    if args.len() != n {
        let unit = if n == 1 { "argument" } else { "arguments" };
        return Err(type_error(format!(
            "{name}() takes exactly {n} {unit} ({} given)",
            args.len()
        )));
    }
    Ok(())
}

fn math_sqrt(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "sqrt", 0)?;
    if x < 0.0 {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(x.sqrt()))
}

fn math_asin(args: &[Object]) -> Result<Object, RuntimeError> {
    // `asin(nan)` is `nan` (not a domain error); `asin(|x|>1)` and
    // `asin(±inf)` produce a NaN from a non-NaN input, which `finish1`
    // turns into the CPython "math domain error".
    let x = to_f64(args, "asin", 0)?;
    finish1(x, x.asin(), false)
}

fn math_acos(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "acos", 0)?;
    finish1(x, x.acos(), false)
}

fn math_atan(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(to_f64(args, "atan", 0)?.atan()))
}

fn math_atan2(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "atan2", 2)?;
    let y = to_f64(args, "atan2", 0)?;
    let x = to_f64(args, "atan2", 1)?;
    Ok(Object::Float(y.atan2(x)))
}

/// `frexp` for a positive big integer: returns `(m, e)` with
/// `n ≈ m · 2**e` and `0.5 <= m < 1`. The top 53 bits become the mantissa,
/// so `loghelper` can take logs of integers far outside `f64`'s range
/// (`math.log(10**1000)`, `math.log2(2**2000)`), matching CPython's
/// `_PyLong_Frexp`-based `loghelper`.
fn bigint_frexp(n: &num_bigint::BigInt) -> (f64, i64) {
    let bits = n.bits() as i64; // position of the most-significant set bit + 1
    let shift = bits - 53;
    let mantissa = if shift > 0 {
        n >> (shift as u64)
    } else {
        n << ((-shift) as u64)
    };
    use num_traits::ToPrimitive;
    // `mantissa` has at most 53 bits, so it fits an i64/f64 exactly.
    let m = mantissa.to_i64().unwrap_or(0) as f64;
    (m / 9_007_199_254_740_992.0_f64, bits) // 2**53
}

/// CPython's `loghelper`: take `func`'s log of `o`. Integers are handled
/// exactly via `frexp` (so huge ints don't overflow to `inf` first);
/// other reals go through the libm `func` with CPython's domain checks
/// (`log(0)`/`log(-x)`/`log(-inf)` → `ValueError`, `log(nan)` → `nan`).
fn loghelper(o: &Object, func: fn(f64) -> f64, name: &str) -> Result<f64, RuntimeError> {
    let as_int = o
        .as_bigint()
        .or_else(|| o.native_value().and_then(|n| n.as_bigint()));
    if let Some(n) = as_int {
        use num_traits::Signed;
        if !n.is_positive() {
            return Err(value_error("math domain error"));
        }
        let (m, e) = bigint_frexp(&n);
        return Ok(func(m) + func(2.0) * (e as f64));
    }
    let x = object_to_f64(o, name)?;
    if x.is_nan() {
        return Ok(x);
    }
    if x > 0.0 {
        return Ok(func(x)); // includes log(+inf) = +inf
    }
    // x == 0 (divide-by-zero), x < 0, or -inf — all domain errors.
    Err(value_error("math domain error"))
}

fn math_log(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() || args.len() > 2 {
        return Err(type_error(format!(
            "log expected 1 to 2 arguments, got {}",
            args.len()
        )));
    }
    let num = loghelper(&args[0], f64::ln, "log")?;
    match args.get(1) {
        Some(base) => {
            let den = loghelper(base, f64::ln, "log")?;
            Ok(Object::Float(num / den))
        }
        None => Ok(Object::Float(num)),
    }
}

fn math_log2(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "log2", 1)?;
    Ok(Object::Float(loghelper(&args[0], f64::log2, "log2")?))
}

fn math_log10(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "log10", 1)?;
    Ok(Object::Float(loghelper(&args[0], f64::log10, "log10")?))
}

/// `math.pow(x, y)` — faithful port of CPython's `math_pow_impl`, including
/// the IEEE-754 special cases and the overflow/domain `errno` discipline.
fn math_pow(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "pow", 2)?;
    let x = to_f64(args, "pow", 0)?;
    let y = to_f64(args, "pow", 1)?;
    let r;
    if !x.is_finite() || !y.is_finite() {
        if x.is_nan() {
            r = if y == 0.0 { 1.0 } else { x }; // NaN**0 = 1
        } else if y.is_nan() {
            r = if x == 1.0 { 1.0 } else { y }; // 1**NaN = 1
        } else if x.is_infinite() {
            let odd_y = y.is_finite() && (y.abs() % 2.0) == 1.0;
            r = if y > 0.0 {
                if odd_y {
                    x
                } else {
                    x.abs()
                }
            } else if y == 0.0 {
                1.0
            } else if odd_y {
                0.0_f64.copysign(x)
            } else {
                0.0
            };
        } else {
            // y is infinite.
            if x.abs() == 1.0 {
                r = 1.0;
            } else if y > 0.0 && x.abs() > 1.0 {
                r = y;
            } else if y < 0.0 && x.abs() < 1.0 {
                r = -y; // +inf
            } else {
                r = 0.0;
            }
        }
        return Ok(Object::Float(r));
    }
    // finite ** finite — let libm handle it, then classify a non-finite result.
    r = x.powf(y);
    if !r.is_finite() {
        if r.is_nan() {
            // (-ve) ** (finite non-integer) → invalid → ValueError.
            return Err(value_error("math domain error"));
        }
        if r.is_infinite() {
            if x == 0.0 {
                // (±0.) ** negative → divide-by-zero → ValueError.
                return Err(value_error("math domain error"));
            }
            return Err(overflow_error("math range error"));
        }
    }
    Ok(Object::Float(r))
}

/// Convert an (already integral) `f64` to a Python int, promoting to a
/// big integer when the value exceeds the 64-bit range so we never wrap.
fn float_to_int_obj(f: f64) -> Result<Object, RuntimeError> {
    use num_traits::FromPrimitive;
    if !f.is_finite() {
        return Err(value_error("cannot convert float infinity to integer"));
    }
    if (i64::MIN as f64..=i64::MAX as f64).contains(&f) {
        Ok(Object::Int(f as i64))
    } else {
        let big = num_bigint::BigInt::from_f64(f)
            .ok_or_else(|| value_error("cannot convert float to integer"))?;
        Ok(Object::int_from_bigint(big))
    }
}

/// Call a special method looked up on the *type* (CPython's
/// `_PyObject_LookupSpecial`) with no arguments, via interpreter reentry.
fn call_special_no_args(method: &Object) -> Result<Object, RuntimeError> {
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("requires an active interpreter"))?;
    // SAFETY: the pointer was published by an enclosing VM call frame still
    // live on this thread's stack; the GIL makes the mutable access exclusive.
    let interp = unsafe { &mut *ptr };
    let globals = interp.builtins_dict();
    interp.call_object_with_globals(method, &[], &[], &globals)
}

/// Shared core for `math.floor`/`ceil`/`trunc`. CPython dispatches the
/// matching dunder (`type(x).__floor__(x)`, …) for non-float arguments,
/// which is how `fractions.Fraction`, `decimal.Decimal`, and user numeric
/// types participate. `floor`/`ceil` additionally fall back to the
/// `__float__`/`__index__` protocol (so `FloatLike` works); `trunc` does
/// not (a missing `__trunc__` is a TypeError).
fn floor_ceil_trunc(
    args: &[Object],
    func: &str,
    dunder: &str,
    op: fn(f64) -> f64,
    float_fallback: bool,
) -> Result<Object, RuntimeError> {
    expect_nargs(args, func, 1)?;
    match &args[0] {
        Object::Int(i) => Ok(Object::Int(*i)),
        Object::Bool(b) => Ok(Object::Int(i64::from(*b))),
        Object::Long(b) => Ok(Object::Long(b.clone())),
        Object::Float(f) => float_to_int_obj(op(*f)),
        obj => {
            // Look up the dunder on the type (instance attrs are ignored,
            // matching `_PyObject_LookupSpecial`); a descriptor's `__get__`
            // runs here, so `BadDescr` correctly propagates its ValueError.
            if let Some(method) = crate::instance_method(obj, dunder) {
                return call_special_no_args(&method);
            }
            if float_fallback {
                if let Some(x) = crate::builtins::coerce_f64_opt(obj)? {
                    return float_to_int_obj(op(x));
                }
                return Err(type_error(format!(
                    "must be real number, not {}",
                    obj.type_name()
                )));
            }
            Err(type_error(format!(
                "type {} doesn't define {dunder} method",
                obj.type_name(),
            )))
        }
    }
}

fn math_floor(args: &[Object]) -> Result<Object, RuntimeError> {
    floor_ceil_trunc(args, "floor", "__floor__", f64::floor, true)
}

fn math_ceil(args: &[Object]) -> Result<Object, RuntimeError> {
    floor_ceil_trunc(args, "ceil", "__ceil__", f64::ceil, true)
}

fn math_trunc(args: &[Object]) -> Result<Object, RuntimeError> {
    floor_ceil_trunc(args, "trunc", "__trunc__", f64::trunc, false)
}

fn math_isnan(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(to_f64(args, "isnan", 0)?.is_nan()))
}

fn math_isinf(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(to_f64(args, "isinf", 0)?.is_infinite()))
}

fn math_isfinite(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Bool(to_f64(args, "isfinite", 0)?.is_finite()))
}

fn math_copysign(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "copysign", 0)?;
    let y = to_f64(args, "copysign", 1)?;
    Ok(Object::Float(x.copysign(y)))
}

fn math_fmod(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "fmod", 2)?;
    let x = to_f64(args, "fmod", 0)?;
    let y = to_f64(args, "fmod", 1)?;
    // fmod(x, ±inf) == x for finite x.
    if y.is_infinite() && x.is_finite() {
        return Ok(Object::Float(x));
    }
    let r = libm_fmod(x, y);
    // A NaN result from non-NaN operands is the IEEE invalid operation
    // (e.g. fmod(inf, 1) or fmod(x, 0)) → ValueError.
    if r.is_nan() && !x.is_nan() && !y.is_nan() {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(r))
}

/// C `fmod`: the IEEE-754 remainder with the sign of `x`. Rust's `%`
/// matches C `fmod` for finite operands, but we keep the special-case
/// handling explicit so `fmod(inf, y)`/`fmod(x, 0)` yield NaN (which the
/// caller turns into a domain error) rather than relying on `%`.
fn libm_fmod(x: f64, y: f64) -> f64 {
    if x.is_infinite() || y == 0.0 {
        return f64::NAN;
    }
    if y.is_infinite() {
        return x;
    }
    x % y
}

fn math_gcd(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_integer::Integer;
    let mut acc = num_bigint::BigInt::from(0);
    for a in args {
        let v = index_bigint(a)?;
        acc = acc.gcd(&v); // num_integer::gcd is always non-negative
    }
    Ok(Object::int_from_bigint(acc))
}

fn math_lcm(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_integer::Integer;
    use num_traits::{Signed, Zero};
    if args.is_empty() {
        return Ok(Object::Int(1));
    }
    let mut acc = index_bigint(&args[0])?.abs();
    for a in &args[1..] {
        // Every argument is coerced (so a bad type raises TypeError) even
        // once the running lcm has hit zero, matching CPython's loop.
        let v = index_bigint(a)?;
        if acc.is_zero() {
            continue;
        }
        if v.is_zero() {
            acc = num_bigint::BigInt::from(0);
            continue;
        }
        acc = acc.lcm(&v);
    }
    Ok(Object::int_from_bigint(acc))
}

/// `n!` for an `__index__`-coercible `o`, with CPython's error discipline:
/// a value past `i64::MAX` overflows (`OverflowError`), a negative value is
/// undefined (`ValueError`), and non-integers raise `TypeError` (via
/// `index_bigint`). Shared by `factorial` and the one-argument `perm`.
fn factorial_impl(o: &Object) -> Result<Object, RuntimeError> {
    use num_traits::{Signed, ToPrimitive};
    let n = index_bigint(o)?;
    let n64 = match n.to_i64() {
        Some(v) if !n.is_negative() => v,
        _ => {
            if n.is_positive() {
                return Err(overflow_error(
                    "factorial() argument should not exceed 9223372036854775807",
                ));
            }
            return Err(value_error("factorial() not defined for negative values"));
        }
    };
    let mut acc = num_bigint::BigInt::from(1);
    for i in 2..=n64 {
        acc *= i;
    }
    Ok(Object::int_from_bigint(acc))
}

fn math_factorial(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "factorial", 1)?;
    factorial_impl(&args[0])
}

/// `math.isclose(a, b, *, rel_tol=1e-09, abs_tol=0.0)` implementing
/// PEP 485. `a`/`b` are positional-or-keyword; `rel_tol`/`abs_tol` are
/// keyword-only. Mirrors CPython's `math_isclose_impl`: the asymmetric
/// `(diff <= |rel_tol*b|) or (diff <= |rel_tol*a|)` test (not a `max`),
/// the bit-exact `a == b` fast path (so `isclose(inf, inf)` is `True`),
/// and the `tolerances must be non-negative` ValueError.
fn math_isclose(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    fn coerce(obj: &Object, what: &str) -> Result<f64, RuntimeError> {
        match crate::builtins::coerce_f64_opt(obj)? {
            Some(f) => Ok(f),
            None => Err(type_error(format!(
                "isclose() {what} must be a real number, not '{}'",
                obj.type_name()
            ))),
        }
    }

    if args.len() > 2 {
        return Err(type_error(format!(
            "isclose() takes at most 2 positional arguments ({} given)",
            args.len()
        )));
    }
    let mut a = args.first().cloned();
    let mut b = args.get(1).cloned();
    let mut rel_tol = 1e-9_f64;
    let mut abs_tol = 0.0_f64;
    for (key, value) in kwargs {
        match key.as_str() {
            "a" => {
                if a.is_some() {
                    return Err(type_error("isclose() got multiple values for argument 'a'"));
                }
                a = Some(value.clone());
            }
            "b" => {
                if b.is_some() {
                    return Err(type_error("isclose() got multiple values for argument 'b'"));
                }
                b = Some(value.clone());
            }
            "rel_tol" => rel_tol = coerce(value, "rel_tol")?,
            "abs_tol" => abs_tol = coerce(value, "abs_tol")?,
            other => {
                return Err(type_error(format!(
                    "isclose() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let a = coerce(
        &a.ok_or_else(|| type_error("isclose() missing required argument 'a' (pos 1)"))?,
        "a",
    )?;
    let b = coerce(
        &b.ok_or_else(|| type_error("isclose() missing required argument 'b' (pos 2)"))?,
        "b",
    )?;
    if rel_tol < 0.0 || abs_tol < 0.0 {
        return Err(value_error("tolerances must be non-negative"));
    }
    #[allow(clippy::float_cmp)]
    if a == b {
        return Ok(Object::Bool(true));
    }
    if a.is_infinite() || b.is_infinite() {
        return Ok(Object::Bool(false));
    }
    let diff = (b - a).abs();
    let result = diff <= (rel_tol * b).abs() || diff <= (rel_tol * a).abs() || diff <= abs_tol;
    Ok(Object::Bool(result))
}

// ---------------------------------------------------------------------
// Math additions (RFC 0030)
// ---------------------------------------------------------------------

/// Collect numbers out of any iterable argument. Mirrors CPython's
/// ``_PyIter_GetIter`` flow: try to make an iterator and pull values
/// until exhaustion, coercing each to f64 along the way.
fn collect_numbers(arg: &Object, func: &str) -> Result<Vec<f64>, RuntimeError> {
    // Iterate through the interpreter so tuple subclasses, generators, and
    // user `__iter__` work (CPython's `dist`/`fsum` accept any iterable):
    // `dist(T((1,2,3)), ...)` for a `tuple` subclass `T` reaches here as an
    // `Object::Instance`, which the native `make_iter` cannot drive.
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error(format!("{func}() requires an active interpreter")))?;
    // SAFETY: published by an enclosing live VM frame; GIL-exclusive.
    let interp = unsafe { &mut *ptr };
    let iter = interp.iter_object(arg.clone()).map_err(|_| {
        type_error(format!(
            "{func}() argument must be iterable, not '{}'",
            arg.type_name()
        ))
    })?;
    let mut out = Vec::new();
    while let Some(item) = interp.iter_next_object(iter.clone())? {
        out.push(object_to_f64(&item, func)?);
    }
    Ok(out)
}

fn object_to_f64(value: &Object, func: &str) -> Result<f64, RuntimeError> {
    // Mirror CPython's `ASSIGN_DOUBLE`: accept floats/ints directly and any
    // object implementing `__float__`/`__index__` (so `fsum`/`hypot`/`dist`
    // take `FloatLike` and `Fraction`-style operands).
    match crate::builtins::coerce_f64_opt(value)? {
        Some(f) => Ok(f),
        None => Err(type_error(format!(
            "{func}() element must be a real number, not '{}'",
            value.type_name()
        ))),
    }
}

fn math_fsum(args: &[Object]) -> Result<Object, RuntimeError> {
    // Faithful port of CPython's `math_fsum` (Shewchuk / Hettinger msum):
    // maintain a set of non-overlapping partial sums so the final result is
    // correctly rounded, with explicit handling of intermediate overflow
    // (OverflowError), inf/nan summands, and a half-even fixup at the end.
    expect_nargs(args, "fsum", 1)?;
    let values = collect_numbers(&args[0], "fsum")?;
    let mut partials: Vec<f64> = Vec::new();
    let mut special_sum = 0.0_f64;
    let mut inf_sum = 0.0_f64;
    for xsave in values {
        let mut x = xsave;
        let mut i = 0usize;
        for j in 0..partials.len() {
            let mut y = partials[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let lo = y - (hi - x);
            if lo != 0.0 {
                partials[i] = lo;
                i += 1;
            }
            x = hi;
        }
        partials.truncate(i);
        if x != 0.0 {
            if !x.is_finite() {
                // A nonfinite running total comes either from intermediate
                // overflow (finite summand) or from an inf/nan summand.
                if xsave.is_finite() {
                    return Err(overflow_error("intermediate overflow in fsum"));
                }
                if xsave.is_infinite() {
                    inf_sum += xsave;
                }
                special_sum += xsave;
                partials.clear();
            } else {
                partials.push(x);
            }
        }
    }
    if special_sum != 0.0 {
        if inf_sum.is_nan() {
            return Err(value_error("-inf + inf in fsum"));
        }
        return Ok(Object::Float(special_sum));
    }
    // Sum the partials from the top, stopping when the sum becomes inexact.
    let mut hi = 0.0_f64;
    let mut n = partials.len();
    if n > 0 {
        n -= 1;
        hi = partials[n];
        let mut lo = 0.0_f64;
        while n > 0 {
            let x = hi;
            n -= 1;
            let y = partials[n];
            hi = x + y;
            let yr = hi - x;
            lo = y - yr;
            if lo != 0.0 {
                break;
            }
        }
        // Half-even rounding fixup across multiple partials, so fsum stays
        // commutative (e.g. sum([1e-16, 1, 1e16]) rounds correctly).
        if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
            let y = lo * 2.0;
            let x = hi + y;
            let yr = x - hi;
            #[allow(clippy::float_cmp)]
            if y == yr {
                hi = x;
            }
        }
    }
    Ok(Object::Float(hi))
}

/// `math.prod(iterable, /, *, start=1)`. Multiplies through the Python `*`
/// operator so the result type is preserved exactly (int→big int, `float`,
/// `Fraction`, `Decimal`, even `str`/`list` starts) and user `__mul__`
/// errors propagate, matching CPython's accumulator semantics.
fn math_prod(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    if args.len() != 1 {
        return Err(type_error(format!(
            "prod() takes exactly one positional argument ({} given)",
            args.len()
        )));
    }
    let mut start = Object::Int(1);
    for (key, value) in kwargs {
        match key.as_str() {
            "start" => start = value.clone(),
            other => {
                return Err(type_error(format!(
                    "prod() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("prod() requires an active interpreter"))?;
    // SAFETY: published by an enclosing live VM frame; GIL-exclusive.
    let interp = unsafe { &mut *ptr };
    let iter = interp
        .iter_object(args[0].clone())
        .map_err(|_| type_error(format!("'{}' object is not iterable", args[0].type_name())))?;
    let mut acc = start;
    while let Some(item) = interp.iter_next_object(iter.clone())? {
        acc = interp.op_binary(&acc, &item, weavepy_compiler::BinOpKind::Mult)?;
    }
    Ok(acc)
}

/// `math.sumprod(p, q)` — sum of the elementwise products of two iterables.
/// Faithful port of CPython's `math_sumprod_impl`: an exact big-integer
/// fast path for int/int pairs, a float fast path for float and float/int
/// pairs, and a general fallback using Python `*`/`+` (so `Fraction`,
/// `Decimal`, and user types are type-preserved). The three accumulators are
/// flushed into a single running `total` in sequence, reproducing CPython's
/// (deliberately lossy) ordering — e.g. `sumprod((-5,-5,10), (1.5, big, big))`
/// is `0.0`, not `-7.5`.
fn math_sumprod(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error(format!(
            "sumprod() takes exactly 2 arguments ({} given)",
            args.len()
        )));
    }
    fn is_exact_int(o: &Object) -> bool {
        matches!(o, Object::Int(_) | Object::Long(_))
    }
    fn is_int_like(o: &Object) -> bool {
        matches!(o, Object::Int(_) | Object::Long(_) | Object::Bool(_))
    }
    fn int_like_to_f64(o: &Object) -> Result<f64, RuntimeError> {
        match o {
            Object::Int(i) => Ok(*i as f64),
            Object::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Object::Long(b) => {
                use num_traits::ToPrimitive;
                match b.to_f64() {
                    Some(f) if f.is_finite() => Ok(f),
                    _ => Err(overflow_error("int too large to convert to float")),
                }
            }
            _ => unreachable!("int_like_to_f64 on non-int"),
        }
    }
    /// Float fast path: handle float/float and float/int(-like) pairs.
    fn try_float_pair(p: &Object, q: &Object) -> Result<Option<(f64, f64)>, RuntimeError> {
        let pf = matches!(p, Object::Float(_));
        let qf = matches!(q, Object::Float(_));
        let as_f = |o: &Object| match o {
            Object::Float(f) => *f,
            _ => unreachable!(),
        };
        if pf && qf {
            Ok(Some((as_f(p), as_f(q))))
        } else if pf && is_int_like(q) {
            Ok(Some((as_f(p), int_like_to_f64(q)?)))
        } else if is_int_like(p) && qf {
            Ok(Some((int_like_to_f64(p)?, as_f(q))))
        } else {
            Ok(None)
        }
    }

    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("sumprod() requires an active interpreter"))?;
    // SAFETY: published by an enclosing live VM frame; GIL-exclusive.
    let interp = unsafe { &mut *ptr };
    let p_iter = interp
        .iter_object(args[0].clone())
        .map_err(|_| type_error(format!("'{}' object is not iterable", args[0].type_name())))?;
    let q_iter = interp
        .iter_object(args[1].clone())
        .map_err(|_| type_error(format!("'{}' object is not iterable", args[1].type_name())))?;

    let mut total = Object::Int(0);
    let mut int_total = num_bigint::BigInt::from(0);
    let mut int_in_use = false;
    let mut int_enabled = true;
    let mut flt_total = 0.0_f64;
    let mut flt_in_use = false;
    let mut flt_enabled = true;

    loop {
        let p_i = interp.iter_next_object(p_iter.clone())?;
        let q_i = interp.iter_next_object(q_iter.clone())?;
        if p_i.is_none() != q_i.is_none() {
            return Err(value_error("Inputs are not the same length"));
        }
        let finished = p_i.is_none();

        if int_enabled {
            if !finished {
                let (p, q) = (p_i.as_ref().unwrap(), q_i.as_ref().unwrap());
                if is_exact_int(p) && is_exact_int(q) {
                    int_total += p.as_bigint().unwrap() * q.as_bigint().unwrap();
                    int_in_use = true;
                    continue;
                }
            }
            int_enabled = false;
            if int_in_use {
                let term = Object::int_from_bigint(std::mem::replace(
                    &mut int_total,
                    num_bigint::BigInt::from(0),
                ));
                total = interp.op_binary(&total, &term, weavepy_compiler::BinOpKind::Add)?;
                int_in_use = false;
            }
        }

        if flt_enabled {
            if !finished {
                let (p, q) = (p_i.as_ref().unwrap(), q_i.as_ref().unwrap());
                if let Some((fp, fq)) = try_float_pair(p, q)? {
                    flt_total += fp * fq;
                    flt_in_use = true;
                    continue;
                }
            }
            flt_enabled = false;
            if flt_in_use {
                let term = Object::Float(flt_total);
                flt_total = 0.0;
                total = interp.op_binary(&total, &term, weavepy_compiler::BinOpKind::Add)?;
                flt_in_use = false;
            }
        }

        if finished {
            break;
        }

        // General path: type-preserving Python `*` then `+`.
        let term = interp.op_binary(
            p_i.as_ref().unwrap(),
            q_i.as_ref().unwrap(),
            weavepy_compiler::BinOpKind::Mult,
        )?;
        total = interp.op_binary(&total, &term, weavepy_compiler::BinOpKind::Add)?;
    }
    Ok(total)
}

/// A double-double `(hi, lo)` value used by `vector_norm`'s compensated
/// summation, mirroring CPython's `DoubleLength`.
struct DoubleLength {
    hi: f64,
    lo: f64,
}

/// Algorithm 1.1: compensated sum of two floats with `|a| >= |b|`.
fn dl_fast_sum(a: f64, b: f64) -> DoubleLength {
    let x = a + b;
    let y = (a - x) + b;
    DoubleLength { hi: x, lo: y }
}

/// Algorithm 3.5: error-free product, using the hardware FMA (`mul_add`).
fn dl_mul(x: f64, y: f64) -> DoubleLength {
    let z = x * y;
    let zz = x.mul_add(y, -z);
    DoubleLength { hi: z, lo: zz }
}

/// `sqrt(sum(x**2 for x in vec))` to within ~1 ulp — faithful port of
/// CPython's `vector_norm`: power-of-two scaling, error-free squaring via
/// FMA, Neumaier compensated summation, and a differential `sqrt`
/// correction. `max` is the largest `|x|`; `found_nan` flags any NaN.
fn vector_norm(vec: &mut [f64], max: f64, found_nan: bool) -> f64 {
    let n = vec.len();
    if max.is_infinite() {
        return max;
    }
    if found_nan {
        return f64::NAN;
    }
    if max == 0.0 || n <= 1 {
        return max;
    }
    let (_, max_e) = frexp(max);
    if max_e < -1023 {
        // `ldexp(1.0, -max_e)` would overflow; rescale subnormals to normals.
        let dbl_min = f64::MIN_POSITIVE; // 2**-1022
        for v in vec.iter_mut() {
            *v /= dbl_min;
        }
        return dbl_min * vector_norm(vec, max / dbl_min, found_nan);
    }
    let scale = ldexp(1.0, -(max_e as i32));
    let mut csum = 1.0_f64;
    let mut frac1 = 0.0_f64;
    let mut frac2 = 0.0_f64;
    for &xi in vec.iter() {
        let x = xi * scale; // lossless
        let pr = dl_mul(x, x); // lossless squaring
        let sm = dl_fast_sum(csum, pr.hi); // lossless addition
        csum = sm.hi;
        frac1 += pr.lo; // lossy
        frac2 += sm.lo; // lossy
    }
    let mut h = (csum - 1.0 + (frac1 + frac2)).sqrt();
    let pr = dl_mul(-h, h);
    let sm = dl_fast_sum(csum, pr.hi);
    csum = sm.hi;
    frac1 += pr.lo;
    frac2 += sm.lo;
    let x = csum - 1.0 + (frac1 + frac2);
    h += x / (2.0 * h); // differential correction
    h / scale
}

fn math_hypot(args: &[Object]) -> Result<Object, RuntimeError> {
    let mut coords: Vec<f64> = Vec::with_capacity(args.len());
    let mut max = 0.0_f64;
    let mut found_nan = false;
    for a in args {
        let x = object_to_f64(a, "hypot")?.abs();
        if x.is_nan() {
            found_nan = true;
        }
        if x > max {
            max = x;
        }
        coords.push(x);
    }
    Ok(Object::Float(vector_norm(&mut coords, max, found_nan)))
}

fn math_dist(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "dist", 2)?;
    let p_vals = collect_numbers(&args[0], "dist")?;
    let q_vals = collect_numbers(&args[1], "dist")?;
    if p_vals.len() != q_vals.len() {
        return Err(value_error(
            "both points must have the same number of dimensions",
        ));
    }
    let mut diffs: Vec<f64> = Vec::with_capacity(p_vals.len());
    let mut max = 0.0_f64;
    let mut found_nan = false;
    for (px, qx) in p_vals.iter().zip(q_vals.iter()) {
        let x = (px - qx).abs();
        if x.is_nan() {
            found_nan = true;
        }
        if x > max {
            max = x;
        }
        diffs.push(x);
    }
    Ok(Object::Float(vector_norm(&mut diffs, max, found_nan)))
}

fn math_expm1(args: &[Object]) -> Result<Object, RuntimeError> {
    // expm1(huge) overflows → OverflowError; expm1(-inf) = -1 (no error).
    let x = to_f64(args, "expm1", 0)?;
    finish1(x, x.exp_m1(), true)
}

fn math_log1p(args: &[Object]) -> Result<Object, RuntimeError> {
    // log1p(x) is a domain error for x <= -1 (→ -inf or nan) → ValueError.
    let x = to_f64(args, "log1p", 0)?;
    finish1(x, x.ln_1p(), false)
}

fn math_ldexp(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "ldexp", 2)?;
    let x = to_f64(args, "ldexp", 0)?;
    // The exponent must be an int (CPython rejects floats here outright).
    use num_traits::{Signed, ToPrimitive};
    let exp: i128 = match &args[1] {
        Object::Int(v) => i128::from(*v),
        Object::Bool(b) => i128::from(*b),
        Object::Long(b) => b.to_i128().unwrap_or(if b.is_negative() {
            i128::MIN
        } else {
            i128::MAX
        }),
        other => {
            // Honour an int subclass's native payload, else TypeError.
            match other.native_value().and_then(|n| n.as_bigint()) {
                Some(b) => b.to_i128().unwrap_or(if b.is_negative() {
                    i128::MIN
                } else {
                    i128::MAX
                }),
                None => return Err(type_error("Expected an int as second argument to ldexp.")),
            }
        }
    };
    // NaNs, zeros and infinities pass through unchanged.
    if x == 0.0 || !x.is_finite() {
        return Ok(Object::Float(x));
    }
    if exp > i128::from(i32::MAX) {
        return Err(overflow_error("math range error"));
    }
    if exp < i128::from(i32::MIN) {
        return Ok(Object::Float(0.0_f64.copysign(x))); // underflow to ±0
    }
    let r = ldexp(x, exp as i32);
    if r.is_infinite() {
        return Err(overflow_error("math range error"));
    }
    Ok(Object::Float(r))
}

/// Correctly-rounded `x * 2**n` (C `scalbn`/`ldexp`), including the
/// subnormal range — `2f64.powi(n)` underflows to 0 for `n < -1022`, so a
/// naive `x * 2f64.powi(n)` cannot produce subnormals like `ldexp(1.0,
/// -1074)` (the smallest positive double). Mirrors musl's `scalbn`.
pub(crate) fn ldexp(mut x: f64, mut n: i32) -> f64 {
    let p1023 = 2f64.powi(1023);
    // 2**-1022 * 2**53 == 2**-969, applied in steps so the running value
    // never underflows before the final scaling (avoids double rounding).
    let p_minus_969 = 2f64.powi(-969);
    if n > 1023 {
        x *= p1023;
        n -= 1023;
        if n > 1023 {
            x *= p1023;
            n -= 1023;
            if n > 1023 {
                n = 1023;
            }
        }
    } else if n < -1022 {
        x *= p_minus_969;
        n += 969;
        if n < -1022 {
            x *= p_minus_969;
            n += 969;
            if n < -1022 {
                n = -1022;
            }
        }
    }
    x * f64::from_bits(((0x3ff + n) as u64) << 52)
}

fn math_frexp(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "frexp", 1)?;
    let x = to_f64(args, "frexp", 0)?;
    // NaN/inf/0 are returned unchanged with exponent 0 (CPython sidesteps
    // platform `frexp` differences for these).
    if x.is_nan() || x.is_infinite() || x == 0.0 {
        return Ok(Object::Tuple(crate::sync::Rc::from(vec![
            Object::Float(x),
            Object::Int(0),
        ])));
    }
    let (mantissa, exp) = frexp(x);
    Ok(Object::Tuple(crate::sync::Rc::from(vec![
        Object::Float(mantissa),
        Object::Int(exp),
    ])))
}

/// C `frexp`: split a finite, nonzero `x` into `(m, e)` with `x = m·2**e`
/// and `0.5 <= |m| < 1`. Implemented via bit twiddling, normalising
/// subnormals first so the mantissa range invariant always holds.
fn frexp(x: f64) -> (f64, i64) {
    let mut bits = x.to_bits();
    let mut exp = ((bits >> 52) & 0x7ff) as i64;
    if exp == 0 {
        // Subnormal: scale up by 2**54 to normalise, then correct the exp.
        let (m, e) = frexp(x * 18_014_398_509_481_984.0_f64); // 2**54
        return (m, e - 54);
    }
    // Set the stored exponent to 0x3fe so the value lands in [0.5, 1).
    exp -= 1022;
    bits = (bits & !(0x7ff << 52)) | (0x3fe << 52);
    (f64::from_bits(bits), exp)
}

fn math_modf(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "modf", 1)?;
    let x = to_f64(args, "modf", 0)?;
    // modf(±inf) = (±0.0, ±inf); modf(nan) = (nan, nan).
    if x.is_infinite() {
        return Ok(Object::Tuple(crate::sync::Rc::from(vec![
            Object::Float(0.0_f64.copysign(x)),
            Object::Float(x),
        ])));
    }
    if x.is_nan() {
        return Ok(Object::Tuple(crate::sync::Rc::from(vec![
            Object::Float(x),
            Object::Float(x),
        ])));
    }
    let int_part = x.trunc();
    let frac = x - int_part;
    Ok(Object::Tuple(crate::sync::Rc::from(vec![
        Object::Float(frac),
        Object::Float(int_part),
    ])))
}

fn math_comb(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_traits::{Signed, ToPrimitive};
    if args.len() != 2 {
        return Err(type_error(format!(
            "comb() takes exactly 2 arguments ({} given)",
            args.len()
        )));
    }
    let n = index_bigint(&args[0])?;
    let k = index_bigint(&args[1])?;
    if n.is_negative() {
        return Err(value_error("n must be a non-negative integer"));
    }
    if k.is_negative() {
        return Err(value_error("k must be a non-negative integer"));
    }
    if k > n {
        return Ok(Object::Int(0));
    }
    // Reduce by symmetry: C(n, k) == C(n, n - k).
    let nmk = &n - &k;
    let kk = if nmk < k { nmk } else { k };
    let kk64 = match kk.to_i64() {
        Some(v) => v,
        None => {
            return Err(overflow_error(
                "min(n - k, k) must not exceed 9223372036854775807",
            ))
        }
    };
    // Incremental, exact: result_i = C(n, i+1) = C(n, i) * (n - i) / (i + 1).
    let mut result = num_bigint::BigInt::from(1);
    for i in 0..kk64 {
        result *= &n - num_bigint::BigInt::from(i);
        result /= num_bigint::BigInt::from(i + 1);
    }
    Ok(Object::int_from_bigint(result))
}

fn math_perm(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_traits::{Signed, ToPrimitive};
    if args.is_empty() || args.len() > 2 {
        return Err(type_error(format!(
            "perm() expected 1 to 2 arguments, got {}",
            args.len()
        )));
    }
    // perm(n) and perm(n, None) are factorial(n).
    if matches!(args.get(1), None | Some(Object::None)) {
        return factorial_impl(&args[0]);
    }
    let n = index_bigint(&args[0])?;
    let k = index_bigint(&args[1])?;
    if n.is_negative() {
        return Err(value_error("n must be a non-negative integer"));
    }
    if k.is_negative() {
        return Err(value_error("k must be a non-negative integer"));
    }
    if k > n {
        return Ok(Object::Int(0));
    }
    let k64 = match k.to_i64() {
        Some(v) => v,
        None => return Err(overflow_error("k must not exceed 9223372036854775807")),
    };
    // n * (n-1) * ... * (n-k+1): a falling factorial of k terms.
    let mut result = num_bigint::BigInt::from(1);
    for i in 0..k64 {
        result *= &n - num_bigint::BigInt::from(i);
    }
    Ok(Object::int_from_bigint(result))
}

/// IEEE-754 remainder `x - n*y` (n chosen even on ties), faithful port of
/// CPython's `m_remainder`. The result is always exact and carries the sign
/// of `x` (so `remainder(-1.0, 1.0)` is `-0.0`).
fn m_remainder(x: f64, y: f64) -> f64 {
    if x.is_finite() && y.is_finite() {
        if y == 0.0 {
            return f64::NAN;
        }
        let absx = x.abs();
        let absy = y.abs();
        let m = libm_fmod(absx, absy);
        // Compare m against absy/2 indirectly via the complement c = absy - m
        // to avoid precision loss when forming 0.5*absy.
        let c = absy - m;
        let r = if m < c {
            m
        } else if m > c {
            -c
        } else {
            // Exactly halfway: pick the even multiple.
            m - 2.0 * libm_fmod(0.5 * (absx - m), absy)
        };
        return 1.0_f64.copysign(x) * r;
    }
    if x.is_nan() {
        return x;
    }
    if y.is_nan() {
        return y;
    }
    if x.is_infinite() {
        return f64::NAN;
    }
    // y is infinite, x finite.
    x
}

fn math_remainder(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "remainder", 2)?;
    let x = to_f64(args, "remainder", 0)?;
    let y = to_f64(args, "remainder", 1)?;
    let r = m_remainder(x, y);
    // A NaN result from non-NaN operands is the IEEE invalid operation
    // (remainder by zero, or remainder of an infinity) → ValueError.
    if r.is_nan() && !x.is_nan() && !y.is_nan() {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(r))
}

/// `math.nextafter(x, y, *, steps=None)` — faithful port of CPython's
/// `math_nextafter_impl`, including the multi-`steps` integer-bit walk added
/// in 3.12.
fn math_nextafter(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    if args.len() != 2 {
        return Err(type_error(format!(
            "nextafter() takes exactly 2 positional arguments ({} given)",
            args.len()
        )));
    }
    let x = to_f64(args, "nextafter", 0)?;
    let y = to_f64(args, "nextafter", 1)?;
    let mut steps_obj: Option<&Object> = None;
    for (key, value) in kwargs {
        match key.as_str() {
            "steps" => steps_obj = Some(value),
            other => {
                return Err(type_error(format!(
                    "nextafter() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }

    // Default (steps is None/absent): a single step via libm nextafter.
    let steps_obj = match steps_obj {
        None | Some(Object::None) => return Ok(Object::Float(next_after(x, y))),
        Some(o) => o,
    };

    use num_traits::Signed;
    let steps_bi = index_bigint(steps_obj)?;
    if steps_bi.is_negative() {
        return Err(value_error("steps must be a non-negative integer"));
    }
    // Saturate at u64::MAX (matching CPython); also covers the huge-int case.
    use num_traits::ToPrimitive;
    let usteps: u64 = steps_bi.to_u64().unwrap_or(u64::MAX);

    if usteps == 0 {
        return Ok(Object::Float(x));
    }
    if x.is_nan() {
        return Ok(Object::Float(x));
    }
    if y.is_nan() {
        return Ok(Object::Float(y));
    }
    let ux = x.to_bits();
    let uy = y.to_bits();
    if ux == uy {
        return Ok(Object::Float(x));
    }
    const SIGN_BIT: u64 = 1u64 << 63;
    let ax = ux & !SIGN_BIT;
    let ay = uy & !SIGN_BIT;
    let result_bits = if (ux ^ uy) & SIGN_BIT != 0 {
        // Opposite signs.
        if ax + ay <= usteps {
            uy
        } else if ax < usteps {
            (uy & SIGN_BIT) | (usteps - ax)
        } else {
            ux - usteps
        }
    } else if ax > ay {
        if ax - ay >= usteps {
            ux - usteps
        } else {
            uy
        }
    } else if ay - ax >= usteps {
        ux + usteps
    } else {
        uy
    };
    Ok(Object::Float(f64::from_bits(result_bits)))
}

/// C `nextafter(x, y)`: the next representable double after `x` toward `y`.
fn next_after(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    #[allow(clippy::float_cmp)]
    if x == y {
        return y; // preserves the sign of y for ±0
    }
    if x == 0.0 {
        // Smallest subnormal with the sign of y.
        return f64::from_bits(1).copysign(y);
    }
    let bits = x.to_bits();
    // Moving away from zero toward +inf increments magnitude; toward zero
    // decrements it. `(y > x)` tells us the direction.
    let next = if (x > 0.0) == (y > x) {
        bits + 1
    } else {
        bits - 1
    };
    f64::from_bits(next)
}

fn math_ulp(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "ulp", 1)?;
    let x = to_f64(args, "ulp", 0)?.abs();
    if x.is_nan() || x.is_infinite() {
        return Ok(Object::Float(x));
    }
    let x2 = next_after(x, f64::INFINITY);
    if x2.is_infinite() {
        // x is the largest finite double: step toward zero instead.
        let x2 = next_after(x, f64::NEG_INFINITY);
        return Ok(Object::Float(x - x2));
    }
    Ok(Object::Float(x2 - x))
}

// ---------------------------------------------------------------------
// Faithful ports of CPython's `mathmodule.c` special functions (RFC 0041).
//
// The previous Abramowitz & Stegun `erf` (~1e-7) and Lanczos `g=7` `gamma`
// were nowhere near `test_math.test_mtestfile`'s few-ulp budget. These are
// CPython's exact algorithms — the erf power-series / erfc continued
// fraction, and the Lanczos `g=6.024…, n=13` rational approximation with
// the same reflection and large/tiny-argument handling — so the special
// values match CPython to within the test's tolerance.
// ---------------------------------------------------------------------

// These constants are transcribed verbatim from CPython's `mathmodule.c` at
// full source precision; the extra digits round to the same f64 bits, so the
// `excessive_precision` lint is allowed to keep the literals byte-faithful.
/// `√π`, to full double precision (CPython's `sqrtpi`).
#[allow(clippy::excessive_precision)]
const SQRTPI: f64 = 1.772_453_850_905_516_027_298_167_5_f64;
/// `π`, matching CPython's `pi` constant used by the gamma reflection.
const M_PI: f64 = std::f64::consts::PI;
/// `log(π)` (CPython's `logpi`), used by the `lgamma` reflection formula.
#[allow(clippy::excessive_precision)]
const LOGPI: f64 = 1.144_729_885_849_400_174_143_427_4_f64;

const ERF_SERIES_CUTOFF: f64 = 1.5;
const ERF_SERIES_TERMS: usize = 25;
const ERFC_CONTFRAC_CUTOFF: f64 = 30.0;
const ERFC_CONTFRAC_TERMS: usize = 50;

/// Power series for `erf`, valid for `|x| < ERF_SERIES_CUTOFF`. Mirrors
/// CPython's `m_erf_series` (Taylor series, summed from the tail in).
fn m_erf_series(x: f64) -> f64 {
    let x2 = x * x;
    let mut acc = 0.0_f64;
    let mut fk = ERF_SERIES_TERMS as f64 + 0.5;
    for _ in 0..ERF_SERIES_TERMS {
        acc = 2.0 + x2 * acc / fk;
        fk -= 1.0;
    }
    acc * x * (-x2).exp() / SQRTPI
}

/// Continued fraction for `erfc(x)`, valid for `x >= ERF_SERIES_CUTOFF`.
/// Mirrors CPython's `m_erfc_contfrac` (Lentz-free two-term recurrence).
fn m_erfc_contfrac(x: f64) -> f64 {
    if x >= ERFC_CONTFRAC_CUTOFF {
        return 0.0;
    }
    let x2 = x * x;
    let mut a = 0.0_f64;
    let mut da = 0.5_f64;
    let mut p = 1.0_f64;
    let mut p_last = 0.0_f64;
    let mut q = da + x2;
    let mut q_last = 1.0_f64;
    for _ in 0..ERFC_CONTFRAC_TERMS {
        a += da;
        da += 2.0;
        let b = da + x2;
        let temp_p = p;
        p = b * p - a * p_last;
        p_last = temp_p;
        let temp_q = q;
        q = b * q - a * q_last;
        q_last = temp_q;
    }
    p / q * x * (-x2).exp() / SQRTPI
}

fn m_erf(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let absx = x.abs();
    if absx < ERF_SERIES_CUTOFF {
        m_erf_series(x)
    } else {
        let cf = m_erfc_contfrac(absx);
        if x > 0.0 {
            1.0 - cf
        } else {
            cf - 1.0
        }
    }
}

fn m_erfc(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let absx = x.abs();
    if absx < ERF_SERIES_CUTOFF {
        1.0 - m_erf_series(x)
    } else {
        let cf = m_erfc_contfrac(absx);
        if x > 0.0 {
            cf
        } else {
            2.0 - cf
        }
    }
}

fn math_erf(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(m_erf(to_f64(args, "erf", 0)?)))
}

fn math_erfc(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(m_erfc(to_f64(args, "erfc", 0)?)))
}

/// Lanczos approximation parameters (CPython `mathmodule.c`): `g`, `n=13`,
/// and the numerator/denominator polynomial coefficients. This is the same
/// high-accuracy rational approximation CPython uses, good to a few ulps —
/// required by `test_math.test_mtestfile`.
#[allow(clippy::excessive_precision)]
const LANCZOS_G: f64 = 6.024_680_040_776_729_583_740_234_375;
#[allow(clippy::excessive_precision)]
const LANCZOS_G_MINUS_HALF: f64 = 5.524_680_040_776_729_583_740_234_375;
const LANCZOS_N: usize = 13;
#[allow(clippy::excessive_precision)]
const LANCZOS_NUM: [f64; LANCZOS_N] = [
    23_531_376_880.410_759_688_572_007_674_451_636_754_734_846_804_94,
    42_919_803_642.649_098_768_957_899_047_001_988_850_926_355_848_96,
    35_711_959_237.355_668_049_440_185_451_547_166_705_960_488_635_84,
    17_921_034_426.037_209_699_919_755_754_458_931_112_671_403_265_39,
    6_039_542_586.352_028_005_064_291_644_307_297_921_069_938_842_07,
    1_439_720_407.311_721_673_663_223_072_794_912_393_971_548_578_68,
    248_874_557.862_054_156_511_460_386_413_229_423_216_321_251_278,
    31_426_415.585_400_194_380_614_231_628_318_205_362_874_684_987_64,
    2_876_370.628_935_372_441_225_409_051_620_849_613_599_114_537_88,
    186_056.265_395_223_495_040_294_989_716_045_699_282_207_842_363_3,
    8_071.672_002_365_816_210_638_002_902_272_250_613_821_851_632_502,
    210.824_277_751_579_345_872_509_733_920_713_362_711_669_695_802_9,
    2.506_628_274_631_000_270_164_908_177_133_837_338_626_431_079_341,
];
const LANCZOS_DEN: [f64; LANCZOS_N] = [
    0.0,
    39_916_800.0,
    120_543_840.0,
    150_917_976.0,
    105_258_076.0,
    45_995_730.0,
    13_339_535.0,
    2_637_558.0,
    357_423.0,
    32_670.0,
    1_925.0,
    66.0,
    1.0,
];

/// Exact factorials `0!..=22!` (CPython's `gamma_integral`), used so
/// `gamma(n)` for small positive integers is bit-exact (`gamma(6) == 120.0`).
const NGAMMA_INTEGRAL: usize = 23;
const GAMMA_INTEGRAL: [f64; NGAMMA_INTEGRAL] = [
    1.0,
    1.0,
    2.0,
    6.0,
    24.0,
    120.0,
    720.0,
    5_040.0,
    40_320.0,
    362_880.0,
    3_628_800.0,
    39_916_800.0,
    479_001_600.0,
    6_227_020_800.0,
    87_178_291_200.0,
    1_307_674_368_000.0,
    20_922_789_888_000.0,
    355_687_428_096_000.0,
    6_402_373_705_728_000.0,
    121_645_100_408_832_000.0,
    2_432_902_008_176_640_000.0,
    51_090_942_171_709_440_000.0,
    1_124_000_727_777_607_680_000.0,
];

/// Lanczos rational sum (CPython `lanczos_sum`). Evaluated Horner-style
/// from the bottom for `x < 5` and reciprocally for larger `x` to keep the
/// terms well-scaled.
fn lanczos_sum(x: f64) -> f64 {
    let mut num = 0.0_f64;
    let mut den = 0.0_f64;
    if x < 5.0 {
        for i in (0..LANCZOS_N).rev() {
            num = num * x + LANCZOS_NUM[i];
            den = den * x + LANCZOS_DEN[i];
        }
    } else {
        for i in 0..LANCZOS_N {
            num = num / x + LANCZOS_NUM[i];
            den = den / x + LANCZOS_DEN[i];
        }
    }
    num / den
}

/// `sin(πx)` computed accurately for finite `x` via argument reduction
/// (CPython `m_sinpi`). Used by the `gamma`/`lgamma` reflection formulas.
fn m_sinpi(x: f64) -> f64 {
    let y = (x.abs()) % 2.0;
    let n = (2.0 * y).round() as i32;
    let r = match n {
        0 => (M_PI * y).sin(),
        1 => (M_PI * (y - 0.5)).cos(),
        // N.B. sin(pi*(1-y)) (not -sin(pi*(y-1))) so y==1.0 gives +0.0.
        2 => (M_PI * (1.0 - y)).sin(),
        3 => -(M_PI * (y - 1.5)).cos(),
        _ => (M_PI * (y - 2.0)).sin(),
    };
    (1.0_f64).copysign(x) * r
}

/// `Γ(x)` — faithful port of CPython's `m_tgamma`. Positive arguments use
/// the Lanczos rational sum directly; negatives use the reflection formula
/// folded so `exp`/`pow` stay accurate and underflow to a signed zero for
/// large magnitudes (rather than losing the subnormal tail).
fn m_tgamma(x: f64) -> Result<f64, RuntimeError> {
    if !x.is_finite() {
        // tgamma(nan) = nan, tgamma(+inf) = +inf, tgamma(-inf) is invalid.
        if x.is_nan() || x > 0.0 {
            return Ok(x);
        }
        return Err(value_error("math domain error"));
    }
    if x == 0.0 {
        // tgamma(±0) = ±inf — a pole (CPython raises ValueError).
        return Err(value_error("math domain error"));
    }
    // Integer arguments: exact for small positive ints; a pole for n <= 0.
    if x == x.floor() {
        if x < 0.0 {
            return Err(value_error("math domain error"));
        }
        if x <= NGAMMA_INTEGRAL as f64 {
            return Ok(GAMMA_INTEGRAL[x as usize - 1]);
        }
    }
    let absx = x.abs();
    // Tiny arguments: Γ(x) ~ 1/x.
    if absx < 1e-20 {
        let r = 1.0 / x;
        return if r.is_infinite() {
            Err(overflow_error("math range error"))
        } else {
            Ok(r)
        };
    }
    // Large arguments overflow (x > ~171.6); negative ones underflow to ±0.
    if absx > 200.0 {
        if x < 0.0 {
            return Ok(0.0 / m_sinpi(x));
        }
        return Err(overflow_error("math range error"));
    }
    let y = absx + LANCZOS_G_MINUS_HALF;
    // The error in `y` from rounding (`z`); CPython's cheap correction.
    let z = if absx > LANCZOS_G_MINUS_HALF {
        let q = y - absx;
        (q - LANCZOS_G_MINUS_HALF) * LANCZOS_G / y
    } else {
        let q = y - LANCZOS_G_MINUS_HALF;
        (q - absx) * LANCZOS_G / y
    };
    let r = if x < 0.0 {
        let mut r = -M_PI / m_sinpi(absx) / absx * y.exp() / lanczos_sum(absx);
        r -= z * r;
        if absx < 140.0 {
            r /= y.powf(absx - 0.5);
        } else {
            let sqrtpow = y.powf(absx / 2.0 - 0.25);
            r /= sqrtpow;
            r /= sqrtpow;
        }
        r
    } else {
        let mut r = lanczos_sum(absx) / y.exp();
        r += z * r;
        if absx < 140.0 {
            r *= y.powf(absx - 0.5);
        } else {
            let sqrtpow = y.powf(absx / 2.0 - 0.25);
            r *= sqrtpow;
            r *= sqrtpow;
        }
        r
    };
    if r.is_infinite() {
        return Err(overflow_error("math range error"));
    }
    Ok(r)
}

fn math_gamma(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(m_tgamma(to_f64(args, "gamma", 0)?)?))
}

/// `ln|Γ(x)|` — faithful port of CPython's `m_lgamma`.
fn m_lgamma(x: f64) -> Result<f64, RuntimeError> {
    if !x.is_finite() {
        // lgamma(nan) = nan, lgamma(±inf) = +inf.
        return Ok(if x.is_nan() { x } else { f64::INFINITY });
    }
    // Integer arguments: lgamma(1)=lgamma(2)=0; a pole for n <= 0.
    if x == x.floor() && x <= 2.0 {
        if x <= 0.0 {
            return Err(value_error("math domain error"));
        }
        return Ok(0.0);
    }
    let absx = x.abs();
    // Tiny arguments: lgamma(x) ~ -log|x|.
    if absx < 1e-20 {
        return Ok(-absx.ln());
    }
    let mut r = lanczos_sum(absx).ln() - LANCZOS_G;
    r += (absx - 0.5) * ((absx + LANCZOS_G - 0.5).ln() - 1.0);
    if x < 0.0 {
        // Reflection for negative x.
        r = LOGPI - m_sinpi(absx).abs().ln() - absx.ln() - r;
    }
    if r.is_infinite() {
        return Err(overflow_error("math range error"));
    }
    Ok(r)
}

fn math_lgamma(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(m_lgamma(to_f64(args, "lgamma", 0)?)?))
}

fn math_isqrt(args: &[Object]) -> Result<Object, RuntimeError> {
    use num_traits::Signed;
    expect_nargs(args, "isqrt", 1)?;
    // Accept any integer, including big and `__index__`-able values
    // (CPython uses `_PyNumber_Index`). A float approximation overflows for
    // large inputs, so we take the exact integer square root over BigInt.
    let n = index_bigint(&args[0])?;
    if n.is_negative() {
        return Err(value_error("isqrt() argument must be nonnegative"));
    }
    Ok(Object::int_from_bigint(n.sqrt()))
}

fn math_cbrt(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(to_f64(args, "cbrt", 0)?.cbrt()))
}

fn math_exp2(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "exp2", 0)?;
    finish1(x, x.exp2(), true)
}

// Faithful ports of CPython's `_math.c` inverse-hyperbolic functions.
// Rust's libm `acosh`/`asinh`/`atanh` lose precision near the domain
// boundary (≥ a few ulps for `acosh(1+ε)`/`atanh(±1∓ε)`) and overflow for
// huge inputs (`acosh(1.3e308)` → `inf` instead of ~710). CPython uses
// `log1p` near the boundary and `log(x)+ln2` in the tail; we mirror that
// exactly so `test_math.test_testfile` is within its 5-ulp budget.
const TWO_POW_M28: f64 = 3.725_290_298_461_914e-9; // 2**-28
const TWO_POW_P28: f64 = 268_435_456.0; // 2**28

fn m_acosh(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x < 1.0 {
        return f64::NAN; // domain error (finish1 → ValueError)
    }
    if x >= TWO_POW_P28 {
        if x.is_infinite() {
            return x;
        }
        // acosh(x) ≈ log(2x) = log(x) + ln2, without overflowing x*x.
        return x.ln() + std::f64::consts::LN_2;
    }
    #[allow(clippy::float_cmp)]
    if x == 1.0 {
        return 0.0;
    }
    if x >= 2.0 {
        let t = x * x;
        (2.0 * x - 1.0 / (x + (t - 1.0).sqrt())).ln()
    } else {
        let t = x - 1.0;
        (t + (2.0 * t + t * t).sqrt()).ln_1p()
    }
}

fn m_asinh(x: f64) -> f64 {
    let absx = x.abs();
    if x.is_nan() || x.is_infinite() {
        return x;
    }
    if absx < TWO_POW_M28 {
        return x;
    }
    let w = if absx > TWO_POW_P28 {
        absx.ln() + std::f64::consts::LN_2
    } else if absx > 2.0 {
        (2.0 * absx + 1.0 / ((x * x + 1.0).sqrt() + absx)).ln()
    } else {
        let t = x * x;
        (absx + t / (1.0 + (1.0 + t).sqrt())).ln_1p()
    };
    w.copysign(x)
}

fn m_atanh(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    let absx = x.abs();
    if absx >= 1.0 {
        return f64::NAN; // domain error (finish1 → ValueError)
    }
    if absx < TWO_POW_M28 {
        return x;
    }
    let t = if absx < 0.5 {
        let t = absx + absx;
        0.5 * (t + t * absx / (1.0 - absx)).ln_1p()
    } else {
        0.5 * ((absx + absx) / (1.0 - absx)).ln_1p()
    };
    t.copysign(x)
}

fn math_atanh(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "atanh", 0)?;
    finish1(x, m_atanh(x), false)
}

fn math_asinh(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "asinh", 0)?;
    finish1(x, m_asinh(x), false)
}

fn math_acosh(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "acosh", 0)?;
    finish1(x, m_acosh(x), false)
}
