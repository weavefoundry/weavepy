//! Exact integer normalization and calendar arithmetic for Python datetime.

use num_traits::ToPrimitive;

use crate::error::{overflow_error, type_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};
use crate::sync::{Rc, RefCell};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let mut dict = DictData::default();
    dict.insert(
        DictKey(Object::from_static("__name__")),
        Object::from_static("_weave_datetime"),
    );
    type Helper = fn(&[Object]) -> Result<Object, RuntimeError>;
    for (name, call) in [
        ("timedelta_parts", timedelta_parts as Helper),
        ("ymd_toordinal", ymd_toordinal as Helper),
        ("ordinal_toymd", ordinal_toymd as Helper),
    ] {
        let function = Object::Builtin(Rc::new(BuiltinFn {
            name,
            binds_instance: false,
            call: Box::new(call),
            call_kw: None,
        }));
        crate::descr_registry::register_module(&function, "_weave_datetime");
        dict.insert(DictKey(Object::from_static(name)), function);
    }
    Rc::new(PyModule {
        name: "_weave_datetime".to_owned(),
        filename: None,
        dict: Rc::new(RefCell::new(dict)),
    })
}

const DAYS_BEFORE_MONTH: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
const DAYS_IN_MONTH: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn ymd_toordinal(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Int(year), Object::Int(month), Object::Int(day)] = args else {
        return Ok(Object::None);
    };
    // The Python wrapper retains validation and arbitrary-integer behavior
    // outside the public date range. No subclass arithmetic is bypassed.
    if !(1..=9999).contains(year) || !(1..=12).contains(month) {
        return Ok(Object::None);
    }
    let leap = is_leap(*year);
    let m = (*month - 1) as usize;
    if !(1..=DAYS_IN_MONTH[m] + i64::from(*month == 2 && leap)).contains(day) {
        return Ok(Object::None);
    }
    let y = year - 1;
    Ok(Object::Int(
        y * 365 + y / 4 - y / 100
            + y / 400
            + DAYS_BEFORE_MONTH[m]
            + i64::from(*month > 2 && leap)
            + day,
    ))
}

fn ordinal_toymd(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Int(n)] = args else {
        return Ok(Object::None);
    };
    if !(1..=3_652_059).contains(n) {
        return Ok(Object::None);
    }
    // Split the ordinal into Gregorian 400-, 100-, four-, and one-year
    // cycles, including the last day of a cycle as a special case.
    let n = n - 1;
    let n400 = n / 146_097;
    let n = n % 146_097;
    let n100 = n / 36_524;
    let n = n % 36_524;
    let n4 = n / 1461;
    let n = n % 1461;
    let n1 = n / 365;
    let mut remaining = n % 365;
    let mut year = n400 * 400 + n100 * 100 + n4 * 4 + n1 + 1;
    let (month, day) = if n1 == 4 || n100 == 4 {
        year -= 1;
        (12, 31)
    } else {
        let leap = is_leap(year);
        let mut month = 1;
        for days in DAYS_IN_MONTH {
            let days = days + i64::from(month == 2 && leap);
            if remaining < days {
                break;
            }
            remaining -= days;
            month += 1;
        }
        (month, remaining + 1)
    };
    Ok(Object::new_tuple(vec![
        Object::Int(year),
        Object::Int(month),
        Object::Int(day),
    ]))
}

// None requests the complete Python path, including subclass validation.
// False certifies built-in numeric types but requests Python normalization.
// The normal success result is a nonempty tuple of normalized components.
fn reference_parts(args: &[Object]) -> Object {
    if args.iter().all(|arg| {
        matches!(
            arg,
            Object::Int(_) | Object::Bool(_) | Object::Long(_) | Object::Float(_)
        )
    }) {
        Object::Bool(false)
    } else {
        Object::None
    }
}

fn timedelta_parts(args: &[Object]) -> Result<Object, RuntimeError> {
    if args.len() != 7 {
        return Err(type_error("timedelta_parts() requires seven components"));
    }
    // Only exact built-in integers can bypass Python arithmetic. Floats,
    // integer subclasses, and huge intermediates retain the reference path.
    // A wide intermediate allows large components to cancel before checking
    // the final day bound, including values beyond an i64 microsecond count.
    const MICROS: [i128; 7] = [
        86_400_000_000,
        1_000_000,
        1,
        1_000,
        60_000_000,
        3_600_000_000,
        604_800_000_000,
    ];
    let mut total = 0i128;
    for (arg, factor) in args.iter().zip(MICROS) {
        let value = match arg {
            Object::Int(value) => i128::from(*value),
            Object::Bool(value) => i128::from(*value),
            Object::Long(value) => match value.to_i128() {
                Some(value) => value,
                None => return Ok(reference_parts(args)),
            },
            Object::Float(_) => return Ok(reference_parts(args)),
            _ => return Ok(Object::None),
        };
        let Some(next) = value.checked_mul(factor).and_then(|n| total.checked_add(n)) else {
            return Ok(reference_parts(args));
        };
        total = next;
    }
    let days = total.div_euclid(MICROS[0]);
    if !(-999_999_999..=999_999_999).contains(&days) {
        return Err(overflow_error(format!(
            "timedelta # of days is too large: {days}"
        )));
    }
    let remainder = total.rem_euclid(MICROS[0]);
    Ok(Object::new_tuple(vec![
        Object::Int(days as i64),
        Object::Int((remainder / 1_000_000) as i64),
        Object::Int((remainder % 1_000_000) as i64),
    ]))
}
