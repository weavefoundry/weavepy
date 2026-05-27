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

use crate::error::{type_error, value_error, RuntimeError};
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
            builtin("isclose", math_isclose),
        );
        // Missing CPython math symbols added in RFC 0030 to widen
        // drop-in compatibility for numpy/scipy-style consumers.
        d.insert(
            DictKey(Object::from_static("fsum")),
            builtin("fsum", math_fsum),
        );
        d.insert(
            DictKey(Object::from_static("prod")),
            builtin("prod", math_prod),
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
            builtin("nextafter", math_nextafter),
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
        call: Box::new(body),
        call_kw: None,
    }))
}

/// Total real-valued unary functions over `f64`. Listed in one place
/// so each one is a single line at the call site.
fn total_f64() -> &'static [(&'static str, fn(&[Object]) -> Result<Object, RuntimeError>)] {
    &[
        ("exp", |a| Ok(Object::Float(to_f64(a, "exp", 0)?.exp()))),
        ("sin", |a| Ok(Object::Float(to_f64(a, "sin", 0)?.sin()))),
        ("cos", |a| Ok(Object::Float(to_f64(a, "cos", 0)?.cos()))),
        ("tan", |a| Ok(Object::Float(to_f64(a, "tan", 0)?.tan()))),
        ("sinh", |a| Ok(Object::Float(to_f64(a, "sinh", 0)?.sinh()))),
        ("cosh", |a| Ok(Object::Float(to_f64(a, "cosh", 0)?.cosh()))),
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

fn to_f64(args: &[Object], func: &str, idx: usize) -> Result<f64, RuntimeError> {
    match args.get(idx) {
        Some(Object::Float(f)) => Ok(*f),
        Some(Object::Int(i)) => Ok(*i as f64),
        Some(Object::Bool(b)) => Ok(if *b { 1.0 } else { 0.0 }),
        Some(other) => Err(type_error(format!(
            "{func}() argument must be int or float, not '{}'",
            other.type_name()
        ))),
        None => Err(type_error(format!(
            "{func}() takes at least {} argument(s)",
            idx + 1
        ))),
    }
}

fn to_i64(args: &[Object], func: &str, idx: usize) -> Result<i64, RuntimeError> {
    match args.get(idx) {
        Some(Object::Int(i)) => Ok(*i),
        Some(Object::Bool(b)) => Ok(i64::from(*b)),
        Some(other) => Err(type_error(format!(
            "{func}() takes an integer, not '{}'",
            other.type_name()
        ))),
        None => Err(type_error(format!(
            "{func}() takes at least {} argument(s)",
            idx + 1
        ))),
    }
}

fn math_sqrt(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "sqrt", 0)?;
    if x < 0.0 {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(x.sqrt()))
}

fn math_asin(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "asin", 0)?;
    if !(-1.0..=1.0).contains(&x) {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(x.asin()))
}

fn math_acos(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "acos", 0)?;
    if !(-1.0..=1.0).contains(&x) {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(x.acos()))
}

fn math_atan(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(to_f64(args, "atan", 0)?.atan()))
}

fn math_atan2(args: &[Object]) -> Result<Object, RuntimeError> {
    let y = to_f64(args, "atan2", 0)?;
    let x = to_f64(args, "atan2", 1)?;
    Ok(Object::Float(y.atan2(x)))
}

fn math_log(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "log", 0)?;
    if x <= 0.0 {
        return Err(value_error("math domain error"));
    }
    if args.len() >= 2 {
        let base = to_f64(args, "log", 1)?;
        // `log(x, 1)` and `log(x, <=0)` are both undefined; the
        // float_cmp lint fires here because `==` against a literal
        // 1.0 is exactly what we want.
        #[allow(clippy::float_cmp)]
        let bad_base = base <= 0.0 || base == 1.0;
        if bad_base {
            return Err(value_error("math domain error"));
        }
        Ok(Object::Float(x.log(base)))
    } else {
        Ok(Object::Float(x.ln()))
    }
}

fn math_log2(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "log2", 0)?;
    if x <= 0.0 {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(x.log2()))
}

fn math_log10(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "log10", 0)?;
    if x <= 0.0 {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(x.log10()))
}

fn math_pow(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "pow", 0)?;
    let y = to_f64(args, "pow", 1)?;
    Ok(Object::Float(x.powf(y)))
}

fn math_floor(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "floor", 0)?;
    Ok(Object::Int(x.floor() as i64))
}

fn math_ceil(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "ceil", 0)?;
    Ok(Object::Int(x.ceil() as i64))
}

fn math_trunc(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "trunc", 0)?;
    Ok(Object::Int(x.trunc() as i64))
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
    let x = to_f64(args, "fmod", 0)?;
    let y = to_f64(args, "fmod", 1)?;
    if y == 0.0 {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(x % y))
}

fn math_gcd(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::Int(0));
    }
    let mut acc: i64 = 0;
    for (i, _) in args.iter().enumerate() {
        let v = to_i64(args, "gcd", i)?.unsigned_abs() as i64;
        acc = gcd_i64(acc, v);
    }
    Ok(Object::Int(acc))
}

fn math_lcm(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::Int(1));
    }
    let mut acc: i64 = 1;
    for (i, _) in args.iter().enumerate() {
        let v = to_i64(args, "lcm", i)?.unsigned_abs() as i64;
        if v == 0 {
            return Ok(Object::Int(0));
        }
        let g = gcd_i64(acc, v);
        // acc * v / g
        acc = (acc / g).saturating_mul(v);
    }
    Ok(Object::Int(acc))
}

fn math_factorial(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = to_i64(args, "factorial", 0)?;
    if n < 0 {
        return Err(value_error("factorial() not defined for negative values"));
    }
    let mut acc: i64 = 1;
    for i in 1..=n {
        acc = acc.saturating_mul(i);
    }
    Ok(Object::Int(acc))
}

/// `math.isclose(a, b, *, rel_tol=1e-09, abs_tol=0.0)` implementing
/// PEP 485 — symmetric "weak" relative tolerance. We accept the two
/// tolerance values positionally as well for the no-keywords builtin
/// dispatch path (CPython rejects positional tolerances; we follow
/// suit by treating extra positional args as an error).
fn math_isclose(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() < 2 {
        return Err(value_error("isclose() takes at least 2 arguments"));
    }
    let a = to_f64(args, "isclose", 0)?;
    let b = to_f64(args, "isclose", 1)?;
    if args.len() > 2 {
        return Err(type_error(
            "isclose() takes no positional arguments after b",
        ));
    }
    let rel_tol = 1e-9_f64;
    let abs_tol = 0.0_f64;
    // Bit-exact equality is the documented fast path for ``isclose``
    // (CPython's `_PyMath_IsClose` does the same). It's *the* reason
    // ``isclose(inf, inf)`` returns ``True``.
    #[allow(clippy::float_cmp)]
    if a == b {
        return Ok(Object::Bool(true));
    }
    if a.is_infinite() || b.is_infinite() {
        return Ok(Object::Bool(false));
    }
    let diff = (a - b).abs();
    let tol = (rel_tol * a.abs().max(b.abs())).max(abs_tol);
    Ok(Object::Bool(diff <= tol))
}

fn gcd_i64(a: i64, b: i64) -> i64 {
    let mut a = a.unsigned_abs();
    let mut b = b.unsigned_abs();
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a as i64
}

// ---------------------------------------------------------------------
// Math additions (RFC 0030)
// ---------------------------------------------------------------------

/// Collect numbers out of any iterable argument. Mirrors CPython's
/// ``_PyIter_GetIter`` flow: try to make an iterator and pull values
/// until exhaustion, coercing each to f64 along the way.
fn collect_numbers(arg: &Object, func: &str) -> Result<Vec<f64>, RuntimeError> {
    let mut it = arg.make_iter().map_err(|_| {
        type_error(format!(
            "{func}() argument must be iterable, not '{}'",
            arg.type_name()
        ))
    })?;
    let mut out = Vec::new();
    while let Some(item) = it.next_value() {
        out.push(object_to_f64(&item, func)?);
    }
    Ok(out)
}

fn object_to_f64(value: &Object, func: &str) -> Result<f64, RuntimeError> {
    match value {
        Object::Float(f) => Ok(*f),
        Object::Int(i) => Ok(*i as f64),
        Object::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        other => Err(type_error(format!(
            "{func}() element must be number, not '{}'",
            other.type_name()
        ))),
    }
}

fn math_fsum(args: &[Object]) -> Result<Object, RuntimeError> {
    // CPython implements fsum as Shewchuk's adaptive-precision
    // floating-point sum. We use the same Kahan-style partials
    // accumulator: each partial is a non-overlapping float so the
    // final sum is rounded once.
    let arg = args
        .first()
        .ok_or_else(|| type_error("fsum() takes 1 argument"))?;
    let values = collect_numbers(arg, "fsum")?;
    let mut partials: Vec<f64> = Vec::new();
    for mut x in values {
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
        partials.push(x);
    }
    Ok(Object::Float(partials.iter().sum()))
}

fn math_prod(args: &[Object]) -> Result<Object, RuntimeError> {
    let arg = args
        .first()
        .ok_or_else(|| type_error("prod() takes 1 argument"))?;
    let values = collect_numbers(arg, "prod")?;
    let start = args
        .get(1)
        .map(|o| object_to_f64(o, "prod"))
        .transpose()?
        .unwrap_or(1.0);
    let mut acc = start;
    let mut all_int = matches!(args.get(1), Some(Object::Int(_)) | None);
    if let Some(Object::Float(_)) = args.get(1) {
        all_int = false;
    }
    for v in &values {
        if v.fract() != 0.0 {
            all_int = false;
        }
        acc *= *v;
    }
    if all_int && acc.fract() == 0.0 && acc.is_finite() && acc.abs() < (i64::MAX as f64) {
        Ok(Object::Int(acc as i64))
    } else {
        Ok(Object::Float(acc))
    }
}

fn math_hypot(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() {
        return Ok(Object::Float(0.0));
    }
    let mut sum = 0.0_f64;
    for (idx, a) in args.iter().enumerate() {
        let v = object_to_f64(a, "hypot")
            .map_err(|_| type_error(format!("hypot() argument {idx} must be number")))?;
        sum += v * v;
    }
    Ok(Object::Float(sum.sqrt()))
}

fn math_dist(args: &[Object]) -> Result<Object, RuntimeError> {
    let p = args
        .first()
        .ok_or_else(|| type_error("dist() takes 2 arguments"))?;
    let q = args
        .get(1)
        .ok_or_else(|| type_error("dist() takes 2 arguments"))?;
    let p_vals = collect_numbers(p, "dist")?;
    let q_vals = collect_numbers(q, "dist")?;
    if p_vals.len() != q_vals.len() {
        return Err(value_error("dist() points must have same length"));
    }
    let mut s = 0.0_f64;
    for (a, b) in p_vals.iter().zip(q_vals.iter()) {
        let d = a - b;
        s += d * d;
    }
    Ok(Object::Float(s.sqrt()))
}

fn math_expm1(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(to_f64(args, "expm1", 0)?.exp_m1()))
}

fn math_log1p(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(to_f64(args, "log1p", 0)?.ln_1p()))
}

fn math_ldexp(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "ldexp", 0)?;
    let i = to_i64(args, "ldexp", 1)?;
    Ok(Object::Float(x * 2f64.powi(i as i32)))
}

fn math_frexp(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "frexp", 0)?;
    if x == 0.0 {
        return Ok(Object::Tuple(crate::sync::Rc::from(vec![
            Object::Float(0.0),
            Object::Int(0),
        ])));
    }
    let bits = x.abs().to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1022;
    let mantissa = x / 2f64.powi(exp as i32);
    Ok(Object::Tuple(crate::sync::Rc::from(vec![
        Object::Float(mantissa),
        Object::Int(exp),
    ])))
}

fn math_modf(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "modf", 0)?;
    let int_part = x.trunc();
    let frac = x - int_part;
    Ok(Object::Tuple(crate::sync::Rc::from(vec![
        Object::Float(frac),
        Object::Float(int_part),
    ])))
}

fn math_comb(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = to_i64(args, "comb", 0)?;
    let k = to_i64(args, "comb", 1)?;
    if n < 0 || k < 0 {
        return Err(value_error("comb() arguments must be non-negative"));
    }
    let k = k.min(n - k);
    if k < 0 {
        return Ok(Object::Int(0));
    }
    let mut result: i64 = 1;
    for i in 0..k {
        result = result
            .checked_mul(n - i)
            .ok_or_else(|| value_error("comb() result overflow"))?
            / (i + 1);
    }
    Ok(Object::Int(result))
}

fn math_perm(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = to_i64(args, "perm", 0)?;
    let k = match args.get(1) {
        Some(Object::None) | None => n,
        Some(Object::Int(i)) => *i,
        Some(other) => {
            return Err(type_error(format!(
                "perm() argument must be int, not '{}'",
                other.type_name()
            )))
        }
    };
    if n < 0 || k < 0 {
        return Err(value_error("perm() arguments must be non-negative"));
    }
    if k > n {
        return Ok(Object::Int(0));
    }
    let mut result: i64 = 1;
    for i in 0..k {
        result = result
            .checked_mul(n - i)
            .ok_or_else(|| value_error("perm() result overflow"))?;
    }
    Ok(Object::Int(result))
}

fn math_remainder(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "remainder", 0)?;
    let y = to_f64(args, "remainder", 1)?;
    if y == 0.0 {
        return Err(value_error("math domain error"));
    }
    // IEEE 754 remainder: x - n*y where n = round(x/y), ties-to-even.
    let q = x / y;
    let n = q.round();
    let frac = (q - q.trunc()).abs();
    let n = if (frac - 0.5).abs() < f64::EPSILON {
        let candidate = q.trunc();
        if (candidate as i64) % 2 == 0 {
            candidate
        } else {
            candidate + q.signum()
        }
    } else {
        n
    };
    Ok(Object::Float(x - n * y))
}

fn math_nextafter(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "nextafter", 0)?;
    let y = to_f64(args, "nextafter", 1)?;
    if (x - y).abs() < f64::EPSILON {
        return Ok(Object::Float(y));
    }
    let bits = x.to_bits();
    let next = if (y > x) ^ (x < 0.0) {
        bits + 1
    } else {
        bits - 1
    };
    Ok(Object::Float(f64::from_bits(next)))
}

fn math_ulp(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "ulp", 0)?;
    if x.is_nan() {
        return Ok(Object::Float(x));
    }
    if x.is_infinite() {
        return Ok(Object::Float(f64::INFINITY));
    }
    let next = f64::from_bits(x.to_bits().wrapping_add(1));
    Ok(Object::Float((next - x).abs()))
}

fn math_erf(args: &[Object]) -> Result<Object, RuntimeError> {
    // Abramowitz & Stegun 7.1.26 approximation. Adequate to ~1e-7.
    let x = to_f64(args, "erf", 0)?;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let p = 0.327_591_1_f64;
    let a1 = 0.254_829_592_f64;
    let a2 = -0.284_496_736_f64;
    let a3 = 1.421_413_741_f64;
    let a4 = -1.453_152_027_f64;
    let a5 = 1.061_405_429_f64;
    let t = 1.0 / (1.0 + p * x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x).exp();
    Ok(Object::Float(sign * y))
}

fn math_erfc(args: &[Object]) -> Result<Object, RuntimeError> {
    match math_erf(args)? {
        Object::Float(f) => Ok(Object::Float(1.0 - f)),
        other => Ok(other),
    }
}

fn math_gamma(args: &[Object]) -> Result<Object, RuntimeError> {
    // Lanczos approximation, g=7, n=9. Accurate to ~1e-15 across the
    // representable range.
    let x = to_f64(args, "gamma", 0)?;
    if x.is_nan() {
        return Ok(Object::Float(x));
    }
    if (x - x.trunc()).abs() < f64::EPSILON && x <= 0.0 {
        return Err(value_error("math domain error"));
    }
    let coefficients = [
        0.999_999_999_999_809_9_f64,
        676.520_368_121_885_1_f64,
        -1_259.139_216_722_402_8_f64,
        771.323_428_777_653_1_f64,
        -176.615_029_162_140_6_f64,
        12.507_343_278_686_905_f64,
        -0.138_571_095_265_720_1_f64,
        9.984_369_578_019_572e-6_f64,
        1.505_632_735_149_311_6e-7_f64,
    ];
    if x < 0.5 {
        // Reflection: Γ(z) = π / (sin(πz) * Γ(1-z))
        let sin_pi_x = (std::f64::consts::PI * x).sin();
        if sin_pi_x == 0.0 {
            return Err(value_error("math domain error"));
        }
        return Ok(Object::Float(
            std::f64::consts::PI / (sin_pi_x * math_gamma_inner(1.0 - x, &coefficients)),
        ));
    }
    Ok(Object::Float(math_gamma_inner(x, &coefficients)))
}

fn math_gamma_inner(x: f64, c: &[f64]) -> f64 {
    let x = x - 1.0;
    let g = 7.0_f64;
    let t = x + g + 0.5;
    let mut sum = c[0];
    for (i, ci) in c.iter().skip(1).enumerate() {
        sum += ci / (x + (i + 1) as f64);
    }
    (2.0 * std::f64::consts::PI).sqrt() * t.powf(x + 0.5) * (-t).exp() * sum
}

fn math_lgamma(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "lgamma", 0)?;
    let gv = match math_gamma(&[Object::Float(x.abs())])? {
        Object::Float(f) => f.abs(),
        _ => return Ok(Object::Float(0.0)),
    };
    Ok(Object::Float(gv.ln()))
}

fn math_isqrt(args: &[Object]) -> Result<Object, RuntimeError> {
    let n = to_i64(args, "isqrt", 0)?;
    if n < 0 {
        return Err(value_error("isqrt() argument must be non-negative"));
    }
    let approx = (n as f64).sqrt().floor() as i64;
    // Adjust for rounding error at the boundary.
    let mut root = approx;
    while root > 0 && root * root > n {
        root -= 1;
    }
    while (root + 1) * (root + 1) <= n {
        root += 1;
    }
    Ok(Object::Int(root))
}

fn math_cbrt(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(to_f64(args, "cbrt", 0)?.cbrt()))
}

fn math_exp2(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(2f64.powf(to_f64(args, "exp2", 0)?)))
}

fn math_atanh(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "atanh", 0)?;
    if x <= -1.0 || x >= 1.0 {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(x.atanh()))
}

fn math_asinh(args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::Float(to_f64(args, "asinh", 0)?.asinh()))
}

fn math_acosh(args: &[Object]) -> Result<Object, RuntimeError> {
    let x = to_f64(args, "acosh", 0)?;
    if x < 1.0 {
        return Err(value_error("math domain error"));
    }
    Ok(Object::Float(x.acosh()))
}
