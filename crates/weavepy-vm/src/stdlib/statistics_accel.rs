//! The `_statistics` accelerator module — a faithful port of CPython's
//! `Modules/_statisticsmodule.c`.
//!
//! It exposes a single function, `_normal_dist_inv_cdf(p, mu, sigma)`, the
//! inverse cumulative distribution function for the normal distribution via
//! Wichura's AS241 rational approximation. The verbatim pure-Python
//! `statistics` module ends with `try: from _statistics import
//! _normal_dist_inv_cdf` so `test_statistics`'s `import_fresh_module` C/Py
//! pair (`TestModules.test_c_functions` / `test_py_functions`) exercises this
//! accelerator and the Python fallback side by side — the C function must
//! report `__module__ == '_statistics'`.

use crate::sync::Rc;
use crate::sync::RefCell;

use crate::error::{type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_statistics"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("Accelerators for the statistics module.\n"),
        );
        let f = Object::Builtin(Rc::new(BuiltinFn {
            name: "_normal_dist_inv_cdf",
            binds_instance: false,
            call: Box::new(|a: &[Object]| normal_dist_inv_cdf(a, &[])),
            call_kw: Some(Box::new(normal_dist_inv_cdf)),
        }));
        crate::descr_registry::register_module(&f, "_statistics");
        d.insert(DictKey(Object::from_static("_normal_dist_inv_cdf")), f);
    }
    Rc::new(PyModule {
        name: "_statistics".to_owned(),
        filename: None,
        dict,
    })
}

/// Coerce an argument to `f64` the way the Argument Clinic `double`
/// converter does: exact for `int`/`float`/`bool`, else through `__float__`.
fn to_f64(o: &Object) -> Result<f64, RuntimeError> {
    if let Some(x) = o.as_f64() {
        return Ok(x);
    }
    let ptr = crate::vm_singletons::current_interpreter_ptr()
        .ok_or_else(|| type_error("_statistics: no active interpreter"))?;
    // SAFETY: published by the enclosing VM frame on this thread.
    let interp = unsafe { &mut *ptr };
    let f = interp.call_object(
        Object::Type(crate::builtin_types::builtin_types().float_.clone()),
        std::slice::from_ref(o),
        &[],
    )?;
    f.as_f64().ok_or_else(|| type_error("must be real number"))
}

fn normal_dist_inv_cdf(
    args: &[Object],
    kwargs: &[(String, Object)],
) -> Result<Object, RuntimeError> {
    if !kwargs.is_empty() {
        return Err(type_error(
            "_normal_dist_inv_cdf() takes no keyword arguments",
        ));
    }
    if args.len() != 3 {
        return Err(type_error(format!(
            "_normal_dist_inv_cdf() takes exactly 3 arguments ({} given)",
            args.len()
        )));
    }
    let p = to_f64(&args[0])?;
    let mu = to_f64(&args[1])?;
    let sigma = to_f64(&args[2])?;
    Ok(Object::Float(inv_cdf(p, mu, sigma)))
}

/// Evaluate a polynomial in `r` by Horner's method, coefficients given
/// highest-degree first: `((c0*r + c1)*r + c2)…*r + cn`.
fn horner(coeffs: &[f64], r: f64) -> f64 {
    let mut acc = 0.0;
    for &c in coeffs {
        acc = acc * r + c;
    }
    acc
}

/// Wichura, M.J. (1988), "Algorithm AS241: The Percentage Points of the
/// Normal Distribution" — identical coefficients to the pure-Python
/// `statistics._normal_dist_inv_cdf`. The rational-approximation constants are
/// quoted verbatim from the paper (more digits than an `f64` can hold, which
/// rounds identically), and `x` follows CPython's branch-then-assign form, so
/// the precision/late-init lints are silenced rather than reshaping the
/// reference algorithm.
#[allow(clippy::excessive_precision, clippy::needless_late_init)]
fn inv_cdf(p: f64, mu: f64, sigma: f64) -> f64 {
    let q = p - 0.5;
    if q.abs() <= 0.425 {
        let r = 0.180_625 - q * q;
        let num = horner(
            &[
                2.509_080_928_730_122_672_7e3,
                3.343_057_558_358_812_810_5e4,
                6.726_577_092_700_870_085_3e4,
                4.592_195_393_154_987_145_7e4,
                1.373_169_376_550_946_112_5e4,
                1.971_590_950_306_551_442_7e3,
                1.331_416_678_917_843_774_5e2,
                3.387_132_872_796_366_608_0e0,
            ],
            r,
        ) * q;
        let den = horner(
            &[
                5.226_495_278_852_854_561_0e3,
                2.872_908_573_572_194_267_4e4,
                3.930_789_580_009_271_061_0e4,
                2.121_379_430_158_659_586_7e4,
                5.394_196_021_424_751_107_7e3,
                6.871_870_074_920_579_083_0e2,
                4.231_333_070_160_091_125_2e1,
                1.0,
            ],
            r,
        );
        let x = num / den;
        return mu + (x * sigma);
    }
    let mut r = if q <= 0.0 { p } else { 1.0 - p };
    r = (-r.ln()).sqrt();
    let x;
    if r <= 5.0 {
        r -= 1.6;
        let num = horner(
            &[
                7.745_450_142_783_414_076_4e-4,
                2.272_384_498_926_918_458_33e-2,
                2.417_807_251_774_506_117_70e-1,
                1.270_458_252_452_368_382_58e0,
                3.647_848_324_763_204_605_04e0,
                5.769_497_221_460_691_405_50e0,
                4.630_337_846_156_545_295_90e0,
                1.423_437_110_749_683_577_34e0,
            ],
            r,
        );
        let den = horner(
            &[
                1.050_750_071_644_416_843_24e-9,
                5.475_938_084_995_344_946_00e-4,
                1.519_866_656_361_645_719_66e-2,
                1.481_039_764_274_800_745_90e-1,
                6.897_673_349_851_000_045_50e-1,
                1.676_384_830_183_803_849_40e0,
                2.053_191_626_637_758_821_87e0,
                1.0,
            ],
            r,
        );
        x = num / den;
    } else {
        r -= 5.0;
        let num = horner(
            &[
                2.010_334_399_292_288_132_65e-7,
                2.711_555_568_743_487_578_15e-5,
                1.242_660_947_388_078_438_60e-3,
                2.653_218_952_657_612_309_30e-2,
                2.965_605_718_285_048_912_30e-1,
                1.784_826_539_917_291_335_80e0,
                5.463_784_911_164_114_369_90e0,
                6.657_904_643_501_103_777_20e0,
            ],
            r,
        );
        let den = horner(
            &[
                2.044_263_103_389_939_785_64e-15,
                1.421_511_758_316_445_888_70e-7,
                1.846_318_317_510_054_681_80e-5,
                7.868_691_311_456_132_591_00e-4,
                1.487_536_129_085_061_485_25e-2,
                1.369_298_809_227_358_053_10e-1,
                5.998_322_065_558_879_376_90e-1,
                1.0,
            ],
            r,
        );
        x = num / den;
    }
    let x = if q < 0.0 { -x } else { x };
    mu + (x * sigma)
}
