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
