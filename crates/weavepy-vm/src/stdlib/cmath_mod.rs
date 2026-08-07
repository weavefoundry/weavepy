//! The `cmath` built-in module.
//!
//! Faithful port of CPython 3.13's `Modules/cmathmodule.c`: every
//! function carries over the C source's algorithm, the 7x7
//! special-value tables (indexed over {-inf, -finite, -0, +0,
//! +finite, +inf, nan} for the real and imaginary parts), the
//! `CM_LARGE_DOUBLE`-style overflow-avoidance thresholds, and the
//! `errno` discipline (`EDOM` -> `ValueError("math domain error")`,
//! `ERANGE` -> `OverflowError("math range error")`, exactly like
//! `cmathmodule.c`'s `math_error()`).
//!
//! Being a native module also matches CPython's binding semantics:
//! builtin functions stored as class attributes do not bind as
//! instance methods (`test_cmath.IsCloseTests` relies on
//! `isclose = cmath.isclose` at class level).

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{overflow_error, type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

/// A C `Py_complex` analogue: `(real, imag)`.
type Cx = (f64, f64);

// `errno` codes threaded through the `c_*` functions, mirroring the C
// source's use of the real EDOM/ERANGE (the numeric values are ours).
const OK: i32 = 0;
const EDOM: i32 = 1;
const ERANGE: i32 = 2;

const P: f64 = std::f64::consts::PI;
const P14: f64 = 0.25 * P;
const P12: f64 = 0.5 * P;
const P34: f64 = 0.75 * P;
const INF: f64 = f64::INFINITY;
const N: f64 = f64::NAN;
/// cmathmodule.c's `U`: "unlikely value, used as placeholder" for
/// table slots that are unreachable (finite/finite arguments never
/// consult the tables).
const U: f64 = -9.542_631_940_771_103e33;

const M_LN2: f64 = std::f64::consts::LN_2;
const M_LN10: f64 = std::f64::consts::LN_10;
const M_E: f64 = std::f64::consts::E;

const DBL_MANT_DIG: i32 = 53;
/// C `DBL_MIN` — the smallest positive *normal* double.
const DBL_MIN: f64 = f64::MIN_POSITIVE;
/// `CM_LARGE_DOUBLE`: avoids spurious overflow in sqrt/log/inverse
/// trig/inverse hyperbolic; its log bounds exp/cos/cosh/sin/sinh/
/// tan/tanh.
const CM_LARGE_DOUBLE: f64 = f64::MAX / 4.0;
/// `CM_SCALE_UP`: odd integer such that scaling by `2**CM_SCALE_UP`
/// turns a subnormal into a normal; `CM_SCALE_DOWN` undoes the square
/// root of that scaling (FLT_RADIX == 2).
const CM_SCALE_UP: i32 = 2 * (DBL_MANT_DIG / 2) + 1;
const CM_SCALE_DOWN: i32 = -(CM_SCALE_UP + 1) / 2;

#[inline]
fn cm_sqrt_large_double() -> f64 {
    CM_LARGE_DOUBLE.sqrt()
}

#[inline]
fn cm_log_large_double() -> f64 {
    CM_LARGE_DOUBLE.ln()
}

#[inline]
fn cm_sqrt_dbl_min() -> f64 {
    DBL_MIN.sqrt()
}

use super::math::ldexp;

// ---------------------------------------------------------------------
// Special-value machinery (cmathmodule.c `special_type` /
// `SPECIAL_VALUE`).
// ---------------------------------------------------------------------

// Indices into the special-value tables (enum `special_types`):
// ST_NINF=0, ST_NEG, ST_NZERO, ST_PZERO, ST_POS, ST_PINF, ST_NAN.
fn special_type(d: f64) -> usize {
    if d.is_finite() {
        if d != 0.0 {
            if d.is_sign_positive() {
                4 // ST_POS
            } else {
                1 // ST_NEG
            }
        } else if d.is_sign_positive() {
            3 // ST_PZERO
        } else {
            2 // ST_NZERO
        }
    } else if d.is_nan() {
        6 // ST_NAN
    } else if d.is_sign_positive() {
        5 // ST_PINF
    } else {
        0 // ST_NINF
    }
}

/// The `SPECIAL_VALUE` macro: when either component is non-finite,
/// the result comes straight from the function's table (errno = 0).
fn special_value(z: Cx, table: &[[Cx; 7]; 7]) -> Option<Cx> {
    if !z.0.is_finite() || !z.1.is_finite() {
        Some(table[special_type(z.0)][special_type(z.1)])
    } else {
        None
    }
}

// The tables below are transcribed verbatim from cmathmodule.c's
// `INIT_SPECIAL_VALUES` blocks. Rows are `special_type(z.real)`
// (-inf, -finite, -0, +0, +finite, +inf, nan); columns are
// `special_type(z.imag)` in the same order.

#[rustfmt::skip]
const ACOS_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(P34, INF), (P, INF),   (P, INF),   (P, -INF),   (P, -INF),   (P34, -INF), (N, INF)],
    [(P12, INF), (U, U),     (U, U),     (U, U),      (U, U),      (P12, -INF), (N, N)],
    [(P12, INF), (U, U),     (P12, 0.),  (P12, -0.),  (U, U),      (P12, -INF), (P12, N)],
    [(P12, INF), (U, U),     (P12, 0.),  (P12, -0.),  (U, U),      (P12, -INF), (P12, N)],
    [(P12, INF), (U, U),     (U, U),     (U, U),      (U, U),      (P12, -INF), (N, N)],
    [(P14, INF), (0., INF),  (0., INF),  (0., -INF),  (0., -INF),  (P14, -INF), (N, INF)],
    [(N, INF),   (N, N),     (N, N),     (N, N),      (N, N),      (N, -INF),   (N, N)],
];

#[rustfmt::skip]
const ACOSH_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(INF, -P34), (INF, -P),  (INF, -P),  (INF, P),  (INF, P),  (INF, P34), (INF, N)],
    [(INF, -P12), (U, U),     (U, U),     (U, U),    (U, U),    (INF, P12), (N, N)],
    [(INF, -P12), (U, U),     (0., -P12), (0., P12), (U, U),    (INF, P12), (N, N)],
    [(INF, -P12), (U, U),     (0., -P12), (0., P12), (U, U),    (INF, P12), (N, N)],
    [(INF, -P12), (U, U),     (U, U),     (U, U),    (U, U),    (INF, P12), (N, N)],
    [(INF, -P14), (INF, -0.), (INF, -0.), (INF, 0.), (INF, 0.), (INF, P14), (INF, N)],
    [(INF, N),    (N, N),     (N, N),     (N, N),    (N, N),    (INF, N),   (N, N)],
];

#[rustfmt::skip]
const ASINH_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(-INF, -P14), (-INF, -0.), (-INF, -0.), (-INF, 0.), (-INF, 0.), (-INF, P14), (-INF, N)],
    [(-INF, -P12), (U, U),      (U, U),      (U, U),     (U, U),     (-INF, P12), (N, N)],
    [(-INF, -P12), (U, U),      (-0., -0.),  (-0., 0.),  (U, U),     (-INF, P12), (N, N)],
    [(INF, -P12),  (U, U),      (0., -0.),   (0., 0.),   (U, U),     (INF, P12),  (N, N)],
    [(INF, -P12),  (U, U),      (U, U),      (U, U),     (U, U),     (INF, P12),  (N, N)],
    [(INF, -P14),  (INF, -0.),  (INF, -0.),  (INF, 0.),  (INF, 0.),  (INF, P14),  (INF, N)],
    [(INF, N),     (N, N),      (N, -0.),    (N, 0.),    (N, N),     (INF, N),    (N, N)],
];

#[rustfmt::skip]
const ATANH_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(-0., -P12), (-0., -P12), (-0., -P12), (-0., P12), (-0., P12), (-0., P12), (-0., N)],
    [(-0., -P12), (U, U),      (U, U),      (U, U),     (U, U),     (-0., P12), (N, N)],
    [(-0., -P12), (U, U),      (-0., -0.),  (-0., 0.),  (U, U),     (-0., P12), (-0., N)],
    [(0., -P12),  (U, U),      (0., -0.),   (0., 0.),   (U, U),     (0., P12),  (0., N)],
    [(0., -P12),  (U, U),      (U, U),      (U, U),     (U, U),     (0., P12),  (N, N)],
    [(0., -P12),  (0., -P12),  (0., -P12),  (0., P12),  (0., P12),  (0., P12),  (0., N)],
    [(0., -P12),  (N, N),      (N, N),      (N, N),     (N, N),     (0., P12),  (N, N)],
];

#[rustfmt::skip]
const COSH_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(INF, N), (U, U), (INF, 0.),  (INF, -0.), (U, U), (INF, N), (INF, N)],
    [(N, N),   (U, U), (U, U),     (U, U),     (U, U), (N, N),   (N, N)],
    [(N, 0.),  (U, U), (1., 0.),   (1., -0.),  (U, U), (N, 0.),  (N, 0.)],
    [(N, 0.),  (U, U), (1., -0.),  (1., 0.),   (U, U), (N, 0.),  (N, 0.)],
    [(N, N),   (U, U), (U, U),     (U, U),     (U, U), (N, N),   (N, N)],
    [(INF, N), (U, U), (INF, -0.), (INF, 0.),  (U, U), (INF, N), (INF, N)],
    [(N, N),   (N, N), (N, 0.),    (N, 0.),    (N, N), (N, N),   (N, N)],
];

#[rustfmt::skip]
const EXP_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(0., 0.), (U, U), (0., -0.),  (0., 0.),  (U, U), (0., 0.), (0., 0.)],
    [(N, N),   (U, U), (U, U),     (U, U),    (U, U), (N, N),   (N, N)],
    [(N, N),   (U, U), (1., -0.),  (1., 0.),  (U, U), (N, N),   (N, N)],
    [(N, N),   (U, U), (1., -0.),  (1., 0.),  (U, U), (N, N),   (N, N)],
    [(N, N),   (U, U), (U, U),     (U, U),    (U, U), (N, N),   (N, N)],
    [(INF, N), (U, U), (INF, -0.), (INF, 0.), (U, U), (INF, N), (INF, N)],
    [(N, N),   (N, N), (N, -0.),   (N, 0.),   (N, N), (N, N),   (N, N)],
];

#[rustfmt::skip]
const LOG_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(INF, -P34), (INF, -P),  (INF, -P),   (INF, P),   (INF, P),  (INF, P34), (INF, N)],
    [(INF, -P12), (U, U),     (U, U),      (U, U),     (U, U),    (INF, P12), (N, N)],
    [(INF, -P12), (U, U),     (-INF, -P),  (-INF, P),  (U, U),    (INF, P12), (N, N)],
    [(INF, -P12), (U, U),     (-INF, -0.), (-INF, 0.), (U, U),    (INF, P12), (N, N)],
    [(INF, -P12), (U, U),     (U, U),      (U, U),     (U, U),    (INF, P12), (N, N)],
    [(INF, -P14), (INF, -0.), (INF, -0.),  (INF, 0.),  (INF, 0.), (INF, P14), (INF, N)],
    [(INF, N),    (N, N),     (N, N),      (N, N),     (N, N),    (INF, N),   (N, N)],
];

#[rustfmt::skip]
const SINH_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(INF, N), (U, U), (-INF, -0.), (-INF, 0.), (U, U), (INF, N), (INF, N)],
    [(N, N),   (U, U), (U, U),      (U, U),     (U, U), (N, N),   (N, N)],
    [(0., N),  (U, U), (-0., -0.),  (-0., 0.),  (U, U), (0., N),  (0., N)],
    [(0., N),  (U, U), (0., -0.),   (0., 0.),   (U, U), (0., N),  (0., N)],
    [(N, N),   (U, U), (U, U),      (U, U),     (U, U), (N, N),   (N, N)],
    [(INF, N), (U, U), (INF, -0.),  (INF, 0.),  (U, U), (INF, N), (INF, N)],
    [(N, N),   (N, N), (N, -0.),    (N, 0.),    (N, N), (N, N),   (N, N)],
];

#[rustfmt::skip]
const SQRT_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(INF, -INF), (0., -INF), (0., -INF), (0., INF), (0., INF), (INF, INF), (N, INF)],
    [(INF, -INF), (U, U),     (U, U),     (U, U),    (U, U),    (INF, INF), (N, N)],
    [(INF, -INF), (U, U),     (0., -0.),  (0., 0.),  (U, U),    (INF, INF), (N, N)],
    [(INF, -INF), (U, U),     (0., -0.),  (0., 0.),  (U, U),    (INF, INF), (N, N)],
    [(INF, -INF), (U, U),     (U, U),     (U, U),    (U, U),    (INF, INF), (N, N)],
    [(INF, -INF), (INF, -0.), (INF, -0.), (INF, 0.), (INF, 0.), (INF, INF), (INF, N)],
    [(INF, -INF), (N, N),     (N, N),     (N, N),    (N, N),    (INF, INF), (N, N)],
];

#[rustfmt::skip]
const TANH_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(-1., 0.), (U, U), (-1., -0.), (-1., 0.), (U, U), (-1., 0.), (-1., 0.)],
    [(N, N),    (U, U), (U, U),     (U, U),    (U, U), (N, N),    (N, N)],
    [(N, N),    (U, U), (-0., -0.), (-0., 0.), (U, U), (N, N),    (N, N)],
    [(N, N),    (U, U), (0., -0.),  (0., 0.),  (U, U), (N, N),    (N, N)],
    [(N, N),    (U, U), (U, U),     (U, U),    (U, U), (N, N),    (N, N)],
    [(1., 0.),  (U, U), (1., -0.),  (1., 0.),  (U, U), (1., 0.),  (1., 0.)],
    [(N, N),    (N, N), (N, -0.),   (N, 0.),   (N, N), (N, N),    (N, N)],
];

#[rustfmt::skip]
const RECT_SPECIAL_VALUES: [[Cx; 7]; 7] = [
    [(INF, N), (U, U), (-INF, 0.), (-INF, -0.), (U, U), (INF, N), (INF, N)],
    [(N, N),   (U, U), (U, U),     (U, U),      (U, U), (N, N),   (N, N)],
    [(0., 0.), (U, U), (-0., 0.),  (-0., -0.),  (U, U), (0., 0.), (0., 0.)],
    [(0., 0.), (U, U), (0., -0.),  (0., 0.),    (U, U), (0., 0.), (0., 0.)],
    [(N, N),   (U, U), (U, U),     (U, U),      (U, U), (N, N),   (N, N)],
    [(INF, N), (U, U), (INF, -0.), (INF, 0.),   (U, U), (INF, N), (INF, N)],
    [(N, N),   (N, N), (N, 0.),    (N, 0.),     (N, N), (N, N),   (N, N)],
];

// ---------------------------------------------------------------------
// The `c_*` workers. Each computes the C99 Annex G recommended result
// and returns the errno cmathmodule.c would set: 0 for no exception,
// EDOM where Annex G recommends divide-by-zero/invalid, ERANGE where
// the overflow signal should be raised.
// ---------------------------------------------------------------------

/// cmathmodule.c `cmath_sqrt_impl`.
fn c_sqrt(z: Cx) -> (Cx, i32) {
    if let Some(r) = special_value(z, &SQRT_SPECIAL_VALUES) {
        return (r, OK);
    }
    if z.0 == 0.0 && z.1 == 0.0 {
        return ((0.0, z.1), OK);
    }
    let mut ax = z.0.abs();
    let ay = z.1.abs();
    let s = if ax < DBL_MIN && ay < DBL_MIN {
        // Catch cases where hypot(ax, ay) is subnormal: rescale into
        // the normal range, take the root, and scale back down.
        ax = ldexp(ax, CM_SCALE_UP);
        ldexp(
            (ax + ax.hypot(ldexp(ay, CM_SCALE_UP))).sqrt(),
            CM_SCALE_DOWN,
        )
    } else {
        // s = 2*sqrt(x/8 + hypot(x/8, y/8)) avoids overflow in
        // x + hypot(x, y) for large x/y.
        ax /= 8.0;
        2.0 * (ax + ax.hypot(ay / 8.0)).sqrt()
    };
    let d = ay / (2.0 * s);
    let r = if z.0 >= 0.0 {
        (s, d.copysign(z.1))
    } else {
        (d, s.copysign(z.1))
    };
    (r, OK)
}

/// cmathmodule.c `cmath_acos_impl`.
fn c_acos(z: Cx) -> (Cx, i32) {
    if let Some(r) = special_value(z, &ACOS_SPECIAL_VALUES) {
        return (r, OK);
    }
    let r = if z.0.abs() > CM_LARGE_DOUBLE || z.1.abs() > CM_LARGE_DOUBLE {
        // Avoid unnecessary overflow for large arguments; the branch
        // split keeps the branch cut's continuity with signed zeros.
        let re = z.1.abs().atan2(z.0);
        let mag = (z.0 / 2.0).hypot(z.1 / 2.0).ln() + M_LN2 * 2.0;
        let im = if z.0 < 0.0 {
            -mag.copysign(z.1)
        } else {
            mag.copysign(-z.1)
        };
        (re, im)
    } else {
        let (s1, _) = c_sqrt((1.0 - z.0, -z.1));
        let (s2, _) = c_sqrt((1.0 + z.0, z.1));
        (2.0 * s1.0.atan2(s2.0), (s2.0 * s1.1 - s2.1 * s1.0).asinh())
    };
    (r, OK)
}

/// cmathmodule.c `cmath_acosh_impl`.
fn c_acosh(z: Cx) -> (Cx, i32) {
    if let Some(r) = special_value(z, &ACOSH_SPECIAL_VALUES) {
        return (r, OK);
    }
    let r = if z.0.abs() > CM_LARGE_DOUBLE || z.1.abs() > CM_LARGE_DOUBLE {
        (
            (z.0 / 2.0).hypot(z.1 / 2.0).ln() + M_LN2 * 2.0,
            z.1.atan2(z.0),
        )
    } else {
        let (s1, _) = c_sqrt((z.0 - 1.0, z.1));
        let (s2, _) = c_sqrt((z.0 + 1.0, z.1));
        ((s1.0 * s2.0 + s1.1 * s2.1).asinh(), 2.0 * s1.1.atan2(s2.0))
    };
    (r, OK)
}

/// cmathmodule.c `cmath_asin_impl`: asin(z) = -i asinh(iz).
fn c_asin(z: Cx) -> (Cx, i32) {
    let (s, errno) = c_asinh((-z.1, z.0));
    ((s.1, -s.0), errno)
}

/// cmathmodule.c `cmath_asinh_impl`.
fn c_asinh(z: Cx) -> (Cx, i32) {
    if let Some(r) = special_value(z, &ASINH_SPECIAL_VALUES) {
        return (r, OK);
    }
    let r = if z.0.abs() > CM_LARGE_DOUBLE || z.1.abs() > CM_LARGE_DOUBLE {
        let mag = (z.0 / 2.0).hypot(z.1 / 2.0).ln() + M_LN2 * 2.0;
        let re = if z.1 >= 0.0 {
            mag.copysign(z.0)
        } else {
            -mag.copysign(-z.0)
        };
        (re, z.1.atan2(z.0.abs()))
    } else {
        let (s1, _) = c_sqrt((1.0 + z.1, -z.0));
        let (s2, _) = c_sqrt((1.0 - z.1, z.0));
        (
            (s1.0 * s2.1 - s2.0 * s1.1).asinh(),
            z.1.atan2(s1.0 * s2.0 - s1.1 * s2.1),
        )
    };
    (r, OK)
}

/// cmathmodule.c `cmath_atan_impl`: atan(z) = -i atanh(iz).
fn c_atan(z: Cx) -> (Cx, i32) {
    let (s, errno) = c_atanh((-z.1, z.0));
    ((s.1, -s.0), errno)
}

/// cmathmodule.c `cmath_atanh_impl`.
fn c_atanh(z: Cx) -> (Cx, i32) {
    if let Some(r) = special_value(z, &ATANH_SPECIAL_VALUES) {
        return (r, OK);
    }
    // Reduce to the case z.real >= 0 via atanh(z) = -atanh(-z).
    if z.0 < 0.0 {
        let (r, errno) = c_atanh((-z.0, -z.1));
        return ((-r.0, -r.1), errno);
    }
    let ay = z.1.abs();
    if z.0 > cm_sqrt_large_double() || ay > cm_sqrt_large_double() {
        // For large |z|, atanh(z) ~ 1/z +/- i*pi/2; the double
        // negation below keeps the branch cut's continuity for
        // unsigned-zero platforms (a no-op with signed zeros).
        let h = (z.0 / 2.0).hypot(z.1 / 2.0); // safe from overflow
        let re = z.0 / 4.0 / h / h;
        let im = -(P12.copysign(-z.1));
        ((re, im), OK)
    } else if z.0 == 1.0 && ay < cm_sqrt_dbl_min() {
        if ay == 0.0 {
            // C99: atanh(1 +/- 0i) is inf +/- 0i, with divide-by-zero.
            ((INF, z.1), EDOM)
        } else {
            (
                (
                    -(ay.sqrt() / ay.hypot(2.0).sqrt()).ln(),
                    (2.0_f64.atan2(-ay) / 2.0).copysign(z.1),
                ),
                OK,
            )
        }
    } else {
        (
            (
                (4.0 * z.0 / ((1.0 - z.0) * (1.0 - z.0) + ay * ay)).ln_1p() / 4.0,
                -(-2.0 * z.1).atan2((1.0 - z.0) * (1.0 + z.0) - ay * ay) / 2.0,
            ),
            OK,
        )
    }
}

/// cmathmodule.c `cmath_cos_impl`: cos(z) = cosh(iz).
fn c_cos(z: Cx) -> (Cx, i32) {
    c_cosh((-z.1, z.0))
}

/// cmathmodule.c `cmath_cosh_impl`.
fn c_cosh(z: Cx) -> (Cx, i32) {
    // Special treatment for cosh(+/-inf + iy) when y is finite nonzero.
    if !z.0.is_finite() || !z.1.is_finite() {
        let r = if z.0.is_infinite() && z.1.is_finite() && z.1 != 0.0 {
            if z.0 > 0.0 {
                (INF.copysign(z.1.cos()), INF.copysign(z.1.sin()))
            } else {
                (INF.copysign(z.1.cos()), -INF.copysign(z.1.sin()))
            }
        } else {
            COSH_SPECIAL_VALUES[special_type(z.0)][special_type(z.1)]
        };
        // EDOM if y is +/-infinity and x is not a NaN.
        let errno = if z.1.is_infinite() && !z.0.is_nan() {
            EDOM
        } else {
            OK
        };
        return (r, errno);
    }
    let r = if z.0.abs() > cm_log_large_double() {
        // cosh(z.real) would overflow even though cosh(z) may not:
        // pull one factor of e out of cosh/sinh.
        let x_minus_one = z.0 - 1.0_f64.copysign(z.0);
        (
            z.1.cos() * x_minus_one.cosh() * M_E,
            z.1.sin() * x_minus_one.sinh() * M_E,
        )
    } else {
        (z.1.cos() * z.0.cosh(), z.1.sin() * z.0.sinh())
    };
    let errno = if r.0.is_infinite() || r.1.is_infinite() {
        ERANGE
    } else {
        OK
    };
    (r, errno)
}

/// cmathmodule.c `cmath_exp_impl`.
fn c_exp(z: Cx) -> (Cx, i32) {
    if !z.0.is_finite() || !z.1.is_finite() {
        let r = if z.0.is_infinite() && z.1.is_finite() && z.1 != 0.0 {
            if z.0 > 0.0 {
                (INF.copysign(z.1.cos()), INF.copysign(z.1.sin()))
            } else {
                (0.0_f64.copysign(z.1.cos()), 0.0_f64.copysign(z.1.sin()))
            }
        } else {
            EXP_SPECIAL_VALUES[special_type(z.0)][special_type(z.1)]
        };
        // EDOM if y is +/-infinity and x is not a NaN and not -infinity.
        let errno = if z.1.is_infinite() && (z.0.is_finite() || (z.0.is_infinite() && z.0 > 0.0)) {
            EDOM
        } else {
            OK
        };
        return (r, errno);
    }
    let r = if z.0 > cm_log_large_double() {
        let l = (z.0 - 1.0).exp();
        (l * z.1.cos() * M_E, l * z.1.sin() * M_E)
    } else {
        let l = z.0.exp();
        (l * z.1.cos(), l * z.1.sin())
    };
    let errno = if r.0.is_infinite() || r.1.is_infinite() {
        ERANGE
    } else {
        OK
    };
    (r, errno)
}

/// cmathmodule.c `c_log`: the shared core of `log`/`log10`, with the
/// subnormal rescaling and |z|-near-1 `log1p` accuracy fixups.
fn c_log(z: Cx) -> (Cx, i32) {
    if let Some(r) = special_value(z, &LOG_SPECIAL_VALUES) {
        return (r, OK);
    }
    let ax = z.0.abs();
    let ay = z.1.abs();
    let re = if ax > CM_LARGE_DOUBLE || ay > CM_LARGE_DOUBLE {
        (ax / 2.0).hypot(ay / 2.0).ln() + M_LN2
    } else if ax < DBL_MIN && ay < DBL_MIN {
        if ax > 0.0 || ay > 0.0 {
            // Catch cases where hypot(ax, ay) is subnormal.
            ldexp(ax, DBL_MANT_DIG).hypot(ldexp(ay, DBL_MANT_DIG)).ln()
                - f64::from(DBL_MANT_DIG) * M_LN2
        } else {
            // log(+/-0. +/- 0i): divide-by-zero.
            return ((-INF, z.1.atan2(z.0)), EDOM);
        }
    } else {
        let h = ax.hypot(ay);
        if (0.71..=1.73).contains(&h) {
            let am = if ax > ay { ax } else { ay };
            let an = if ax > ay { ay } else { ax };
            ((am - 1.0) * (am + 1.0) + an * an).ln_1p() / 2.0
        } else {
            h.ln()
        }
    };
    ((re, z.1.atan2(z.0)), OK)
}

/// cmathmodule.c `cmath_sin_impl`: sin(z) = -i sinh(iz).
fn c_sin(z: Cx) -> (Cx, i32) {
    let (s, errno) = c_sinh((-z.1, z.0));
    ((s.1, -s.0), errno)
}

/// cmathmodule.c `cmath_sinh_impl`.
fn c_sinh(z: Cx) -> (Cx, i32) {
    if !z.0.is_finite() || !z.1.is_finite() {
        let r = if z.0.is_infinite() && z.1.is_finite() && z.1 != 0.0 {
            if z.0 > 0.0 {
                (INF.copysign(z.1.cos()), INF.copysign(z.1.sin()))
            } else {
                (-INF.copysign(z.1.cos()), INF.copysign(z.1.sin()))
            }
        } else {
            SINH_SPECIAL_VALUES[special_type(z.0)][special_type(z.1)]
        };
        let errno = if z.1.is_infinite() && !z.0.is_nan() {
            EDOM
        } else {
            OK
        };
        return (r, errno);
    }
    let r = if z.0.abs() > cm_log_large_double() {
        let x_minus_one = z.0 - 1.0_f64.copysign(z.0);
        (
            z.1.cos() * x_minus_one.sinh() * M_E,
            z.1.sin() * x_minus_one.cosh() * M_E,
        )
    } else {
        (z.1.cos() * z.0.sinh(), z.1.sin() * z.0.cosh())
    };
    let errno = if r.0.is_infinite() || r.1.is_infinite() {
        ERANGE
    } else {
        OK
    };
    (r, errno)
}

/// cmathmodule.c `cmath_tan_impl`: tan(z) = -i tanh(iz).
fn c_tan(z: Cx) -> (Cx, i32) {
    let (s, errno) = c_tanh((-z.1, z.0));
    ((s.1, -s.0), errno)
}

/// cmathmodule.c `cmath_tanh_impl`.
fn c_tanh(z: Cx) -> (Cx, i32) {
    if !z.0.is_finite() || !z.1.is_finite() {
        let r = if z.0.is_infinite() && z.1.is_finite() && z.1 != 0.0 {
            if z.0 > 0.0 {
                (1.0, 0.0_f64.copysign(2.0 * z.1.sin() * z.1.cos()))
            } else {
                (-1.0, 0.0_f64.copysign(2.0 * z.1.sin() * z.1.cos()))
            }
        } else {
            TANH_SPECIAL_VALUES[special_type(z.0)][special_type(z.1)]
        };
        // EDOM if z.imag is +/-infinity and z.real is finite.
        let errno = if z.1.is_infinite() && z.0.is_finite() {
            EDOM
        } else {
            OK
        };
        return (r, errno);
    }
    let r = if z.0.abs() > cm_log_large_double() {
        // Approximate 1-tanh(x)^2 by 4 exp(-2*|x|) to dodge overflow
        // in cosh(x) (and the danger of overflow in 2*z.imag).
        (
            1.0_f64.copysign(z.0),
            4.0 * z.1.sin() * z.1.cos() * (-2.0 * z.0.abs()).exp(),
        )
    } else {
        let tx = z.0.tanh();
        let ty = z.1.tan();
        let cx = 1.0 / z.0.cosh();
        let txty = tx * ty;
        let denom = 1.0 + txty * txty;
        (tx * (1.0 + ty * ty) / denom, ((ty / denom) * cx) * cx)
    };
    (r, OK)
}

/// complexobject.c `_Py_c_quot`, used by two-argument `log`. The
/// returned errno is EDOM for division by (exact) zero, otherwise the
/// caller's errno is left alone (represented here by returning OK).
fn c_quot(a: Cx, b: Cx) -> (Cx, i32) {
    let abs_breal = b.0.abs();
    let abs_bimag = b.1.abs();
    if abs_breal >= abs_bimag {
        // Divide tops and bottom by b.real.
        if abs_breal == 0.0 {
            ((0.0, 0.0), EDOM)
        } else {
            let ratio = b.1 / b.0;
            let denom = b.0 + b.1 * ratio;
            (
                ((a.0 + a.1 * ratio) / denom, (a.1 - a.0 * ratio) / denom),
                OK,
            )
        }
    } else if abs_bimag >= abs_breal {
        // Divide tops and bottom by b.imag.
        let ratio = b.0 / b.1;
        let denom = b.0 * ratio + b.1;
        (
            ((a.0 * ratio + a.1) / denom, (a.1 * ratio - a.0) / denom),
            OK,
        )
    } else {
        // At least one of b.real or b.imag is a NaN.
        ((N, N), OK)
    }
}

/// cmathmodule.c `c_atan2`: a C99-correct atan2 over the complex
/// components, immune to platform quirks for inf/nan/zero operands.
fn c_atan2(z: Cx) -> f64 {
    if z.0.is_nan() || z.1.is_nan() {
        return N;
    }
    if z.1.is_infinite() {
        if z.0.is_infinite() {
            if z.0.is_sign_positive() {
                return P14.copysign(z.1); // atan2(+-inf, +inf)
            }
            return P34.copysign(z.1); // atan2(+-inf, -inf)
        }
        return P12.copysign(z.1); // atan2(+-inf, finite x)
    }
    if z.0.is_infinite() || z.1 == 0.0 {
        if z.0.is_sign_positive() {
            return 0.0_f64.copysign(z.1); // atan2(+-y, +inf), atan2(+-0, +x)
        }
        return P.copysign(z.1); // atan2(+-y, -inf), atan2(+-0, -x)
    }
    z.1.atan2(z.0)
}

/// complexobject.c `_Py_c_abs`: if either component is infinite the
/// result is +inf even when the other is a NaN (C99); overflow of the
/// hypot of two finite components is ERANGE.
fn c_abs(z: Cx) -> (f64, i32) {
    if !z.0.is_finite() || !z.1.is_finite() {
        if z.0.is_infinite() {
            return (z.0.abs(), OK);
        }
        if z.1.is_infinite() {
            return (z.1.abs(), OK);
        }
        return (N, OK);
    }
    let result = z.0.hypot(z.1);
    if !result.is_finite() {
        (result, ERANGE)
    } else {
        (result, OK)
    }
}

// ---------------------------------------------------------------------
// Python-facing glue.
// ---------------------------------------------------------------------

/// cmathmodule.c `math_error`: EDOM -> ValueError, ERANGE ->
/// OverflowError, with CPython's exact messages.
fn math_error(errno: i32) -> Result<(), RuntimeError> {
    match errno {
        OK => Ok(()),
        EDOM => Err(value_error("math domain error")),
        _ => Err(overflow_error("math range error")),
    }
}

/// `PyComplex_AsCComplex` over an `Object`: exact complex values pass
/// through; instances and foreign scalars dispatch `__complex__` then
/// `__float__`/`__index__` via interpreter reentry (the same coercion
/// `complex()` uses); other reals coerce through the float protocol.
/// Anything else raises `PyFloat_AsDouble`'s TypeError.
fn to_complex(o: &Object, func: &str) -> Result<Cx, RuntimeError> {
    if let Object::Complex(c) = o {
        return Ok((c.real, c.imag));
    }
    if matches!(o, Object::Instance(_) | Object::Foreign(_)) {
        if let Some(ptr) = crate::vm_singletons::current_interpreter_ptr() {
            // SAFETY: the pointer was published by an enclosing VM frame
            // still live on this thread; the GIL keeps the access exclusive.
            let interp = unsafe { &mut *ptr };
            let globals = interp.builtins_dict();
            let r = interp.coerce_complex_arg(o, true, &globals)?;
            if let Object::Complex(c) = &r {
                return Ok((c.real, c.imag));
            }
            return match crate::builtins::coerce_f64_opt(&r)? {
                Some(f) => Ok((f, 0.0)),
                None => Err(type_error(format!(
                    "{func}() argument must be a number, not '{}'",
                    o.type_name()
                ))),
            };
        }
    }
    match crate::builtins::coerce_f64_opt(o)? {
        Some(f) => Ok((f, 0.0)),
        None => Err(type_error(format!(
            "must be real number, not {}",
            o.type_name()
        ))),
    }
}

/// Coerce a real-valued argument (`rect`'s operands, `isclose`
/// tolerances) with `PyFloat_AsDouble` semantics.
fn to_f64(o: &Object, what: &str) -> Result<f64, RuntimeError> {
    match crate::builtins::coerce_f64_opt(o)? {
        Some(f) => Ok(f),
        None => Err(type_error(format!(
            "{what} must be a real number, not '{}'",
            o.type_name()
        ))),
    }
}

/// Enforce an exact positional arity (clinic-style TypeError).
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

/// Wrap a `f64` result the way `PyFloat_FromDouble` does: a fresh
/// object per call, so NaN results never alias an input's identity
/// tag (see [`crate::object::fresh_float`]).
fn float_obj(f: f64) -> Object {
    if f.is_nan() {
        crate::object::fresh_float(f)
    } else {
        Object::Float(f)
    }
}

/// Register a unary complex -> complex worker under `name`.
fn make_unary(name: &'static str, f: fn(Cx) -> (Cx, i32)) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(move |args: &[Object]| {
            expect_nargs(args, name, 1)?;
            let z = to_complex(&args[0], name)?;
            let (r, errno) = f(z);
            math_error(errno)?;
            Ok(Object::new_complex(r.0, r.1))
        }),
        call_kw: None,
    }))
}

fn builtin(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        binds_instance: false,
        call: Box::new(body),
        call_kw: None,
    }))
}

/// `cmath.log(z[, base])` — cmathmodule.c `cmath_log_impl`, including
/// its exact errno flow: `c_log(base)` runs *after* (and its errno
/// supersedes) `c_log(z)`'s, and `_Py_c_quot` flags division by a
/// zero log (base 1).
fn cmath_log(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.is_empty() || args.len() > 2 {
        return Err(type_error(format!(
            "log expected 1 to 2 arguments, got {}",
            args.len()
        )));
    }
    let z = to_complex(&args[0], "log")?;
    let (mut x, mut errno) = c_log(z);
    if let Some(base) = args.get(1) {
        let y = to_complex(base, "log")?;
        let (ly, e_base) = c_log(y);
        errno = e_base;
        let (q, e_quot) = c_quot(x, ly);
        if e_quot != OK {
            errno = e_quot;
        }
        x = q;
    }
    math_error(errno)?;
    Ok(Object::new_complex(x.0, x.1))
}

/// `cmath.log10(z)` — `c_log` scaled by 1/ln(10), errno preserved.
fn cmath_log10(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "log10", 1)?;
    let z = to_complex(&args[0], "log10")?;
    let (r, errno) = c_log(z);
    math_error(errno)?;
    Ok(Object::new_complex(r.0 / M_LN10, r.1 / M_LN10))
}

/// `cmath.phase(z)` — cmathmodule.c `cmath_phase_impl` (`c_atan2`
/// never sets errno, so this cannot raise past coercion).
fn cmath_phase(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "phase", 1)?;
    let z = to_complex(&args[0], "phase")?;
    Ok(float_obj(c_atan2(z)))
}

/// `cmath.polar(z)` — cmathmodule.c `cmath_polar_impl`; `_Py_c_abs`
/// overflow surfaces as OverflowError.
fn cmath_polar(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "polar", 1)?;
    let z = to_complex(&args[0], "polar")?;
    let phi = c_atan2(z);
    let (r, errno) = c_abs(z);
    math_error(errno)?;
    Ok(Object::new_tuple(vec![float_obj(r), float_obj(phi)]))
}

/// `cmath.rect(r, phi)` — cmathmodule.c `cmath_rect_impl`, including
/// the special-value table (rect isn't covered by C99; this is the
/// "spirit of C99" table from the C source) and the phi == 0.0
/// workaround for buggy platform cos/sin at -0.0.
fn cmath_rect(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "rect", 2)?;
    let r = to_f64(&args[0], "rect() argument 'r'")?;
    let phi = to_f64(&args[1], "rect() argument 'phi'")?;
    let (z, errno) = if !r.is_finite() || !phi.is_finite() {
        // If r is +/-inf and phi is finite nonzero, the result is
        // (+-inf +- inf i) with signs from cos(phi)/sin(phi).
        let z = if r.is_infinite() && phi.is_finite() && phi != 0.0 {
            if r > 0.0 {
                (INF.copysign(phi.cos()), INF.copysign(phi.sin()))
            } else {
                (-INF.copysign(phi.cos()), -INF.copysign(phi.sin()))
            }
        } else {
            RECT_SPECIAL_VALUES[special_type(r)][special_type(phi)]
        };
        // EDOM if r is a nonzero number and phi is infinite.
        let errno = if r != 0.0 && !r.is_nan() && phi.is_infinite() {
            EDOM
        } else {
            OK
        };
        (z, errno)
    } else if phi == 0.0 {
        // r*phi (not a bare copy of phi's sign) — the workaround for
        // buggy cos/sin results with phi = -0.0 (bpo-18513).
        ((r, r * phi), OK)
    } else {
        ((r * phi.cos(), r * phi.sin()), OK)
    };
    math_error(errno)?;
    Ok(Object::new_complex(z.0, z.1))
}

fn cmath_isfinite(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "isfinite", 1)?;
    let z = to_complex(&args[0], "isfinite")?;
    Ok(Object::Bool(z.0.is_finite() && z.1.is_finite()))
}

fn cmath_isnan(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "isnan", 1)?;
    let z = to_complex(&args[0], "isnan")?;
    Ok(Object::Bool(z.0.is_nan() || z.1.is_nan()))
}

fn cmath_isinf(args: &[Object]) -> Result<Object, RuntimeError> {
    expect_nargs(args, "isinf", 1)?;
    let z = to_complex(&args[0], "isinf")?;
    Ok(Object::Bool(z.0.is_infinite() || z.1.is_infinite()))
}

/// `cmath.isclose(a, b, *, rel_tol=1e-09, abs_tol=0.0)` — faithful
/// port of `cmath_isclose_impl`: the bit-exact equality fast path
/// (two same-signed infinities compare close), the any-infinite ->
/// False short circuit, and the "weak" symmetric test over `|a-b|`.
/// Tolerances are real numbers (complex tolerances are a TypeError).
fn cmath_isclose(args: &[Object], kwargs: &[(String, Object)]) -> Result<Object, RuntimeError> {
    if args.len() > 2 {
        return Err(type_error(format!(
            "isclose() takes at most 2 positional arguments ({} given)",
            args.len()
        )));
    }
    let mut a_obj = args.first().cloned();
    let mut b_obj = args.get(1).cloned();
    let mut rel_tol = 1e-9_f64;
    let mut abs_tol = 0.0_f64;
    for (key, value) in kwargs {
        match key.as_str() {
            "a" => {
                if a_obj.is_some() {
                    return Err(type_error("isclose() got multiple values for argument 'a'"));
                }
                a_obj = Some(value.clone());
            }
            "b" => {
                if b_obj.is_some() {
                    return Err(type_error("isclose() got multiple values for argument 'b'"));
                }
                b_obj = Some(value.clone());
            }
            "rel_tol" => rel_tol = to_f64(value, "isclose() rel_tol")?,
            "abs_tol" => abs_tol = to_f64(value, "isclose() abs_tol")?,
            other => {
                return Err(type_error(format!(
                    "isclose() got an unexpected keyword argument '{other}'"
                )))
            }
        }
    }
    let a = to_complex(
        &a_obj.ok_or_else(|| type_error("isclose() missing required argument 'a' (pos 1)"))?,
        "isclose",
    )?;
    let b = to_complex(
        &b_obj.ok_or_else(|| type_error("isclose() missing required argument 'b' (pos 2)"))?,
        "isclose",
    )?;
    if rel_tol < 0.0 || abs_tol < 0.0 {
        return Err(value_error("tolerances must be non-negative"));
    }
    #[allow(clippy::float_cmp)]
    if a.0 == b.0 && a.1 == b.1 {
        return Ok(Object::Bool(true));
    }
    if a.0.is_infinite() || a.1.is_infinite() || b.0.is_infinite() || b.1.is_infinite() {
        return Ok(Object::Bool(false));
    }
    let (diff, _) = c_abs((a.0 - b.0, a.1 - b.1));
    let result = diff <= rel_tol * c_abs(b).0 || diff <= rel_tol * c_abs(a).0 || diff <= abs_tol;
    Ok(Object::Bool(result))
}

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("cmath"),
        );
        d.insert(
            DictKey(Object::from_static("__package__")),
            Object::from_static(""),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static(
                "This module provides access to mathematical functions for complex\nnumbers.",
            ),
        );

        // Constants — mirroring cmathmodule.c's `cmath_exec`.
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
        d.insert(
            DictKey(Object::from_static("infj")),
            Object::new_complex(0.0, f64::INFINITY),
        );
        // Positive (sign-bit-clear) NaN, minted once — matching the
        // `math.nan` identity discipline (see stdlib/math.rs).
        d.insert(
            DictKey(Object::from_static("nan")),
            crate::object::fresh_float(f64::NAN.abs()),
        );
        d.insert(
            DictKey(Object::from_static("nanj")),
            Object::new_complex(0.0, f64::NAN.abs()),
        );

        for (name, f) in [
            ("acos", c_acos as fn(Cx) -> (Cx, i32)),
            ("acosh", c_acosh),
            ("asin", c_asin),
            ("asinh", c_asinh),
            ("atan", c_atan),
            ("atanh", c_atanh),
            ("cos", c_cos),
            ("cosh", c_cosh),
            ("exp", c_exp),
            ("sin", c_sin),
            ("sinh", c_sinh),
            ("sqrt", c_sqrt),
            ("tan", c_tan),
            ("tanh", c_tanh),
        ] {
            d.insert(DictKey(Object::from_static(name)), make_unary(name, f));
        }

        d.insert(
            DictKey(Object::from_static("log")),
            builtin("log", cmath_log),
        );
        d.insert(
            DictKey(Object::from_static("log10")),
            builtin("log10", cmath_log10),
        );
        d.insert(
            DictKey(Object::from_static("phase")),
            builtin("phase", cmath_phase),
        );
        d.insert(
            DictKey(Object::from_static("polar")),
            builtin("polar", cmath_polar),
        );
        d.insert(
            DictKey(Object::from_static("rect")),
            builtin("rect", cmath_rect),
        );
        d.insert(
            DictKey(Object::from_static("isfinite")),
            builtin("isfinite", cmath_isfinite),
        );
        d.insert(
            DictKey(Object::from_static("isnan")),
            builtin("isnan", cmath_isnan),
        );
        d.insert(
            DictKey(Object::from_static("isinf")),
            builtin("isinf", cmath_isinf),
        );
        d.insert(
            DictKey(Object::from_static("isclose")),
            Object::Builtin(Rc::new(BuiltinFn::with_kwargs("isclose", cmath_isclose))),
        );
    }
    Rc::new(PyModule {
        name: "cmath".to_owned(),
        filename: None,
        dict,
    })
}
