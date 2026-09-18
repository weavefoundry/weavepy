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
        ("parse_time_parts", parse_time_parts as Helper),
        ("date_fields", date_fields as Helper),
        ("time_fields", time_fields as Helper),
        ("format_time_parts", format_time_parts as Helper),
        ("install_native", super::datetime_native::install as Helper),
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

struct TimeText {
    bytes: [u8; 15],
    len: usize,
}

impl TimeText {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("ASCII time fields")
    }
}

/// Decline invalid fields and unknown precision; callers retain their fallback.
fn ascii_time_text(
    hour: i64,
    minute: i64,
    second: i64,
    micros: i64,
    spec: &str,
) -> Option<TimeText> {
    if !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
        || !(0..=999_999).contains(&micros)
    {
        return None;
    }
    let len = match spec {
        "hours" => 2,
        "minutes" => 5,
        "seconds" => 8,
        "milliseconds" => 12,
        "microseconds" => 15,
        "auto" => {
            if micros == 0 {
                8
            } else {
                15
            }
        }
        _ => return None,
    };
    let mut bytes = [b'0'; 15];
    for (offset, field) in [(0, hour), (3, minute), (6, second)] {
        bytes[offset] += (field / 10) as u8;
        bytes[offset + 1] += (field % 10) as u8;
    }
    bytes[2] = b':';
    bytes[5] = b':';
    bytes[8] = b'.';
    let mut fraction = micros;
    for byte in bytes[9..].iter_mut().rev() {
        *byte += (fraction % 10) as u8;
        fraction /= 10;
    }
    Some(TimeText { bytes, len })
}

const DAYS_BEFORE_MONTH: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
const DAYS_IN_MONTH: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Format exact ordinary fields without Python format callbacks or a format map.
/// Unusual values, subclasses, and all errors retain the existing Python path.
fn format_time_parts(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Int(hour), Object::Int(minute), Object::Int(second), Object::Int(micros), Object::Str(spec)] =
        args
    else {
        return Ok(Object::None);
    };
    Ok(ascii_time_text(*hour, *minute, *second, *micros, spec)
        .map_or(Object::None, |text| Object::from_str(text.as_str())))
}

/// Parse exact ASCII time components without Python slicing or digit callbacks.
/// None leaves unsupported inputs and all errors to the existing Python parser.
fn ascii_time_parts(text: &[u8]) -> Option<[i64; 4]> {
    let mut parts = [0; 4];
    let mut pos = 0;
    let mut separated = false;
    for (component, part) in parts[..3].iter_mut().enumerate() {
        let digits = text.get(pos..pos + 2)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *part = i64::from(digits[0] - b'0') * 10 + i64::from(digits[1] - b'0');
        pos += 2;
        if component == 0 {
            separated = text.get(pos) == Some(&b':');
        }
        if pos == text.len() || component == 2 {
            break;
        }
        if separated {
            if text.get(pos) != Some(&b':') {
                return None;
            }
            pos += 1;
        }
    }
    if pos < text.len() {
        if !matches!(text[pos], b'.' | b',') {
            return None;
        }
        let fraction = &text[pos + 1..];
        // Short fractions use the Python module's correction table. Keep
        // that path, including any replacement of the table or its entries.
        if fraction.len() < 6 || !fraction.iter().all(u8::is_ascii_digit) {
            return None;
        }
        parts[3] = fraction[..6]
            .iter()
            .fold(0, |value, digit| value * 10 + i64::from(digit - b'0'));
    }
    Some(parts)
}

fn parse_time_parts(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Str(text)] = args else {
        return Ok(Object::None);
    };
    Ok(match ascii_time_parts(text.as_bytes()) {
        Some(parts) => Object::new_list(parts.into_iter().map(Object::Int).collect()),
        None => Object::None,
    })
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Accept only already-normalized fields. Leave conversion, callbacks, and
/// every invalid-input error to the Python validation functions.
fn date_fields(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Int(year), Object::Int(month), Object::Int(day)] = args else {
        return Ok(Object::None);
    };
    if !(1..=9999).contains(year) || !(1..=12).contains(month) {
        return Ok(Object::None);
    }
    let days = DAYS_IN_MONTH[(*month - 1) as usize] + i64::from(*month == 2 && is_leap(*year));
    if !(1..=days).contains(day) {
        return Ok(Object::None);
    }
    Ok(Object::new_tuple_array([
        Object::Int(*year),
        Object::Int(*month),
        Object::Int(*day),
    ]))
}

fn time_fields(args: &[Object]) -> Result<Object, RuntimeError> {
    let [Object::Int(hour), Object::Int(minute), Object::Int(second), Object::Int(microsecond), Object::Int(fold)] =
        args
    else {
        return Ok(Object::None);
    };
    if !(0..=23).contains(hour)
        || !(0..=59).contains(minute)
        || !(0..=59).contains(second)
        || !(0..=999_999).contains(microsecond)
        || !(0..=1).contains(fold)
    {
        return Ok(Object::None);
    }
    Ok(Object::new_tuple_array([
        Object::Int(*hour),
        Object::Int(*minute),
        Object::Int(*second),
        Object::Int(*microsecond),
        Object::Int(*fold),
    ]))
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
    Ok(Object::new_tuple_array([
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
    Ok(Object::new_tuple_array([
        Object::Int(days as i64),
        Object::Int((remainder / 1_000_000) as i64),
        Object::Int((remainder % 1_000_000) as i64),
    ]))
}

#[cfg(test)]
mod tests {
    use super::ascii_time_parts;

    #[test]
    fn ascii_time_components_preserve_shape_and_fraction_truncation() {
        for (text, expected) in [
            ("00", [0, 0, 0, 0]),
            ("1234", [12, 34, 0, 0]),
            ("12:34", [12, 34, 0, 0]),
            ("123456", [12, 34, 56, 0]),
            ("12:34:56", [12, 34, 56, 0]),
            ("12:34:56.123456", [12, 34, 56, 123_456]),
            ("123456,000001999", [12, 34, 56, 1]),
            // Range validation belongs to the original caller.
            ("99:99:99", [99, 99, 99, 0]),
            ("24:00:00.000000", [24, 0, 0, 0]),
        ] {
            assert_eq!(ascii_time_parts(text.as_bytes()), Some(expected), "{text}");
        }
    }

    #[test]
    fn unsupported_time_inputs_request_the_existing_parser() {
        for text in [
            "",
            "1",
            "123",
            "12:",
            "12:3",
            "12:3456",
            "1234:56",
            "12.123456",
            "12:34.123456",
            "12:34:56.",
            "12:34:56.1",
            "12:34:56.12345",
            "12:34:56.123456x",
            "12:34:56+00:00",
            "12:34:56Z",
            "１２:３４:５６",
            "12:34:56.١٢٣٤٥٦",
            "-1",
            "+1",
        ] {
            assert_eq!(ascii_time_parts(text.as_bytes()), None, "{text}");
        }
    }
}

#[cfg(test)]
mod time_format_tests {
    use super::ascii_time_text as format_time;

    #[test]
    fn object_adapter_accepts_only_exact_supported_fields() {
        use crate::object::Object;
        let good = [
            Object::Int(3),
            Object::Int(4),
            Object::Int(5),
            Object::Int(1999),
            Object::from_str("milliseconds"),
        ];
        assert!(
            matches!(super::format_time_parts(&good).unwrap(), Object::Str(s) if &*s == "03:04:05.001")
        );
        for index in 0..4 {
            for replacement in [
                Object::Bool(true),
                Object::Float(1.0),
                Object::None,
                Object::from_str("1"),
            ] {
                let mut args = good.clone();
                args[index] = replacement;
                assert!(matches!(
                    super::format_time_parts(&args).unwrap(),
                    Object::None
                ));
            }
        }
        let mut args = good.clone();
        args[4] = Object::Bool(false);
        assert!(matches!(
            super::format_time_parts(&args).unwrap(),
            Object::None
        ));
        assert!(matches!(
            super::format_time_parts(&good[..4]).unwrap(),
            Object::None
        ));
    }

    #[test]
    fn precisions_pad_and_truncate_without_rounding() {
        for (spec, expected) in [
            ("hours", "03"),
            ("minutes", "03:04"),
            ("seconds", "03:04:05"),
            ("milliseconds", "03:04:05.001"),
            ("microseconds", "03:04:05.001999"),
            ("auto", "03:04:05.001999"),
        ] {
            assert_eq!(format_time(3, 4, 5, 1999, spec).unwrap().as_str(), expected);
        }
    }

    #[test]
    fn automatic_precision_depends_only_on_nonzero_fraction() {
        for (us, expected) in [
            (0, "23:59:59"),
            (1, "23:59:59.000001"),
            (999, "23:59:59.000999"),
            (1000, "23:59:59.001000"),
            (999_999, "23:59:59.999999"),
        ] {
            assert_eq!(
                format_time(23, 59, 59, us, "auto").unwrap().as_str(),
                expected
            );
        }
        assert_eq!(
            format_time(0, 0, 0, 0, "microseconds").unwrap().as_str(),
            "00:00:00.000000"
        );
    }

    #[test]
    fn out_of_range_and_unknown_values_request_fallback() {
        for fields in [
            [i64::MIN, 0, 0, 0],
            [i64::MAX, 0, 0, 0],
            [24, 0, 0, 0],
            [0, -1, 0, 0],
            [0, 60, 0, 0],
            [0, 0, -1, 0],
            [0, 0, 60, 0],
            [0, 0, 0, -1],
            [0, 0, 0, 1_000_000],
            [0, 0, 0, i64::MAX],
        ] {
            assert!(format_time(fields[0], fields[1], fields[2], fields[3], "hours").is_none());
        }
        for spec in ["", "Auto", "nanoseconds", "秒", "seconds\0"] {
            assert!(format_time(1, 2, 3, 4, spec).is_none());
        }
    }
}
